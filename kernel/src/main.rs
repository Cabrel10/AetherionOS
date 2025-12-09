// Aetherion OS Kernel - Entry Point
// Phase 0: Minimal bootable kernel with VGA text output

#![no_std]
#![no_main]
#![feature(asm_const)]

use core::panic::PanicInfo;

// VGA Text Mode Constants
const VGA_BUFFER: *mut u8 = 0xb8000 as *mut u8;
const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;
const VGA_COLOR_WHITE_ON_BLACK: u8 = 0x0f;

// Serial Port Constants (COM1)
const SERIAL_PORT: u16 = 0x3F8;

/// Kernel Entry Point
/// Called by bootloader after CPU is in 64-bit long mode
#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Initialize hardware
    init_serial();
    clear_screen();
    
    // Display boot message
    print_str("AETHERION OS v0.0.1 - Boot OK!", 0, 0);
    print_str("============================", 0, 1);
    print_str("Kernel loaded successfully!", 0, 3);
    print_str("Phase 0: Foundations", 0, 4);
    print_str("Status: OPERATIONAL", 0, 5);
    
    // Log to serial
    serial_print("[KERNEL] Aetherion OS booted successfully\n");
    serial_print("[KERNEL] VGA driver initialized\n");
    serial_print("[KERNEL] Serial output configured\n");
    serial_print("[KERNEL] Entering infinite loop...\n");
    
    // Display system info
    print_str("System Information:", 0, 7);
    print_str("  Architecture: x86_64", 0, 8);
    print_str("  Kernel: Aetherion v0.0.1", 0, 9);
    print_str("  Language: Rust (nightly)", 0, 10);
    print_str("  Mode: 64-bit Long Mode", 0, 11);
    
    // Halt loop (CPU will halt and wait for interrupts)
    print_str("System halted. Press Reset to reboot.", 0, 23);
    
    loop {
        // Use hlt instruction to save power
        unsafe {
            core::arch::asm!("hlt");
        }
    }
}

/// Panic Handler
/// Called when kernel encounters unrecoverable error
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Display panic message on VGA
    clear_screen();
    print_str("!!! KERNEL PANIC !!!", 0, 0);
    print_str("====================", 0, 1);
    
    // Try to display panic info (may not work if alloc failed)
    if let Some(location) = info.location() {
        // Format: "File: {file}, Line: {line}"
        print_str("Location:", 0, 3);
        print_str(location.file(), 2, 4);
        
        // Log to serial (more reliable)
        serial_print("[PANIC] Kernel panic occurred!\n");
        serial_print("[PANIC] File: ");
        serial_print(location.file());
        serial_print("\n[PANIC] Line: ");
        // Note: Can't format numbers without alloc, will show in hex later
        serial_print("\n");
    }
    
    print_str("System halted.", 0, 6);
    
    // Infinite halt loop
    loop {
        unsafe {
            core::arch::asm!("cli; hlt"); // Disable interrupts and halt
        }
    }
}

/// Initialize Serial Port (COM1)
fn init_serial() {
    unsafe {
        // Disable interrupts
        outb(SERIAL_PORT + 1, 0x00);
        
        // Set baud rate (115200)
        outb(SERIAL_PORT + 3, 0x80); // Enable DLAB
        outb(SERIAL_PORT + 0, 0x01); // Divisor low byte (115200)
        outb(SERIAL_PORT + 1, 0x00); // Divisor high byte
        
        // Configure: 8 bits, no parity, one stop bit
        outb(SERIAL_PORT + 3, 0x03);
        
        // Enable FIFO with 14-byte threshold
        outb(SERIAL_PORT + 2, 0xC7);
        
        // Enable RTS/DSR
        outb(SERIAL_PORT + 4, 0x0B);
    }
}

/// Print string to serial port
fn serial_print(s: &str) {
    for byte in s.bytes() {
        unsafe {
            // Wait for transmit buffer to be empty
            while (inb(SERIAL_PORT + 5) & 0x20) == 0 {}
            outb(SERIAL_PORT, byte);
        }
    }
}

/// Clear VGA screen
fn clear_screen() {
    let buffer = VGA_BUFFER;
    for i in 0..(VGA_WIDTH * VGA_HEIGHT) {
        unsafe {
            *buffer.offset(i as isize * 2) = b' ';
            *buffer.offset(i as isize * 2 + 1) = VGA_COLOR_WHITE_ON_BLACK;
        }
    }
}

/// Print string to VGA at specific position
fn print_str(s: &str, x: usize, y: usize) {
    let buffer = VGA_BUFFER;
    let offset = (y * VGA_WIDTH + x) * 2;
    
    for (i, byte) in s.bytes().enumerate() {
        if x + i >= VGA_WIDTH {
            break; // Don't overflow line
        }
        unsafe {
            *buffer.offset((offset + i * 2) as isize) = byte;
            *buffer.offset((offset + i * 2 + 1) as isize) = VGA_COLOR_WHITE_ON_BLACK;
        }
    }
}

/// Output byte to I/O port
#[inline]
unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") value,
        options(nomem, nostack, preserves_flags)
    );
}

/// Input byte from I/O port
#[inline]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!(
        "in al, dx",
        out("al") value,
        in("dx") port,
        options(nomem, nostack, preserves_flags)
    );
    value
}

// Additional compiler-required functions

#[no_mangle]
pub extern "C" fn memcpy(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    unsafe {
        for i in 0..n {
            *dest.add(i) = *src.add(i);
        }
    }
    dest
}

#[no_mangle]
pub extern "C" fn memset(s: *mut u8, c: i32, n: usize) -> *mut u8 {
    unsafe {
        for i in 0..n {
            *s.add(i) = c as u8;
        }
    }
    s
}

#[no_mangle]
pub extern "C" fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    unsafe {
        for i in 0..n {
            let a = *s1.add(i);
            let b = *s2.add(i);
            if a != b {
                return a as i32 - b as i32;
            }
        }
    }
    0
}

#[no_mangle]
pub extern "C" fn memmove(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if dest < src as *mut u8 {
        // Copy forward
        unsafe {
            for i in 0..n {
                *dest.add(i) = *src.add(i);
            }
        }
    } else {
        // Copy backward
        unsafe {
            for i in (0..n).rev() {
                *dest.add(i) = *src.add(i);
            }
        }
    }
    dest
}
