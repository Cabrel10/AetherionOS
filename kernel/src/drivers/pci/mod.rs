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
            0x30 => Some(UsbControllerType::Xhci),  // USB 3.0+
            0x40 => Some(UsbControllerType::Usb4),  // USB 4.0
            0xFE => Some(UsbControllerType::UsbDevice),
            _ => None,
        }
    }
    
    /// Check if this is a network controller
    pub fn is_network_controller(&self) -> bool {
        self.class_code == 0x02
    }
    
    /// Check if this is a WiFi controller
    pub fn is_wifi_controller(&self) -> bool {
        // Network controller with wireless subclass
        self.class_code == 0x02 && self.subclass == 0x80
    }
    
    /// Check if this is an Ethernet controller
    pub fn is_ethernet_controller(&self) -> bool {
        // Network controller with Ethernet subclass
        self.class_code == 0x02 && self.subclass == 0x00
    }
    
    /// Check if this is a storage controller
    pub fn is_storage_controller(&self) -> bool {
        self.class_code == 0x01
    }
    
    /// Check if this is an NVMe controller
    pub fn is_nvme_controller(&self) -> bool {
        // Mass storage, Non-Volatile Memory subsystem
        self.class_code == 0x01 && self.subclass == 0x08 && self.prog_if == 0x02
    }
    
    /// Check if this is a SATA/AHCI controller
    pub fn is_sata_controller(&self) -> bool {
        // Mass storage, SATA controller, AHCI interface
        self.class_code == 0x01 && self.subclass == 0x06 && self.prog_if == 0x01
    }
    
    /// Check if this is a display/graphics controller
    pub fn is_display_controller(&self) -> bool {
        self.class_code == 0x03
    }
    
    /// Check if this is a Bluetooth controller
    pub fn is_bluetooth_controller(&self) -> bool {
        // Serial bus controller, Wireless controller (Bluetooth)
        self.class_code == 0x0C && self.subclass == 0x09
    }
    
    /// Get BAR (Base Address Register) type
    pub fn get_bar_type(&self, bar_index: u8) -> Option<BarType> {
        let bar = match bar_index {
            0 => self.bar0,
            1 => self.bar1,
            2 => self.bar2,
            3 => self.bar3,
            4 => self.bar4,
            5 => self.bar5,
            _ => return None,
        };
        
        if bar == 0 {
            return None;
        }
        
        if (bar & 0x1) != 0 {
            // I/O Space
            Some(BarType::IoSpace(bar & !0x3))
        } else {
            // Memory Space
            let mem_type = (bar >> 1) & 0x3;
            let prefetchable = (bar & 0x8) != 0;
            let address = bar & !0xF;
            
            Some(BarType::MemorySpace {
                address,
                mem_type: match mem_type {
                    0 => MemoryType::Bit32,
                    2 => MemoryType::Bit64,
                    _ => return None,
                },
                prefetchable,
            })
        }
    }
    
    /// Get device name string
    pub fn device_name(&self) -> &'static str {
        // USB Controllers
        match (self.vendor_id, self.device_id) {
            // Intel USB
            (0x8086, 0x9D2F) => "Intel USB 3.0 XHCI Controller",
            (0x8086, 0xA12F) => "Intel USB 3.0 XHCI Controller",
            (0x8086, 0x9DED) => "Intel Cannon Point USB 3.1 XHCI",
            (0x8086, 0xA36D) => "Intel Cannon Lake USB 3.1 XHCI",
            (0x8086, 0x02ED) => "Intel Comet Lake USB 3.1 XHCI",
            (0x8086, 0x43ED) => "Intel Tiger Lake USB 3.2 XHCI",
            (0x8086, 0x51ED) => "Intel Alder Lake USB 3.2 XHCI",
            (0x8086, 0x7A60) => "Intel Raptor Lake USB 3.2 XHCI",
            
            // AMD USB
            (0x1022, 0x149C) => "AMD USB 3.1 XHCI Controller",
            (0x1022, 0x43D5) => "AMD USB 3.1 XHCI Controller",
            (0x1022, 0x15E0) => "AMD Ryzen USB 3.1 XHCI",
            (0x1022, 0x15E1) => "AMD Ryzen USB 3.1 XHCI",
            
            // ASMedia USB
            (0x1B21, 0x1142) => "ASMedia USB 3.0 XHCI Controller",
            (0x1B21, 0x2142) => "ASMedia USB 3.1 XHCI Controller",
            (0x1B21, 0x3242) => "ASMedia USB 3.2 XHCI Controller",
            
            // VIA USB
            (0x1106, 0x3483) => "VIA USB 3.0 XHCI Controller",
            
            // Texas Instruments USB
            (0x104C, 0x8241) => "TI USB 3.0 XHCI Controller",
            
            // Renesas USB
            (0x1912, 0x0014) => "Renesas USB 3.0 XHCI Controller",
            (0x1912, 0x0015) => "Renesas USB 3.1 XHCI Controller",
            
            // Network Controllers
            // Intel Ethernet
            (0x8086, 0x15B8) => "Intel I219-V Gigabit Ethernet",
            (0x8086, 0x15D8) => "Intel I219-LM Gigabit Ethernet",
            (0x8086, 0x0D4E) => "Intel Ethernet I219",
            (0x8086, 0x125C) => "Intel I210 Gigabit Ethernet",
            (0x8086, 0x1539) => "Intel I211 Gigabit Ethernet",
            (0x8086, 0x10D3) => "Intel 82574L Gigabit Ethernet",
            
            // Intel WiFi
            (0x8086, 0x2723) => "Intel WiFi 6 AX200",
            (0x8086, 0x2725) => "Intel WiFi 6 AX201",
            (0x8086, 0x51F0) => "Intel WiFi 6E AX210",
            (0x8086, 0x272B) => "Intel WiFi 6E AX211",
            (0x8086, 0x7AF0) => "Intel WiFi 6E AX411",
            (0x8086, 0x24FD) => "Intel WiFi 5 AC 9560",
            (0x8086, 0x2526) => "Intel WiFi 5 AC 9260",
            
            // Realtek Network
            (0x10EC, 0x8168) => "Realtek RTL8111/8168 Gigabit Ethernet",
            (0x10EC, 0x8125) => "Realtek RTL8125 2.5G Ethernet",
            (0x10EC, 0x8136) => "Realtek RTL810xE Fast Ethernet",
            (0x10EC, 0xB852) => "Realtek RTL8852BE WiFi 6",
            (0x10EC, 0xC852) => "Realtek RTL8852CE WiFi 6E",
            
            // Qualcomm Atheros WiFi
            (0x168C, 0x003E) => "Qualcomm Atheros QCA6174 WiFi",
            (0x168C, 0x0042) => "Qualcomm Atheros QCA9377 WiFi",
            (0x168C, 0x003C) => "Qualcomm Atheros QCA9565 WiFi",
            
            // Broadcom Network
            (0x14E4, 0x43A0) => "Broadcom BCM4360 WiFi",
            (0x14E4, 0x43B1) => "Broadcom BCM4352 WiFi",
            (0x14E4, 0x4727) => "Broadcom BCM4313 WiFi",
            
            // Storage Controllers
            // Intel NVMe
            (0x8086, 0x0953) => "Intel NVMe SSD DC P3520",
            (0x8086, 0x0A54) => "Intel NVMe SSD 660p",
            (0x8086, 0x0A55) => "Intel NVMe SSD 670p",
            (0x8086, 0xF1A5) => "Intel NVMe SSD DC P4510",
            
            // Samsung NVMe
            (0x144D, 0xA808) => "Samsung NVMe SSD 970 EVO/PRO",
            (0x144D, 0xA809) => "Samsung NVMe SSD 980 PRO",
            (0x144D, 0xA80A) => "Samsung NVMe SSD 990 PRO",
            (0x144D, 0xA824) => "Samsung NVMe SSD PM9A1",
            
            // Western Digital NVMe
            (0x15B7, 0x5002) => "WD Black NVMe SSD",
            (0x15B7, 0x5003) => "WD Blue NVMe SSD",
            
            // Intel SATA
            (0x8086, 0x02D3) => "Intel Comet Lake SATA AHCI",
            (0x8086, 0x43D3) => "Intel Tiger Lake SATA AHCI",
            (0x8086, 0xA102) => "Intel Sunrise Point SATA AHCI",
            
            // AMD Storage
            (0x1022, 0x7901) => "AMD FCH SATA Controller",
            (0x1022, 0x7904) => "AMD FCH RAID Controller",
            
            // Display Controllers
            // Intel Graphics
            (0x8086, 0x3E92) => "Intel UHD Graphics 630",
            (0x8086, 0x9BC8) => "Intel UHD Graphics 630",
            (0x8086, 0x4C8A) => "Intel Rocket Lake Graphics",
            (0x8086, 0x4680) => "Intel Alder Lake Graphics",
            
            // NVIDIA Graphics
            (0x10DE, 0x2204) => "NVIDIA GeForce RTX 3090",
            (0x10DE, 0x2206) => "NVIDIA GeForce RTX 3080",
            (0x10DE, 0x2208) => "NVIDIA GeForce RTX 3070",
            (0x10DE, 0x2486) => "NVIDIA GeForce RTX 3060",
            
            // AMD Graphics
            (0x1002, 0x73BF) => "AMD Radeon RX 6900 XT",
            (0x1002, 0x73DF) => "AMD Radeon RX 6700 XT",
            (0x1002, 0x73FF) => "AMD Radeon RX 6600 XT",
            
            // Generic
            _ => {
                match self.class_code {
                    0x00 => "Unclassified Device",
                    0x01 => "Mass Storage Controller",
                    0x02 => "Network Controller",
                    0x03 => "Display Controller",
                    0x04 => "Multimedia Controller",
                    0x05 => "Memory Controller",
                    0x06 => "Bridge Device",
                    0x07 => "Communication Controller",
                    0x08 => "System Peripheral",
                    0x09 => "Input Device",
                    0x0A => "Docking Station",
                    0x0B => "Processor",
                    0x0C => "Serial Bus Controller",
                    0x0D => "Wireless Controller",
                    0x0E => "Intelligent IO Controller",
                    0x0F => "Satellite Controller",
                    0x10 => "Encryption Controller",
                    0x11 => "Signal Processing Controller",
                    _ => "Unknown PCI Device",
                }
            }
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

/// BAR (Base Address Register) Type
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum BarType {
    IoSpace(u32),
    MemorySpace {
        address: u32,
        mem_type: MemoryType,
        prefetchable: bool,
    },
}

/// Memory Type for BARs
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum MemoryType {
    Bit32,
    Bit64,
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

/// Enable Bus Mastering for a PCI device (required for DMA)
pub fn enable_bus_mastering(device: &PciDevice) {
    // Read command register (offset 0x04)
    let command = read_config_word(device.bus, device.device, device.function, 0x04);
    
    // Set bit 2 (Bus Master Enable)
    let new_command = command | 0x04;
    write_config_word(device.bus, device.device, device.function, 0x04, new_command);
    
    crate::serial_print("[PCI] Bus mastering enabled for device\n");
}

/// Enable memory space access for a PCI device
pub fn enable_memory_space(device: &PciDevice) {
    // Read command register (offset 0x04)
    let command = read_config_word(device.bus, device.device, device.function, 0x04);
    
    // Set bit 1 (Memory Space Enable)
    let new_command = command | 0x02;
    write_config_word(device.bus, device.device, device.function, 0x04, new_command);
    
    crate::serial_print("[PCI] Memory space enabled for device\n");
}

/// Enable I/O space access for a PCI device
pub fn enable_io_space(device: &PciDevice) {
    // Read command register (offset 0x04)
    let command = read_config_word(device.bus, device.device, device.function, 0x04);
    
    // Set bit 0 (I/O Space Enable)
    let new_command = command | 0x01;
    write_config_word(device.bus, device.device, device.function, 0x04, new_command);
    
    crate::serial_print("[PCI] I/O space enabled for device\n");
}

/// Disable interrupts for a PCI device
pub fn disable_interrupts(device: &PciDevice) {
    // Read command register (offset 0x04)
    let command = read_config_word(device.bus, device.device, device.function, 0x04);
    
    // Set bit 10 (Interrupt Disable)
    let new_command = command | 0x0400;
    write_config_word(device.bus, device.device, device.function, 0x04, new_command);
}

/// Find device by vendor and device ID
pub fn find_device(vendor_id: u16, device_id: u16) -> Option<PciDevice> {
    scan_bus()
        .into_iter()
        .find(|dev| dev.vendor_id == vendor_id && dev.device_id == device_id)
}

/// Find all devices matching a class code
pub fn find_devices_by_class(class_code: u8) -> Vec<PciDevice> {
    scan_bus()
        .into_iter()
        .filter(|dev| dev.class_code == class_code)
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

/// Write a byte to PCI configuration space
fn write_config_byte(bus: u8, device: u8, function: u8, offset: u8, value: u8) {
    let address = pci_address(bus, device, function, offset & 0xFC);
    outl(PCI_CONFIG_ADDRESS, address);
    let shift = (offset & 3) * 8;
    let mask = !(0xFF << shift);
    let old_value = inl(PCI_CONFIG_DATA);
    let new_value = (old_value & mask) | ((value as u32) << shift);
    outl(PCI_CONFIG_DATA, new_value);
}

/// Write a word to PCI configuration space
fn write_config_word(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    let address = pci_address(bus, device, function, offset & 0xFC);
    outl(PCI_CONFIG_ADDRESS, address);
    let shift = (offset & 2) * 8;
    let mask = !(0xFFFF << shift);
    let old_value = inl(PCI_CONFIG_DATA);
    let new_value = (old_value & mask) | ((value as u32) << shift);
    outl(PCI_CONFIG_DATA, new_value);
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

/// Initialize PCI subsystem and return list of devices
pub fn init() -> Vec<PciDevice> {
    // Note: Serial macros don't work yet, using direct serial output
    crate::serial_print("[PCI] Initializing PCI bus...\n");
    
    let devices = scan_bus();
    crate::serial_print("[PCI] Found ");
    // Note: Can't format numbers yet without full fmt support
    crate::serial_print(" PCI device(s)\n");
    
    // Categorize devices
    let usb_controllers: Vec<_> = devices.iter()
        .filter(|d| d.is_usb_controller())
        .collect();
    
    let network_controllers: Vec<_> = devices.iter()
        .filter(|d| d.class_code == 0x02)  // Network controllers
        .collect();
        
    let storage_controllers: Vec<_> = devices.iter()
        .filter(|d| d.class_code == 0x01)  // Mass storage
        .collect();
    
    crate::serial_print("[PCI] Found ");
    crate::serial_print(" USB controller(s)\n");
    
    for controller in &usb_controllers {
        crate::serial_print("[PCI]   USB: ");
        crate::serial_print(controller.device_name());
        crate::serial_print("\n");
        
        if let Some(ctrl_type) = controller.usb_controller_type() {
            match ctrl_type {
                UsbControllerType::Xhci => crate::serial_print("[PCI]     Type: XHCI (USB 3.0+)\n"),
                UsbControllerType::Ehci => crate::serial_print("[PCI]     Type: EHCI (USB 2.0)\n"),
                UsbControllerType::Uhci => crate::serial_print("[PCI]     Type: UHCI (USB 1.0)\n"),
                UsbControllerType::Ohci => crate::serial_print("[PCI]     Type: OHCI (USB 1.1)\n"),
                _ => crate::serial_print("[PCI]     Type: Other USB\n"),
            }
        }
    }
    
    crate::serial_print("[PCI] Network controllers: ");
    for net in &network_controllers {
        crate::serial_print("\n[PCI]   ");
        crate::serial_print(net.device_name());
    }
    crate::serial_print("\n");
    
    crate::serial_print("[PCI] Storage controllers: ");
    for storage in &storage_controllers {
        crate::serial_print("\n[PCI]   ");
        crate::serial_print(storage.device_name());
    }
    crate::serial_print("\n");
    
    devices
}
