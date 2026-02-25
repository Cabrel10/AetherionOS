// src/security/mod.rs - Security Implementation (Couche 1 HAL)
// TPM stub + PCR (Platform Configuration Register) measurements

use sha2::{Sha256, Digest};

/// Initialise la sécurité - vérifie TPM et mesure PCR0
pub fn init() {
    crate::serial_println!("[SECURITY] Initializing...");

    // Vérifier présence TPM 2.0
    if !has_tpm() {
        panic!("[SECURITY] TPM 2.0 absent - Boot refused!");
    }

    // Mesurer PCR0 (boot integrity)
    let pcr0 = measure_pcr0();

    crate::serial_println!("[SECURITY] TPM OK, PCR0 hash initialized");
    crate::serial_println!("[SECURITY] PCR0: {:02x}{:02x}{:02x}{:02x}...",
        pcr0[0], pcr0[1], pcr0[2], pcr0[3]);
}

/// Vérifie la présence d'un TPM 2.0
/// Retourne true si présent, false sinon
/// Stub: dans une vraie implémentation, parser ACPI tables
fn has_tpm() -> bool {
    // TODO: Parser ACPI tables pour vérifier TPM2 table
    // Pour l'instant: stub qui retourne true
    // En production: vérifier via TPM_CRB (Command Response Buffer)
    // ou FIFO interface sur les ports I/O appropriés

    crate::serial_println!("[TPM] ACPI table check stub - assuming present");

    // Simuler présence pour le développement
    // Dans une implémentation réelle:
    // 1. Chercher RSDP (Root System Description Pointer)
    // 2. Parser RSDT/XSDT pour trouver TPM2
    // 3. Vérifier TPM2 table présente et valid

    true
}

/// Mesure PCR0 - représente l'intégrité du boot
/// PCR0 contient typiquement: bootloader hash + kernel hash + config hash
fn measure_pcr0() -> [u8; 32] {
    let mut hasher = Sha256::new();

    // Version du HAL
    hasher.update(b"Aetherion HAL v0.1.0");

    // Hash du bootloader (stub)
    hasher.update(b"bootloader:0.9.23");

    // Hash du kernel (stub - en vrai: mesurer le binaire chargé)
    hasher.update(b"kernel:mvp-core-v0.1.0");

    // Configuration de boot
    hasher.update(b"config:hal-couche1-complete");

    // Timestamp de boot (stub - en vrai: utiliser RTC)
    hasher.update(b"boot:2026-02-20");

    hasher.finalize().into()
}

/// Étend un PCR avec de nouvelles données
/// Utilisé pour chaîner les mesures (boot -> kernel -> modules)
pub fn extend_pcr(pcr_index: u8, data: &[u8]) -> [u8; 32] {
    if pcr_index > 23 {
        panic!("[SECURITY] Invalid PCR index {}, max 23", pcr_index);
    }

    // TPM2.0 PCR extend: new_value = SHA256(old_value || data)
    let mut hasher = Sha256::new();

    // Lire ancienne valeur (stub - en vrai: lire depuis TPM)
    let old_value = [0u8; 32]; // Placeholder
    hasher.update(old_value);
    hasher.update(data);

    let new_value: [u8; 32] = hasher.finalize().into();

    crate::serial_println!("[SECURITY] PCR[{}] extended", pcr_index);

    new_value
}

/// Vérifie l'intégrité d'une donnée contre un hash attendu
pub fn verify_integrity(data: &[u8], expected_hash: &[u8; 32]) -> bool {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let computed_hash: [u8; 32] = hasher.finalize().into();

    computed_hash == *expected_hash
}

/// Retourne le nombre de PCR disponibles (TPM2.0 = 24)
pub fn pcr_count() -> u8 {
    24
}

/// Commandes TPM stub pour démonstration
#[derive(Debug)]
pub enum TpmCommand {
    Startup,
    SelfTest,
    PcrRead(u8),
    PcrExtend(u8),
    GetCapability,
}

impl TpmCommand {
    /// Exécute la commande (stub)
    pub fn execute(&self) -> Result<(), &'static str> {
        match self {
            TpmCommand::Startup => {
                crate::serial_println!("[TPM] Startup command executed");
                Ok(())
            }
            TpmCommand::SelfTest => {
                crate::serial_println!("[TPM] Self-test passed");
                Ok(())
            }
            TpmCommand::PcrRead(idx) => {
                if *idx > 23 {
                    return Err("Invalid PCR index");
                }
                crate::serial_println!("[TPM] PCR[{}] read", idx);
                Ok(())
            }
            TpmCommand::PcrExtend(idx) => {
                if *idx > 23 {
                    return Err("Invalid PCR index");
                }
                crate::serial_println!("[TPM] PCR[{}] extended", idx);
                Ok(())
            }
            TpmCommand::GetCapability => {
                crate::serial_println!("[TPM] Capabilities retrieved");
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_security_init() {
        init();
        // Si on arrive ici, TPM stub a fonctionné
    }

    #[test_case]
    fn test_pcr_count() {
        assert_eq!(pcr_count(), 24);
    }

    #[test_case]
    fn test_measure_pcr0() {
        let pcr0 = measure_pcr0();
        // Vérifie que c'est bien 32 bytes (SHA256)
        assert_eq!(pcr0.len(), 32);
    }

    #[test_case]
    fn test_extend_pcr() {
        let new_pcr = extend_pcr(0, b"test data");
        assert_eq!(new_pcr.len(), 32);
    }

    #[test_case]
    fn test_integrity_verification() {
        let data = b"test data for integrity";
        let mut hasher = Sha256::new();
        hasher.update(data);
        let expected: [u8; 32] = hasher.finalize().into();

        assert!(verify_integrity(data, &expected));
        assert!(!verify_integrity(b"tampered data", &expected));
    }

    #[test_case]
    fn test_tpm_commands() {
        assert!(TpmCommand::Startup.execute().is_ok());
        assert!(TpmCommand::SelfTest.execute().is_ok());
        assert!(TpmCommand::PcrRead(0).execute().is_ok());
        assert!(TpmCommand::PcrRead(25).execute().is_err());
    }
}
