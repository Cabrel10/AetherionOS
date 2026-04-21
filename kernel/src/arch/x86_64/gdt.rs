// src/arch/x86_64/gdt.rs - GDT Implementation (Couche 1 HAL + Couche 6 Ring 3)
// Jalon 102: Per-Core GDT/TSS for True SMP Ring 3 Execution
//
// Each CPU core needs its own TSS because:
//   1. The TSS is marked "busy" by LTR; a second core loading the same TSS causes #GP
//   2. RSP0 in the TSS must point to a per-core kernel stack for Ring3->Ring0 transitions
//   3. IST stacks must be per-core to avoid stack corruption during concurrent exceptions
//
// GDT Layout (identical on every core):
//   Entry 0: Null descriptor
//   Entry 1: Kernel Code Segment (Ring 0, CS=0x08)
//   Entry 2: Kernel Data Segment (Ring 0, DS=0x10)
//   Entry 3: User Data Segment   (Ring 3, DS=0x1B) -- data before code for syscall/sysret
//   Entry 4: User Code Segment   (Ring 3, CS=0x23)
//   Entry 5-6: TSS (64-bit TSS takes 2 GDT entries) -- per-core TSS pointer differs
//
// Scales to Threadripper/EPYC 128 cores: just increase MAX_CPUS in apic.rs.

use x86_64::VirtAddr;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use lazy_static::lazy_static;
use core::sync::atomic::{AtomicBool, Ordering};

/// Maximum CPUs supported (matches apic.rs MAX_CPUS)
const MAX_CPUS: usize = 16;

// Index IST for double-fault (separate stack)
const DOUBLE_FAULT_IST_INDEX: u16 = 0;
// Jalon 134c: Index IST for #PF (page fault) - a dedicated stack so that a
// PF taken while syscall_entry is still on user RSP has a trustworthy kernel
// stack to run on. Required to diagnose/recover from SYSCALL-path faults.
const PAGE_FAULT_IST_INDEX: u16 = 1;

// Task State Segment - contains stack pointers for exceptions
// RING3_INT_STACK: Dedicated kernel stack for Ring 3 -> Ring 0 transitions
// (interrupts, exceptions from user mode use TSS.privilege_stack_table[0] = RSP0)
const RING3_INT_STACK_SIZE: usize = 4096 * 64; // 256 KiB for deep VFS/FAT32/VirtIO chains

// =====================================================================
// Per-Core TSS/GDT Static Storage (Jalon 102)
// =====================================================================

/// Per-core RSP0 stacks (Ring3 -> Ring0 interrupt/exception stacks).
/// Each AP core gets its own 16 KiB RSP0 stack.
/// BSP uses the lazy_static TSS (256 KiB), APs use these smaller stacks.
const AP_RSP0_STACK_SIZE: usize = 16384; // 16 KiB per AP core

/// Per-core double-fault IST stacks (20 KiB each)
const AP_DF_STACK_SIZE: usize = 4096 * 5; // 20 KiB

/// Per-core page-fault IST stacks (32 KiB each) - Jalon 134c
const AP_PF_STACK_SIZE: usize = 4096 * 8; // 32 KiB

/// Static per-core RSP0 stacks for AP cores (cores 1..MAX_CPUS-1)
/// Only 15 AP cores need stacks; BSP (core 0) uses the lazy_static TSS.
static mut AP_RSP0_STACKS: [[u8; AP_RSP0_STACK_SIZE]; MAX_CPUS] = [[0; AP_RSP0_STACK_SIZE]; MAX_CPUS];

/// Static per-core double-fault stacks for AP cores
static mut AP_DF_STACKS: [[u8; AP_DF_STACK_SIZE]; MAX_CPUS] = [[0; AP_DF_STACK_SIZE]; MAX_CPUS];

/// Static per-core page-fault stacks for AP cores (Jalon 134c)
static mut AP_PF_STACKS: [[u8; AP_PF_STACK_SIZE]; MAX_CPUS] = [[0; AP_PF_STACK_SIZE]; MAX_CPUS];

/// Per-core TSS structures (runtime-initialized)
static mut AP_TSS: [TaskStateSegment; MAX_CPUS] = {
    const INIT: TaskStateSegment = TaskStateSegment::new();
    [INIT; MAX_CPUS]
};

/// Per-core GDT structures (runtime-initialized)
static mut AP_GDTS: [GlobalDescriptorTable; MAX_CPUS] = {
    const INIT: GlobalDescriptorTable = GlobalDescriptorTable::new();
    [INIT; MAX_CPUS]
};

/// Per-core TSS selectors (stored after GDT construction)
static mut AP_TSS_SELECTORS: [u16; MAX_CPUS] = [0; MAX_CPUS];

/// Per-core GDT ready flags
static AP_GDT_READY: [AtomicBool; MAX_CPUS] = {
    const INIT: AtomicBool = AtomicBool::new(false);
    [INIT; MAX_CPUS]
};

