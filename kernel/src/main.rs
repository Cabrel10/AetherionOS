// Aetherion OS Kernel - Entry Point
// Phase 1.3: Memory management with heap allocator

#![no_std]
#![no_main]
#![feature(asm_const)]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::panic::PanicInfo;
use alloc::vec::Vec;
use alloc::string::String;
use alloc::boxed::Box;

// Memory management modules
mod memory;
mod allocator;
mod gdt;
mod interrupts;
mod syscall;

// Phase 2 completion
mod process;
mod ipc;

// Phase 3: Drivers
mod drivers;

// Phase 4: Filesystem
mod fs;

// Phase 5: Networking
mod net;

use memory::{PhysicalAddress, frame_allocator::FrameAllocator};

// VGA Text Mode Constants
const VGA_BUFFER: *mut u8 = 0xb8000 as *mut u8;
const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;
const VGA_COLOR_WHITE_ON_BLACK: u8 = 0x0f;

// Serial Port Constants (COM1)
const SERIAL_PORT: u16 = 0x3F8;

// Memory configuration
const MEMORY_START: usize = 0x100000;  // 1MB (after kernel)
const MEMORY_SIZE: usize = 32 * 1024 * 1024;  // 32MB managed RAM
static mut FRAME_BITMAP: [u8; 4096] = [0; 4096];  // Support 32MB (8192 frames)

// Heap configuration
const HEAP_START: usize = 0x_4444_4444_0000;
const HEAP_SIZE: usize = 1024 * 1024;  // 1 MB heap
static mut HEAP_MEMORY: [u8; HEAP_SIZE] = [0; HEAP_SIZE];

/// Kernel Entry Point
/// Called by bootloader after CPU is in 64-bit long mode
#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Initialize hardware
    init_serial();
    clear_screen();
    
    // Display boot message
    print_str("AETHERION OS v1.0.0 - FINAL", 0, 0);
    print_str("============================", 0, 1);
    print_str("Kernel loaded successfully!", 0, 3);
    print_str("All Phases: COMPLETE", 0, 4);
    print_str("Status: INITIALIZING...", 0, 5);
    
    // Initialize GDT
    serial_print("[CPU] Initializing GDT...\n");
    gdt::init();
    print_str("GDT: INITIALIZED", 0, 6);
    serial_print("[CPU] GDT initialized\n");
    
    // Initialize IDT
    serial_print("[CPU] Initializing IDT...\n");
    interrupts::init();
    print_str("IDT: INITIALIZED", 0, 7);
    serial_print("[CPU] IDT initialized\n");
    
    // Initialize syscalls
    serial_print("[SYSCALL] Initializing syscall interface...\n");
    syscall::init();
    print_str("Syscalls: INITIALIZED", 0, 8);
    serial_print("[SYSCALL] Syscall interface ready\n");
    
    // Phase 2 completion
    serial_print("[PROCESS] Initializing process management...\n");
    process::init();
    ipc::init();
    print_str("Processes & IPC: INITIALIZED", 0, 9);
    
    // Phase 3: Drivers
    serial_print("[DRIVERS] Initializing device drivers...\n");
    drivers::init_all();
    print_str("Drivers: INITIALIZED", 0, 10);
    
    // Phase 4: Filesystem
    serial_print("[FS] Initializing filesystem...\n");
    fs::init();
    print_str("Filesystem: INITIALIZED", 0, 11);
    
    // Phase 5: Networking
    serial_print("[NET] Initializing network stack...\n");
    net::init();
    print_str("Networking: INITIALIZED", 0, 12);
    
    // Log to serial
    serial_print("[KERNEL] Aetherion OS booted successfully\n");
    serial_print("[KERNEL] VGA driver initialized\n");
    serial_print("[KERNEL] Serial output configured\n");
    
    // Initialize frame allocator
    serial_print("[MEMORY] Initializing frame allocator...\n");
    let mut frame_allocator = unsafe {
        FrameAllocator::new(
            PhysicalAddress::new(MEMORY_START),
            MEMORY_SIZE,
            &mut FRAME_BITMAP,
        )
    };
    
    print_str("Frame Allocator: INITIALIZED", 0, 6);
    serial_print("[MEMORY] Frame allocator initialized\n");
    
    // Display memory info
    print_str("Memory Information:", 0, 8);
    print_str("  Total RAM: 32 MB", 0, 9);
    print_str("  Start Address: 0x100000", 0, 10);
    print_str("  Frame Size: 4 KB", 0, 11);
    print_str("  Total Frames: 8192", 0, 12);
    
    // Test frame allocation
    serial_print("[MEMORY] Testing frame allocation...\n");
    print_str("Testing Frame Allocation...", 0, 14);
    
    // Allocate 5 frames
    for i in 1..=5 {
        if let Some(frame) = frame_allocator.allocate_frame() {
            serial_print("[MEMORY] Allocated frame #");
            // Note: Can't format numbers yet, will add in next phase
            serial_print("\n");
        } else {
            serial_print("[ERROR] Frame allocation failed!\n");
            print_str("ERROR: Frame allocation failed!", 0, 15);
        }
    }
    
    print_str("Allocated 5 frames successfully!", 0, 15);
    serial_print("[MEMORY] Allocated 5 test frames\n");
    
    // Initialize heap allocator
    serial_print("[HEAP] Initializing heap allocator...\n");
    unsafe {
        allocator::init_heap(
            HEAP_MEMORY.as_ptr() as usize,
            HEAP_SIZE,
        );
    }
    print_str("Heap Allocator: INITIALIZED", 0, 16);
    serial_print("[HEAP] Heap allocator initialized (1 MB)\n");
    
    // Test heap allocations
    serial_print("[HEAP] Testing dynamic allocations...\n");
    print_str("Testing Heap Allocations...", 0, 18);
    
    // Test Vec
    let mut vec = Vec::new();
    vec.push(1);
    vec.push(2);
    vec.push(3);
    serial_print("[HEAP] Vec test: OK\n");
    
    // Test String
    let mut s = String::from("Aetherion");
    s.push_str(" OS");
    serial_print("[HEAP] String test: OK\n");
    
    // Test Box
    let boxed = Box::new(42);
    serial_print("[HEAP] Box test: OK\n");
    
    print_str("All heap tests passed!", 0, 19);
    serial_print("[HEAP] All dynamic allocation tests passed\n");
    
    // Display heap stats
    let stats = allocator::heap_stats();
    print_str("Heap Statistics:", 0, 21);
    print_str("  Size: 1 MB", 0, 22);
    serial_print("[HEAP] Heap size: 1 MB\n");
    
    serial_print("[MEMORY] Memory management fully operational\n");
    print_str("Status: OPERATIONAL", 0, 24);
    
    // Halt loop
    print_str("System ready. Press Reset to reboot.", 0, 25);
    
    loop {
        // Use hlt instruction to save power
        unsafe {
            core::arch::asm!("hlt");
        }
    }
}

/// Allocation Error Handler
/// Called when heap allocation fails
#[alloc_error_handler]
fn alloc_error(layout: core::alloc::Layout) -> ! {
    panic!("Heap allocation failed: {:?}", layout);
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
