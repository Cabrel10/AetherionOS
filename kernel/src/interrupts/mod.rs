// Aetherion OS - Interrupt Descriptor Table (IDT)
// Phase 2.1: Interrupt and exception handling

pub mod idt;
pub mod handlers;

pub use idt::{Idt, InterruptDescriptorTable};
pub use handlers::*;

/// Initialize interrupt system
pub fn init() {
    idt::init_idt();
}
