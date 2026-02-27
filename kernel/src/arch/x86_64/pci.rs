// arch/x86_64/pci.rs - PCI Configuration Space Access (Couche 8 GPU Support)
//
// Provides read_config_u32 and read_bar for PCI device enumeration.
// Uses I/O ports 0xCF8 (CONFIG_ADDRESS) and 0xCFC (CONFIG_DATA).

use x86_64::instructions::port::Port;

/// PCI Configuration Address port
const CONFIG_ADDRESS: u16 = 0xCF8;
/// PCI Configuration Data port
const CONFIG_DATA: u16 = 0xCFC;

/// Read a 32-bit value from PCI configuration space.
///
/// # Arguments
/// * `bus` - PCI bus number (0-255)
/// * `device` - PCI device number (0-31)
/// * `function` - PCI function number (0-7)
/// * `offset` - Register offset (must be 4-byte aligned)
pub fn read_config_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | (((device & 0x1F) as u32) << 11)
        | (((function & 0x07) as u32) << 8)
        | ((offset & 0xFC) as u32);

    // SAFETY: I/O ports 0xCF8 and 0xCFC are the standard PCI configuration
    // mechanism on x86. Writing to CONFIG_ADDRESS selects a register, and
    // reading from CONFIG_DATA returns its value. This is the standard
    // method for PCI access on all x86 systems.
    unsafe {
        let mut addr_port = Port::<u32>::new(CONFIG_ADDRESS);
        let mut data_port = Port::<u32>::new(CONFIG_DATA);
        addr_port.write(address);
        data_port.read()
    }
}

/// Read the Vendor ID and Device ID of a PCI device.
/// Returns (vendor_id, device_id). vendor_id == 0xFFFF means no device.
pub fn read_vendor_device(bus: u8, device: u8, function: u8) -> (u16, u16) {
    let data = read_config_u32(bus, device, function, 0x00);
    let vendor_id = (data & 0xFFFF) as u16;
    let device_id = ((data >> 16) & 0xFFFF) as u16;
    (vendor_id, device_id)
}

/// Read the class code, subclass, and prog-IF of a PCI device.
/// Returns (class_code, subclass, prog_if)
pub fn read_class(bus: u8, device: u8, function: u8) -> (u8, u8, u8) {
    let data = read_config_u32(bus, device, function, 0x08);
    let class_code = ((data >> 24) & 0xFF) as u8;
    let subclass = ((data >> 16) & 0xFF) as u8;
    let prog_if = ((data >> 8) & 0xFF) as u8;
    (class_code, subclass, prog_if)
}

/// Read a Base Address Register (BAR) for a PCI device.
///
/// # Arguments
/// * `bus`, `device`, `function` - PCI address
/// * `bar_index` - BAR number (0-5)
///
/// # Returns
/// The raw BAR value (address + type flags)
pub fn read_bar(bus: u8, device: u8, function: u8, bar_index: u8) -> u32 {
    if bar_index > 5 {
        return 0;
    }
    let offset = 0x10 + (bar_index * 4);
    read_config_u32(bus, device, function, offset)
}

/// Scan PCI bus 0 for devices of a given class code.
/// Returns Vec of (bus, device, function, vendor_id, device_id, subclass).
pub fn scan_for_class(target_class: u8) -> alloc::vec::Vec<PciDevice> {
    let mut found = alloc::vec::Vec::new();
    for device_num in 0..32u8 {
        let (vendor_id, device_id) = read_vendor_device(0, device_num, 0);
        if vendor_id == 0xFFFF {
            continue; // no device
        }
        let (class_code, subclass, prog_if) = read_class(0, device_num, 0);
        if class_code == target_class {
            found.push(PciDevice {
                bus: 0,
                device: device_num,
                function: 0,
                vendor_id,
                device_id,
                class_code,
                subclass,
                prog_if,
            });
        }
    }
    found
}

/// Represents a discovered PCI device
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
}

impl core::fmt::Display for PciDevice {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "PCI {:02x}:{:02x}.{} [{:04x}:{:04x}] class={:02x}.{:02x}.{:02x}",
            self.bus, self.device, self.function,
            self.vendor_id, self.device_id,
            self.class_code, self.subclass, self.prog_if)
    }
}
