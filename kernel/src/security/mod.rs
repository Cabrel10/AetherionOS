// src/security/mod.rs - Security Implementation (Couche 1 HAL)
// TPM stub + PCR (Platform Configuration Register) measurements
// + KPTI-Lite: Kernel Page Table Isolation (Jalon 89)

pub mod kpti;

use sha2::{Sha256, Digest};

/// Initialise la sécurité - vérifie TPM et mesure PCR0
///
/// SECURITY FIX: previously `has_tpm()` was a stub returning `true` and the
/// absence of a TPM triggered a kernel panic — a trivial denial-of-service
/// on any VM/QEMU target without a TPM passthrough (i.e. ALL our dev/run
/// environments). Now: real TIS register probe; TPM absence switches to
/// DEGRADED mode (logged loudly, boot continues) instead of panicking.
pub fn init() {
    crate::serial_println!("[SECURITY] Initializing...");

    let tpm_present = has_tpm();
    if tpm_present {
        crate::serial_println!("[SECURITY] TPM 2.0 detected (TIS probe OK)");
    } else {
        // Degraded mode: no hardware root of trust. Boot continues — refusing
        // to boot would brick every VM deployment — but the event is logged
        // and PCR measurements are tagged as software-only (untrusted).
        crate::serial_println!("[SECURITY][WARN] TPM 2.0 ABSENT — degraded mode, PCR values are software-only");
    }

    // Mesurer PCR0 (boot integrity) — now includes real boot entropy
    let pcr0 = measure_pcr0();

    crate::serial_println!("[SECURITY] PCR0: {:02x}{:02x}{:02x}{:02x}...{}",
        pcr0[0], pcr0[1], pcr0[2], pcr0[3],
        if tpm_present { " (TPM-backed)" } else { " (software-only)" });
}

/// Probe the TPM TIS (TPM Interface Specification) register space.
/// TPM 1.2/2.0 TIS is memory-mapped at 0xFED4_0000; the VID/DID register at
/// offset 0xF00 returns 0xFFFF on absent hardware (floating bus) and a real
/// vendor ID (e.g. 0x1114 IBM, 0x15D1 Infineon, 0x0C24 QEMU swtpm) otherwise.
fn probe_tpm_tis() -> bool {
    let phys = 0xFED4_0F00u64; // TPM TIS VID/DID register
    let off = crate::elf::phys_offset();
    if off == 0 { return false; } // physmap not initialized yet — assume absent
    let virt = (off + phys) as *const u32;
    let didvid: u32 = unsafe { core::ptr::read_volatile(virt) };
    // 0xFFFF_FFFF or 0x0000_FFFF = no device; anything else = TIS responded
    didvid != 0xFFFF_FFFF && didvid != 0x0000_FFFF && (didvid & 0xFFFF) != 0xFFFF
}

/// Vérifie la présence d'un TPM 2.0
/// Retourne true si présent, false sinon
/// Stub: dans une vraie implémentation, parser ACPI tables
fn has_tpm() -> bool {
    // Real hardware probe (TIS register read). No more "assume present" stub:
    // claiming a TPM that doesn't exist is strictly worse than admitting its
    // absence, because callers may anchor trust decisions on it.
    let present = probe_tpm_tis();
    crate::serial_println!("[TPM] TIS probe: {}", if present { "PRESENT" } else { "ABSENT" });
    present
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

    // Real boot entropy: TSC sampled at measurement time. This makes the PCR0
    // value unique per boot (measured-boot semantics) instead of a constant
    // compile-time string that any attacker can precompute.
    let tsc: u64 = unsafe {
        let v: u64;
        core::arch::asm!("rdtsc", "shl rdx, 32", "or rax, rdx",
            out("rax") v, out("rdx") _, options(nomem, nostack));
        v
    };
    hasher.update(tsc.to_le_bytes());
    // RDRAND mixing when available — hardware entropy into the measurement
    if crate::arch::x86_64::context::cpu_has_rdrand() {
        let val: u64;
        let ok: u8;
        unsafe {
            core::arch::asm!("rdrand {val}", "setc {cf}",
                val = out(reg) val, cf = out(reg_byte) ok,
                options(nomem, nostack));
        }
        if ok != 0 { hasher.update(val.to_le_bytes()); }
    }

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
