//! PS/2 Controller (8042) Initialization — Production Hardware Fix
//!
//! Initializes the PS/2 controller to enable keyboard interrupts (IRQ1).
//! 
//! CRITICAL FIX (Jalon 68): The previous code cleared Bit 6 (Translation)
//! in the config byte, causing the controller to emit raw Scancode Set 2
//! bytes. On KVM/QEMU with `-vga std`, the 8042 emulation expects
//! Translation=ON so it converts Set 2 → Set 1 before delivering to IRQ1.
//! Without translation, the kernel's scancode-to-ASCII table was
//! mismatched and EVERY key press was silently dropped.
//!
//! FIX: Set Bit 6 (Translation=ON) AND Bit 0 (IRQ1=ON) → config |= 0x41.
//! The keyboard handler in idt.rs now uses Scancode Set 1 (make/break).
//! Key releases are detected via scancode & 0x80 != 0.
//!
//! Based on OSDev Wiki: https://wiki.osdev.org/PS/2_Keyboard

use x86_64::instructions::port::Port;

const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;
const PS2_COMMAND: u16 = 0x64;

/// Initialize the PS/2 controller and enable keyboard interrupts.
///
/// Boot sequence position: [3.5/12] — after PIC remap, before security init.
/// 
/// Config byte layout:
///   Bit 0: First PS/2 port interrupt (IRQ1) — MUST be 1
///   Bit 1: Second PS/2 port interrupt (IRQ12, mouse)
///   Bit 4: First PS/2 port clock (0=enabled)
///   Bit 5: Second PS/2 port clock (0=enabled)
///   Bit 6: Translation (1=Set2→Set1) — MUST be 1 for KVM/QEMU
pub fn init() {
    crate::serial_println!("[PS/2] ──────────────────────────────────────");
    crate::serial_println!("[PS/2] Initializing 8042 controller...");
    
    unsafe {
        let mut data_port = Port::<u8>::new(PS2_DATA);
        let mut cmd_port = Port::<u8>::new(PS2_COMMAND);
        let _status_port = Port::<u8>::new(PS2_STATUS);
        
        // Step 1: Disable both PS/2 ports during configuration
        cmd_port.write(0xAD);  // Disable first port (keyboard)
        wait_input_buffer();
        cmd_port.write(0xA7);  // Disable second port (mouse)
        wait_input_buffer();
        crate::serial_println!("[PS/2] Step 1: Both ports disabled for config");
        
        // Step 2: Flush output buffer (discard stale data)
        let stale = data_port.read();
        crate::serial_println!("[PS/2] Step 2: Flushed output buffer (stale=0x{:02X})", stale);
        
        // Step 3: Read current configuration byte
        cmd_port.write(0x20);  // Command: read config byte
        wait_output_buffer();
        let config_before = data_port.read();
        crate::serial_println!("[PS/2] Step 3: Config byte BEFORE = 0x{:02X}", config_before);
        crate::serial_println!("[PS/2]   Bit 0 (IRQ1 enable)   = {}", (config_before & 0x01) != 0);
        crate::serial_println!("[PS/2]   Bit 1 (IRQ12 enable)  = {}", (config_before & 0x02) != 0);
        crate::serial_println!("[PS/2]   Bit 4 (Port1 clock)   = {}", if config_before & 0x10 != 0 { "disabled" } else { "enabled" });
        crate::serial_println!("[PS/2]   Bit 6 (Translation)   = {}", if config_before & 0x40 != 0 { "ON (Set2→Set1)" } else { "OFF (raw Set2)" });
        
        // Step 4: CRITICAL FIX — Enable Translation (Bit 6) AND IRQ1 (Bit 0)
        //
        // config |= 0x41  sets:
        //   Bit 0 = 1 → Enable keyboard interrupt (IRQ1)
        //   Bit 6 = 1 → Enable Set2→Set1 translation
        //
        // This is the PRODUCTION fix: KVM's 8042 emulation sends Set 2 from
        // the keyboard device. With Bit 6=1, the controller translates to
        // Set 1 before raising IRQ1. Our handler then uses a Set 1 table.
        //
        // PREVIOUS BUG: config &= !0x40 DISABLED translation → raw Set 2
        // was delivered → our Set 2 table only covered make codes, not
        // releases → key presses appeared to work but repeat/state was wrong
        // → terminal received no input.
        let config_after = (config_before | 0x41) & !0x10;  // Enable IRQ1 + Translation, ensure port1 clock enabled
        
        // Step 5: Write the corrected configuration byte
        cmd_port.write(0x60);  // Command: write config byte
        wait_input_buffer();
        data_port.write(config_after);
        wait_input_buffer();
        crate::serial_println!("[PS/2] Step 5: Config byte AFTER  = 0x{:02X}", config_after);
        crate::serial_println!("[PS/2]   Bit 0 (IRQ1 enable)   = {}", (config_after & 0x01) != 0);
        crate::serial_println!("[PS/2]   Bit 6 (Translation)   = {}", if config_after & 0x40 != 0 { "ON (Set2→Set1)" } else { "OFF" });
        
        // Step 6: Verify the config was written correctly
        cmd_port.write(0x20);
        wait_output_buffer();
        let config_verify = data_port.read();
        if config_verify & 0x41 == 0x41 {
            crate::serial_println!("[PS/2] Step 6: Config VERIFIED OK (0x{:02X})", config_verify);
        } else {
            crate::serial_println!("[PS/2] Step 6: WARNING — config read back 0x{:02X}, expected bits 0x41 set!", config_verify);
        }
        
        // Step 7: Controller self-test
        cmd_port.write(0xAA);  // Self-test command
        wait_output_buffer();
        let self_test = data_port.read();
        if self_test == 0x55 {
            crate::serial_println!("[PS/2] Step 7: Controller self-test PASSED (0x55)");
        } else {
            crate::serial_println!("[PS/2] Step 7: Controller self-test returned 0x{:02X} (expected 0x55)", self_test);
        }
        
        // Step 7b: Re-write config after self-test (some controllers reset it)
        cmd_port.write(0x60);
        wait_input_buffer();
        data_port.write(config_after);
        wait_input_buffer();
        
        // Step 8: Test first PS/2 port
        cmd_port.write(0xAB);  // Test first port
        wait_output_buffer();
        let port_test = data_port.read();
        if port_test == 0x00 {
            crate::serial_println!("[PS/2] Step 8: Port 1 test PASSED (0x00)");
        } else {
            crate::serial_println!("[PS/2] Step 8: Port 1 test returned 0x{:02X} (0x00=OK)", port_test);
        }
        
        // Step 9: Enable first PS/2 port
        cmd_port.write(0xAE);
        wait_input_buffer();
        crate::serial_println!("[PS/2] Step 9: First port ENABLED");
        
        // Step 10: Reset keyboard device
        data_port.write(0xFF);
        wait_output_buffer();
        let reset_ack = data_port.read();
        crate::serial_println!("[PS/2] Step 10: Keyboard reset ACK = 0x{:02X} (0xFA=OK)", reset_ack);
        
        // Wait for BAT (Basic Assurance Test) result
        if reset_ack == 0xFA {
            wait_output_buffer();
            let bat = data_port.read();
            crate::serial_println!("[PS/2] Step 10b: BAT result = 0x{:02X} (0xAA=pass)", bat);
        }
        
        // Step 11: Enable keyboard scanning
        data_port.write(0xF4);
        wait_output_buffer();
        let scan_ack = data_port.read();
        crate::serial_println!("[PS/2] Step 11: Enable scanning ACK = 0x{:02X} (0xFA=OK)", scan_ack);
    }
    
    crate::serial_println!("[PS/2] ──────────────────────────────────────");
    crate::serial_println!("[PS/2] Initialization COMPLETE");
    crate::serial_println!("[PS/2]   Translation: ON (Set 2 → Set 1)");
    crate::serial_println!("[PS/2]   IRQ1: ENABLED");
    crate::serial_println!("[PS/2]   Scancode Set: 1 (via hardware translation)");
    crate::serial_println!("[PS/2]   Key releases: detected via bit 7 (scancode & 0x80)");
    crate::serial_println!("[PS/2] ──────────────────────────────────────");
}

/// Check if there are pending keystrokes in the keyboard buffer.
/// This is a non-consuming check used by epoll_wait for stdin readiness.
/// Delegates to the kernel's KbdBuffer ring buffer which is filled by IRQ1.
pub fn has_pending_key() -> bool {
    crate::process::kbd_has_pending()
}

/// Wait until the input buffer is empty (bit 1 of status register is 0).
/// Timeout after ~100µs to prevent infinite hang on broken hardware.
fn wait_input_buffer() {
    unsafe {
        let mut status_port = Port::<u8>::new(PS2_STATUS);
        for _ in 0..10_000 {
            let status = status_port.read();
            if (status & 0x02) == 0 {
                return;
            }
        }
        crate::serial_println!("[PS/2] WARNING: Input buffer wait timeout");
    }
}

/// Wait until the output buffer is full (bit 0 of status register is 1).
/// Timeout after ~100µs to prevent infinite hang on broken hardware.
fn wait_output_buffer() {
    unsafe {
        let mut status_port = Port::<u8>::new(PS2_STATUS);
        for _ in 0..10_000 {
            let status = status_port.read();
            if (status & 0x01) != 0 {
                return;
            }
        }
        crate::serial_println!("[PS/2] WARNING: Output buffer wait timeout");
    }
}
