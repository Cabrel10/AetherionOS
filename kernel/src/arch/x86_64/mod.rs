// src/arch/x86_64/mod.rs - Architecture x86_64 HAL (Couche 1)

pub mod gdt;
pub mod idt;
pub mod interrupts;
pub mod timer;

/// Initialise tous les modules HAL x86_64
/// Ordre critique: GDT -> IDT -> Interrupts
pub fn init() {
    // 1. GDT doit être chargée en premier (segments et TSS)
    gdt::init();

    // 2. IDT dépend du TSS (pour IST double-fault)
    // L'IDT est chargée ici, les handlers IRQ seront ajoutés après PIC init
    idt::init();

    // 3. PIC et interrupts - handlers IRQ ajoutés dans init_idt_handlers
    interrupts::init();

    // 4. Finaliser avec les handlers IRQ spécifiques
    init_idt_handlers();
}

/// Ajoute les handlers IRQ à l'IDT après initialisation PIC
/// Doit être appelé après idt::init() mais avant interrupts::enable()
fn init_idt_handlers() {
    // Les handlers sont déjà définis dans idt.rs avec set_handler_fn
    // mais pour les IRQs dynamiques, on les configure via la fonction externe
    // Note: En pratique, on pourrait utiliser une IDT mutable ou lazy init
    // Pour l'instant, les handlers sont statiques dans idt.rs

    crate::serial_println!("[HAL] IDT handlers configured");
}
