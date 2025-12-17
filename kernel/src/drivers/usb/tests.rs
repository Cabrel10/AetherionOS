// USB Driver Tests

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::super::descriptor::*;
    use super::super::hid::*;
    
    #[test]
    fn test_usb_device_creation() {
        let device = UsbDevice {
            vendor_id: 0x046d,
            product_id: 0xc534,
            device_class: 0x03,
            device_subclass: 0x01,
            protocol: 0x01,
            max_packet_size: 8,
            manufacturer: 1,
            product: 2,
            serial_number: 0,
        };
        
        assert_eq!(device.vendor_id, 0x046d);
        assert_eq!(device.device_class, 0x03); // HID class
    }
    
    #[test]
    fn test_endpoint_descriptor_parsing() {
        let endpoint = EndpointDescriptor {
            length: 7,
            descriptor_type: 5,
            endpoint_address: 0x81, // IN, endpoint 1
            attributes: 0x03,        // Interrupt
            max_packet_size: 8,
            interval: 10,
        };
        
        assert_eq!(endpoint.endpoint_number(), 1);
        assert!(endpoint.is_in());
        assert_eq!(endpoint.transfer_type(), EndpointTransferType::Interrupt);
    }
    
    #[test]
    fn test_scancode_to_ascii_lowercase() {
        let keyboard = UsbKeyboard {
            device: UsbDevice {
                vendor_id: 0x046d,
                product_id: 0xc534,
                device_class: 0x03,
                device_subclass: 0x01,
                protocol: 0x01,
                max_packet_size: 8,
                manufacturer: 1,
                product: 2,
                serial_number: 0,
            },
            endpoint: 1,
            buffer: [0; 8],
            modifiers: 0, // No shift
            last_keys: [0; 6],
        };
        
        assert_eq!(keyboard.scancode_to_ascii(0x04), Some('a'));
        assert_eq!(keyboard.scancode_to_ascii(0x1D), Some('z'));
        assert_eq!(keyboard.scancode_to_ascii(0x2C), Some(' '));
    }
    
    #[test]
    fn test_scancode_to_ascii_uppercase() {
        let mut keyboard = UsbKeyboard {
            device: UsbDevice {
                vendor_id: 0x046d,
                product_id: 0xc534,
                device_class: 0x03,
                device_subclass: 0x01,
                protocol: 0x01,
                max_packet_size: 8,
                manufacturer: 1,
                product: 2,
                serial_number: 0,
            },
            endpoint: 1,
            buffer: [0; 8],
            modifiers: 0x02, // Left shift
            last_keys: [0; 6],
        };
        
        assert_eq!(keyboard.scancode_to_ascii(0x04), Some('A'));
        assert_eq!(keyboard.scancode_to_ascii(0x1D), Some('Z'));
    }
    
    #[test]
    fn test_scancode_numbers() {
        let keyboard = UsbKeyboard {
            device: UsbDevice {
                vendor_id: 0x046d,
                product_id: 0xc534,
                device_class: 0x03,
                device_subclass: 0x01,
                protocol: 0x01,
                max_packet_size: 8,
                manufacturer: 1,
                product: 2,
                serial_number: 0,
            },
            endpoint: 1,
            buffer: [0; 8],
            modifiers: 0,
            last_keys: [0; 6],
        };
        
        assert_eq!(keyboard.scancode_to_ascii(0x1E), Some('1'));
        assert_eq!(keyboard.scancode_to_ascii(0x27), Some('0'));
    }
    
    #[test]
    fn test_scancode_special_keys() {
        let keyboard = UsbKeyboard {
            device: UsbDevice {
                vendor_id: 0x046d,
                product_id: 0xc534,
                device_class: 0x03,
                device_subclass: 0x01,
                protocol: 0x01,
                max_packet_size: 8,
                manufacturer: 1,
                product: 2,
                serial_number: 0,
            },
            endpoint: 1,
            buffer: [0; 8],
            modifiers: 0,
            last_keys: [0; 6],
        };
        
        assert_eq!(keyboard.scancode_to_ascii(0x28), Some('\n')); // Enter
        assert_eq!(keyboard.scancode_to_ascii(0x2B), Some('\t')); // Tab
        assert_eq!(keyboard.scancode_to_ascii(0x2A), Some('\x08')); // Backspace
    }
}
