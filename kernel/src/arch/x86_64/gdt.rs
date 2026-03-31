// src/arch/x86_64/gdt.rs - GDT Implementation (Couche 1 HAL + Couche 6 Ring 3)
// Global Descriptor Table with TSS for double-fault handler
// Ring 3 user-mode selectors for process isolation
//
// GDT Layout:
//   Entry 0: Null descriptor
//   Entry 1: Kernel Code Segment (Ring 0, CS=0x08)
//   Entry 2: Kernel Data Segment (Ring 0, DS=0x10)
//   Entry 3: User Data Segment   (Ring 3, DS=0x1B) -- data before code for syscall/sysret
//   Entry 4: User Code Segment   (Ring 3, CS=0x23)
//   Entry 5-6: TSS (64-bit TSS takes 2 GDT entries)

use x86_64::VirtAddr;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use lazy_static::lazy_static;

// Index IST for double-fault (separate stack)
const DOUBLE_FAULT_IST_INDEX: u16 = 0;

// Task State Segment - contains stack pointers for exceptions
// RING3_INT_STACK: Dedicated kernel stack for Ring 3 -> Ring 0 transitions
// (interrupts, exceptions from user mode use TSS.privilege_stack_table[0] = RSP0)
const RING3_INT_STACK_SIZE: usize = 4096 * 64; // 256 KiB for deep VFS/FAT32/VirtIO chains

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();

        // IST[0]: Double-fault stack (separate, always valid)
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            const STACK_SIZE: usize = 4096 * 5;  // 20KB stack for double-fault
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
            let stack_start = VirtAddr::from_ptr(unsafe { &STACK });
            stack_start + STACK_SIZE as u64  // Stack grows downwards
        };

        // RSP0 (privilege_stack_table[0]): Kernel stack for Ring 3 -> Ring 0
        // When any interrupt/exception occurs in Ring 3, the CPU loads RSP from
        // TSS.privilege_stack_table[0]. Without this, Ring 3 exceptions (page faults,
        // timer ticks, etc.) write to RSP=0 causing immediate double fault.
        tss.privilege_stack_table[0] = {
            static mut RING3_STACK: [u8; RING3_INT_STACK_SIZE] = [0; RING3_INT_STACK_SIZE];
            let stack_start = VirtAddr::from_ptr(unsafe { &RING3_STACK });
            stack_start + RING3_INT_STACK_SIZE as u64
        };

        tss
    };

    /// Global Descriptor Table with kernel, user segments and TSS
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        // Entry 1: Kernel code segment (Ring 0)
        let code_selector = gdt.add_entry(Descriptor::kernel_code_segment());
        // Entry 2: Kernel data segment (Ring 0)
        let data_selector = gdt.add_entry(Descriptor::kernel_data_segment());
        // Entry 3: User data segment (Ring 3) - must come before user code for sysret
        let user_data_selector = gdt.add_entry(Descriptor::user_data_segment());
        // Entry 4: User code segment (Ring 3)
        let user_code_selector = gdt.add_entry(Descriptor::user_code_segment());
        // Entry 5-6: TSS segment for exceptions
        let tss_selector = gdt.add_entry(Descriptor::tss_segment(&TSS));
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

/// Initialize the GDT and load segments
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

    crate::serial_println!("[GDT] Loaded: Kernel(R0) + User(R3) + TSS");
}

/// Load the BSP's GDT on an AP core (Jalon 101).
/// Called from ap_main after the trampoline. Loads the same GDT and segment
/// selectors that the BSP uses. Does NOT load the TSS (shared TSS causes #GP
/// because it's already marked as "busy" by the BSP).
/// Safe to call from any core after BSP's init() has completed.
pub fn load_for_ap() {
    use x86_64::instructions::segmentation::{Segment, CS, DS};

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        DS::set_reg(GDT.1.data_selector);
        // NOTE: We intentionally do NOT load the TSS on the AP.
        // The TSS is already marked as "busy" by the BSP, and loading it
        // on another CPU would cause #GP. The AP uses the BSP's IST stacks
        // which are shared (acceptable for our 2-core setup).
    }
}

/// Return the IST index for double-fault
pub const fn double_fault_ist_index() -> u16 {
    DOUBLE_FAULT_IST_INDEX
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
