// Aetherion OS - Kernel HAL Couche 1 (Finalisé)
// Architecture: x86_64, Bootloader: 0.9.23
// Modules: GDT, IDT, PIC, TPM/Security

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

use core::panic::PanicInfo;
use lazy_static::lazy_static;
use spin::Mutex;

// ===== Modules HAL =====
mod arch;
mod security;

// ===== Configuration =====
const KERNEL_VERSION: &str = "0.1.0-hal";
const KERNEL_NAME: &str = "AetherionOS";

// VGA text buffer
const VGA_BUFFER: *mut u8 = 0xb8000 as *mut u8;
const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;

// Serial port COM1
const SERIAL_PORT: u16 = 0x3F8;

// ===== VGA Buffer =====
struct VgaBuffer {
    row: usize,
    col: usize,
    color: u8,
}

impl VgaBuffer {
    const fn new() -> Self {
        VgaBuffer { row: 0, col: 0, color: 0x0F } // White on black
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
        match byte {
            b'\n' => {
                self.row += 1;
                self.col = 0;
            }
            b'\r' => {
                self.col = 0;
            }
            _ => {
                if self.row >= VGA_HEIGHT {
                    self.scroll();
                    self.row = VGA_HEIGHT - 1;
                    self.col = 0;
                }

                let offset = (self.row * VGA_WIDTH + self.col) * 2;
                unsafe {
                    core::ptr::write_volatile(VGA_BUFFER.add(offset), byte);
                    core::ptr::write_volatile(VGA_BUFFER.add(offset + 1), self.color);
                }

                self.col += 1;
                if self.col >= VGA_WIDTH {
                    self.col = 0;
                    self.row += 1;
                }
            }
        }
    }

    fn write_str(&mut self, s: &str) {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
    }

    fn scroll(&mut self) {
        unsafe {
            for row in 1..VGA_HEIGHT {
                for col in 0..VGA_WIDTH {
                    let src_offset = (row * VGA_WIDTH + col) * 2;
                    let dst_offset = ((row - 1) * VGA_WIDTH + col) * 2;
                    let char_byte = core::ptr::read_volatile(VGA_BUFFER.add(src_offset));
                    let attr_byte = core::ptr::read_volatile(VGA_BUFFER.add(src_offset + 1));
                    core::ptr::write_volatile(VGA_BUFFER.add(dst_offset), char_byte);
                    core::ptr::write_volatile(VGA_BUFFER.add(dst_offset + 1), attr_byte);
                }
            }
            for col in 0..VGA_WIDTH {
                let offset = ((VGA_HEIGHT - 1) * VGA_WIDTH + col) * 2;
                core::ptr::write_volatile(VGA_BUFFER.add(offset), b' ');
                core::ptr::write_volatile(VGA_BUFFER.add(offset + 1), 0x07);
            }
        }
    }
}

lazy_static! {
    static ref VGA: Mutex<VgaBuffer> = Mutex::new(VgaBuffer::new());
}

// ===== Serial Output =====
pub fn serial_write(s: &str) {
    for byte in s.bytes() {
        serial_write_byte(byte);
    }
}

fn serial_write_byte(byte: u8) {
    unsafe {
        while (inb(SERIAL_PORT + 5) & 0x20) == 0 {}
        outb(SERIAL_PORT, byte);
    }
}

// Macro pour println! sur serial
#[macro_export]
macro_rules! serial_println {
    ($($arg:tt)*) => {
        {
            use core::fmt::Write;
            let mut s = arrayvec::ArrayString::<256>::new();
            let _ = write!(s, $($arg)*);
            $crate::serial_write(s.as_str());
            $crate::serial_write("\n");
        }
    };
}

// ===== I/O Ports =====
unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack));
    value
}

// ===== Panic Handler =====
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let mut vga = VGA.lock();
    vga.color = 0x4F; // White on red
    vga.write_str("\n[KERNEL PANIC] ");
    if let Some(msg) = info.message() {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<256>::new();
        let _ = write!(s, "{}", msg);
        vga.write_str(&s);
    }

    serial_write("\n[PANIC] Kernel panic: ");
    if let Some(msg) = info.message() {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<256>::new();
        let _ = write!(s, "{}", msg);
        serial_write(&s);
    }
    serial_write("\nSystem halted.\n");

    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
    }
}

// ===== Entry Point =====
#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Initialiser VGA
    {
        let mut vga = VGA.lock();
        vga.clear();
        vga.write_str("[AETHERION] Couche 1 HAL - Initialisation\n");
    }

    // Header serial
    serial_write("\n========================================\n");
    serial_write("[AETHERION] Kernel ");
    serial_write(KERNEL_VERSION);
    serial_write("\n");
    serial_write("========================================\n");

    // ===== ÉTAPE 1: GDT =====
    serial_write("[1/4] Loading GDT...\n");
    arch::x86_64::gdt::init();
    serial_write("      [OK] GDT with TSS loaded\n");

    // ===== ÉTAPE 2: IDT =====
    serial_write("[2/4] Loading IDT...\n");
    arch::x86_64::idt::init();
    serial_write("      [OK] IDT with 20 handlers loaded\n");

    // ===== ÉTAPE 3: Interrupts =====
    serial_write("[3/4] Initializing PIC...\n");
    arch::x86_64::interrupts::init();
    serial_write("      [OK] PIC 8259 remapped (32-47), IRQs enabled\n");

    // ===== ÉTAPE 4: Security =====
    serial_write("[4/4] Initializing Security...\n");
    security::init();
    serial_write("      [OK] TPM checked, PCR0 measured\n");

    // ===== Boot Complete =====
    serial_write("\n[BOOT] AetherionOS HAL initialized successfully!\n");
    serial_write("       Timer: 100Hz, Keyboard: enabled\n");
    serial_write("       Interrupts: enabled\n");
    serial_write("       Security: TPM stub active\n");

    // Update VGA
    {
        let mut vga = VGA.lock();
        vga.write_str("\n[OK] HAL initialized - System ready\n");
    }

    // Idle loop
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
    }
}
