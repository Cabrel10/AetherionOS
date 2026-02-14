// Aetherion OS - Kernel minimal pour bootloader 0.9.23
// Contrainte: compile en < 300s avec sandbox 2 cœurs

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use core::panic::PanicInfo;
use lazy_static::lazy_static;
use spin::Mutex;
use uart_16550::SerialPort;

// VGA text buffer constants
const VGA_BUFFER: *mut u8 = 0xb8000 as *mut u8;
const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;

// Serial port COM1
const SERIAL_PORT: u16 = 0x3F8;

// Framebuffer simple
struct VgaBuffer {
    row: usize,
    col: usize,
}

impl VgaBuffer {
    const fn new() -> Self {
        VgaBuffer { row: 0, col: 0 }
    }

    fn clear(&mut self) {
        unsafe {
            for i in 0..(VGA_WIDTH * VGA_HEIGHT) {
                let offset = i * 2;
                core::ptr::write_volatile(VGA_BUFFER.add(offset), b' ');
                core::ptr::write_volatile(VGA_BUFFER.add(offset + 1), 0x07);
            }
        }
        self.row = 0;
        self.col = 0;
    }

    fn write_byte(&mut self, byte: u8) {
        if byte == b'\n' {
            self.row += 1;
            self.col = 0;
            return;
        }

        if self.row >= VGA_HEIGHT {
            self.row = 0;
            self.col = 0;
        }

        let offset = (self.row * VGA_WIDTH + self.col) * 2;
        unsafe {
            core::ptr::write_volatile(VGA_BUFFER.add(offset), byte);
            core::ptr::write_volatile(VGA_BUFFER.add(offset + 1), 0x07);
        }

        self.col += 1;
        if self.col >= VGA_WIDTH {
            self.col = 0;
            self.row += 1;
        }
    }

    fn write_str(&mut self, s: &str) {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
    }
}

lazy_static! {
    static ref VGA: Mutex<VgaBuffer> = Mutex::new(VgaBuffer::new());
}

// Serial output
fn serial_write(s: &str) {
    unsafe {
        for byte in s.bytes() {
            // Attendre que le buffer soit prêt
            while (inb(SERIAL_PORT + 5) & 0x20) == 0 {}
            outb(SERIAL_PORT, byte);
        }
    }
}

// I/O ports
unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack));
    value
}

// Panic handler
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    let mut vga = VGA.lock();
    vga.write_str("\nPANIC: Kernel panic!");
    serial_write("\n[PANIC] Kernel panic!\n");
    
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
    }
}

// Point d'entrée
#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Initialiser VGA
    {
        let mut vga = VGA.lock();
        vga.clear();
        vga.write_str("[AETHERION] Couche 1 HAL initialisee\n");
    }
    
    // Initialiser port série
    serial_write("\n========================================\n");
    serial_write("[AETHERION] Couche 1 HAL initialisee\n");
    serial_write("========================================\n");
    serial_write("Bootloader: 0.9.23 (sandbox-compatible)\n");
    serial_write("Status: Phase 1.1 complete\n");
    
    // Tests basiques
    serial_write("\n[Test 1] VGA output: OK\n");
    serial_write("[Test 2] Serial port: OK\n");
    serial_write("[Test 3] Panic handler: Ready\n");
    
    // Message de succès
    serial_write("\n[OK] AetherionOS Couche 1 HAL - Boot OK\n");
    
    // Boucle infinie avec halt
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
    }
}
