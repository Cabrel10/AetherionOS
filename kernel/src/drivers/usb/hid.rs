// USB HID (Human Interface Device) Driver
// Supports keyboards, mice, and other HID devices

use super::UsbDevice;
use alloc::vec::Vec;

/// HID Report Descriptor Types
#[derive(Debug, Copy, Clone)]
pub enum HidReportType {
    Input = 1,
    Output = 2,
    Feature = 3,
}

/// USB Keyboard State
pub struct UsbKeyboard {
    device: UsbDevice,
    endpoint: u8,
    buffer: [u8; 8],
    modifiers: u8,
    last_keys: [u8; 6],
}

impl UsbKeyboard {
    /// Create new USB keyboard instance
    pub fn new(device: UsbDevice, endpoint: u8) -> Self {
        Self {
            device,
            endpoint,
            buffer: [0; 8],
            modifiers: 0,
            last_keys: [0; 6],
        }
    }
    
    /// Read keyboard report (8 bytes)
    /// Byte 0: Modifiers (Ctrl, Shift, Alt, etc.)
    /// Byte 1: Reserved
    /// Bytes 2-7: Up to 6 simultaneous key presses (scancodes)
    pub fn read_report(&mut self) -> Result<(), &'static str> {
        // In real implementation, this would:
        // 1. Perform USB interrupt transfer
        // 2. Read 8 bytes from the keyboard endpoint
        // 3. Parse the report
        
        // Placeholder - would call USB controller read function
        // usb_controller.read(device_id, self.endpoint, &mut self.buffer)?;
        
        Ok(())
    }
    
    /// Get currently pressed keys
    pub fn get_pressed_keys(&self) -> &[u8] {
        &self.buffer[2..8]
    }
    
    /// Convert USB HID scancode to ASCII character
    pub fn scancode_to_ascii(&self, scancode: u8) -> Option<char> {
        let shift_pressed = (self.modifiers & 0x22) != 0; // Left or Right Shift
        
        match scancode {
            0x00 => None, // No key
            0x04 => Some(if shift_pressed { 'A' } else { 'a' }),
            0x05 => Some(if shift_pressed { 'B' } else { 'b' }),
            0x06 => Some(if shift_pressed { 'C' } else { 'c' }),
            0x07 => Some(if shift_pressed { 'D' } else { 'd' }),
            0x08 => Some(if shift_pressed { 'E' } else { 'e' }),
            0x09 => Some(if shift_pressed { 'F' } else { 'f' }),
            0x0A => Some(if shift_pressed { 'G' } else { 'g' }),
            0x0B => Some(if shift_pressed { 'H' } else { 'h' }),
            0x0C => Some(if shift_pressed { 'I' } else { 'i' }),
            0x0D => Some(if shift_pressed { 'J' } else { 'j' }),
            0x0E => Some(if shift_pressed { 'K' } else { 'k' }),
            0x0F => Some(if shift_pressed { 'L' } else { 'l' }),
            0x10 => Some(if shift_pressed { 'M' } else { 'm' }),
            0x11 => Some(if shift_pressed { 'N' } else { 'n' }),
            0x12 => Some(if shift_pressed { 'O' } else { 'o' }),
            0x13 => Some(if shift_pressed { 'P' } else { 'p' }),
            0x14 => Some(if shift_pressed { 'Q' } else { 'q' }),
            0x15 => Some(if shift_pressed { 'R' } else { 'r' }),
            0x16 => Some(if shift_pressed { 'S' } else { 's' }),
            0x17 => Some(if shift_pressed { 'T' } else { 't' }),
            0x18 => Some(if shift_pressed { 'U' } else { 'u' }),
            0x19 => Some(if shift_pressed { 'V' } else { 'v' }),
            0x1A => Some(if shift_pressed { 'W' } else { 'w' }),
            0x1B => Some(if shift_pressed { 'X' } else { 'x' }),
            0x1C => Some(if shift_pressed { 'Y' } else { 'y' }),
            0x1D => Some(if shift_pressed { 'Z' } else { 'z' }),
            
            // Numbers
            0x1E => Some(if shift_pressed { '!' } else { '1' }),
            0x1F => Some(if shift_pressed { '@' } else { '2' }),
            0x20 => Some(if shift_pressed { '#' } else { '3' }),
            0x21 => Some(if shift_pressed { '$' } else { '4' }),
            0x22 => Some(if shift_pressed { '%' } else { '5' }),
            0x23 => Some(if shift_pressed { '^' } else { '6' }),
            0x24 => Some(if shift_pressed { '&' } else { '7' }),
            0x25 => Some(if shift_pressed { '*' } else { '8' }),
            0x26 => Some(if shift_pressed { '(' } else { '9' }),
            0x27 => Some(if shift_pressed { ')' } else { '0' }),
            
            // Special keys
            0x28 => Some('\n'), // Enter
            0x29 => Some('\x1b'), // Escape
            0x2A => Some('\x08'), // Backspace
            0x2B => Some('\t'), // Tab
            0x2C => Some(' '),  // Space
            
            _ => None, // Unknown or unsupported key
        }
    }
    
    /// Poll for new key presses
    pub fn poll(&mut self) -> Vec<char> {
        let mut chars = Vec::new();
        
        // Read new report
        if self.read_report().is_err() {
            return chars;
        }
        
        // Update modifiers
        self.modifiers = self.buffer[0];
        
        // Check for new key presses
        for i in 2..8 {
            let scancode = self.buffer[i];
            if scancode == 0 {
                continue;
            }
            
            // Check if this is a new key (not in last_keys)
            let is_new = !self.last_keys.contains(&scancode);
            
            if is_new {
                if let Some(ch) = self.scancode_to_ascii(scancode) {
                    chars.push(ch);
                }
            }
        }
        
        // Update last keys
        self.last_keys.copy_from_slice(&self.buffer[2..8]);
        
        chars
    }
}

/// USB Mouse State
pub struct UsbMouse {
    device: UsbDevice,
    endpoint: u8,
    buffer: [u8; 4],
    pub x: i16,
    pub y: i16,
    pub buttons: u8,
}

impl UsbMouse {
    pub fn new(device: UsbDevice, endpoint: u8) -> Self {
        Self {
            device,
            endpoint,
            buffer: [0; 4],
            x: 0,
            y: 0,
            buttons: 0,
        }
    }
    
    /// Read mouse report (typically 3-4 bytes)
    /// Byte 0: Buttons (bit 0=left, bit 1=right, bit 2=middle)
    /// Byte 1: X movement (signed)
    /// Byte 2: Y movement (signed)
    /// Byte 3: Wheel movement (optional, signed)
    pub fn poll(&mut self) -> Result<(), &'static str> {
        // Read from USB controller
        // Placeholder
        
        self.buttons = self.buffer[0];
        let dx = self.buffer[1] as i8;
        let dy = self.buffer[2] as i8;
        
        self.x = self.x.saturating_add(dx as i16);
        self.y = self.y.saturating_add(dy as i16);
        
        Ok(())
    }
}
