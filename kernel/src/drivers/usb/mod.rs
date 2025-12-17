// USB Driver Module
// Supports USB 3.0 (XHCI) and USB HID devices

pub mod xhci;
pub mod hid;
pub mod descriptor;

use crate::drivers::pci::{PciDevice, PciClass};

/// USB Device Descriptor
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct UsbDevice {
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub protocol: u8,
    pub max_packet_size: u8,
    pub manufacturer: u8,
    pub product: u8,
    pub serial_number: u8,
}

/// USB Controller Interface
pub trait UsbController {
    /// Initialize the USB controller
    fn init(&mut self) -> Result<(), &'static str>;
    
    /// Enumerate connected devices
    fn enumerate_devices(&mut self) -> Result<alloc::vec::Vec<UsbDevice>, &'static str>;
    
    /// Read data from device
    fn read(&mut self, device_id: u8, endpoint: u8, buffer: &mut [u8]) -> Result<usize, &'static str>;
    
    /// Write data to device
    fn write(&mut self, device_id: u8, endpoint: u8, data: &[u8]) -> Result<(), &'static str>;
}

/// Initialize USB subsystem
pub fn init() -> Result<(), &'static str> {
    serial_print!("[USB] Initializing USB subsystem...\n");
    
    // Scan PCI bus for USB controllers
    let controllers = scan_usb_controllers()?;
    
    if controllers.is_empty() {
        serial_print!("[USB] No USB controllers found\n");
        return Err("No USB controllers found");
    }
    
    serial_print!("[USB] Found {} USB controller(s)\n", controllers.len());
    
    for controller in controllers {
        serial_print!("[USB] Controller: {:04x}:{:04x} at {:02x}:{:02x}.{}\n",
                     controller.vendor_id, controller.device_id,
                     controller.bus, controller.device, controller.function);
    }
    
    Ok(())
}

/// Scan PCI bus for USB controllers
fn scan_usb_controllers() -> Result<alloc::vec::Vec<PciDevice>, &'static str> {
    use alloc::vec::Vec;
    
    let mut controllers = Vec::new();
    
    // PCI Class 0x0C = Serial Bus Controller
    // Subclass 0x03 = USB Controller
    // We specifically look for XHCI (Programming Interface 0x30)
    
    serial_print!("[USB] Scanning PCI bus for USB controllers...\n");
    
    // For now, simulate finding one XHCI controller
    // In real implementation, we'd scan the entire PCI configuration space
    
    // TODO: Implement real PCI scan
    // This is a placeholder that shows the structure
    
    serial_print!("[USB] PCI scan complete\n");
    
    Ok(controllers)
}

// Helper macro for serial printing (to be defined in main.rs or serial module)
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {{
        // Serial output implementation
    }};
}
