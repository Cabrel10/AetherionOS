// USB Descriptor Types and Parsing

/// USB Device Descriptor (18 bytes)
#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
pub struct DeviceDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub usb_version: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub max_packet_size0: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_release: u16,
    pub manufacturer_string: u8,
    pub product_string: u8,
    pub serial_number_string: u8,
    pub num_configurations: u8,
}

/// USB Configuration Descriptor (9 bytes)
#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
pub struct ConfigurationDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub total_length: u16,
    pub num_interfaces: u8,
    pub configuration_value: u8,
    pub configuration_string: u8,
    pub attributes: u8,
    pub max_power: u8,
}

/// USB Interface Descriptor (9 bytes)
#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
pub struct InterfaceDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub interface_number: u8,
    pub alternate_setting: u8,
    pub num_endpoints: u8,
    pub interface_class: u8,
    pub interface_subclass: u8,
    pub interface_protocol: u8,
    pub interface_string: u8,
}

/// USB Endpoint Descriptor (7 bytes)
#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
pub struct EndpointDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub endpoint_address: u8,
    pub attributes: u8,
    pub max_packet_size: u16,
    pub interval: u8,
}

impl EndpointDescriptor {
    /// Get endpoint number (bits 0-3)
    pub fn endpoint_number(&self) -> u8 {
        self.endpoint_address & 0x0F
    }
    
    /// Check if endpoint is IN (device to host)
    pub fn is_in(&self) -> bool {
        (self.endpoint_address & 0x80) != 0
    }
    
    /// Get transfer type (bits 0-1 of attributes)
    pub fn transfer_type(&self) -> EndpointTransferType {
        match self.attributes & 0x03 {
            0 => EndpointTransferType::Control,
            1 => EndpointTransferType::Isochronous,
            2 => EndpointTransferType::Bulk,
            3 => EndpointTransferType::Interrupt,
            _ => unreachable!(),
        }
    }
}

/// Endpoint Transfer Types
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum EndpointTransferType {
    Control = 0,
    Isochronous = 1,
    Bulk = 2,
    Interrupt = 3,
}

/// USB Descriptor Types
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum DescriptorType {
    Device = 1,
    Configuration = 2,
    String = 3,
    Interface = 4,
    Endpoint = 5,
    DeviceQualifier = 6,
    OtherSpeedConfiguration = 7,
    InterfacePower = 8,
    Hid = 0x21,
    HidReport = 0x22,
    HidPhysical = 0x23,
}

/// USB Device Classes
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum DeviceClass {
    PerInterface = 0x00,
    Audio = 0x01,
    Communications = 0x02,
    Hid = 0x03,
    Physical = 0x05,
    Image = 0x06,
    Printer = 0x07,
    MassStorage = 0x08,
    Hub = 0x09,
    CdcData = 0x0A,
    SmartCard = 0x0B,
    ContentSecurity = 0x0D,
    Video = 0x0E,
    PersonalHealthcare = 0x0F,
    AudioVideo = 0x10,
    Diagnostic = 0xDC,
    Wireless = 0xE0,
    Miscellaneous = 0xEF,
    ApplicationSpecific = 0xFE,
    VendorSpecific = 0xFF,
}
