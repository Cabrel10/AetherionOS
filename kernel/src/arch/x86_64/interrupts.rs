// src/arch/x86_64/interrupts.rs - Interrupts Implementation (Couche 1 HAL)
// PIC 8259 Programmable Interrupt Controller + Handlers IRQ

use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::structures::idt::InterruptStackFrame;

// Offset IRQ dans l'IDT (après les 32 exceptions CPU)
// IRQ 0-7 -> vecteurs 32-39
// IRQ 8-15 -> vecteurs 40-47
pub const PIC1_OFFSET: u8 = 32;
pub const PIC2_OFFSET: u8 = PIC1_OFFSET + 8;

/// PIC 8259 chaîné (maître + esclave)
pub static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC1_OFFSET, PIC2_OFFSET) });

/// Numéros d'interruptions pour les IRQ
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC1_OFFSET,           // IRQ 0 - Timer PIT
    Keyboard = PIC1_OFFSET + 1,    // IRQ 1 - Clavier PS/2
    Cascade = PIC1_OFFSET + 2,     // IRQ 2 - Cascade PIC
    Com2 = PIC1_OFFSET + 3,        // IRQ 3 - Serial COM2
    Com1 = PIC1_OFFSET + 4,        // IRQ 4 - Serial COM1
    Lpt2 = PIC1_OFFSET + 5,        // IRQ 5 - LPT2
    Floppy = PIC1_OFFSET + 6,      // IRQ 6 - Floppy
    Lpt1 = PIC1_OFFSET + 7,        // IRQ 7 - LPT1
    Rtc = PIC2_OFFSET,             // IRQ 8 - RTC
    Mouse = PIC2_OFFSET + 4,       // IRQ 12 - Souris PS/2
    IdePrimary = PIC2_OFFSET + 6,  // IRQ 14 - IDE Primary
    IdeSecondary = PIC2_OFFSET + 7,// IRQ 15 - IDE Secondary
}

impl InterruptIndex {
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn as_usize(self) -> usize {
        usize::from(self.as_u8())
    }
}

/// Initialise le PIC et active les interruptions
/// Remap IRQs 0-15 vers vecteurs 32-47 pour éviter conflits avec CPU exceptions
pub fn init() {
    unsafe {
        // Remap et initialise les PICs
        PICS.lock().initialize();

        // Masque: enable timer (IRQ0), keyboard (IRQ1)
        // 0xFC = 11111100 - active IRQ0, IRQ1
        // 0xFF = 11111111 - désactive tous les IRQ esclave pour l'instant
        PICS.lock().write_masks(0xFC, 0xFF);
    }

    // Active les interruptions (instruction STI)
    x86_64::instructions::interrupts::enable();

    crate::serial_println!("[INTERRUPTS] PIC initialized (IRQs remapped 32-47)");
    crate::serial_println!("[INTERRUPTS] Timer (IRQ0) and Keyboard (IRQ1) enabled");
}

/// End of Interrupt - notifie le PIC qu'on a fini de traiter l'IRQ
pub fn end_of_interrupt(interrupt_id: u8) {
    unsafe {
        PICS.lock().notify_end_of_interrupt(interrupt_id);
    }
}

/// Désactive toutes les interruptions (CLI)
pub fn disable() {
    x86_64::instructions::interrupts::disable();
}

/// Réactive les interruptions si elles étaient activées
pub fn enable() {
    x86_64::instructions::interrupts::enable();
}

/// Execute une fonction avec interruptions désactivées
pub fn without_interrupts<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    x86_64::instructions::interrupts::without_interrupts(f)
}

/// Active une IRQ spécifique
pub fn unmask_irq(irq: u8) {
    unsafe {
        let mut pics = PICS.lock();
        let masks = pics.read_masks();
        if irq < 8 {
            // IRQ maître
            pics.write_masks(masks.0 & !(1 << irq), masks.1);
        } else {
            // IRQ esclave
            let slave_irq = irq - 8;
            pics.write_masks(masks.0, masks.1 & !(1 << slave_irq));
        }
    }
}

/// Désactive une IRQ spécifique
pub fn mask_irq(irq: u8) {
    unsafe {
        let mut pics = PICS.lock();
        let masks = pics.read_masks();
        if irq < 8 {
            pics.write_masks(masks.0 | (1 << irq), masks.1);
        } else {
            let slave_irq = irq - 8;
            pics.write_masks(masks.0, masks.1 | (1 << slave_irq));
        }
    }
}

// ===== Handlers IRQ =====

pub extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Tick du timer - pour l'instant juste EOI
    // Plus tard: scheduler, timeouts, etc.

    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

pub extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    // Lire le scancode du port 0x60
    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };

    crate::serial_println!("[KEYBOARD] Scancode: 0x{:02x}", scancode);

    // Notifier fin d'interruption
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_interrupts_init() {
        // Note: Ce test ne peut pas vraiment tester init() car il nécessite
        // un environnement QEMU/hardware. On teste les constantes.
        assert_eq!(PIC1_OFFSET, 32);
        assert_eq!(PIC2_OFFSET, 40);
    }

    #[test_case]
    fn test_interrupt_index_values() {
        assert_eq!(InterruptIndex::Timer.as_u8(), 32);
        assert_eq!(InterruptIndex::Keyboard.as_u8(), 33);
        assert_eq!(InterruptIndex::Rtc.as_u8(), 40);
        assert_eq!(InterruptIndex::IdePrimary.as_u8(), 46);
    }

    #[test_case]
    fn test_without_interrupts() {
        // Teste que without_interrupts fonctionne
        let result = without_interrupts(|| {
            // Dans cette section, interruptions sont désactivées
            42
        });
        assert_eq!(result, 42);
    }
}
