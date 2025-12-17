// Aetherion OS - Device Drivers
// Phase 3+: Hardware abstraction with advanced features

// Basic drivers
pub mod keyboard;
pub mod vga;
pub mod ata;

// Advanced drivers
pub mod pci;      // PCI bus enumeration
pub mod usb;      // USB stack (XHCI, HID)
pub mod sdr;      // Software Defined Radio (RTL-SDR, demodulation)

/// Initialize all device drivers
pub fn init_all() {
    // Basic I/O
    keyboard::init();
    vga::init();
    ata::init();
    
    // Advanced peripherals
    pci::init();
    let _ = usb::init();  // May fail if no USB controllers
    let _ = sdr::init();  // May fail if no SDR devices
}
