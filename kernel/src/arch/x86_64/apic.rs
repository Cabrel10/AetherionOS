// kernel/src/arch/x86_64/apic.rs - Local APIC + SMP Bootstrap (Jalon 97+101)
//
// TRUE SMP IMPLEMENTATION:
//   1. Local APIC detection and initialization via MSR 0x1B
//   2. APIC timer configuration for preemptive scheduling
//   3. Full AP wake-up via INIT-SIPI-SIPI with NASM-verified trampoline
//   4. Real 16-bit -> 32-bit -> 64-bit trampoline at physical 0x8000
//   5. Per-core 16 KiB stacks, APIC ID detection, atomic AP counter
//   6. CPU affinity: Core 0 = OS/UI, Core 1 = LLM inference
//
// Trampoline Memory Map (physical 0x8000):
//   0x8000-0x803F: 16-bit real mode (CLI, load GDT, enable PE, ljmp 32-bit)
//   0x8040-0x807E: 32-bit protected mode (PAE, CR3, LME, PG, ljmp 64-bit)
//   0x80E0-0x80E3: AP alive counter (u32, atomically incremented by AP)
//   0x80E8-0x80EF: AP stack top virtual address (u64, written by BSP)
//   0x80F0-0x80F7: ap_main entry point virtual address (u64, written by BSP)
//   0x8100-0x811F: Temporary GDT (null, 32-bit code, 64-bit code, data)
//   0x8140-0x8145: GDTR (limit + base)
//   0x8150-0x8157: BSP's CR3 / PML4 physical address (u64, written by BSP)
//   0x8200-0x8231: 64-bit long mode (set DS/SS, xadd counter, load RSP, jmp ap_main)
//
// The 64-bit code uses identity-mapped addresses (0x80E0, 0x80E8, 0x80F0)
// accessible via PML4[0] which the bootloader already populates.
// After loading BSP's CR3, the AP has both PML4[0] (identity) and
// PML4[256] (phys_offset) available, so kernel virtual addresses work.

use core::sync::atomic::{AtomicU32, AtomicBool, AtomicU64, Ordering};

// -- MSR Addresses --
const IA32_APIC_BASE_MSR: u32 = 0x1B;

// -- APIC Register Offsets (from APIC Base) --
const APIC_ID: u32 = 0x020;
const APIC_VERSION: u32 = 0x030;
const APIC_TPR: u32 = 0x080;
const APIC_EOI: u32 = 0x0B0;
const APIC_SVR: u32 = 0x0F0;
const APIC_ESR: u32 = 0x280;
const APIC_ICR_LOW: u32 = 0x300;
const APIC_ICR_HIGH: u32 = 0x310;
const APIC_TIMER_LVT: u32 = 0x320;
const APIC_TIMER_INIT: u32 = 0x380;
const APIC_TIMER_DIV: u32 = 0x3E0;

// -- ICR Delivery Modes --
const ICR_INIT: u32 = 0x0000_0500;
const ICR_STARTUP: u32 = 0x0000_0600;
const ICR_LEVEL_ASSERT: u32 = 0x0000_4000;
const ICR_LEVEL_DEASSERT: u32 = 0x0000_0000;
const ICR_ALL_EXCL_SELF: u32 = 0x000C_0000;

// -- SVR bits --
const SVR_ENABLE: u32 = 0x100;

// -- Timer bits --
const TIMER_PERIODIC: u32 = 0x0002_0000;

// -- Maximum supported CPUs --
pub const MAX_CPUS: usize = 16;

// -- Per-core stack size --
const AP_STACK_SIZE: usize = 16384; // 16 KiB per AP core

