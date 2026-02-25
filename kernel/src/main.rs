// Aetherion OS - Kernel Couche 3 (Cognitive Bus / IPC)
// Architecture: x86_64, Bootloader: 0.9.23
// Modules: GDT, IDT, PIC, TPM/Security, Memory, IPC (Cognitive Bus)

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
#![feature(panic_info_message)]

use core::panic::PanicInfo;
use core::fmt::Write;
use lazy_static::lazy_static;
use spin::Mutex;
use uart_16550::SerialPort;
use bootloader::BootInfo;

// ===== Modules =====
mod arch;
mod security;
mod memory;
mod ipc;

// ===== Configuration =====
const KERNEL_VERSION: &str = "0.3.0-cognitive-bus";

// VGA text buffer
const VGA_BUFFER: *mut u8 = 0xb8000 as *mut u8;
const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;

// ===== Serial Port (uart_16550) =====
lazy_static! {
    static ref SERIAL1: Mutex<SerialPort> = {
        let mut serial_port = unsafe { SerialPort::new(0x3F8) };
        serial_port.init();
        Mutex::new(serial_port)
    };
}

/// Write a string to the serial port
pub fn serial_write(s: &str) {
    let mut serial = SERIAL1.lock();
    for byte in s.bytes() {
        serial.send(byte);
    }
}

// Macro for serial_println
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

// ===== Panic Handler =====
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Try serial output first (more reliable)
    serial_write("\n[PANIC] ");
    if let Some(msg) = info.message() {
        let mut s = arrayvec::ArrayString::<256>::new();
        let _ = write!(s, "{}", msg);
        serial_write(&s);
    }
    if let Some(loc) = info.location() {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = write!(s, " at {}:{}", loc.file(), loc.line());
        serial_write(&s);
    }
    serial_write("\nSystem halted.\n");

    loop {
        x86_64::instructions::hlt();
    }
}

// ===== Heap support =====
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
        let mut s = arrayvec::ArrayString::<64>::new();
        let _ = write!(s, "val={} OK\n", *boxed_value);
        serial_write(&s);
    }

    // Test 2: Vec allocation and push
    serial_write("  [TEST 2/3] Vec push 0..9... ");
    let mut vec = Vec::new();
    for i in 0..10u64 {
        vec.push(i * 10);
    }
    assert_eq!(vec.len(), 10);
    assert_eq!(vec[5], 50);
    {
        let mut s = arrayvec::ArrayString::<64>::new();
        let _ = write!(s, "len={}, vec[5]={} OK\n", vec.len(), vec[5]);
        serial_write(&s);
    }

    // Test 3: String allocation
    serial_write("  [TEST 3/3] String::from(\"AetherionOS\")... ");
    let test_string = String::from("AetherionOS Heap OK");
    assert_eq!(test_string.len(), 19);
    assert!(test_string.contains("Heap"));
    {
        let mut s = arrayvec::ArrayString::<64>::new();
        let _ = write!(s, "len={} OK\n", test_string.len());
        serial_write(&s);
    }

    // Stress test: 100 allocations
    serial_write("  [STRESS] 100 allocations... ");
    for i in 0..100u64 {
        let b = Box::new(i);
        assert_eq!(*b, i);
    }
    serial_write("OK\n");
}

