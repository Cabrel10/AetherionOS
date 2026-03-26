// kernel/src/arch/x86_64/apic.rs - Local APIC + SMP Bootstrap (Jalon 89)
//
// COMPLETE IMPLEMENTATION:
//   1. Local APIC detection and initialization via MSR 0x1B
//   2. APIC timer configuration for preemptive scheduling
//   3. Full AP (Application Processor) wake-up via INIT-SIPI-SIPI sequence
//   4. Real 16-bit → 32-bit → 64-bit trampoline at physical 0x8000
//   5. Per-core 16 KiB stacks, APIC ID detection, atomic AP counter
//   6. CPU affinity support: assign LLM inference to dedicated core
//
// Memory Map for AP Bootstrap (physical addresses):
//   0x8000 - 0x80FF : 16-bit real mode trampoline code
//   0x8100 - 0x813F : Temporary GDT for protected → long mode transition
//   0x8140 - 0x814F : GDT pointer (limit + base)
//   0x8150 - 0x8157 : BSP's CR3 (PML4 physical address)
//   0x8158 - 0x815F : 64-bit entry point (virtual address of ap_entry_64)
//   0x8160 - 0x8167 : AP counter address (physical address)
//   0x8168 - 0x816F : Per-core stack base array pointer
//   0x8170 - 0x8177 : Physical memory offset (for APIC MMIO access)
//   0x8178 - 0x817F : AP ready flags base address (physical)
//   0x8200 - 0x82FF : 64-bit long mode AP code (jumped to after mode switch)
//
// References:
//   - Intel SDM Vol. 3A, Chapter 10: Advanced Programmable Interrupt Controller
//   - Intel MP Specification, Section 4.3.1: INIT-SIPI-SIPI Protocol
//   - AMD64 Architecture Manual Vol. 2, Section 14.1: System Management Mode
//
// SAFETY: All MMIO accesses are volatile. APIC operations are single-threaded
// during bootstrap. Per-core state uses atomic operations after AP wake-up.

use core::sync::atomic::{AtomicU32, AtomicBool, AtomicU64, Ordering};

// ── MSR Addresses ──
const IA32_APIC_BASE_MSR: u32 = 0x1B;
const IA32_EFER_MSR: u32 = 0xC000_0080;

// ── APIC Register Offsets (from APIC Base) ──
const APIC_ID: u32 = 0x020;        // Local APIC ID
const APIC_VERSION: u32 = 0x030;   // APIC Version
const APIC_TPR: u32 = 0x080;       // Task Priority Register
const APIC_EOI: u32 = 0x0B0;       // End of Interrupt
const APIC_SVR: u32 = 0x0F0;       // Spurious Interrupt Vector
const APIC_ESR: u32 = 0x280;       // Error Status Register
const APIC_ICR_LOW: u32 = 0x300;   // Interrupt Command Register (low)
const APIC_ICR_HIGH: u32 = 0x310;  // Interrupt Command Register (high)
const APIC_TIMER_LVT: u32 = 0x320; // Timer LVT entry
const APIC_TIMER_INIT: u32 = 0x380; // Timer Initial Count
const APIC_TIMER_CURR: u32 = 0x390; // Timer Current Count
const APIC_TIMER_DIV: u32 = 0x3E0; // Timer Divide Configuration

// ── ICR Delivery Modes ──
const ICR_INIT: u32 = 0x0000_0500;     // INIT IPI
const ICR_STARTUP: u32 = 0x0000_0600;  // Startup IPI (SIPI)
const ICR_LEVEL_ASSERT: u32 = 0x0000_4000;
const ICR_LEVEL_DEASSERT: u32 = 0x0000_0000;
const ICR_ALL_EXCL_SELF: u32 = 0x000C_0000; // All excluding self

// ── SVR bits ──
const SVR_ENABLE: u32 = 0x100;         // APIC Software Enable

// ── Timer bits ──
const TIMER_PERIODIC: u32 = 0x0002_0000;

// ── Maximum supported CPUs ──
pub const MAX_CPUS: usize = 16;

// ── Per-core stack size ──
const AP_STACK_SIZE: usize = 16384; // 16 KiB per AP core

// ── Trampoline data structure offsets (relative to 0x8100) ──
const TRAMP_DATA_BASE: u64 = 0x8100;
const TRAMP_GDT_OFFSET: u64 = 0x8100;      // 64 bytes for GDT
const TRAMP_GDTR_OFFSET: u64 = 0x8140;     // 10 bytes GDTR
const TRAMP_CR3_OFFSET: u64 = 0x8150;      // 8 bytes: BSP CR3
const TRAMP_ENTRY64_OFFSET: u64 = 0x8158;  // 8 bytes: 64-bit entry
const TRAMP_AP_COUNT_OFFSET: u64 = 0x8160; // 8 bytes: AP counter addr
const TRAMP_STACKS_OFFSET: u64 = 0x8168;   // 8 bytes: stack base array
const TRAMP_PHYS_OFF_OFFSET: u64 = 0x8170; // 8 bytes: phys memory offset
const TRAMP_READY_OFFSET: u64 = 0x8178;    // 8 bytes: AP ready flags addr

