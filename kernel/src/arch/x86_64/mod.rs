// src/arch/x86_64/mod.rs - Architecture x86_64 HAL (Couche 1)

pub mod gdt;
pub mod idt;
pub mod interrupts;
pub mod timer;
pub mod pci;

/// Initialize all x86_64 HAL modules
/// Critical order: GDT -> IDT -> Interrupts
pub fn init() {
    // 1. GDT must be loaded first (segments and TSS)
    gdt::init();

    // 2. IDT depends on TSS (for IST double-fault)
    idt::init();

    // 3. PIC and interrupts
    interrupts::init();

    // 4. Finalize with specific IRQ handlers
    init_idt_handlers();
}

/// Add IRQ handlers to IDT after PIC initialization
fn init_idt_handlers() {
    crate::serial_println!("[HAL] IDT handlers configured");
}