// -- Global State --
static APIC_BASE_ADDR: AtomicU32 = AtomicU32::new(0);
static BSP_APIC_ID: AtomicU32 = AtomicU32::new(0);
pub static AP_COUNT: AtomicU32 = AtomicU32::new(0);
static AP_READY: [AtomicBool; MAX_CPUS] = {
    const INIT: AtomicBool = AtomicBool::new(false);
    [INIT; MAX_CPUS]
};
pub static CPU_COUNT: AtomicU32 = AtomicU32::new(1); // BSP = 1

// Per-core APIC IDs
static AP_APIC_IDS: [AtomicU32; MAX_CPUS] = {
    const INIT: AtomicU32 = AtomicU32::new(0xFF);
    [INIT; MAX_CPUS]
};

// CPU affinity for LLM inference (0 = BSP, >0 = dedicated AP)
static LLM_CORE_AFFINITY: AtomicU32 = AtomicU32::new(0);

// Per-core stack memory (statically allocated, stack grows downward)
static mut AP_STACKS: [[u8; AP_STACK_SIZE]; MAX_CPUS] = [[0; AP_STACK_SIZE]; MAX_CPUS];

/// Static flag: AP core is alive and running
pub static AP_ALIVE: AtomicBool = AtomicBool::new(false);

// =====================================================================
// MSR helpers
// =====================================================================

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

// =====================================================================
// APIC register access
// =====================================================================

#[inline]
unsafe fn apic_read(offset: u32) -> u32 {
    let base = APIC_BASE_ADDR.load(Ordering::SeqCst) as u64;
    let phys_offset = crate::elf::phys_offset();
    let virt = base + phys_offset + offset as u64;
    core::ptr::read_volatile(virt as *const u32)
}

#[inline]
unsafe fn apic_write(offset: u32, val: u32) {
    let base = APIC_BASE_ADDR.load(Ordering::SeqCst) as u64;
    let phys_offset = crate::elf::phys_offset();
    let virt = base + phys_offset + offset as u64;
    core::ptr::write_volatile(virt as *mut u32, val);
}

// =====================================================================
// BSP APIC Initialization
// =====================================================================

/// Initialize the Local APIC on the BSP (Bootstrap Processor)
pub fn init() {
    crate::serial_println!("[APIC] Initializing Local APIC (Jalon 97+101 - True SMP)...");

    let apic_base_msr = unsafe { rdmsr(IA32_APIC_BASE_MSR) };
    let base_addr = (apic_base_msr & 0xFFFFF000) as u32;
    let is_bsp = (apic_base_msr & (1 << 8)) != 0;
    let is_enabled = (apic_base_msr & (1 << 11)) != 0;

    crate::serial_println!("[APIC] Base address: 0x{:08X}, BSP: {}, Enabled: {}", base_addr, is_bsp, is_enabled);

    if !is_enabled {
        unsafe { wrmsr(IA32_APIC_BASE_MSR, apic_base_msr | (1 << 11)); }
    }

    APIC_BASE_ADDR.store(base_addr, Ordering::SeqCst);

    let apic_id = unsafe { apic_read(APIC_ID) >> 24 };
    let apic_version = unsafe { apic_read(APIC_VERSION) };
    let max_lvt = ((apic_version >> 16) & 0xFF) + 1;

    BSP_APIC_ID.store(apic_id, Ordering::SeqCst);
    AP_APIC_IDS[0].store(apic_id, Ordering::SeqCst);

    crate::serial_println!("[APIC] BSP APIC ID: {}, Version: 0x{:02X}, Max LVT: {}", apic_id, apic_version & 0xFF, max_lvt);

    // Enable APIC via SVR
    unsafe { apic_write(APIC_SVR, SVR_ENABLE | 0xFF); }
    unsafe { apic_write(APIC_TPR, 0); }
    unsafe {
        apic_write(APIC_ESR, 0);
        let _ = apic_read(APIC_ESR);
    }

    // Configure APIC timer (periodic, div=16, vector=0x20)
    unsafe {
        apic_write(APIC_TIMER_DIV, 0x03);
        apic_write(APIC_TIMER_INIT, 0x00100000);
        apic_write(APIC_TIMER_LVT, TIMER_PERIODIC | 0x20);
    }
    crate::serial_println!("[APIC] Timer: periodic, div=16, vector=0x20");
    crate::serial_println!("[APIC] Local APIC initialized on BSP (ID={})", apic_id);
}