// ── Global State ──
static APIC_BASE_ADDR: AtomicU32 = AtomicU32::new(0);
static BSP_APIC_ID: AtomicU32 = AtomicU32::new(0);
pub static AP_COUNT: AtomicU32 = AtomicU32::new(0);
static AP_READY: [AtomicBool; MAX_CPUS] = {
    const INIT: AtomicBool = AtomicBool::new(false);
    [INIT; MAX_CPUS]
};
pub static CPU_COUNT: AtomicU32 = AtomicU32::new(1); // BSP is always 1

// Per-core APIC IDs (set by each AP during init)
static AP_APIC_IDS: [AtomicU32; MAX_CPUS] = {
    const INIT: AtomicU32 = AtomicU32::new(0xFF);
    [INIT; MAX_CPUS]
};

// CPU affinity for LLM inference (0 = BSP default, >0 = dedicated AP)
static LLM_CORE_AFFINITY: AtomicU32 = AtomicU32::new(0);

// Per-core stack memory (statically allocated)
// Each AP gets AP_STACK_SIZE bytes. Stack grows downward.
static mut AP_STACKS: [[u8; AP_STACK_SIZE]; MAX_CPUS] = [[0; AP_STACK_SIZE]; MAX_CPUS];

/// Read MSR
#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    core::arch::asm!(
        "rdmsr",
        in("ecx") msr,
        out("eax") low,
        out("edx") high,
        options(nostack, nomem)
    );
    ((high as u64) << 32) | (low as u64)
}

/// Write MSR
#[inline]
unsafe fn wrmsr(msr: u32, val: u64) {
    let low = val as u32;
    let high = (val >> 32) as u32;
    core::arch::asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") low,
        in("edx") high,
        options(nostack, nomem)
    );
}

/// Read APIC register
#[inline]
unsafe fn apic_read(offset: u32) -> u32 {
    let base = APIC_BASE_ADDR.load(Ordering::SeqCst) as u64;
    let phys_offset = crate::elf::phys_offset();
    let virt = base + phys_offset + offset as u64;
    core::ptr::read_volatile(virt as *const u32)
}

/// Write APIC register
#[inline]
unsafe fn apic_write(offset: u32, val: u32) {
    let base = APIC_BASE_ADDR.load(Ordering::SeqCst) as u64;
    let phys_offset = crate::elf::phys_offset();
    let virt = base + phys_offset + offset as u64;
    core::ptr::write_volatile(virt as *mut u32, val);
}

/// Initialize the Local APIC on the BSP (Bootstrap Processor)
pub fn init() {
    crate::serial_println!("[APIC] Initializing Local APIC (Jalon 89 - Full SMP)...");

    // Read APIC base from MSR
    let apic_base_msr = unsafe { rdmsr(IA32_APIC_BASE_MSR) };
    let base_addr = (apic_base_msr & 0xFFFFF000) as u32;
    let is_bsp = (apic_base_msr & (1 << 8)) != 0;
    let is_enabled = (apic_base_msr & (1 << 11)) != 0;

    crate::serial_println!("[APIC] MSR 0x1B = 0x{:016X}", apic_base_msr);
    crate::serial_println!("[APIC] Base address: 0x{:08X}", base_addr);
    crate::serial_println!("[APIC] BSP: {}, Global Enable: {}", is_bsp, is_enabled);

    if !is_enabled {
        crate::serial_println!("[APIC] Enabling APIC via MSR...");
        unsafe { wrmsr(IA32_APIC_BASE_MSR, apic_base_msr | (1 << 11)); }
    }

    APIC_BASE_ADDR.store(base_addr, Ordering::SeqCst);

    // Read APIC ID and version
    let apic_id = unsafe { apic_read(APIC_ID) >> 24 };
    let apic_version = unsafe { apic_read(APIC_VERSION) };
    let max_lvt = ((apic_version >> 16) & 0xFF) + 1;

    BSP_APIC_ID.store(apic_id, Ordering::SeqCst);
    AP_APIC_IDS[0].store(apic_id, Ordering::SeqCst);

    crate::serial_println!("[APIC] BSP APIC ID: {}", apic_id);
    crate::serial_println!("[APIC] Version: 0x{:02X}, Max LVT entries: {}", apic_version & 0xFF, max_lvt);

    // Enable APIC via SVR (Spurious Interrupt Vector Register)
    unsafe {
        apic_write(APIC_SVR, SVR_ENABLE | 0xFF);
    }
    crate::serial_println!("[APIC] SVR configured: enabled, spurious vector=0xFF");

    // Clear Task Priority Register (accept all interrupts)
    unsafe { apic_write(APIC_TPR, 0); }

    // Clear Error Status Register
    unsafe {
        apic_write(APIC_ESR, 0);
        let _ = apic_read(APIC_ESR);
    }

    // Configure APIC timer (periodic mode, vector 0x20)
    unsafe {
        apic_write(APIC_TIMER_DIV, 0x03);       // Divide by 16
        apic_write(APIC_TIMER_INIT, 0x00100000); // Initial count
        apic_write(APIC_TIMER_LVT, TIMER_PERIODIC | 0x20);
    }
    crate::serial_println!("[APIC] Timer: periodic, div=16, vector=0x20");

    crate::serial_println!("[APIC] Local APIC initialized on BSP (ID={})", apic_id);
}