// =====================================================================
// BSP GDT/TSS (unchanged lazy_static for backwards compatibility)
// =====================================================================

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();

        // IST[0]: Double-fault stack (separate, always valid)
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            const STACK_SIZE: usize = 4096 * 5;  // 20KB stack for double-fault
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
            let stack_start = VirtAddr::from_ptr(&raw const STACK as *const u8);
            stack_start + STACK_SIZE as u64  // Stack grows downwards
        };

        // Jalon 134c: IST[1]: Page-fault stack (dedicated kernel stack for #PF).
        // Needed because SYSCALL does not swap RSP automatically, so a fault in
        // syscall_entry before the `mov gs:0x0, rsp` would otherwise run on the
        // user stack - unreliable for diagnostics and can cascade into #DF.
        tss.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = {
            const STACK_SIZE: usize = 4096 * 8;  // 32KB stack for page-fault
            static mut PF_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
            let stack_start = VirtAddr::from_ptr(&raw const PF_STACK as *const u8);
            stack_start + STACK_SIZE as u64  // Stack grows downwards
        };

        // RSP0 (privilege_stack_table[0]): Kernel stack for Ring 3 -> Ring 0
        // When any interrupt/exception occurs in Ring 3, the CPU loads RSP from
        // TSS.privilege_stack_table[0]. Without this, Ring 3 exceptions (page faults,
        // timer ticks, etc.) write to RSP=0 causing immediate double fault.
        tss.privilege_stack_table[0] = {
            static mut RING3_STACK: [u8; RING3_INT_STACK_SIZE] = [0; RING3_INT_STACK_SIZE];
            let stack_start = VirtAddr::from_ptr(&raw const RING3_STACK as *const u8);
            stack_start + RING3_INT_STACK_SIZE as u64
        };

        tss
    };

    /// Global Descriptor Table with kernel, user segments and TSS
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        // Entry 1: Kernel code segment (Ring 0)
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        // Entry 2: Kernel data segment (Ring 0)
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        // Entry 3: User data segment (Ring 3) - must come before user code for sysret
        let user_data_selector = gdt.append(Descriptor::user_data_segment());
        // Entry 4: User code segment (Ring 3)
        let user_code_selector = gdt.append(Descriptor::user_code_segment());
        // Entry 5-6: TSS segment for exceptions
        let tss_selector = gdt.append(Descriptor::tss_segment(&TSS));
        (gdt, Selectors {
            code_selector,
            data_selector,
            user_code_selector,
            user_data_selector,
            tss_selector,
        })
    };
}

/// GDT Segment Selectors
pub struct Selectors {
    pub code_selector: SegmentSelector,
    pub data_selector: SegmentSelector,
    pub user_code_selector: SegmentSelector,
    pub user_data_selector: SegmentSelector,
    pub tss_selector: SegmentSelector,
}

/// Initialize the GDT and load segments (BSP only)
/// Must be called before IDT initialization
pub fn init() {
    use x86_64::instructions::tables::load_tss;
    use x86_64::instructions::segmentation::{Segment, CS, DS};

    // Load the GDT
    GDT.0.load();

    // SAFETY: The GDT was just loaded above, so the selectors are valid.
    // CS::set_reg reloads the code segment register to point at kernel code.
    // DS::set_reg sets the data segment. load_tss activates the TSS for IST.
    // This sequence must happen exactly once during boot, in this order.
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        DS::set_reg(GDT.1.data_selector);
        load_tss(GDT.1.tss_selector);
    }

    // J134c: dump TSS IST entries for diagnostic
    let ist0 = TSS.interrupt_stack_table[0].as_u64();
    let ist1 = TSS.interrupt_stack_table[1].as_u64();
    let rsp0 = TSS.privilege_stack_table[0].as_u64();
    crate::serial_println!(
        "[GDT] Loaded: Kernel(R0) + User(R3) + TSS (BSP) IST0=0x{:X} IST1=0x{:X} RSP0=0x{:X}",
        ist0, ist1, rsp0
    );
}

// =====================================================================
// Jalon 102: Per-Core GDT/TSS for Application Processors
// =====================================================================

