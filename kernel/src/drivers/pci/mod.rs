// PCI (Peripheral Component Interconnect) Bus Driver
// Used to discover and configure PCI devices

use core::arch::asm;
use alloc::vec::Vec;

/// PCI Configuration Space I/O Ports
const PCI_CONFIG_ADDRESS: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

/// PCI Device Information
#[derive(Debug, Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision_id: u8,
    pub header_type: u8,
    pub bar0: u32,
    pub bar1: u32,
    pub bar2: u32,
    pub bar3: u32,
    pub bar4: u32,
    pub bar5: u32,
    pub interrupt_line: u8,
    pub interrupt_pin: u8,
}

/// PCI Device Class Codes
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum PciClass {
    Unclassified = 0x00,
    MassStorage = 0x01,
    Network = 0x02,
    Display = 0x03,
    Multimedia = 0x04,
    Memory = 0x05,
    Bridge = 0x06,
    Communication = 0x07,
    SystemPeripheral = 0x08,
    InputDevice = 0x09,
    DockingStation = 0x0A,
    Processor = 0x0B,
    SerialBus = 0x0C, // USB controllers are in this class
    Wireless = 0x0D,
    IntelligentIO = 0x0E,
    Satellite = 0x0F,
    Encryption = 0x10,
    SignalProcessing = 0x11,
}

impl PciDevice {
    /// Create a new PCI device by reading its configuration space
    pub fn new(bus: u8, device: u8, function: u8) -> Option<Self> {
        let vendor_id = read_config_word(bus, device, function, 0x00);
        
        // 0xFFFF means no device
        if vendor_id == 0xFFFF {
            return None;
        }
        
        let device_id = read_config_word(bus, device, function, 0x02);
        let revision_id = read_config_byte(bus, device, function, 0x08);
        let prog_if = read_config_byte(bus, device, function, 0x09);
        let subclass = read_config_byte(bus, device, function, 0x0A);
        let class_code = read_config_byte(bus, device, function, 0x0B);
        let header_type = read_config_byte(bus, device, function, 0x0E);
        
        let bar0 = read_config_dword(bus, device, function, 0x10);
        let bar1 = read_config_dword(bus, device, function, 0x14);
        let bar2 = read_config_dword(bus, device, function, 0x18);
        let bar3 = read_config_dword(bus, device, function, 0x1C);
        let bar4 = read_config_dword(bus, device, function, 0x20);
        let bar5 = read_config_dword(bus, device, function, 0x24);
        
        let interrupt_line = read_config_byte(bus, device, function, 0x3C);
        let interrupt_pin = read_config_byte(bus, device, function, 0x3D);
        
        Some(Self {
            bus,
            device,
            function,
            vendor_id,
            device_id,
            class_code,
            subclass,
            prog_if,
            revision_id,
            header_type,
            bar0,
            bar1,
            bar2,
            bar3,
            bar4,
            bar5,
            interrupt_line,
            interrupt_pin,
        })
    }
    
    /// Check if this is a USB controller
    pub fn is_usb_controller(&self) -> bool {
        // Class 0x0C = Serial Bus Controller
        // Subclass 0x03 = USB Controller
        self.class_code == 0x0C && self.subclass == 0x03
    }
    
    /// Get USB controller type
    pub fn usb_controller_type(&self) -> Option<UsbControllerType> {
        if !self.is_usb_controller() {
            return None;
        }
        
        match self.prog_if {
            0x00 => Some(UsbControllerType::Uhci),  // USB 1.0
            0x10 => Some(UsbControllerType::Ohci),  // USB 1.1
            0x20 => Some(UsbControllerType::Ehci),  // USB 2.0
            0x30 => Some(UsbControllerType::Xhci),  // USB 3.0
            0x40 => Some(UsbControllerType::Usb4),  // USB 4.0
            0xFE => Some(UsbControllerType::UsbDevice),
            _ => None,
        }
    }
    
    /// Get device name string
    pub fn device_name(&self) -> &'static str {
        match (self.vendor_id, self.device_id) {
            (0x8086, 0x9D2F) => "Intel USB 3.0 XHCI Controller",
            (0x8086, 0xA12F) => "Intel USB 3.0 XHCI Controller",
            (0x1022, 0x149C) => "AMD USB 3.1 XHCI Controller",
            (0x1B21, 0x1142) => "ASMedia USB 3.0 XHCI Controller",
            _ => "Unknown PCI Device",
        }
    }
}