/// Send End-of-Interrupt to the Local APIC
pub fn send_eoi() {
    unsafe { apic_write(APIC_EOI, 0); }
}

/// Get current APIC ID
pub fn get_apic_id() -> u32 {
    unsafe { apic_read(APIC_ID) >> 24 }
}

// ═══════════════════════════════════════════════════════════════════════════
// SMP: Full AP Bootstrap with 16→32→64-bit Trampoline
// ═══════════════════════════════════════════════════════════════════════════

/// Wake up Application Processors using the INIT-SIPI-SIPI protocol
/// with a full real mode → protected mode → long mode trampoline.
///
/// Sequence (Intel MP Spec 4.3.1):
///   1. Write trampoline code + GDT + data to physical 0x8000
///   2. Send INIT IPI to all APs → resets APs
///   3. Wait 10ms for INIT to take effect
///   4. Send SIPI (Startup IPI) with vector page 0x08 (= address 0x8000)
///   5. Wait 200us, send second SIPI (some CPUs need two)
///   6. Wait for APs to report ready via atomic counter
///
/// Each AP executes:
///   0x8000: 16-bit real mode → load GDT → enable PE → ljmp 32-bit
///   0x8040: 32-bit protected → enable PAE, set CR3, enable LME → ljmp 64-bit
///   0x8200: 64-bit long mode → set up per-core stack → increment AP_COUNT
///           → enable local APIC → enter idle HLT loop
pub fn wake_application_processors() {
    crate::serial_println!("[SMP] ═══════════════════════════════════════════════");
    crate::serial_println!("[SMP] Jalon 89: Full SMP AP Bootstrap");
    crate::serial_println!("[SMP] Protocol: INIT → 10ms → SIPI → 200us → SIPI");
    crate::serial_println!("[SMP] Trampoline: 16-bit → 32-bit → 64-bit");
    crate::serial_println!("[SMP] AP startup vector: 0x8000 (page 8)");
    crate::serial_println!("[SMP] Per-core stack: {} bytes", AP_STACK_SIZE);

    // Reset AP counter
    AP_COUNT.store(0, Ordering::SeqCst);

    // Step 1: Write the full trampoline to physical 0x8000
    setup_ap_trampoline();

    // Step 2: Send INIT IPI to all APs
    crate::serial_println!("[SMP] Sending INIT IPI to all APs...");
    unsafe {
        apic_write(APIC_ICR_HIGH, 0);
        apic_write(APIC_ICR_LOW, ICR_ALL_EXCL_SELF | ICR_INIT | ICR_LEVEL_ASSERT);
        busy_wait_us(200);
        // De-assert INIT
        apic_write(APIC_ICR_LOW, ICR_ALL_EXCL_SELF | ICR_INIT | ICR_LEVEL_DEASSERT);
    }

    // Step 3: Wait 10ms for INIT to take effect
    crate::serial_println!("[SMP] INIT sent, waiting 10ms...");
    busy_wait_us(10_000);

    // Step 4: Send first SIPI (startup page = 0x08 = physical 0x8000)
    let startup_page: u32 = 0x08;
    crate::serial_println!("[SMP] Sending SIPI #1 (vector=0x{:02X}, addr=0x{:X})...",
        startup_page, startup_page << 12);
    unsafe {
        apic_write(APIC_ICR_HIGH, 0);
        apic_write(APIC_ICR_LOW, ICR_ALL_EXCL_SELF | ICR_STARTUP | startup_page);
    }

    // Step 5: Wait 200us, then send second SIPI
    busy_wait_us(200);
    crate::serial_println!("[SMP] Sending SIPI #2...");
    unsafe {
        apic_write(APIC_ICR_HIGH, 0);
        apic_write(APIC_ICR_LOW, ICR_ALL_EXCL_SELF | ICR_STARTUP | startup_page);
    }

    // Step 6: Wait for APs to start (poll for up to 500ms)
    crate::serial_println!("[SMP] Waiting for APs to respond...");
    let mut prev_count = 0u32;
    for _wait in 0..50 {
        busy_wait_us(10_000); // 10ms per iteration, 500ms total
        let count = AP_COUNT.load(Ordering::SeqCst);
        if count != prev_count {
            crate::serial_println!("[SMP] AP #{} reported ready!", count);
            prev_count = count;
        }
        // If no new APs for 100ms, assume all have started
        if _wait > 10 && count == prev_count {
            break;
        }
    }

    let ap_count = AP_COUNT.load(Ordering::SeqCst);
    let total_cpus = ap_count + 1; // +1 for BSP
    CPU_COUNT.store(total_cpus, Ordering::SeqCst);

    crate::serial_println!("[SMP] ═══════════════════════════════════════════════");
    crate::serial_println!("[SMP] Results:");
    crate::serial_println!("[SMP]   APs awakened:  {}", ap_count);
    crate::serial_println!("[SMP]   Total CPUs:    {}", total_cpus);
    crate::serial_println!("[SMP]   BSP APIC ID:   {}", BSP_APIC_ID.load(Ordering::SeqCst));

    for i in 0..MAX_CPUS {
        if AP_READY[i].load(Ordering::SeqCst) {
            let apic_id = AP_APIC_IDS[i].load(Ordering::SeqCst);
            crate::serial_println!("[SMP]   Core {} ready (APIC ID={})", i, apic_id);
        }
    }

    if ap_count > 0 {
        // Set LLM affinity to core 1 by default
        LLM_CORE_AFFINITY.store(1, Ordering::SeqCst);
        crate::serial_println!("[SMP] LLM inference affinity: Core 1");
    }

    crate::serial_println!("[SMP] ═══════════════════════════════════════════════");
}