/// Create and load a per-core GDT with its own TSS for an AP core.
///
/// This function:
///   1. Initializes a TSS with per-core RSP0 and IST stacks
///   2. Builds a GDT with kernel/user segments + per-core TSS
///   3. Loads the GDT, sets CS/DS, and loads the TSS via LTR
///
/// SAFETY: Must be called exactly once per core, from the AP itself.
/// The core_id must be 1..MAX_CPUS-1 (core 0 = BSP uses lazy_static GDT).
pub fn create_and_load_per_core_gdt(core_id: u8) {
    use x86_64::instructions::tables::load_tss;
    use x86_64::instructions::segmentation::{Segment, CS, DS};

    let idx = core_id as usize;
    if idx == 0 || idx >= MAX_CPUS {
        // Core 0 uses BSP's GDT; out-of-range cores are ignored
        return;
    }

    unsafe {
        // Step 1: Initialize per-core TSS
        let tss = &mut AP_TSS[idx];
        *tss = TaskStateSegment::new();

        // RSP0: Per-core kernel stack for Ring3->Ring0 transitions
        let rsp0_stack_ptr = &AP_RSP0_STACKS[idx][0] as *const u8 as u64;
        let rsp0_top = rsp0_stack_ptr + AP_RSP0_STACK_SIZE as u64;
        tss.privilege_stack_table[0] = VirtAddr::new(rsp0_top);

        // IST[0]: Per-core double-fault stack
        let df_stack_ptr = &AP_DF_STACKS[idx][0] as *const u8 as u64;
        let df_top = df_stack_ptr + AP_DF_STACK_SIZE as u64;
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = VirtAddr::new(df_top);

        // Jalon 134c: IST[1]: Per-core page-fault stack
        let pf_stack_ptr = &AP_PF_STACKS[idx][0] as *const u8 as u64;
        let pf_top = pf_stack_ptr + AP_PF_STACK_SIZE as u64;
        tss.interrupt_stack_table[PAGE_FAULT_IST_INDEX as usize] = VirtAddr::new(pf_top);

        // Step 2: Build per-core GDT
        // Use a fresh GDT with the same segment layout as BSP
        let gdt = &mut AP_GDTS[idx];
        *gdt = GlobalDescriptorTable::new();

        // Same segment order as BSP: kernel code, kernel data, user data, user code, TSS
        let _code_sel = gdt.append(Descriptor::kernel_code_segment());
        let _data_sel = gdt.append(Descriptor::kernel_data_segment());
        let _udata_sel = gdt.append(Descriptor::user_data_segment());
        let _ucode_sel = gdt.append(Descriptor::user_code_segment());

        // TSS descriptor uses tss_segment_unchecked to avoid the &'static requirement
        let tss_ptr = tss as *const TaskStateSegment;
        let tss_sel = gdt.append(Descriptor::tss_segment_unchecked(tss_ptr));
        AP_TSS_SELECTORS[idx] = tss_sel.0;

        // Step 3: Load GDT using load_unsafe (does not require &'static self)
        gdt.load_unsafe();

        // Step 4: Set segment registers
        // CS=0x08 (kernel code), DS=0x10 (kernel data), SS=0x10 (kernel data)
        // CRITICAL: Must set SS explicitly! The trampoline left SS=0x18 (its data segment),
        // but in our GDT entry 3 is user_data (DPL=3). If we don't fix SS, the first
        // timer interrupt will push SS=0x18 and IRETQ will #GP trying to load a
        // DPL=3 segment into SS while returning to Ring 0.
        CS::set_reg(GDT.1.code_selector);
        DS::set_reg(GDT.1.data_selector);
        // Set SS = 0x10 (kernel data segment, same as DS)
        core::arch::asm!(
            "mov ax, 0x10",
            "mov ss, ax",
            options(nomem, nostack)
        );

        // Step 5: Load per-core TSS (LTR instruction)
        load_tss(tss_sel);

        // Mark this core's GDT as ready
        AP_GDT_READY[idx].store(true, Ordering::SeqCst);
    }
}

/// Check if a core's per-core GDT has been initialized.
pub fn ap_gdt_ready(core_id: u8) -> bool {
    let idx = core_id as usize;
    if idx >= MAX_CPUS { return false; }
    if idx == 0 { return true; } // BSP always ready
    AP_GDT_READY[idx].load(Ordering::SeqCst)
}

/// Load the BSP's GDT on an AP core (Jalon 101 legacy fallback).
/// DEPRECATED for Jalon 102+: Use create_and_load_per_core_gdt() instead.
/// Kept for compatibility. Does NOT load the TSS.
pub fn load_for_ap() {
    use x86_64::instructions::segmentation::{Segment, CS, DS};

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        DS::set_reg(GDT.1.data_selector);
    }
}

/// Return the IST index for double-fault
pub const fn double_fault_ist_index() -> u16 {
    DOUBLE_FAULT_IST_INDEX
}

/// Return the IST index for page-fault (Jalon 134c)
pub const fn page_fault_ist_index() -> u16 {
    PAGE_FAULT_IST_INDEX
}

/// Return the kernel code segment selector
pub fn kernel_code_selector() -> SegmentSelector {
    GDT.1.code_selector
}

/// Return the kernel data segment selector
pub fn kernel_data_selector() -> SegmentSelector {
    GDT.1.data_selector
}

/// Return the user code segment selector (Ring 3)
pub fn user_code_selector() -> SegmentSelector {
    GDT.1.user_code_selector
}

/// Return the user data segment selector (Ring 3)
pub fn user_data_selector() -> SegmentSelector {
    GDT.1.user_data_selector
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_gdt_init() {
        init();
        // If we reach here without panic, GDT is correctly loaded
    }

    #[test_case]
    fn test_tss_ist_index() {
        assert_eq!(double_fault_ist_index(), 0);
    }

    #[test_case]
    fn test_user_selectors_rpl3() {
        let ucs = user_code_selector();
        let uds = user_data_selector();
        // RPL is in bits 0:1 of the selector
        assert_eq!(ucs.0 & 0x3, 3, "User code selector must have RPL=3");
        assert_eq!(uds.0 & 0x3, 3, "User data selector must have RPL=3");
    }
}