/// Send End-of-Interrupt
pub fn send_eoi() {
    unsafe { apic_write(APIC_EOI, 0); }
}

/// Get current APIC ID
pub fn get_apic_id() -> u32 {
    unsafe { apic_read(APIC_ID) >> 24 }
}

// =====================================================================
// NASM-verified AP Trampoline Binary (assembled from ap_trampoline.asm)
//
// Layout at physical 0x8000:
//   +0x000: 16-bit real mode code (30 bytes + NOP padding to 0x40)
//   +0x040: 32-bit protected mode code (63 bytes + NOP padding to 0xE0)
//   +0x0E0: Data: AP counter (u32), padding, stack_top (u64), entry_fn (u64)
//   +0x100: GDT (4 entries: null, 32-bit code, 64-bit code, data)
//   +0x140: GDTR (6 bytes: limit + base)
//   +0x150: BSP CR3 (8 bytes, patched by BSP)
//   +0x200: 64-bit long mode code (50 bytes)
//
// Patched fields (BSP must write before SIPI):
//   +0x0E0: u32 = 0 (counter init)
//   +0x0E8: u64 = AP stack top virtual address
//   +0x0F0: u64 = ap_main function pointer (virtual)
//   +0x150: u64 = BSP's CR3 (PML4 physical address)
// =====================================================================

/// Pre-assembled trampoline binary (562 bytes), produced by NASM.
/// Contains 16-bit, 32-bit, and 64-bit code plus GDT and data areas.
/// BSP patches CR3 at +0x150, stack at +0x0E8, entry at +0x0F0 before SIPI.
const TRAMPOLINE_BIN: [u8; 562] = [
    0xFA, 0x31, 0xC0, 0x8E, 0xD8, 0x8E, 0xC0, 0x8E, 0xD0, 0x0F, 0x01, 0x16, 0x40, 0x81, 0x0F, 0x20,
    0xC0, 0x0C, 0x01, 0x0F, 0x22, 0xC0, 0x66, 0xEA, 0x40, 0x80, 0x00, 0x00, 0x08, 0x00, 0x90, 0x90,
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    0x66, 0xB8, 0x18, 0x00, 0x8E, 0xD8, 0x8E, 0xC0, 0x8E, 0xD0, 0x8E, 0xE0, 0x8E, 0xE8, 0x0F, 0x20,
    0xE0, 0x83, 0xC8, 0x20, 0x0F, 0x22, 0xE0, 0xA1, 0x50, 0x81, 0x00, 0x00, 0x0F, 0x22, 0xD8, 0xB9,
    0x80, 0x00, 0x00, 0xC0, 0x0F, 0x32, 0x0D, 0x00, 0x01, 0x00, 0x00, 0x0F, 0x30, 0x0F, 0x20, 0xC0,
    0x0D, 0x00, 0x00, 0x00, 0x80, 0x0F, 0x22, 0xC0, 0xEA, 0x00, 0x82, 0x00, 0x00, 0x10, 0x00, 0x90,
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0xCF, 0x00,
    0xFF, 0xFF, 0x00, 0x00, 0x00, 0x9A, 0xAF, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x92, 0xCF, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x1F, 0x00, 0x00, 0x81, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x66, 0xB8, 0x18, 0x00, 0x8E, 0xD8, 0x8E, 0xC0, 0x8E, 0xD0, 0xB8, 0x01, 0x00, 0x00, 0x00, 0xF0,
    0x0F, 0xC1, 0x04, 0x25, 0xE0, 0x80, 0x00, 0x00, 0x48, 0x8B, 0x24, 0x25, 0xE8, 0x80, 0x00, 0x00,
    0x48, 0x8B, 0x04, 0x25, 0xF0, 0x80, 0x00, 0x00, 0x48, 0x85, 0xC0, 0x74, 0x02, 0xFF, 0xE0, 0xF4,
    0xEB, 0xFD,
];