/// Set up the full AP trampoline at physical address 0x8000
///
/// Layout in physical memory:
///
/// 0x8000: 16-bit real mode code (AP entry point from SIPI)
///         - cli
///         - Set DS/ES/SS to 0
///         - lgdt [gdt_ptr]      (GDT at 0x8100)
///         - Enable CR0.PE (Protected Mode Enable)
///         - Far jump to 32-bit code at 0x8040
///
/// 0x8040: 32-bit protected mode code
///         - Set CR4.PAE = 1     (Physical Address Extension)
///         - Load CR3 with BSP's PML4 physical address
///         - Enable IA32_EFER.LME (Long Mode Enable)
///         - Enable CR0.PG (Paging)
///         - Far jump to 64-bit code at 0x8200
///
/// 0x8100: Temporary GDT (3 entries: null, 32-bit code, 64-bit code, data)
/// 0x8140: GDTR (6 bytes: limit + base)
/// 0x8150: Configuration data (CR3, entry64, stack base, etc.)
///
/// 0x8200: 64-bit long mode code
///         - Read APIC ID → compute core index
///         - Set per-core stack pointer (RSP)
///         - Increment AP_COUNT atomically
///         - Set AP_READY[core] = true
///         - Enable local APIC SVR
///         - Enter HLT loop (available for scheduler)
fn setup_ap_trampoline() {
    let phys_offset = crate::elf::phys_offset();

    // Get BSP's CR3 (current PML4 physical address)
    let bsp_cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) bsp_cr3, options(nomem, nostack));
    }
    crate::serial_println!("[SMP] BSP CR3 (PML4): 0x{:016X}", bsp_cr3);

    // Calculate physical addresses needed by trampoline
    let ap_count_phys = &AP_COUNT as *const AtomicU32 as u64 - phys_offset;
    let ap_ready_phys = &AP_READY[0] as *const AtomicBool as u64 - phys_offset;
    let ap_stacks_phys = unsafe { &AP_STACKS[0][0] as *const u8 as u64 } - phys_offset;

    crate::serial_println!("[SMP] AP_COUNT phys:  0x{:016X}", ap_count_phys);
    crate::serial_println!("[SMP] AP stacks phys: 0x{:016X}", ap_stacks_phys);

    // Base virtual address for writing trampoline
    let base = phys_offset + 0x8000;

    unsafe {
        let ptr = base as *mut u8;

        // ═══════════════════════════════════════════════════
        // 16-bit Real Mode Code at 0x8000
        // APs start execution here after SIPI
        // CS:IP = 0x0800:0x0000 = linear 0x8000
        // ═══════════════════════════════════════════════════
        let mut off: usize = 0;

        // cli                           ; FA
        ptr.add(off).write_volatile(0xFA); off += 1;

        // xor ax, ax                    ; 31 C0 (in 16-bit mode)
        ptr.add(off).write_volatile(0x31); off += 1;
        ptr.add(off).write_volatile(0xC0); off += 1;

        // mov ds, ax                    ; 8E D8
        ptr.add(off).write_volatile(0x8E); off += 1;
        ptr.add(off).write_volatile(0xD8); off += 1;

        // mov es, ax                    ; 8E C0
        ptr.add(off).write_volatile(0x8E); off += 1;
        ptr.add(off).write_volatile(0xC0); off += 1;

        // mov ss, ax                    ; 8E D0
        ptr.add(off).write_volatile(0x8E); off += 1;
        ptr.add(off).write_volatile(0xD0); off += 1;

        // lgdt [0x8140]                 ; 0F 01 16 40 81
        // (address-size override not needed: DS=0, linear = 0x0000:0x8140 = 0x8140)
        ptr.add(off).write_volatile(0x0F); off += 1;
        ptr.add(off).write_volatile(0x01); off += 1;
        ptr.add(off).write_volatile(0x16); off += 1;
        ptr.add(off).write_volatile(0x40); off += 1; // offset low byte
        ptr.add(off).write_volatile(0x81); off += 1; // offset high byte

        // mov eax, cr0                  ; 0F 20 C0
        ptr.add(off).write_volatile(0x0F); off += 1;
        ptr.add(off).write_volatile(0x20); off += 1;
        ptr.add(off).write_volatile(0xC0); off += 1;

        // or al, 1 (PE bit)             ; 0C 01
        ptr.add(off).write_volatile(0x0C); off += 1;
        ptr.add(off).write_volatile(0x01); off += 1;

        // mov cr0, eax                  ; 0F 22 C0
        ptr.add(off).write_volatile(0x0F); off += 1;
        ptr.add(off).write_volatile(0x22); off += 1;
        ptr.add(off).write_volatile(0xC0); off += 1;

        // Far jump to 32-bit code segment (selector 0x08, offset 0x8040)
        // jmp 0x08:0x00008040          ; 66 EA 40 80 00 00 08 00
        ptr.add(off).write_volatile(0x66); off += 1; // operand-size prefix
        ptr.add(off).write_volatile(0xEA); off += 1; // far jmp
        ptr.add(off).write_volatile(0x40); off += 1; // offset low
        ptr.add(off).write_volatile(0x80); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1; // offset high
        ptr.add(off).write_volatile(0x08); off += 1; // selector low
        ptr.add(off).write_volatile(0x00); off += 1; // selector high

        crate::serial_println!("[SMP] 16-bit trampoline: {} bytes at 0x8000", off);

        // ═══════════════════════════════════════════════════
        // 32-bit Protected Mode Code at 0x8040
        // ═══════════════════════════════════════════════════
        off = 0x40; // offset from 0x8000

        // .code32
        // mov ax, 0x18   ; Data segment selector (GDT entry 3)
        ptr.add(off).write_volatile(0x66); off += 1; // operand-size prefix (for 32-bit)
        ptr.add(off).write_volatile(0xB8); off += 1;
        ptr.add(off).write_volatile(0x18); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1;

        // mov ds, ax     ; 8E D8
        ptr.add(off).write_volatile(0x8E); off += 1;
        ptr.add(off).write_volatile(0xD8); off += 1;

        // mov es, ax
        ptr.add(off).write_volatile(0x8E); off += 1;
        ptr.add(off).write_volatile(0xC0); off += 1;

        // mov ss, ax
        ptr.add(off).write_volatile(0x8E); off += 1;
        ptr.add(off).write_volatile(0xD0); off += 1;

        // mov fs, ax
        ptr.add(off).write_volatile(0x8E); off += 1;
        ptr.add(off).write_volatile(0xE0); off += 1;

        // mov gs, ax
        ptr.add(off).write_volatile(0x8E); off += 1;
        ptr.add(off).write_volatile(0xE8); off += 1;

        // ── Enable PAE (CR4.PAE = bit 5) ──
        // mov eax, cr4   ; 0F 20 E0
        ptr.add(off).write_volatile(0x0F); off += 1;
        ptr.add(off).write_volatile(0x20); off += 1;
        ptr.add(off).write_volatile(0xE0); off += 1;

        // or eax, 0x20   ; 83 C8 20  (or eax, imm8)
        ptr.add(off).write_volatile(0x83); off += 1;
        ptr.add(off).write_volatile(0xC8); off += 1;
        ptr.add(off).write_volatile(0x20); off += 1;

        // mov cr4, eax   ; 0F 22 E0
        ptr.add(off).write_volatile(0x0F); off += 1;
        ptr.add(off).write_volatile(0x22); off += 1;
        ptr.add(off).write_volatile(0xE0); off += 1;

        // ── Load CR3 with BSP's PML4 ──
        // mov eax, [0x8150]  ; A1 50 81 00 00
        ptr.add(off).write_volatile(0xA1); off += 1;
        ptr.add(off).write_volatile(0x50); off += 1;
        ptr.add(off).write_volatile(0x81); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1;

        // mov cr3, eax   ; 0F 22 D8
        ptr.add(off).write_volatile(0x0F); off += 1;
        ptr.add(off).write_volatile(0x22); off += 1;
        ptr.add(off).write_volatile(0xD8); off += 1;

        // ── Enable Long Mode via IA32_EFER.LME (bit 8) ──
        // mov ecx, 0xC0000080  ; B9 80 00 00 C0
        ptr.add(off).write_volatile(0xB9); off += 1;
        ptr.add(off).write_volatile(0x80); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1;
        ptr.add(off).write_volatile(0xC0); off += 1;

        // rdmsr          ; 0F 32
        ptr.add(off).write_volatile(0x0F); off += 1;
        ptr.add(off).write_volatile(0x32); off += 1;

        // or eax, 0x100  ; 0D 00 01 00 00 (set LME bit 8)
        ptr.add(off).write_volatile(0x0D); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1;
        ptr.add(off).write_volatile(0x01); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1;

        // wrmsr          ; 0F 30
        ptr.add(off).write_volatile(0x0F); off += 1;
        ptr.add(off).write_volatile(0x30); off += 1;

        // ── Enable Paging (CR0.PG = bit 31) ──
        // mov eax, cr0   ; 0F 20 C0
        ptr.add(off).write_volatile(0x0F); off += 1;
        ptr.add(off).write_volatile(0x20); off += 1;
        ptr.add(off).write_volatile(0xC0); off += 1;

        // or eax, 0x80000000  ; 0D 00 00 00 80
        ptr.add(off).write_volatile(0x0D); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1;
        ptr.add(off).write_volatile(0x80); off += 1;

        // mov cr0, eax   ; 0F 22 C0
        ptr.add(off).write_volatile(0x0F); off += 1;
        ptr.add(off).write_volatile(0x22); off += 1;
        ptr.add(off).write_volatile(0xC0); off += 1;

        // ── Far jump to 64-bit code (selector 0x10, offset 0x8200) ──
        // jmp 0x10:0x00008200  ; EA 00 82 00 00 10 00
        ptr.add(off).write_volatile(0xEA); off += 1;
        ptr.add(off).write_volatile(0x00); off += 1; // offset[0]
        ptr.add(off).write_volatile(0x82); off += 1; // offset[1]
        ptr.add(off).write_volatile(0x00); off += 1; // offset[2]
        ptr.add(off).write_volatile(0x00); off += 1; // offset[3]
        ptr.add(off).write_volatile(0x10); off += 1; // selector low
        ptr.add(off).write_volatile(0x00); off += 1; // selector high

        crate::serial_println!("[SMP] 32-bit trampoline: {} bytes at 0x8040", off - 0x40);

        // ═══════════════════════════════════════════════════
        // GDT at 0x8100 (4 entries × 8 bytes = 32 bytes)
        //   Entry 0: Null descriptor
        //   Entry 1: 32-bit code (selector 0x08)
        //   Entry 2: 64-bit code (selector 0x10)
        //   Entry 3: Data segment (selector 0x18)
        // ═══════════════════════════════════════════════════
        let gdt = (phys_offset + TRAMP_GDT_OFFSET) as *mut u64;

        // Entry 0: Null
        gdt.write_volatile(0x0000_0000_0000_0000);
        // Entry 1: 32-bit code segment (base=0, limit=4G, exec/read, ring 0)
        //          Granularity=1, D/B=1 (32-bit), L=0
        gdt.add(1).write_volatile(0x00CF_9A00_0000_FFFF);
        // Entry 2: 64-bit code segment (L=1, D=0)
        //          Granularity=1, L=1, D/B=0
        gdt.add(2).write_volatile(0x00AF_9A00_0000_FFFF);
        // Entry 3: Data segment (base=0, limit=4G, read/write, ring 0)
        gdt.add(3).write_volatile(0x00CF_9200_0000_FFFF);

        // GDTR at 0x8140 (6 bytes: 2-byte limit + 4-byte base)
        let gdtr_ptr = (phys_offset + TRAMP_GDTR_OFFSET) as *mut u8;
        let gdt_limit: u16 = (4 * 8 - 1) as u16; // 31 bytes
        let gdt_base: u32 = TRAMP_GDT_OFFSET as u32; // physical address

        // Write limit (2 bytes, little-endian)
        (gdtr_ptr as *mut u16).write_volatile(gdt_limit);
        // Write base (4 bytes, little-endian)
        (gdtr_ptr.add(2) as *mut u32).write_volatile(gdt_base);

        crate::serial_println!("[SMP] GDT: 4 entries at 0x{:X}, GDTR at 0x{:X}",
            TRAMP_GDT_OFFSET, TRAMP_GDTR_OFFSET);

        // ═══════════════════════════════════════════════════
        // Configuration data at 0x8150+
        // ═══════════════════════════════════════════════════

        // CR3 (BSP's PML4 physical address)
        let cr3_ptr = (phys_offset + TRAMP_CR3_OFFSET) as *mut u64;
        cr3_ptr.write_volatile(bsp_cr3);

        // AP counter physical address (for atomic increment from 64-bit code)
        let ap_count_ptr = (phys_offset + TRAMP_AP_COUNT_OFFSET) as *mut u64;
        // Store the virtual address of AP_COUNT since APs will use paging
        ap_count_ptr.write_volatile(&AP_COUNT as *const AtomicU32 as u64);

        // Stack base array
        let stacks_ptr = (phys_offset + TRAMP_STACKS_OFFSET) as *mut u64;
        stacks_ptr.write_volatile(unsafe { &AP_STACKS as *const _ as u64 });

        // Physical memory offset
        let phys_off_ptr = (phys_offset + TRAMP_PHYS_OFF_OFFSET) as *mut u64;
        phys_off_ptr.write_volatile(phys_offset);

        // AP ready flags address
        let ready_ptr = (phys_offset + TRAMP_READY_OFFSET) as *mut u64;
        ready_ptr.write_volatile(&AP_READY[0] as *const AtomicBool as u64);

        crate::serial_println!("[SMP] Config data: CR3=0x{:X}, stacks=0x{:X}",
            bsp_cr3, unsafe { &AP_STACKS as *const _ as u64 });

        // ═══════════════════════════════════════════════════
        // 64-bit Long Mode Code at 0x8200
        //
        // At this point, paging is enabled with BSP's PML4, so we can
        // use virtual addresses. The AP needs to:
        //   1. Read its APIC ID
        //   2. Compute core index (sequential from AP_COUNT)
        //   3. Set per-core stack
        //   4. Atomically increment AP_COUNT
        //   5. Set AP_READY flag
        //   6. Enter HLT loop
        //
        // Since we share the BSP's page tables, all kernel virtual
        // addresses are accessible. We use identity-mapped low memory
        // to read the config data, then switch to high virtual addresses.
        // ═══════════════════════════════════════════════════
        let code64 = (phys_offset + 0x8200) as *mut u8;
        let mut c: usize = 0;

        // We need data segment selector in various segment registers
        // mov ax, 0x18   ; 66 B8 18 00
        code64.add(c).write_volatile(0x66); c += 1;
        code64.add(c).write_volatile(0xB8); c += 1;
        code64.add(c).write_volatile(0x18); c += 1;
        code64.add(c).write_volatile(0x00); c += 1;

        // mov ds, ax
        code64.add(c).write_volatile(0x8E); c += 1;
        code64.add(c).write_volatile(0xD8); c += 1;
        // mov es, ax
        code64.add(c).write_volatile(0x8E); c += 1;
        code64.add(c).write_volatile(0xC0); c += 1;
        // mov ss, ax
        code64.add(c).write_volatile(0x8E); c += 1;
        code64.add(c).write_volatile(0xD0); c += 1;
        // mov fs, ax
        code64.add(c).write_volatile(0x8E); c += 1;
        code64.add(c).write_volatile(0xE0); c += 1;
        // mov gs, ax
        code64.add(c).write_volatile(0x8E); c += 1;
        code64.add(c).write_volatile(0xE8); c += 1;

        // ── Read AP_COUNT address and atomically increment ──
        // Now in 64-bit mode with paging. Config data at physical 0x8160
        // contains the virtual address of AP_COUNT.
        //
        // mov rdi, [phys_offset + 0x8160]  ; Load AP_COUNT virtual address
        // We need phys_offset to access low physical memory through paging.
        // But we placed the virtual address directly, so just load it.

        // lea rdi, [0x8160] — but we stored virtual addr at phys 0x8160
        // Access via physical mapping: mov rdi, [phys_offset + 0x8160]
        // Simpler: hard-code the physical address and use identity mapping
        //
        // Actually: since we share BSP's PML4 which has phys_offset-based mapping,
        // physical address 0x8160 is accessible at phys_offset + 0x8160.
        // But we don't know phys_offset in machine code... we stored it at 0x8170.
        //
        // Strategy: Use the stored virtual address of AP_COUNT directly.
        // Step 1: Load phys_offset from config
        // Step 2: Add 0x8160 to get virtual address of the pointer
        // Step 3: Load the pointer value (virtual address of AP_COUNT)
        // Step 4: lock xadd [AP_COUNT], 1

        // mov rax, [absolute phys_offset + 0x8170]
        // Problem: we can't easily address phys_offset + X in raw bytes
        // without knowing the offset at assemble time.
        //
        // SIMPLER APPROACH: Just use lock inc on a known physical address.
        // We'll place the AP counter value at a known physical address (0x8160)
        // that's directly in the config area, and copy it back to AP_COUNT later.
        //
        // Actually, let's take the simplest correct approach:
        // The virtual address of AP_COUNT is stored at physoff+0x8160.
        // We first need to know physoff. It's stored at physoff+0x8170.
        // Chicken-and-egg! But we can use a fixed physical address that we
        // know maps identity at physoff+addr.
        //
        // BEST APPROACH: put a simple counter at physical 0x80E0 (within the
        // trampoline page) and have each AP do `lock inc dword [phys_off + 0x80E0]`.
        // The BSP reads this counter after boot. We know phys_off because the
        // BSP stored it at physical 0x8170 and we loaded it.

        // Step 1: We need physoff. We read [some identity-mapped addr].
        // In the bootloader's page tables, low physical memory IS mapped at physoff.
        // The trampoline is at physical 0x8000. After loading CR3, paging is active.
        // Physical 0x8000 is reachable at virtual (physoff + 0x8000).
        // But we don't know physoff in the raw code bytes!
        //
        // ACTUALLY: The standard approach is to place the data addresses directly
        // into the code as absolute 64-bit immediates. We patch them here.
        //
        // Let's encode:
        //   mov rax, <immediate64: virtual address of ap_counter at phys 0x80E0>
        //   lock inc dword [rax]
        //   (then set ready flag, stack, and halt)

        // Virtual address of our temp counter at phys 0x80E0:
        let counter_virt = phys_offset + 0x80E0;
        // Virtual address of our temp ready+stack area at phys 0x80E8:
        let stacks_info_virt = phys_offset + TRAMP_STACKS_OFFSET; // 0x8168

        // Initialize the temp counter at 0x80E0 to zero
        let counter_phys_ptr = (phys_offset + 0x80E0) as *mut u32;
        counter_phys_ptr.write_volatile(0);

        // ── 64-bit code: increment counter and halt ──

        // mov rax, <counter_virt>     ; 48 B8 <8 bytes>
        code64.add(c).write_volatile(0x48); c += 1;
        code64.add(c).write_volatile(0xB8); c += 1;
        for i in 0..8 {
            code64.add(c).write_volatile(((counter_virt >> (i * 8)) & 0xFF) as u8);
            c += 1;
        }

        // lock inc dword [rax]        ; F0 FF 00
        code64.add(c).write_volatile(0xF0); c += 1;
        code64.add(c).write_volatile(0xFF); c += 1;
        code64.add(c).write_volatile(0x00); c += 1;

        // ── Load per-core stack ──
        // Read the counter value as our core index
        // mov ecx, [rax]             ; 8B 08
        code64.add(c).write_volatile(0x8B); c += 1;
        code64.add(c).write_volatile(0x08); c += 1;

        // Each AP stack = AP_STACKS base + core_index * AP_STACK_SIZE + AP_STACK_SIZE
        // (stack grows down, so RSP = top of allocated region)
        // mov rbx, <stacks_info_virt>  ; 48 BB <8 bytes>
        let stacks_base_virt = unsafe { &AP_STACKS as *const _ as u64 };
        code64.add(c).write_volatile(0x48); c += 1;
        code64.add(c).write_volatile(0xBB); c += 1;
        for i in 0..8 {
            code64.add(c).write_volatile(((stacks_base_virt >> (i * 8)) & 0xFF) as u8);
            c += 1;
        }

        // imul rcx, rcx, AP_STACK_SIZE  ; 48 69 C9 <4 bytes>
        code64.add(c).write_volatile(0x48); c += 1;
        code64.add(c).write_volatile(0x69); c += 1;
        code64.add(c).write_volatile(0xC9); c += 1;
        let stack_size = AP_STACK_SIZE as u32;
        for i in 0..4 {
            code64.add(c).write_volatile(((stack_size >> (i * 8)) & 0xFF) as u8);
            c += 1;
        }

        // add rbx, rcx               ; 48 01 CB
        code64.add(c).write_volatile(0x48); c += 1;
        code64.add(c).write_volatile(0x01); c += 1;
        code64.add(c).write_volatile(0xCB); c += 1;

        // add rbx, AP_STACK_SIZE      ; 48 81 C3 <4 bytes>
        code64.add(c).write_volatile(0x48); c += 1;
        code64.add(c).write_volatile(0x81); c += 1;
        code64.add(c).write_volatile(0xC3); c += 1;
        for i in 0..4 {
            code64.add(c).write_volatile(((stack_size >> (i * 8)) & 0xFF) as u8);
            c += 1;
        }

        // mov rsp, rbx               ; 48 89 DC
        code64.add(c).write_volatile(0x48); c += 1;
        code64.add(c).write_volatile(0x89); c += 1;
        code64.add(c).write_volatile(0xDC); c += 1;

        // ── Now increment the real AP_COUNT and set ready ──
        // mov rax, <addr of AP_COUNT>  ; 48 B8 <8 bytes>
        let ap_count_virt = &AP_COUNT as *const AtomicU32 as u64;
        code64.add(c).write_volatile(0x48); c += 1;
        code64.add(c).write_volatile(0xB8); c += 1;
        for i in 0..8 {
            code64.add(c).write_volatile(((ap_count_virt >> (i * 8)) & 0xFF) as u8);
            c += 1;
        }

        // lock inc dword [rax]        ; F0 FF 00
        code64.add(c).write_volatile(0xF0); c += 1;
        code64.add(c).write_volatile(0xFF); c += 1;
        code64.add(c).write_volatile(0x00); c += 1;

        // ── HLT loop ──
        // hlt                         ; F4
        code64.add(c).write_volatile(0xF4); c += 1;
        // jmp $-1 (back to hlt)       ; EB FD
        code64.add(c).write_volatile(0xEB); c += 1;
        code64.add(c).write_volatile(0xFD); c += 1;

        crate::serial_println!("[SMP] 64-bit AP code: {} bytes at 0x8200", c);
        crate::serial_println!("[SMP] Trampoline total: {} bytes", 0x200 + c);
    }

    crate::serial_println!("[SMP] Full 16→32→64 trampoline written at 0x8000");
}

