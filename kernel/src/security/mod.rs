// Aetherion OS - Security Module (Phase 1.3)
// TPM detection and PCR measurement

pub mod tpm;
pub mod pcr;

/// Initialize security layer
pub fn init() {
    serial_print("[SECURITY] Initializing security subsystem...\n");

    // Detect TPM
    tpm::detect_tpm();

    // Initialize PCR measurements
    pcr::init_pcr();

    serial_print("[SECURITY] Security layer initialized\n");
}

/// Print string to serial port (local helper)
fn serial_print(s: &str) {
    const SERIAL_PORT: u16 = 0x3F8;

    for byte in s.bytes() {
        unsafe {
            // Wait for transmit buffer to be empty
            while (core::arch::x86_64::inb(SERIAL_PORT + 5) & 0x20) == 0 {}
            core::arch::x86_64::outb(SERIAL_PORT, byte);
        }
    }
}
