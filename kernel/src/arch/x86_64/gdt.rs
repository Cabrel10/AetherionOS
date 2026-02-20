// src/arch/x86_64/gdt.rs - GDT Implementation (Couche 1 HAL)
// Global Descriptor Table avec TSS pour double-fault handler

use x86_64::VirtAddr;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::registers::segmentation::{CS, DS};
use lazy_static::lazy_static;

// Index IST pour double-fault (stack séparé)
const DOUBLE_FAULT_IST_INDEX: u16 = 0;

/// Task State Segment - contient les pointeurs de stack pour exceptions
lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            const STACK_SIZE: usize = 4096 * 5;  // 20KB stack pour double-fault
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
            // Safety: STACK est static mut, accessible uniquement ici une fois
            let stack_start = VirtAddr::from_ptr(unsafe { &STACK });
            stack_start + STACK_SIZE as u64  // Stack grows downwards
        };
        tss
    };

    /// Global Descriptor Table avec segments kernel et TSS
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        // Segment code kernel (ring 0)
        let code_selector = gdt.append(Descriptor::kernel_code_segment());
        // Segment data kernel (ring 0)
        let data_selector = gdt.append(Descriptor::kernel_data_segment());
        // TSS segment pour exceptions
        let tss_selector = gdt.append(Descriptor::tss_segment(&TSS));
        (gdt, Selectors { code_selector, data_selector, tss_selector })
    };
}

/// Selecteurs de segments GDT
struct Selectors {
    code_selector: SegmentSelector,
    data_selector: SegmentSelector,
    tss_selector: SegmentSelector,
}

/// Initialise le GDT et charge les segments
/// Doit être appelé avant l'initialisation de l'IDT
pub fn init() {
    use x86_64::instructions::tables::load_tss;
    use x86_64::instructions::segmentation::{Segment, CS, DS};

    // Charger la GDT
    GDT.0.load();

    // Safety: Les selecteurs sont valides car GDT vient d'être chargée
    unsafe {
        CS::set_reg(GDT.1.code_selector);
        DS::set_reg(GDT.1.data_selector);
        load_tss(GDT.1.tss_selector);
    }

    crate::serial_println!("[GDT] Loaded with TSS (IST for double-fault)");
}

/// Retourne l'index IST pour double-fault
pub const fn double_fault_ist_index() -> u16 {
    DOUBLE_FAULT_IST_INDEX
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_gdt_init() {
        init();
        // Si on arrive ici sans panic, le GDT est correctement chargé
    }

    #[test_case]
    fn test_tss_ist_index() {
        assert_eq!(double_fault_ist_index(), 0);
    }
}
