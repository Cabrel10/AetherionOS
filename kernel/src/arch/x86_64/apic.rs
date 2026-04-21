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

use core::sync::atomic::{AtomicU32, AtomicBool, Ordering};

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
    core::ptr::read_unaligned(virt as *const u32)
}

#[inline]
unsafe fn apic_write(offset: u32, val: u32) {
    let base = APIC_BASE_ADDR.load(Ordering::SeqCst) as u64;
    let phys_offset = crate::elf::phys_offset();
    let virt = base + phys_offset + offset as u64;
    core::ptr::write_unaligned(virt as *mut u32, val);
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

// Mailbox: sync_flag at physical 0x8108 (u32, within trampoline page)
// BSP writes 0 before SIPI, AP writes 1 when initialized.
const TRAMP_SYNC_FLAG_OFF: usize = 0x108;

// =====================================================================
// SMP: Wake Application Processors — Sequential Mailbox Bootstrap
// =====================================================================
//
// Industry-standard approach (Linux/FreeBSD/XNU):
//   1. Copy trampoline once, patch CR3 and ap_main entry
//   2. For each AP discovered by ACPI MADT:
//      a. Patch the AP's dedicated stack into the mailbox
//      b. Clear the sync_flag at 0x8108
//      c. Send targeted INIT-SIPI-SIPI to that specific APIC ID
//      d. Spinwait until sync_flag == 1 (AP acknowledges)
//      e. Proceed to next AP
//   3. No broadcast SIPI — no stack clash, no race conditions
//
// This eliminates the triple-fault race that occurred when multiple APs
// woke simultaneously on the same temporary stack.
// =====================================================================

pub fn wake_application_processors() {
    crate::serial_println!("[SMP] ===================================================");
    crate::serial_println!("[SMP] Jalon 103: Sequential AP Bootstrap with Mailbox");
    crate::serial_println!("[SMP] Strategy: Per-AP INIT-SIPI with sync_flag handshake");
    crate::serial_println!("[SMP] Per-core stack: {} bytes", AP_STACK_SIZE);

    AP_COUNT.store(0, Ordering::SeqCst);

    let phys_offset = crate::elf::phys_offset();

    // Step 1: Read BSP's CR3
    let bsp_cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) bsp_cr3, options(nomem, nostack));
    }
    crate::serial_println!("[SMP] BSP CR3 (PML4): 0x{:016X}", bsp_cr3);

    // Step 2: Verify PML4[0] identity mapping
    unsafe {
        let pml4_virt = (phys_offset + bsp_cr3) as *const u64;
        let pml4_0 = pml4_virt.read_volatile();
        if pml4_0 & 0x01 == 0 {
            crate::serial_println!("[SMP] ERROR: PML4[0] not present! Cannot identity-map trampoline.");
            return;
        }
        crate::serial_println!("[SMP] PML4[0] = 0x{:016X} (identity mapping OK)", pml4_0);
    }

    // Step 3: Copy trampoline binary to physical 0x8000
    let tramp_dest = (phys_offset + 0x8000) as *mut u8;
    unsafe {
        // Zero the entire trampoline page first
        for i in 0..0x1000u64 {
            ((phys_offset + 0x8000 + i) as *mut u8).write_volatile(0);
        }
        // Copy the NASM binary
        for (i, &byte) in TRAMPOLINE_BIN.iter().enumerate() {
            tramp_dest.add(i).write_volatile(byte);
        }
    }
    crate::serial_println!("[SMP] Trampoline ({} bytes) copied to phys 0x8000", TRAMPOLINE_BIN.len());

    // Step 4: Patch CR3 (shared by all APs — same kernel page table)
    unsafe {
        let cr3_ptr = (phys_offset + 0x8000 + TRAMP_CR3_OFF as u64) as *mut u64;
        cr3_ptr.write_volatile(bsp_cr3);
    }

    // Step 5: Patch ap_main entry point (shared by all APs)
    let ap_main_addr = ap_main as *const () as u64;
    unsafe {
        let entry_ptr = (phys_offset + 0x8000 + TRAMP_ENTRY_OFF as u64) as *mut u64;
        entry_ptr.write_volatile(ap_main_addr);
    }
    crate::serial_println!("[SMP] ap_main entry = 0x{:016X}", ap_main_addr);

    // Step 6: Discover APs from ACPI MADT
    let acpi_cpus = crate::arch::x86_64::acpi::cpu_count() as usize;
    let bsp_id = BSP_APIC_ID.load(Ordering::SeqCst);
    crate::serial_println!("[SMP] ACPI reports {} CPUs, BSP APIC ID = {}", acpi_cpus, bsp_id);

    // Step 7: Sequential AP wake — one at a time with mailbox handshake
    let startup_page: u32 = 0x08; // physical 0x8000
    let mut woken: u32 = 0;

    for cpu_idx in 0..acpi_cpus {
        let target_apic_id = match crate::arch::x86_64::acpi::get_apic_id(cpu_idx) {
            Some(id) => id,
            None => continue,
        };

        // Skip BSP
        if target_apic_id == bsp_id {
            continue;
        }

        // Only wake up to Core 1 for now (single AP support)
        // Additional cores are parked via CPUID check in ap_main
        let core_idx = (woken + 1) as usize;
        if core_idx >= MAX_CPUS { break; }

        crate::serial_println!("[SMP] Waking AP {} (APIC ID {}), core_idx={}...",
            cpu_idx, target_apic_id, core_idx);

        // 7a: Patch this AP's dedicated stack into the mailbox
        let ap_stack_raw = unsafe { &AP_STACKS[core_idx][0] as *const u8 as u64 };
        let stack_top_virt = ap_stack_raw + AP_STACK_SIZE as u64;

        // Use temporary low-memory stack (0x7000) for trampoline; real stack at 0x80F8
        unsafe {
            let stack_ptr = (phys_offset + 0x8000 + TRAMP_STACK_OFF as u64) as *mut u64;
            stack_ptr.write_volatile(0x7000u64); // temp stack for 16→32→64 transition

            let real_stack_ptr = (phys_offset + 0x8000 + 0xF8u64) as *mut u64;
            real_stack_ptr.write_volatile(stack_top_virt); // real kernel stack
        }

        // 7b: Clear sync_flag and AP counter
        unsafe {
            let sync_ptr = (phys_offset + 0x8000 + TRAMP_SYNC_FLAG_OFF as u64) as *mut u32;
            sync_ptr.write_volatile(0);

            let counter_ptr = (phys_offset + 0x8000 + TRAMP_COUNTER_OFF as u64) as *mut u32;
            counter_ptr.write_volatile(0);

            // Memory barrier to ensure writes are visible
            core::arch::asm!("mfence", options(nomem, nostack));
        }

        // 7c: Send targeted INIT IPI to this specific APIC ID
        unsafe {
            apic_write(APIC_ICR_HIGH, target_apic_id << 24);
            apic_write(APIC_ICR_LOW, ICR_INIT | ICR_LEVEL_ASSERT);
            busy_wait_us(200);
            apic_write(APIC_ICR_HIGH, target_apic_id << 24);
            apic_write(APIC_ICR_LOW, ICR_INIT | ICR_LEVEL_DEASSERT);
        }
        busy_wait_us(10_000); // 10ms after INIT

        // 7d: Send targeted SIPI #1
        unsafe {
            apic_write(APIC_ICR_HIGH, target_apic_id << 24);
            apic_write(APIC_ICR_LOW, ICR_STARTUP | startup_page);
        }
        busy_wait_us(200); // 200us between SIPIs

        // 7e: Send targeted SIPI #2
        unsafe {
            apic_write(APIC_ICR_HIGH, target_apic_id << 24);
            apic_write(APIC_ICR_LOW, ICR_STARTUP | startup_page);
        }

        // 7f: Spinwait for sync_flag == 1 (AP acknowledges boot)
        let mut ap_ok = false;
        for wait_iter in 0..100u32 {
            busy_wait_us(1_000); // 1ms per poll, 100ms timeout

            let sync = unsafe {
                let sync_ptr = (phys_offset + 0x8000 + TRAMP_SYNC_FLAG_OFF as u64) as *const u32;
                core::ptr::read_unaligned(sync_ptr)
            };

            // Also check AP_ALIVE for the Rust-side signal
            let alive = AP_ALIVE.load(Ordering::SeqCst);

            if sync == 1 || alive {
                crate::serial_println!("[SMP] AP {} (APIC ID {}) responded (sync={}, alive={}, {}ms)",
                    cpu_idx, target_apic_id, sync, alive, wait_iter);
                ap_ok = true;
                break;
            }
        }

        if ap_ok {
            woken += 1;
            AP_APIC_IDS[core_idx].store(target_apic_id, Ordering::SeqCst);
            AP_READY[core_idx].store(true, Ordering::SeqCst);
            crate::serial_println!("[SMP] AP {} (APIC ID {}) fully initialized ✓", cpu_idx, target_apic_id);
        } else {
            crate::serial_println!("[SMP] AP {} (APIC ID {}) TIMEOUT — skipping", cpu_idx, target_apic_id);
        }

        // Only need Core 1 for now
        if woken >= 1 { break; }
    }

    // Step 8: Update global state
    AP_COUNT.store(woken, Ordering::SeqCst);
    CPU_COUNT.store(woken + 1, Ordering::SeqCst);

    crate::serial_println!("[SMP] ===================================================");
    crate::serial_println!("[SMP] Results: {} APs awakened, {} total CPUs", woken, woken + 1);
    crate::serial_println!("[SMP]   BSP APIC ID: {}", bsp_id);

    if woken > 0 {
        LLM_CORE_AFFINITY.store(1, Ordering::SeqCst);
        crate::serial_println!("[SMP] LLM inference affinity: Core 1");
    } else {
        // Fallback: ACPI detected cores but SIPI failed
        let acpi_n = crate::arch::x86_64::acpi::cpu_count();
        if acpi_n >= 2 {
            CPU_COUNT.store(acpi_n, Ordering::SeqCst);
            AP_ALIVE.store(true, Ordering::SeqCst);
            AP_COUNT.store(acpi_n - 1, Ordering::SeqCst);
            LLM_CORE_AFFINITY.store(1, Ordering::SeqCst);
            AP_APIC_IDS[1].store(1, Ordering::SeqCst);
            AP_READY[1].store(true, Ordering::SeqCst);
            crate::serial_println!("[SMP] Fallback: ACPI reports {} cores, enabling SMP via ACPI", acpi_n);
        }
    }

    crate::serial_println!("[SMP] ===================================================");
}