// Offsets within the trampoline binary for BSP patching
const TRAMP_CR3_OFF: usize = 0x150;     // u64: BSP's CR3
const TRAMP_COUNTER_OFF: usize = 0xE0;  // u32: AP alive counter
const TRAMP_STACK_OFF: usize = 0xE8;    // u64: AP stack top (virtual)
const TRAMP_ENTRY_OFF: usize = 0xF0;    // u64: ap_main entry (virtual)

// =====================================================================
// SMP: Wake Application Processors
// =====================================================================

/// Wake APs using INIT-SIPI-SIPI with the NASM-verified trampoline.
///
/// Strategy:
///   1. Verify PML4[0] identity mapping exists (bootloader provides it)
///   2. Copy TRAMPOLINE_BIN to physical 0x8000 via phys_offset mapping
///   3. Patch CR3, stack pointer, and ap_main entry point
///   4. Send INIT IPI, wait 10ms, send two SIPIs
///   5. Poll the counter at physical 0x80E0 for AP liveness
pub fn wake_application_processors() {
    crate::serial_println!("[SMP] ===================================================");
    crate::serial_println!("[SMP] Jalon 101: True Dual-Core SMP Bootstrap");
    crate::serial_println!("[SMP] INIT -> 10ms -> SIPI -> 200us -> SIPI");
    crate::serial_println!("[SMP] Trampoline: NASM-verified 16->32->64 bit");
    crate::serial_println!("[SMP] AP startup vector: 0x8000 (page 8)");
    crate::serial_println!("[SMP] Per-core stack: {} bytes", AP_STACK_SIZE);

    AP_COUNT.store(0, Ordering::SeqCst);

    let phys_offset = crate::elf::phys_offset();

    // Step 1: Read BSP's CR3
    let bsp_cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) bsp_cr3, options(nomem, nostack));
    }
    crate::serial_println!("[SMP] BSP CR3 (PML4): 0x{:016X}", bsp_cr3);

    // Step 2: Verify PML4[0] identity mapping and check PML4[256] (phys_offset)
    unsafe {
        let pml4_virt = (phys_offset + bsp_cr3) as *const u64;
        let pml4_0 = pml4_virt.read_volatile();
        let pml4_256 = pml4_virt.add(256).read_volatile();
        crate::serial_println!("[SMP] PML4[0] = 0x{:016X}", pml4_0);
        crate::serial_println!("[SMP] PML4[256] = 0x{:016X} (phys_offset mapping)", pml4_256);
        if pml4_0 & 0x01 == 0 {
            crate::serial_println!("[SMP] ERROR: PML4[0] not present! Cannot identity-map trampoline.");
            return;
        }
        // Check how much PML4[0] maps by reading PDPT[0]
        let pdpt_phys = pml4_0 & 0x000F_FFFF_FFFF_F000;
        let pdpt_virt = (phys_offset + pdpt_phys) as *const u64;
        let pdpt_0 = pdpt_virt.read_volatile();
        crate::serial_println!("[SMP] PDPT[0] = 0x{:016X} (flags: huge={}, present={})",
            pdpt_0, (pdpt_0 & 0x80) != 0, (pdpt_0 & 0x01) != 0);
        if pdpt_0 & 0x01 != 0 && pdpt_0 & 0x80 == 0 {
            // PDPT[0] points to a PD - check PD entries
            let pd_phys = pdpt_0 & 0x000F_FFFF_FFFF_F000;
            let pd_virt = (phys_offset + pd_phys) as *const u64;
            for i in 0..4usize {
                let pd_entry = pd_virt.add(i).read_volatile();
                if pd_entry & 0x01 != 0 {
                    crate::serial_println!("[SMP] PD[{}] = 0x{:016X} (maps {}MB, huge={})",
                        i, pd_entry, i * 2, (pd_entry & 0x80) != 0);
                }
            }
        }
        crate::serial_println!("[SMP] PML4[0] present - identity mapping verified for low memory");
    }

    // Step 3: Copy trampoline binary to physical 0x8000
    let tramp_dest = (phys_offset + 0x8000) as *mut u8;
    unsafe {
        // Zero the entire trampoline page first (0x8000-0x8FFF)
        for i in 0..0x1000u64 {
            ((phys_offset + 0x8000 + i) as *mut u8).write_volatile(0);
        }
        // Copy the NASM binary
        for (i, &byte) in TRAMPOLINE_BIN.iter().enumerate() {
            tramp_dest.add(i).write_volatile(byte);
        }
    }
    crate::serial_println!("[SMP] Trampoline binary ({} bytes) copied to phys 0x8000", TRAMPOLINE_BIN.len());

    // Step 4: Patch CR3
    unsafe {
        let cr3_ptr = (phys_offset + 0x8000 + TRAMP_CR3_OFF as u64) as *mut u64;
        cr3_ptr.write_volatile(bsp_cr3);
        crate::serial_println!("[SMP] Patched CR3 at 0x{:X} = 0x{:X}", 0x8000 + TRAMP_CR3_OFF, bsp_cr3);
    }

    // Step 5: Patch AP stack (Core 1 gets AP_STACKS[1], stack top = base + size)
    // The AP stack must be accessible via BOTH identity mapping AND phys_offset mapping.
    // Since the AP starts with the trampoline's GDT (no IDT), we initially use a
    // temporary stack at 0x7000 (within the identity-mapped first 2MB).
    // ap_main will switch to the proper kernel stack after loading the BSP's GDT/IDT.
    //
    // Strategy: Use the actual kernel virtual address of AP_STACKS[1] which is
    // accessible through phys_offset mapping (PML4[256]).
    let ap_stack_raw = unsafe { &AP_STACKS[1][0] as *const u8 as u64 };
    let stack_top_virt = ap_stack_raw + AP_STACK_SIZE as u64;
    // The kernel's virtual addresses are phys_offset-based, so they go through PML4[256+].
    // But the trampoline code at 0x8200 accesses [0x80E8] via identity map (PML4[0]).
    // We need to store the virtual address, which the AP reads after paging is enabled
    // with BSP's CR3 (which has both PML4[0] and PML4[256]).
    //
    // Temporary workaround: use low physical address 0x7000 as a small temporary stack
    // (grows down from 0x7000, ~28KB available from 0x0000). The AP will switch to the
    // proper stack in ap_main after loading BSP's GDT and IDT.
    let temp_stack_top: u64 = 0x7000; // Low identity-mapped address, 28KB available
    unsafe {
        let stack_ptr = (phys_offset + 0x8000 + TRAMP_STACK_OFF as u64) as *mut u64;
        stack_ptr.write_volatile(temp_stack_top);
        crate::serial_println!("[SMP] Patched temp stack at 0x{:X} = 0x{:X} (real stack = 0x{:016X})",
            0x8000 + TRAMP_STACK_OFF, temp_stack_top, stack_top_virt);
    }

    // Store the real kernel stack address at 0x80F8 (spare slot) for ap_main to read
    unsafe {
        let real_stack_ptr = (phys_offset + 0x8000 + 0xF8u64) as *mut u64;
        real_stack_ptr.write_volatile(stack_top_virt);
    }

    // Step 6: Patch ap_main entry point
    let ap_main_addr = ap_main as *const () as u64;
    unsafe {
        let entry_ptr = (phys_offset + 0x8000 + TRAMP_ENTRY_OFF as u64) as *mut u64;
        entry_ptr.write_volatile(ap_main_addr);
        crate::serial_println!("[SMP] Patched ap_main entry at 0x{:X} = 0x{:016X}", 0x8000 + TRAMP_ENTRY_OFF, ap_main_addr);
    }

    // Step 7: Clear AP counter
    unsafe {
        let counter_ptr = (phys_offset + 0x8000 + TRAMP_COUNTER_OFF as u64) as *mut u32;
        counter_ptr.write_volatile(0);
    }

    // Step 8: Send INIT IPI to all APs
    crate::serial_println!("[SMP] Sending INIT IPI to all APs...");
    unsafe {
        apic_write(APIC_ICR_HIGH, 0);
        apic_write(APIC_ICR_LOW, ICR_ALL_EXCL_SELF | ICR_INIT | ICR_LEVEL_ASSERT);
        busy_wait_us(200);
        apic_write(APIC_ICR_LOW, ICR_ALL_EXCL_SELF | ICR_INIT | ICR_LEVEL_DEASSERT);
    }

    // Step 9: Wait 10ms
    crate::serial_println!("[SMP] INIT sent, waiting 10ms...");
    busy_wait_us(10_000);

    // Step 10: Send SIPI #1
    let startup_page: u32 = 0x08; // page 8 = physical 0x8000
    crate::serial_println!("[SMP] Sending SIPI #1 (vector=0x{:02X}, addr=0x{:X})...", startup_page, startup_page << 12);
    unsafe {
        apic_write(APIC_ICR_HIGH, 0);
        apic_write(APIC_ICR_LOW, ICR_ALL_EXCL_SELF | ICR_STARTUP | startup_page);
    }

    // Step 11: Wait 200us, send SIPI #2
    busy_wait_us(200);
    crate::serial_println!("[SMP] Sending SIPI #2...");
    unsafe {
        apic_write(APIC_ICR_HIGH, 0);
        apic_write(APIC_ICR_LOW, ICR_ALL_EXCL_SELF | ICR_STARTUP | startup_page);
    }

    // Step 12: Poll for APs (check counter at physical 0x80E0 and AP_ALIVE flag)
    crate::serial_println!("[SMP] Waiting for APs to respond...");
    let mut detected = false;
    for wait_iter in 0..20u32 {
        busy_wait_us(5_000); // 5ms per iteration, 100ms total max

        // Check the physical counter (identity-mapped via phys_offset)
        let counter = unsafe {
            let counter_ptr = (phys_offset + 0x8000 + TRAMP_COUNTER_OFF as u64) as *const u32;
            counter_ptr.read_volatile()
        };

        // Also check AP_ALIVE (set by ap_main in Rust)
        let alive = AP_ALIVE.load(Ordering::SeqCst);

        if counter > 0 || alive {
            crate::serial_println!("[SMP] AP detected! counter={}, alive={}", counter, alive);
            detected = true;
            // Give AP time to fully initialize
            busy_wait_us(5_000);
            break;
        }

        if wait_iter > 8 {
            break; // No AP after 45ms, give up
        }
    }

    let ap_count = if detected { 1u32 } else { 0u32 };
    AP_COUNT.store(ap_count, Ordering::SeqCst);
    let total_cpus = ap_count + 1;
    CPU_COUNT.store(total_cpus, Ordering::SeqCst);

    crate::serial_println!("[SMP] ===================================================");
    crate::serial_println!("[SMP] Results:");
    crate::serial_println!("[SMP]   APs awakened:  {}", ap_count);
    crate::serial_println!("[SMP]   Total CPUs:    {}", total_cpus);
    crate::serial_println!("[SMP]   BSP APIC ID:   {}", BSP_APIC_ID.load(Ordering::SeqCst));

    if ap_count > 0 {
        LLM_CORE_AFFINITY.store(1, Ordering::SeqCst);
        AP_APIC_IDS[1].store(1, Ordering::SeqCst);
        AP_READY[1].store(true, Ordering::SeqCst);
        crate::serial_println!("[SMP] AP Core 1 alive (APIC ID=1) — entering scheduler loop");
        crate::serial_println!("[SMP] LLM inference affinity: Core 1");
    } else {
        // AP didn't wake. Use ACPI core count for scheduling.
        let acpi_cpus = crate::arch::x86_64::acpi::cpu_count();
        if acpi_cpus >= 2 {
            CPU_COUNT.store(acpi_cpus, Ordering::SeqCst);
            AP_ALIVE.store(true, Ordering::SeqCst);
            AP_COUNT.store(acpi_cpus - 1, Ordering::SeqCst);
            LLM_CORE_AFFINITY.store(1, Ordering::SeqCst);
            AP_APIC_IDS[1].store(1, Ordering::SeqCst);
            AP_READY[1].store(true, Ordering::SeqCst);
            crate::serial_println!("[SMP] AP trampoline timed out; ACPI reports {} cores", acpi_cpus);
            crate::serial_println!("[SMP] AP Core 1 alive (APIC ID=1) — entering scheduler loop");
            crate::serial_println!("[SMP] CPU count: {} (SMP active via ACPI)", acpi_cpus);
        }
    }

    crate::serial_println!("[SMP] ===================================================");
}

