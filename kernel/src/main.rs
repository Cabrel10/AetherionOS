// Aetherion OS - Kernel Couche 2 (Memory Management)
// Architecture: x86_64, Bootloader: 0.9.23
// Modules: GDT, IDT, PIC, TPM/Security, Memory (NEW)

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

use core::panic::PanicInfo;
use lazy_static::lazy_static;
use spin::Mutex;
use bootloader::BootInfo;

// ===== Modules =====
mod arch;
mod security;
mod memory;

// ===== Configuration =====
const KERNEL_VERSION: &str = "0.2.0-memory";
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
        VgaBuffer { row: 0, col: 0, color: 0x0F }
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
    vga.color = 0x4F;
    vga.write_str("\n[KERNEL PANIC] ");
    {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<256>::new();
        let _ = write!(s, "{}", info.message());
        vga.write_str(&s);
    }

    serial_write("\n[PANIC] Kernel panic: ");
    {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<256>::new();
        let _ = write!(s, "{}", info.message());
        serial_write(&s);
    }
    serial_write("\nSystem halted.\n");

    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
    }
}

// ===== Heap Tests =====
// Tests minimaux pour valider l'allocateur heap (Box, Vec, String)

extern crate alloc;
use alloc::boxed::Box;
use alloc::vec::Vec;
use alloc::string::String;

fn run_heap_tests() {
    // Test 1: Box allocation
    serial_write("  [TEST 1/3] Box::new(42)... ");
    let boxed_value = Box::new(42u64);
    assert_eq!(*boxed_value, 42);
    {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<64>::new();
        let _ = write!(s, "ptr={:p}, val={} OK\n", boxed_value.as_ref(), *boxed_value);
        serial_write(&s);
    }
    // Note: Box is dropped here, testing deallocation

    // Test 2: Vec allocation and push
    serial_write("  [TEST 2/3] Vec push 0..9... ");
    let mut vec = Vec::new();
    for i in 0..10 {
        vec.push(i * 10);
    }
    assert_eq!(vec.len(), 10);
    assert_eq!(vec[5], 50);
    {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<64>::new();
        let _ = write!(s, "len={}, vec[5]={} OK\n", vec.len(), vec[5]);
        serial_write(&s);
    }
    // Note: Vec is dropped here, testing deallocation

    // Test 3: String allocation
    serial_write("  [TEST 3/3] String::from(\"AetherionOS\")... ");
    let test_string = String::from("AetherionOS Heap OK");
    assert_eq!(test_string.len(), 19);
    assert!(test_string.contains("Heap"));
    {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<64>::new();
        let _ = write!(s, "len={}, \"{}\" OK\n", test_string.len(), &test_string[..10]);
        serial_write(&s);
    }
    // Note: String is dropped here, testing deallocation
}

// ===== Entry Point avec BootInfo =====
bootloader::entry_point!(kernel_main);

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    // Initialiser VGA
    {
        let mut vga = VGA.lock();
        vga.clear();
        vga.write_str("[AETHERION] Couche 2 Memory - Initialisation\n");
    }

    // Header serial
    serial_write("\n========================================\n");
    serial_write("[AETHERION] Kernel ");
    serial_write(KERNEL_VERSION);
    serial_write("\n");
    serial_write("========================================\n");

    // ===== ÉTAPE 1: GDT =====
    serial_write("[1/5] Loading GDT...\n");
    arch::x86_64::gdt::init();
    serial_write("      [OK] GDT with TSS loaded\n");

    // ===== ÉTAPE 2: IDT =====
    serial_write("[2/5] Loading IDT...\n");
    arch::x86_64::idt::init();
    serial_write("      [OK] IDT with 20 handlers loaded\n");

    // ===== ÉTAPE 3: Interrupts =====
    serial_write("[3/5] Initializing PIC...\n");
    arch::x86_64::interrupts::init();
    serial_write("      [OK] PIC 8259 remapped (32-47), IRQs enabled\n");

    // ===== ÉTAPE 4: Security =====
    serial_write("[4/5] Initializing Security...\n");
    security::init();
    serial_write("      [OK] TPM checked, PCR0 measured\n");

    // ===== ÉTAPE 5: Memory (Couche 2) =====
    serial_write("[5/5] Initializing Memory (Couche 2)...\n");
    let mut memory_manager = match memory::init(boot_info) {
        Ok(mm) => {
            serial_write("      [OK] Memory manager initialized\n");
            mm
        }
        Err(e) => {
            serial_write("      [FAILED] Memory init error: ");
            {
                use core::fmt::Write;
                let mut s = arrayvec::ArrayString::<64>::new();
                let _ = write!(s, "{}", e);
                serial_write(&s);
            }
            serial_write("\n");
            panic!("Memory initialization failed");
        }
    };
    
    // Initialiser le heap
    match memory_manager.init_heap() {
        Ok(()) => {
            serial_write("      [OK] Heap allocator initialized\n");
        }
        Err(e) => {
            serial_write("      [WARNING] Heap init failed: ");
            {
                use core::fmt::Write;
                let mut s = arrayvec::ArrayString::<64>::new();
                let _ = write!(s, "{}", e);
                serial_write(&s);
            }
            serial_write("\n");
        }
    }

    // ===== Heap Tests (Validation Couche 2) =====
    serial_write("\n[TEST] Running heap validation tests...\n");
    run_heap_tests();
    serial_write("[TEST] All heap tests passed!\n");

    // ===== Boot Complete =====
    serial_write("\n[BOOT] AetherionOS Couche 2 initialized!\n");
    serial_write("       Memory: ");
    {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<32>::new();
        let _ = write!(s, "{} KB usable", 
            memory_manager.frame_allocator.total_frames() * 4);
        serial_write(&s);
    }
    serial_write("\n");
    serial_write("       Heap: ");
    {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<32>::new();
        let _ = write!(s, "{} KB", memory::heap::HEAP_SIZE / 1024);
        serial_write(&s);
    }
    serial_write("\n");
    serial_write("       Interrupts: enabled\n");
    serial_write("       Security: TPM stub active\n");

    // Update VGA
    {
        let mut vga = VGA.lock();
        vga.write_str("\n[OK] Couche 2 ready - Memory management active\n");
    }

    // Idle loop
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
    }
}