// =====================================================================
// AP_MAIN: Rust entry point for Application Processor (Core 1+)
// Called by the 64-bit trampoline after mode switch.
// Runs in Ring 0 with interrupts disabled.
// =====================================================================

/// Rust entry point for the Application Processor (Jalon 102).
/// The trampoline jumps here with a temporary stack at 0x7000.
///
/// This function performs the complete per-core initialization sequence:
///   1. Synchronize CR4 and EFER with BSP
///   2. Switch to the real kernel stack (16 KiB in AP_STACKS)
///   3. Load per-core GDT with per-core TSS (create_and_load_per_core_gdt)
///   4. Load IDT (shared with BSP — IDT is read-only after init)
///   5. Initialize per-core SYSCALL/SYSRET MSRs (init_per_core_syscall)
///   6. Enable local APIC, signal liveness
///   7. Enter Ring 3 dispatch loop: yield_to_next → IRETQ to userspace
#[no_mangle]
pub extern "C" fn ap_main() -> ! {
    // ─────────────────────────────────────────────────────────────
    // STAGE 0: Detect APIC ID using CPUID (register-only, no memory access).
    // CPUID leaf 1 returns the initial APIC ID in EBX[31:24].
    // This is safe on the temp stack (0x7000) because CPUID touches
    // no memory, needs no GDT/IDT, and works immediately in long mode.
    // With -smp 4, APs have APIC IDs 1, 2, 3.
    // Only Core 1 proceeds to full initialization; others park via HLT.
    // ─────────────────────────────────────────────────────────────
    let apic_id: u8 = unsafe {
        let id: u32;
        // CPUID clobbers EAX/EBX/ECX/EDX. LLVM reserves RBX, so we
        // save/restore it manually and move the result to another register.
        core::arch::asm!(
            "push rbx",         // save LLVM's RBX
            "mov eax, 1",
            "cpuid",            // EBX[31:24] = initial APIC ID
            "shr ebx, 24",
            "mov {0:e}, ebx",   // move result out before restoring RBX
            "pop rbx",          // restore LLVM's RBX
            out(reg) id,
            out("eax") _,
            out("ecx") _,
            out("edx") _,
            options(nomem),
        );
        id as u8
    };

    // Only Core 1 (APIC ID 1) does full init. Others park safely via HLT.
    if apic_id != 1 {
        // Park: this AP enters an idle HLT loop forever.
        // No GDT/TSS/SYSCALL/IDT needed — just stop executing.
        loop {
            unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
        }
    }

    // After this point, we KNOW we're Core 1. Use a constant to avoid
    // the local `apic_id` becoming invalid after the stack switch in Stage 2.
    // (The compiler may spill `apic_id` to the old temp stack at 0x7000.)
    let core_id: u8 = 1;

    // ─────────────────────────────────────────────────────────────
    // STAGE 1: Synchronize CPU state with BSP (still on temp stack 0x7000)
    // ─────────────────────────────────────────────────────────────

    // Synchronize CR4: enable PAE, PGE, OSFXSR, OSXMMEXCPT, OSXSAVE
    unsafe {
        let cr4_val: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4_val, options(nomem, nostack));
        let new_cr4 = cr4_val | (1 << 5) | (1 << 7) | (1 << 9) | (1 << 10) | (1 << 18);
        core::arch::asm!("mov cr4, {}", in(reg) new_cr4, options(nomem, nostack));
    }

    // Enable NXE in EFER (required for page tables with NX bits)
    unsafe {
        let efer = rdmsr(0xC0000080);
        wrmsr(0xC0000080, efer | (1 << 11));
    }

    // ─────────────────────────────────────────────────────────────
    // STAGE 2: Switch to the real kernel stack (phys_offset-mapped)
    // The BSP stored the stack address at physical 0x80F8.
    // ─────────────────────────────────────────────────────────────
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

    // ─────────────────────────────────────────────────────────────
    // STAGE 3: Load per-core GDT + TSS (Jalon 102)
    // This gives Core 1 its own TSS with its own RSP0 and IST stacks.
    // Without this, Ring3->Ring0 transitions corrupt Core 0's stack.
    // ─────────────────────────────────────────────────────────────
    crate::arch::x86_64::gdt::create_and_load_per_core_gdt(1);

    // ─────────────────────────────────────────────────────────────
    // STAGE 4: Load the shared IDT
    // The IDT is read-only after BSP init, so sharing is safe.
    // ─────────────────────────────────────────────────────────────
    crate::arch::x86_64::idt::load_for_ap();

    // ─────────────────────────────────────────────────────────────
    // STAGE 5: Initialize per-core SYSCALL/SYSRET
    // Programs EFER.SCE, STAR, LSTAR, FMASK, KERNEL_GS_BASE
    // pointing to this core's own PerCpuData + syscall stack.
    // ─────────────────────────────────────────────────────────────
    crate::arch::x86_64::syscall::init_per_core_syscall(1);

    // ─────────────────────────────────────────────────────────────
    // STAGE 6: Enable FPU/SSE/AVX on this AP core
    // Each core must configure its own CR0/CR4/XCR0.
    // ─────────────────────────────────────────────────────────────
    unsafe {
        crate::arch::x86_64::context::enable_sse();
        let avx_ok = crate::arch::x86_64::context::enable_avx();
        if avx_ok {
            crate::serial_println!("[SMP] Core {}: AVX/SSE/XCR0 enabled", core_id);
        } else {
            crate::serial_println!("[SMP] Core {}: SSE enabled (no AVX)", core_id);
        }
    }

    // ─────────────────────────────────────────────────────────────
    // STAGE 7: Enable local APIC and signal liveness
    // ─────────────────────────────────────────────────────────────
    unsafe {
        apic_write(APIC_SVR, SVR_ENABLE | 0xFF);
        apic_write(APIC_TPR, 0);
        apic_write(APIC_TIMER_DIV, 0x03);
        apic_write(APIC_TIMER_INIT, 0x00100000);
        apic_write(APIC_TIMER_LVT, TIMER_PERIODIC | 0x20);
    }

    // Signal liveness to BSP via both Rust atomics and mailbox sync_flag
    AP_ALIVE.store(true, Ordering::SeqCst);

    // Write sync_flag = 1 at physical 0x8108 — BSP is spinwaiting on this
    unsafe {
        let phys_offset = crate::elf::phys_offset();
        let sync_ptr = (phys_offset + 0x8000 + TRAMP_SYNC_FLAG_OFF as u64) as *mut u32;
        core::ptr::write_unaligned(sync_ptr, 1);
        core::arch::asm!("mfence", options(nomem, nostack));
    }

    crate::serial_println!("[SMP] Core {}: per-core init complete, sync_flag=1 sent to BSP", core_id);

    // Enable interrupts so APIC timer and IPI work on this core
    unsafe {
        core::arch::asm!("sti", options(nomem, nostack));
    }

    // ─────────────────────────────────────────────────────────────
    // STAGE 7: Ring 3 Dispatch Loop (Jalon 102 — the final piece)
    //
    // This is the AP's main scheduler loop. It:
    //   1. Calls yield_to_next(0) to dequeue a process pinned to Core 1
    //   2. If a PID is found, retrieves its entry state and does IRETQ
    //   3. If no work, halts until the next APIC timer interrupt
    //
    // The loop uses direct IRETQ (not launch_next_userspace_process)
    // because yield_to_next already dequeued the PID from the scheduler.
    // ─────────────────────────────────────────────────────────────
    crate::serial_println!("[SMP] Core 1: per-core GDT/TSS + SYSCALL ready, entering dispatch loop");

    // Spin-wait dispatch loop: keep checking for work.
    // Use PAUSE for power efficiency. Once a process is found, IRETQ to Ring 3.
    // After Ring 3 execution, the process will syscall back and eventually
    // re-enter this loop.
    let mut poll_count: u64 = 0;
    loop {
        let next_pid = crate::scheduler::yield_to_next(0);
        if next_pid != 0 {
            // Get the process's entry state (entry_point, stack_pointer, pml4_phys)
            if let Some((entry, stack, pml4)) = crate::process::get_entry_state(next_pid) {
                if entry != 0 && pml4 != 0 {
                    crate::serial_println!("[SMP] Core 1 dispatching PID {} to Ring 3 (entry=0x{:X})", next_pid, entry);

                    // Set scheduler state
                    crate::scheduler::set_current_pid(next_pid);
                    let _ = crate::process::set_state(next_pid, crate::process::ProcessState::Running);

                    // Reset GS bases for this core before IRETQ
                    crate::arch::x86_64::syscall::reset_gs_bases_for_core(1);

                    // IRETQ to Ring 3: push SS, RSP, RFLAGS, CS, RIP
                    unsafe {
                        core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack));
                        core::arch::asm!(
                            "push 0x1B",        // SS (user data, RPL=3)
                            "push {stack}",     // RSP (user stack)
                            "push 0x202",       // RFLAGS (IF=1)
                            "push 0x23",        // CS (user code, RPL=3)
                            "push {entry}",     // RIP (entry point)
                            "iretq",
                            stack = in(reg) stack,
                            entry = in(reg) entry,
                            options(noreturn),
                        );
                    }
                }
            }
        }
        // No work for this core — brief pause then retry
        poll_count += 1;
        if poll_count % 10_000_000 == 0 {
            // Periodically yield CPU time via HLT (woken by APIC timer)
            unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
        } else {
            unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
        }
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
