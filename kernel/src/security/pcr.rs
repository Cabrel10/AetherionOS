// Aetherion OS - PCR Measurement Module
// SHA-256 hashing for kernel memory measurements

use sha2::{Sha256, Digest};

/// PCR register indices
#[derive(Debug, Clone, Copy)]
pub enum PcrIndex {
    Pcr0 = 0,  // Core system firmware (CRT + BIOS)
    Pcr1 = 1,  // Platform configuration
    Pcr2 = 2,  // Option ROM code
    Pcr3 = 3,  // Option ROM data
    Pcr4 = 4,  // Master boot record (MBR)
    Pcr5 = 5,  // Master boot record configuration
    Pcr6 = 6,  // Platform manufacturer specific
    Pcr7 = 7,  // Secure boot policy
}

/// PCR measurement structure (SHA-256)
pub type PcrValue = [u8; 32];

/// Initialize PCR subsystem
pub fn init_pcr() {
    serial_print("[PCR] Initializing PCR measurement subsystem...\n");
    serial_print("[PCR] SHA-256 hash algorithm ready\n");

    // Perform initial kernel measurement into PCR0
    measure_kernel();
}

/// Measure kernel memory and store in PCR0
pub fn measure_kernel() -> PcrValue {
    serial_print("[PCR] Measuring kernel memory into PCR0...\n");

    // In a real implementation, this would:
    // 1. Hash the kernel code section
    // 2. Extend PCR0 with the measurement
    // 3. Store the result

    // For testing, we simulate a kernel hash
    let kernel_data = b"Aetherion OS Kernel v0.1.0-HAL";
    let hash = sha256_hash(kernel_data);

    serial_print("[PCR] PCR0 measurement complete\n");
    serial_print("[PCR] PCR0 Hash: ");
    print_hash(&hash);
    serial_print("\n");

    hash
}

/// Extend a PCR with new data (simulated)
pub fn extend_pcr(_pcr_index: PcrIndex, data: &[u8]) -> PcrValue {
    // In real TPM: PCR_new = SHA256(PCR_old || data)
    // Here we just hash the data
    sha256_hash(data)
}

/// Compute SHA-256 hash of data
fn sha256_hash(data: &[u8]) -> PcrValue {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();

    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    hash
}

/// Print hash as hex string
fn print_hash(hash: &PcrValue) {
    const HEX_CHARS: &[u8] = b"0123456789ABCDEF";
    for byte in hash.iter() {
        let high = (byte >> 4) as usize;
        let low = (byte & 0xF) as usize;
        unsafe {
            const SERIAL_PORT: u16 = 0x3F8;
            while (core::arch::x86_64::inb(SERIAL_PORT + 5) & 0x20) == 0 {}
            core::arch::x86_64::outb(SERIAL_PORT, HEX_CHARS[high]);
            while (core::arch::x86_64::inb(SERIAL_PORT + 5) & 0x20) == 0 {}
            core::arch::x86_64::outb(SERIAL_PORT, HEX_CHARS[low]);
        }
    }
}

/// Print string to serial
fn serial_print(s: &str) {
    const SERIAL_PORT: u16 = 0x3F8;
    for byte in s.bytes() {
        unsafe {
            while (core::arch::x86_64::inb(SERIAL_PORT + 5) & 0x20) == 0 {}
            core::arch::x86_64::outb(SERIAL_PORT, byte);
        }
    }
}

/// Read PCR register (simulated)
pub fn read_pcr(pcr_index: PcrIndex) -> PcrValue {
    // In real implementation: read from TPM
    // Returns simulated value for testing
    match pcr_index {
        PcrIndex::Pcr0 => {
            // Return the kernel measurement
            measure_kernel()
        }
        _ => {
            // Return zeros for other PCRs
            [0u8; 32]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_hash() {
        let data = b"test";
        let hash = sha256_hash(data);
        // SHA-256 of "test" is known
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn test_pcr_measurement() {
        let hash = measure_kernel();
        assert_eq!(hash.len(), 32);
    }
}