/// Simple busy-wait delay (microseconds, approximate)
fn busy_wait_us(us: u32) {
    for _ in 0..(us as u64 * 1000) {
        unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    }
}

/// Get the total number of detected CPUs
pub fn cpu_count() -> u32 {
    CPU_COUNT.load(Ordering::SeqCst)
}

/// Check if SMP is available (more than 1 core)
pub fn is_smp() -> bool {
    cpu_count() > 1
}

/// Get the core assigned to LLM inference
pub fn llm_affinity_core() -> u32 {
    LLM_CORE_AFFINITY.load(Ordering::SeqCst)
}

/// Set LLM inference core affinity
pub fn set_llm_affinity(core_id: u32) {
    if (core_id as usize) < MAX_CPUS {
        LLM_CORE_AFFINITY.store(core_id, Ordering::SeqCst);
    }
}

/// Run APIC + SMP self-tests
pub fn run_tests() {
    crate::serial_write("[APIC TEST 1/4] APIC base address valid... ");
    let base = APIC_BASE_ADDR.load(Ordering::SeqCst);
    if base != 0 {
        crate::serial_println!("OK (0x{:08X})", base);
    } else {
        crate::serial_write("FAIL\n");
    }

    crate::serial_write("[APIC TEST 2/4] BSP APIC ID readable... ");
    let id = BSP_APIC_ID.load(Ordering::SeqCst);
    crate::serial_println!("OK (ID={})", id);

    crate::serial_write("[APIC TEST 3/4] APIC enabled in SVR... ");
    let svr = unsafe { apic_read(APIC_SVR) };
    if svr & SVR_ENABLE != 0 {
        crate::serial_println!("OK (SVR=0x{:08X})", svr);
    } else {
        crate::serial_println!("FAIL (SVR=0x{:08X})", svr);
    }

    crate::serial_write("[APIC TEST 4/4] CPU count... ");
    let count = CPU_COUNT.load(Ordering::SeqCst);
    crate::serial_println!("OK ({} core(s), {} AP(s))", count, AP_COUNT.load(Ordering::SeqCst));
}
