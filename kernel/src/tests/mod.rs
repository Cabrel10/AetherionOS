// src/tests/mod.rs - HAL Tests (Couche 1)
// Tests unitaires pour tous les modules HAL

#[cfg(test)]
pub mod hal_tests {
    // Test GDT initialization
    #[test_case]
    fn test_gdt_load() {
        // Le GDT est déjà initialisé par le main
        // On teste juste que les fonctions existent
        crate::arch::x86_64::gdt::double_fault_ist_index();
    }

    // Test IDT initialization
    #[test_case]
    fn test_idt_load() {
        // L'IDT est déjà initialisée par le main
        // On peut tester que la référence existe
        let _idt = crate::arch::x86_64::idt::idt_ref();
    }

    // Test Interrupt constants
    #[test_case]
    fn test_interrupts_init() {
        use crate::arch::x86_64::interrupts;
        assert_eq!(interrupts::PIC1_OFFSET, 32);
        assert_eq!(interrupts::PIC2_OFFSET, 40);
    }

    // Test Security PCR
    #[test_case]
    fn test_security_init() {
        use crate::security;
        // Test que les fonctions existent et fonctionnent
        assert_eq!(security::pcr_count(), 24);
    }

    // Test des index d'interruption
    #[test_case]
    fn test_interrupt_index() {
        use crate::arch::x86_64::interrupts::InterruptIndex;
        assert_eq!(InterruptIndex::Timer.as_u8(), 32);
        assert_eq!(InterruptIndex::Keyboard.as_u8(), 33);
        assert_eq!(InterruptIndex::Rtc.as_u8(), 40);
    }

    // Test de without_interrupts
    #[test_case]
    fn test_without_interrupts() {
        use crate::arch::x86_64::interrupts::without_interrupts;
        let result = without_interrupts(|| 42);
        assert_eq!(result, 42);
    }

    // Test de verify_integrity
    #[test_case]
    fn test_verify_integrity() {
        use crate::security::verify_integrity;
        use sha2::{Sha256, Digest};

        let data = b"test data";
        let mut hasher = Sha256::new();
        hasher.update(data);
        let hash: [u8; 32] = hasher.finalize().into();

        assert!(verify_integrity(data, &hash));
        assert!(!verify_integrity(b"wrong data", &hash));
    }
}