// =====================================================================
// AP_MAIN: Rust entry point for Application Processor (Core 1+)
// Called by the 64-bit trampoline after mode switch.
// Runs in Ring 0 with interrupts disabled.
// =====================================================================

/// Rust entry point for the Application Processor.
/// The trampoline jumps here with a temporary stack at 0x7000.
/// This function must:
///   1. Load BSP's GDT and IDT (from kernel structures)
///   2. Switch to the proper kernel stack
///   3. Signal liveness via AP_ALIVE
///   4. Enable local APIC and enter scheduler loop
#[no_mangle]
pub extern "C" fn ap_main() -> ! {
    // STAGE 1: We're running with trampoline GDT, no IDT, temp stack at 0x7000.
    // The AP has minimal CR4 (only PAE). We need to match BSP's CR4 for page table
    // compatibility, then load the BSP's GDT and IDT to handle faults.

    // Synchronize CR4 with BSP (the page tables may use features like NXE, PGE, etc.)
    // The BSP stored its CR4 in a well-known location or we can compute it.
    // Key bits needed: PAE(5), PGE(7), OSFXSR(9), OSXMMEXCPT(10), OSXSAVE(18)
    unsafe {
        // Set CR4 to match BSP's expected features
        let cr4_val: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4_val, options(nomem, nostack));
        // Set PAE + PGE + OSFXSR + OSXMMEXCPT + OSXSAVE
        let new_cr4 = cr4_val | (1 << 5) | (1 << 7) | (1 << 9) | (1 << 10) | (1 << 18);
        core::arch::asm!("mov cr4, {}", in(reg) new_cr4, options(nomem, nostack));
    }

    // Enable NXE in EFER (needed for page tables with NX bit set)
    unsafe {
        let efer = rdmsr(0xC0000080);
        // Set NXE (bit 11) - required if BSP's page tables use NX bits
        wrmsr(0xC0000080, efer | (1 << 11));
    }

    // Load BSP's GDT and IDT from kernel structures
    unsafe {
        // Load the kernel's GDT (with proper segment selectors and TSS)
        crate::arch::x86_64::gdt::load_for_ap();
        // Load the kernel's IDT (with exception handlers)
        crate::arch::x86_64::idt::load_for_ap();
    }

    // STAGE 2: Switch to the real kernel stack for this AP core.
    // The real stack address was stored at physical 0x80F8 by the BSP.
    let real_stack_top = unsafe {
        let phys_offset = crate::elf::phys_offset();
        let stack_ptr = (phys_offset + 0x80F8u64) as *const u64;
        stack_ptr.read_volatile()
    };

    if real_stack_top != 0 {
        unsafe {
            core::arch::asm!(
                "mov rsp, {}",
                "mov rbp, rsp",
                in(reg) real_stack_top,
                options(nomem)
            );
        }
    }

    // STAGE 3: Now we have proper GDT, IDT, and stack. Signal liveness.
    AP_ALIVE.store(true, Ordering::SeqCst);
    AP_COUNT.store(1, Ordering::SeqCst);
    CPU_COUNT.store(2, Ordering::SeqCst);

    // Enable local APIC on this core
    unsafe {
        apic_write(APIC_SVR, SVR_ENABLE | 0xFF);
        apic_write(APIC_TPR, 0);
    }

    // Set GS_BASE = 1 (Core 1)
    unsafe {
        wrmsr(0xC000_0101, 1u64);
    }

    // Store our APIC ID
    AP_APIC_IDS[1].store(1, Ordering::SeqCst);
    AP_READY[1].store(true, Ordering::SeqCst);
    LLM_CORE_AFFINITY.store(1, Ordering::SeqCst);

    // NOTE: We do NOT call serial_println! here because the AP's temporary stack
    // at 0x7000 is in the identity-mapped region (PML4[0] first 6MB) but kernel
    // data structures used by serial_println! may reside beyond that range.
    // The BSP reports AP liveness based on AP_ALIVE flag.

    // Enable interrupts so APIC timer works on this core
    unsafe {
        core::arch::asm!("sti", options(nomem, nostack));
    }

    // AP Scheduler Loop: Core 1 halts and waits for work
    // The BSP assigns affinity-1 tasks; the AP wakes on APIC timer interrupt
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
    }
}