/// USB Controller Types
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum UsbControllerType {
    Uhci,      // USB 1.0 (Universal Host Controller Interface)
    Ohci,      // USB 1.1 (Open Host Controller Interface)
    Ehci,      // USB 2.0 (Enhanced Host Controller Interface)
    Xhci,      // USB 3.0+ (eXtensible Host Controller Interface)
    Usb4,      // USB 4.0
    UsbDevice, // USB Device (not host)
}

/// Scan entire PCI bus for devices
pub fn scan_bus() -> Vec<PciDevice> {
    let mut devices = Vec::new();
    
    // Scan all 256 buses
    for bus in 0..256 {
        // Scan all 32 devices per bus
        for device in 0..32 {
            // Check function 0 first
            if let Some(pci_dev) = PciDevice::new(bus as u8, device as u8, 0) {
                devices.push(pci_dev);
                
                // If it's a multi-function device, scan other functions
                if (pci_dev.header_type & 0x80) != 0 {
                    for function in 1..8 {
                        if let Some(pci_func) = PciDevice::new(bus as u8, device as u8, function) {
                            devices.push(pci_func);
                        }
                    }
                }
            }
        }
    }
    
    devices
}

/// Scan for USB controllers specifically
pub fn scan_usb_controllers() -> Vec<PciDevice> {
    scan_bus()
        .into_iter()
        .filter(|dev| dev.is_usb_controller())
        .collect()
}

/// Read a byte from PCI configuration space
fn read_config_byte(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let address = pci_address(bus, device, function, offset & 0xFC);
    outl(PCI_CONFIG_ADDRESS, address);
    let value = inl(PCI_CONFIG_DATA);
    ((value >> ((offset & 3) * 8)) & 0xFF) as u8
}

/// Read a word (16-bit) from PCI configuration space
fn read_config_word(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let address = pci_address(bus, device, function, offset & 0xFC);
    outl(PCI_CONFIG_ADDRESS, address);
    let value = inl(PCI_CONFIG_DATA);
    ((value >> ((offset & 2) * 8)) & 0xFFFF) as u16
}

/// Read a dword (32-bit) from PCI configuration space
fn read_config_dword(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address = pci_address(bus, device, function, offset & 0xFC);
    outl(PCI_CONFIG_ADDRESS, address);
    inl(PCI_CONFIG_DATA)
}

/// Write a dword to PCI configuration space
pub fn write_config_dword(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let address = pci_address(bus, device, function, offset & 0xFC);
    outl(PCI_CONFIG_ADDRESS, address);
    outl(PCI_CONFIG_DATA, value);
}

/// Construct PCI configuration address
fn pci_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let bus = bus as u32;
    let device = device as u32;
    let function = function as u32;
    let offset = offset as u32;
    
    (1 << 31) | (bus << 16) | (device << 11) | (function << 8) | (offset & 0xFC)
}

/// Output a 32-bit value to an I/O port
fn outl(port: u16, value: u32) {
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack));
    }
}

/// Input a 32-bit value from an I/O port
fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack));
    }
    value
}

/// Initialize PCI subsystem
pub fn init() {
    serial_print!("[PCI] Initializing PCI bus...\n");
    
    let devices = scan_bus();
    serial_print!("[PCI] Found {} PCI device(s)\n", devices.len());
    
    // Print USB controllers
    let usb_controllers: Vec<_> = devices.iter()
        .filter(|d| d.is_usb_controller())
        .collect();
    
    serial_print!("[PCI] Found {} USB controller(s)\n", usb_controllers.len());
    
    for controller in usb_controllers {
        serial_print!("[PCI]   {:04x}:{:04x} - {} (Bus {}, Device {}, Function {})\n",
                     controller.vendor_id, controller.device_id,
                     controller.device_name(),
                     controller.bus, controller.device, controller.function);
        
        if let Some(ctrl_type) = controller.usb_controller_type() {
            serial_print!("[PCI]     Type: {:?}\n", ctrl_type);
        }
    }
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {{}};
}
