//! PS/2 Controller (8042) Initialization
//! 
//! Initializes the PS/2 controller to enable keyboard interrupts (IRQ1).
//! Based on OSDev Wiki: https://wiki.osdev.org/PS/2_Keyboard

use x86_64::instructions::port::Port;

const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_COMMAND: u16 = 0x64;

/// Initialize the PS/2 controller and enable keyboard interrupts
pub fn init() {
    crate::serial_println!("[PS/2] Initializing 8042 controller...");
    
    unsafe {
        let mut data_port = Port::<u8>::new(PS2_DATA);
        let mut cmd_port = Port::<u8>::new(PS2_COMMAND);
        let mut status_port = Port::<u8>::new(PS2_STATUS);
        
        // Step 1: Disable first PS/2 port (keyboard)
        cmd_port.write(0xAD);
        wait_input_buffer();
        
        // Step 2: Flush output buffer
        let _ = data_port.read();
        
        // Step 3: Read configuration byte
        cmd_port.write(0x20);
        wait_output_buffer();
        let mut config = data_port.read();
        
        crate::serial_println!("[PS/2] Config byte before: 0x{:02X}", config);
        
        // Step 4: Modify configuration byte to enable interrupts
        // Bit 0: Enable keyboard interrupt (IRQ1)
        // Bit 1: Enable mouse interrupt (IRQ12) - optional
        // Bit 6: Disable translation
        config |= 0x01;  // Enable keyboard interrupt
        config &= !0x40; // Disable translation
        
        // Step 5: Write modified configuration byte
        cmd_port.write(0x60);
        wait_input_buffer();
        data_port.write(config);
        wait_input_buffer();
        
        crate::serial_println!("[PS/2] Config byte after: 0x{:02X}", config);
        
        // Step 6: Enable first PS/2 port (keyboard)
        cmd_port.write(0xAE);
        wait_input_buffer();
        
        // Step 7: Reset and enable keyboard scanning
        data_port.write(0xFF); // Reset keyboard
        wait_output_buffer();
        let response = data_port.read();
        crate::serial_println!("[PS/2] Keyboard reset response: 0x{:02X}", response);
        
        // Step 8: Enable keyboard scanning
        data_port.write(0xF4);
        wait_output_buffer();
        let ack = data_port.read();
        crate::serial_println!("[PS/2] Keyboard enable scanning ACK: 0x{:02X}", ack);
    }
    
    crate::serial_println!("[PS/2] Initialization complete - IRQ1 should now fire");
}

/// Wait until the input buffer is empty (bit 1 of status register is 0)
fn wait_input_buffer() {
    unsafe {
        let mut status_port = Port::<u8>::new(PS2_STATUS);
        for _ in 0..1000 {
            let status = status_port.read();
            if (status & 0x02) == 0 {
                return;
            }
        }
    }
}

/// Wait until the output buffer is full (bit 0 of status register is 1)
fn wait_output_buffer() {
    unsafe {
        let mut status_port = Port::<u8>::new(PS2_STATUS);
        for _ in 0..1000 {
            let status = status_port.read();
            if (status & 0x01) != 0 {
                return;
            }
        }
    }
}