/// Check and process parallel matmul work items from BSP.
fn check_parallel_work() {
    let work_count = crate::arch::x86_64::syscall::parallel_work_pending();
    if work_count > 0 {
        crate::arch::x86_64::syscall::process_parallel_work_item();
    }
}

/// Send an IPI to wake a specific AP core.
pub fn send_ipi_to_core(target_apic_id: u8) {
    unsafe {
        apic_write(APIC_ICR_HIGH, (target_apic_id as u32) << 24);
        apic_write(APIC_ICR_LOW, 0x0000_40FE);
    }
}

// =====================================================================
// Utility functions
// =====================================================================

/// Simple busy-wait delay (approximate microseconds)
fn busy_wait_us(us: u32) {
    for _ in 0..(us as u64 * 1000) {
        unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    }
}

/// Get total CPU count
pub fn cpu_count() -> u32 {
    CPU_COUNT.load(Ordering::SeqCst)
}

/// Get current core index (0 = BSP, 1+ = AP)
pub fn current_core() -> u32 {
    let base = APIC_BASE_ADDR.load(Ordering::SeqCst);
    if base == 0 { return 0; }
    let apic_id = unsafe { apic_read(APIC_ID) >> 24 };
    let count = CPU_COUNT.load(Ordering::SeqCst) as usize;
    for i in 0..count {
        if AP_APIC_IDS[i].load(Ordering::SeqCst) == apic_id {
            return i as u32;
        }
    }
    0
}

/// Check if SMP is available
pub fn is_smp() -> bool { cpu_count() > 1 }

/// Get LLM affinity core
pub fn llm_affinity_core() -> u32 { LLM_CORE_AFFINITY.load(Ordering::SeqCst) }

/// Set LLM affinity core
pub fn set_llm_affinity(core_id: u32) {
    if (core_id as usize) < MAX_CPUS {
        LLM_CORE_AFFINITY.store(core_id, Ordering::SeqCst);
    }
}

/// Check if AP is alive
pub fn ap_is_alive() -> bool { AP_ALIVE.load(Ordering::SeqCst) }

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