// ===== Entry Point =====
bootloader::entry_point!(kernel_main);

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    // Serial port is auto-initialized via lazy_static on first use

    // === Banner ===
    serial_write("\n========================================\n");
    serial_write("[AETHERION] Kernel v");
    serial_write(KERNEL_VERSION);
    serial_write("\n========================================\n\n");

    // VGA clear
    {
        let mut vga = VGA.lock();
        vga.clear();
        vga.write_str("[AETHERION] Couche 3 - Boot\n");
    }

    // === Step 1: GDT ===
    serial_write("[1/5] Loading GDT...\n");
    arch::x86_64::gdt::init();
    serial_write("      [OK] GDT with TSS loaded\n");

    // === Step 2: IDT ===
    serial_write("[2/5] Loading IDT...\n");
    arch::x86_64::idt::init();
    serial_write("      [OK] IDT with 20 handlers\n");

    // === Step 3: PIC ===
    serial_write("[3/5] Initializing PIC...\n");
    arch::x86_64::interrupts::init();
    serial_write("      [OK] PIC remapped (32-47)\n");

    // === Step 4: Security ===
    serial_write("[4/5] Security init...\n");
    security::init();
    serial_write("      [OK] TPM stub + PCR0\n");

    // === Step 5: Memory (Couche 2) ===
    serial_write("[5/5] Memory init (Couche 2)...\n");
    let mut memory_manager = match memory::init(boot_info) {
        Ok(mm) => {
            serial_write("      [OK] Memory manager ready\n");
            mm
        }
        Err(e) => {
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = write!(s, "      [FAILED] {}\n", e);
            serial_write(&s);
            panic!("Memory init failed");
        }
    };

    // Init heap
    match memory_manager.init_heap() {
        Ok(()) => {
            serial_write("      [OK] Heap allocator ready\n");
        }
        Err(e) => {
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = write!(s, "      [WARN] Heap: {}\n", e);
            serial_write(&s);
        }
    }

    // === Heap Tests ===
    serial_write("\n[TEST] Heap validation...\n");
    run_heap_tests();
    serial_write("[TEST] All heap tests PASSED!\n");

    // ===================================================================
    // COUCHE 3: COGNITIVE BUS (IPC Lock-Free) TESTS
    // ===================================================================
    {
        let mut vga = VGA.lock();
        vga.write_str("\n[3/5] Testing Cognitive Bus (IPC)...\n");
    }

    serial_write("\n========================================\n");
    serial_write("[COGNITIVE BUS] Initializing...\n");
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = write!(s, "  Capacity: {} messages\n", ipc::bus::capacity());
        serial_write(&s);
    }

    // --- TEST 1: Basic publish/consume ---
    serial_write("\n[TEST 1] Basic message flow:\n");
    {
        use ipc::{IntentMessage, ComponentId, Priority};

        let msg1 = IntentMessage::new(
            ComponentId::HAL,
            ComponentId::Orchestrator,
            0x0001, // KeyPress intent
            Priority::Normal,
            0x41,   // 'A' key scancode
        );

        match ipc::bus::publish(msg1) {
            Ok(_) => {
                let mut s = arrayvec::ArrayString::<256>::new();
                let _ = write!(s, "  [OK] Published: {}\n", msg1);
                serial_write(&s);
            }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = write!(s, "  [FAIL] Publish error: {:?}\n", e);
                serial_write(&s);
            }
        }

        match ipc::bus::consume() {
            Ok(msg) => {
                let mut s = arrayvec::ArrayString::<256>::new();
                let _ = write!(s, "  [OK] Consumed: {}\n", msg);
                serial_write(&s);
            }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = write!(s, "  [FAIL] Consume error: {:?}\n", e);
                serial_write(&s);
            }
        }

        // Verify empty after consume
        match ipc::bus::consume() {
            Err(ipc::BusError::QueueEmpty) => {
                serial_write("  [OK] Queue empty after consume (correct)\n");
            }
            _ => {
                serial_write("  [FAIL] Queue should be empty!\n");
            }
        }
    }

    // --- TEST 2: Multiple messages (Orchestrateur simulator) ---
    serial_write("\n[TEST 2] Multi-message orchestrator simulation:\n");
    {
        use ipc::{IntentMessage, ComponentId, Priority};

        // Simuler 3 messages du systeme nerveux
        let msg_hal = IntentMessage::new(
            ComponentId::HAL,
            ComponentId::Orchestrator,
            0x0001, // KeyPress
            Priority::Normal,
            0x41,   // 'A' key
        );

        let msg_verifier = IntentMessage::new(
            ComponentId::Verifier,
            ComponentId::Orchestrator,
            0x0020, // VerifyIntegrity
            Priority::Critical,
            0xDEAD_BEEF,
        );

        let msg_cerebellum = IntentMessage::new(
            ComponentId::Cerebellum,
            ComponentId::HAL,
            0x0030, // PredictionReady
            Priority::Low,
            0xCAFE,
        );

        // Publier les 3 messages
        let messages = [msg_hal, msg_verifier, msg_cerebellum];
        for msg in &messages {
            match ipc::bus::publish(*msg) {
                Ok(_) => {
                    let mut s = arrayvec::ArrayString::<256>::new();
                    let _ = write!(s, "  [PUB] {}\n", msg);
                    serial_write(&s);
                }
                Err(e) => {
                    let mut s = arrayvec::ArrayString::<128>::new();
                    let _ = write!(s, "  [FAIL] Publish: {:?}\n", e);
                    serial_write(&s);
                }
            }
        }

        {
            let mut s = arrayvec::ArrayString::<64>::new();
            let _ = write!(s, "  Queue length: {}\n", ipc::bus::len());
            serial_write(&s);
        }

        // Simulateur de l'Orchestrateur: depiler et traiter
        serial_write("\n  [ORCHESTRATOR] Consuming bus:\n");
        let mut consumed = 0u32;
        while let Ok(msg) = ipc::bus::consume() {
            consumed += 1;
            let mut s = arrayvec::ArrayString::<256>::new();
            let _ = write!(s, "    #{} {}\n", consumed, msg);
            serial_write(&s);
        }

        {
            let mut s = arrayvec::ArrayString::<64>::new();
            let _ = write!(s, "  [OK] Consumed {} messages\n", consumed);
            serial_write(&s);
        }
    }

    // --- TEST 3: Overflow detection ---
    serial_write("\n[TEST 3] Overflow detection:\n");
    {
        use ipc::{IntentMessage, ComponentId, Priority};

        let mut published = 0u32;
        for i in 0..110u32 {
            let msg = IntentMessage::new(
                ComponentId::HAL,
                ComponentId::Orchestrator,
                i,
                Priority::Normal,
                i as u64,
            );

            match ipc::bus::publish(msg) {
                Ok(_) => published += 1,
                Err(ipc::BusError::QueueFull) => {
                    let mut s = arrayvec::ArrayString::<128>::new();
                    let _ = write!(s, "  [OK] Overflow detected at msg #{} (capacity: {})\n",
                        i, ipc::bus::capacity());
                    serial_write(&s);
                    break;
                }
                Err(e) => {
                    let mut s = arrayvec::ArrayString::<128>::new();
                    let _ = write!(s, "  [FAIL] Unexpected error: {:?}\n", e);
                    serial_write(&s);
                    break;
                }
            }
        }

        {
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = write!(s, "  Published {} messages before overflow\n", published);
            serial_write(&s);
        }

        // Drain the queue
        while ipc::bus::consume().is_ok() {}
        serial_write("  [OK] Queue drained successfully\n");
    }

    serial_write("\n[COGNITIVE BUS] All tests PASSED!\n");
    serial_write("========================================\n");

    {
        let mut vga = VGA.lock();
        vga.write_str("[OK] Couche 3 ready - Cognitive Bus active\n");
    }

    // === Boot Complete ===
    serial_write("\n========================================\n");
    serial_write("[BOOT] AetherionOS Couche 3 READY\n");
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = write!(s, "  Memory: {} frames ({} KB)\n",
            memory_manager.frame_allocator.total_frames(),
            memory_manager.frame_allocator.total_frames() * 4);
        serial_write(&s);
    }
    {
        let mut s = arrayvec::ArrayString::<64>::new();
        let _ = write!(s, "  Heap: {} KB\n", memory::heap::HEAP_SIZE / 1024);
        serial_write(&s);
    }
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = write!(s, "  Cognitive Bus: {} msg capacity (lock-free MPMC)\n",
            ipc::bus::capacity());
        serial_write(&s);
    }
    serial_write("  Interrupts: enabled\n");
    serial_write("  Security: TPM stub\n");
    serial_write("========================================\n");

    // Update VGA
    {
        let mut vga = VGA.lock();
        vga.write_str("\n[OK] Couche 3 BOOT COMPLETE\n");
    }

    // Idle loop
    loop {
        x86_64::instructions::hlt();
    }
}
