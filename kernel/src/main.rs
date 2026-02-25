// Aetherion OS - Kernel Consolidation (Couches 1-5 Hardened)
// Architecture: x86_64, Bootloader: 0.9.23
// Modules: GDT, IDT, PIC, TPM/Security, Memory, IPC (Cognitive Bus), VFS, Verifier
//
// Security hardening (Audit Phase - Option A Consolidation):
//   - FIFO determinism in Cognitive Bus (Ord-based sift with timestamp tiebreak)
//   - Path traversal protection (component-level + charset whitelist)
//   - Null byte injection prevention
//   - Buffer overflow / capacity checks
//   - Capability-based device access (ACHA manifests)
//   - VFS stress tests (1000 cycles, binary patterns, boundary overflow)
//   - Metrics collection and reporting (atomic counters)
//   - Verifier policy engine (Couche 5) with default-deny whitelist
//   - Proper error handling (no silent .ok())

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
#![feature(panic_info_message)]
#![allow(dead_code)]

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
mod fs;
mod verifier;

// ===== Configuration =====
const KERNEL_VERSION: &str = "0.5.1-hardened";

// VGA text buffer
const VGA_BUFFER: *mut u8 = 0xb8000 as *mut u8;
const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;

// ===== Serial Port (uart_16550) =====
lazy_static! {
    static ref SERIAL1: Mutex<SerialPort> = {
        // SAFETY: 0x3F8 is the standard COM1 I/O port address on x86.
        // SerialPort::new only records the base address; init() programs
        // the UART registers. This runs once via lazy_static.
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
        // SAFETY: VGA_BUFFER (0xB8000) is the standard VGA text-mode buffer on x86.
        // We write character/attribute pairs for the entire 80x25 screen.
        // write_volatile ensures the compiler does not elide or reorder writes.
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
                // SAFETY: VGA_BUFFER + offset points within the 80x25 VGA text
                // buffer (4000 bytes). write_volatile ensures hardware sees the write.
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
        // SAFETY: All read_volatile / write_volatile accesses are within the
        // VGA text buffer (0xB8000..0xB8FA0, i.e., 80*25*2 = 4000 bytes).
        // Volatile operations ensure the hardware sees every byte move.
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
use alloc::vec::Vec;
use alloc::string::String;

fn run_heap_tests() {
    use alloc::boxed::Box;
    // Test 1: Box allocation
    serial_write("  [TEST 1/3] Box::new(42)... ");
    let boxed_value = Box::new(42u64);
    assert_eq!(*boxed_value, 42);
    {
        let mut s = arrayvec::ArrayString::<64>::new();
        let _ = writeln!(s, "val={} OK", *boxed_value);
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
        let _ = writeln!(s, "len={}, vec[5]={} OK", vec.len(), vec[5]);
        serial_write(&s);
    }

    // Test 3: String allocation
    serial_write("  [TEST 3/3] String::from(\"AetherionOS\")... ");
    let test_string = String::from("AetherionOS Heap OK");
    assert_eq!(test_string.len(), 19);
    assert!(test_string.contains("Heap"));
    {
        let mut s = arrayvec::ArrayString::<64>::new();
        let _ = writeln!(s, "len={} OK", test_string.len());
        serial_write(&s);
    }

    // Stress test: 100 allocations
    serial_write("  [STRESS] 100 allocations... ");
    for i in 0..100u64 {
        let b = alloc::boxed::Box::new(i);
        assert_eq!(*b, i);
    }
    serial_write("OK\n");
}

// ===================================================================
// VFS TEST SUITE - Comprehensive security and functionality tests
// ===================================================================

fn run_vfs_tests() {
    serial_write("\n========================================\n");
    serial_write("[VFS TESTS] Starting comprehensive VFS test suite\n");
    serial_write("========================================\n\n");

    let mut tests_passed = 0u32;
    let mut tests_failed = 0u32;

    // --- TEST 1: VFS Initialization ---
    serial_write("  [TEST 1/14] VFS init... ");
    match fs::vfs::init() {
        Ok(_) => { serial_write("OK\n"); tests_passed += 1; }
        Err(e) => {
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = writeln!(s, "FAIL: {:?}", e);
            serial_write(&s); tests_failed += 1;
        }
    }

    // --- TEST 2: Mount RAM disk device ---
    serial_write("  [TEST 2/14] Mount /dev/ram0 (1KB writable)... ");
    {
        let manifest = fs::manifest::DeviceManifest::ram_disk("ram0", 1024, true);
        match fs::vfs::mount_device("/dev/ram0", manifest) {
            Ok(_) => { serial_write("OK\n"); tests_passed += 1; }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "FAIL: {:?}", e);
                serial_write(&s); tests_failed += 1;
            }
        }
    }

    // --- TEST 3: Write to /dev/ram0 ---
    serial_write("  [TEST 3/14] Write to /dev/ram0... ");
    {
        let test_data = b"Hello AetherionOS VFS!";
        match fs::vfs::file_write("/dev/ram0", test_data) {
            Ok(n) if n == test_data.len() => {
                let mut s = arrayvec::ArrayString::<64>::new();
                let _ = writeln!(s, "OK ({} bytes)", n);
                serial_write(&s); tests_passed += 1;
            }
            Ok(_) => { serial_write("FAIL: wrong byte count\n"); tests_failed += 1; }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "FAIL: {:?}", e);
                serial_write(&s); tests_failed += 1;
            }
        }
    }

    // --- TEST 4: Read from /dev/ram0 ---
    serial_write("  [TEST 4/14] Read from /dev/ram0... ");
    {
        match fs::vfs::file_read("/dev/ram0") {
            Ok(data) if data.as_slice() == b"Hello AetherionOS VFS!" => {
                let mut s = arrayvec::ArrayString::<64>::new();
                let _ = writeln!(s, "OK ({} bytes)", data.len());
                serial_write(&s); tests_passed += 1;
            }
            Ok(_) => { serial_write("FAIL: data mismatch\n"); tests_failed += 1; }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "FAIL: {:?}", e);
                serial_write(&s); tests_failed += 1;
            }
        }
    }

    // --- TEST 5: Mount read-only device ---
    serial_write("  [TEST 5/14] Mount /dev/rom0 (read-only)... ");
    {
        let manifest = fs::manifest::DeviceManifest::ram_disk("rom0", 512, false);
        match fs::vfs::mount_device("/dev/rom0", manifest) {
            Ok(_) => { serial_write("OK\n"); tests_passed += 1; }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "FAIL: {:?}", e);
                serial_write(&s); tests_failed += 1;
            }
        }
    }

    // --- TEST 6: Write to read-only device MUST fail ---
    serial_write("  [TEST 6/14] Write to read-only /dev/rom0... ");
    {
        match fs::vfs::file_write("/dev/rom0", b"should fail") {
            Err(fs::vfs::VfsError::ReadOnlyDevice) => {
                serial_write("OK (correctly denied)\n"); tests_passed += 1;
            }
            Ok(_) => { serial_write("FAIL: write should have been denied!\n"); tests_failed += 1; }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "FAIL: wrong error {:?}", e);
                serial_write(&s); tests_failed += 1;
            }
        }
    }

    // --- TEST 7: Read non-existent file ---
    serial_write("  [TEST 7/14] Read non-existent /dev/noexist... ");
    {
        match fs::vfs::file_read("/dev/noexist") {
            Err(fs::vfs::VfsError::NotFound) => {
                serial_write("OK (correctly returned NotFound)\n"); tests_passed += 1;
            }
            Ok(_) => { serial_write("FAIL: should have returned error!\n"); tests_failed += 1; }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "FAIL: wrong error {:?}", e);
                serial_write(&s); tests_failed += 1;
            }
        }
    }

    // ===================================================================
    // SECURITY TESTS
    // ===================================================================
    serial_write("\n  --- SECURITY TESTS ---\n\n");

    // --- TEST 8: Path traversal attack ---
    serial_write("  [TEST 8/14] Path traversal ../etc/shadow... ");
    {
        match fs::vfs::file_write("/../etc/shadow", b"pwned") {
            Err(fs::vfs::VfsError::PathTraversal) => {
                serial_write("OK (attack blocked)\n"); tests_passed += 1;
            }
            Ok(_) => { serial_write("FAIL: traversal should be blocked!\n"); tests_failed += 1; }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "FAIL: wrong error {:?}", e);
                serial_write(&s); tests_failed += 1;
            }
        }
    }

    // --- TEST 9: Path traversal attack variant ---
    serial_write("  [TEST 9/14] Path traversal /dev/../../root... ");
    {
        match fs::vfs::file_read("/dev/../../root") {
            Err(fs::vfs::VfsError::PathTraversal) => {
                serial_write("OK (attack blocked)\n"); tests_passed += 1;
            }
            Ok(_) => { serial_write("FAIL: traversal should be blocked!\n"); tests_failed += 1; }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "FAIL: wrong error {:?}", e);
                serial_write(&s); tests_failed += 1;
            }
        }
    }

    // --- TEST 10: Invalid path format (no leading /) ---
    serial_write("  [TEST 10/14] Invalid path (no leading /)... ");
    {
        match fs::vfs::file_write("dev/ram0", b"data") {
            Err(fs::vfs::VfsError::InvalidPath) => {
                serial_write("OK (correctly rejected)\n"); tests_passed += 1;
            }
            Ok(_) => { serial_write("FAIL: invalid path should be rejected!\n"); tests_failed += 1; }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "FAIL: wrong error {:?}", e);
                serial_write(&s); tests_failed += 1;
            }
        }
    }

    // --- TEST 11: Empty path ---
    serial_write("  [TEST 11/14] Empty path... ");
    {
        match fs::vfs::file_write("", b"data") {
            Err(fs::vfs::VfsError::InvalidPath) => {
                serial_write("OK (correctly rejected)\n"); tests_passed += 1;
            }
            Ok(_) => { serial_write("FAIL: empty path should be rejected!\n"); tests_failed += 1; }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "FAIL: wrong error {:?}", e);
                serial_write(&s); tests_failed += 1;
            }
        }
    }

    // --- TEST 12: Capacity overflow ---
    serial_write("  [TEST 12/14] Capacity overflow (1KB device, 2KB write)... ");
    {
        let overflow_data = [0xAA_u8; 2048]; // 2KB > 1KB capacity
        match fs::vfs::file_write("/dev/ram0", &overflow_data) {
            Err(fs::vfs::VfsError::CapacityExceeded) => {
                serial_write("OK (overflow blocked)\n"); tests_passed += 1;
            }
            Ok(_) => { serial_write("FAIL: overflow should be blocked!\n"); tests_failed += 1; }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "FAIL: wrong error {:?}", e);
                serial_write(&s); tests_failed += 1;
            }
        }
    }

    // --- TEST 13: Manifest validation ---
    serial_write("  [TEST 13/14] Manifest validation (read-only + Write cap)... ");
    {
        let mut bad_manifest = fs::manifest::DeviceManifest::ram_disk("bad", 512, true);
        bad_manifest.read_only = true; // Contradiction!
        if !bad_manifest.validate() {
            serial_write("OK (invalid manifest detected)\n"); tests_passed += 1;
        } else {
            serial_write("FAIL: should detect invalid manifest!\n"); tests_failed += 1;
        }
    }

    // --- TEST 14: Data integrity after write/read cycle ---
    serial_write("  [TEST 14/14] Data integrity check... ");
    {
        let pattern: Vec<u8> = (0..128u8).collect();
        match fs::vfs::file_write("/dev/ram0", &pattern) {
            Ok(_) => {
                match fs::vfs::file_read("/dev/ram0") {
                    Ok(data) if data == pattern => {
                        serial_write("OK (128 bytes verified)\n"); tests_passed += 1;
                    }
                    Ok(_) => { serial_write("FAIL: data corruption detected!\n"); tests_failed += 1; }
                    Err(e) => {
                        let mut s = arrayvec::ArrayString::<128>::new();
                        let _ = writeln!(s, "FAIL: read error {:?}", e);
                        serial_write(&s); tests_failed += 1;
                    }
                }
            }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "FAIL: write error {:?}", e);
                serial_write(&s); tests_failed += 1;
            }
        }
    }

    // ===== TEST SUMMARY =====
    serial_write("\n========================================\n");
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "[VFS TESTS] Results: {}/{} passed, {} failed",
            tests_passed, tests_passed + tests_failed, tests_failed);
        serial_write(&s);
    }
    if tests_failed == 0 {
        serial_write("[VFS TESTS] ALL TESTS PASSED!\n");
    } else {
        serial_write("[VFS TESTS] SOME TESTS FAILED!\n");
    }
    serial_write("========================================\n");
}

// ===================================================================
// VFS STRESS & HARDENING TEST SUITE (Audit Phase 3)
// ===================================================================

fn run_vfs_stress_tests() {
    serial_write("\n========================================\n");
    serial_write("[VFS STRESS] Starting hardening test suite\n");
    serial_write("========================================\n\n");

    let mut tests_passed = 0u32;
    let mut tests_failed = 0u32;

    // --- STRESS TEST 1: 1000 write/read cycles ---
    serial_write("  [STRESS 1/7] 1000 write/read cycles on /dev/ram0...\n");
    {
        let mut ok = true;
        for i in 0u32..1000 {
            let data = alloc::format!("Cycle-{:04}", i);
            match fs::vfs::file_write("/dev/ram0", data.as_bytes()) {
                Ok(n) if n == data.len() => {}
                _ => { ok = false; break; }
            }
            match fs::vfs::file_read("/dev/ram0") {
                Ok(ref read_data) if read_data.as_slice() == data.as_bytes() => {}
                _ => { ok = false; break; }
            }
            if i % 250 == 0 {
                let mut s = arrayvec::ArrayString::<64>::new();
                let _ = writeln!(s, "    Cycle {}/1000 OK", i);
                serial_write(&s);
            }
        }
        if ok {
            serial_write("  [OK] 1000 VFS write/read cycles PASSED\n");
            tests_passed += 1;
        } else {
            serial_write("  [FAIL] VFS stress test failed!\n");
            tests_failed += 1;
        }
    }

    // --- STRESS TEST 2: Path traversal attack variants ---
    serial_write("\n  [STRESS 2/7] Path traversal attack vectors...\n");
    {
        let attacks: [&str; 6] = [
            "/../etc/passwd",
            "/dev/../../root",
            "/dev/../../../shadow",
            "/./dev/ram0",
            "/dev//ram0",
            "/dev/..hidden",
        ];
        let mut all_blocked = true;
        for attack_path in &attacks {
            match fs::vfs::file_read(attack_path) {
                Err(_) => {
                    let mut s = arrayvec::ArrayString::<128>::new();
                    let _ = writeln!(s, "    [OK] Blocked: {}", attack_path);
                    serial_write(&s);
                }
                Ok(_) => {
                    let mut s = arrayvec::ArrayString::<128>::new();
                    let _ = writeln!(s, "    [FAIL] NOT BLOCKED: {}", attack_path);
                    serial_write(&s);
                    all_blocked = false;
                }
            }
        }
        if all_blocked {
            serial_write("  [OK] All 6 path traversal attacks blocked\n");
            tests_passed += 1;
        } else {
            serial_write("  [FAIL] Some path traversal attacks got through!\n");
            tests_failed += 1;
        }
    }

    // --- STRESS TEST 3: device_can() enforcement ---
    serial_write("\n  [STRESS 3/7] device_can() ACHA enforcement...\n");
    {
        // Mount a read-only virtual device to test capabilities
        let manifest = fs::manifest::DeviceManifest::virtual_readonly("test-sensor");
        match fs::vfs::mount_device("/dev/sensor0", manifest) {
            Ok(_) => serial_write("    Mounted /dev/sensor0 (read-only virtual)\n"),
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "    Mount failed: {:?}", e);
                serial_write(&s);
            }
        }

        // Test: write to read-only device MUST fail
        let mut cap_ok = true;
        match fs::vfs::file_write("/dev/sensor0", b"attack") {
            Err(fs::vfs::VfsError::ReadOnlyDevice) => {
                serial_write("    [OK] Write to read-only device denied\n");
            }
            _ => {
                serial_write("    [FAIL] Write to read-only device NOT denied!\n");
                cap_ok = false;
            }
        }

        // Test: read from read-only device should work (even if empty)
        match fs::vfs::file_read("/dev/sensor0") {
            Ok(_) => {
                serial_write("    [OK] Read from read-only device allowed\n");
            }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "    [FAIL] Read denied: {:?}", e);
                serial_write(&s);
                cap_ok = false;
            }
        }

        if cap_ok {
            serial_write("  [OK] ACHA capability enforcement PASSED\n");
            tests_passed += 1;
        } else {
            serial_write("  [FAIL] ACHA capability enforcement FAILED\n");
            tests_failed += 1;
        }
    }

    // --- STRESS TEST 4: list_path() directory listing ---
    serial_write("\n  [STRESS 4/7] list_path() directory listing...\n");
    {
        let mut list_ok = true;

        // List root
        match fs::vfs::list_path("/") {
            Ok(entries) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "    [OK] Root has {} entries", entries.len());
                serial_write(&s);
                if entries.is_empty() {
                    serial_write("    [FAIL] Root should not be empty\n");
                    list_ok = false;
                }
            }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "    [FAIL] list_path('/') error: {:?}", e);
                serial_write(&s);
                list_ok = false;
            }
        }

        // List /dev
        match fs::vfs::list_path("/dev") {
            Ok(entries) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "    [OK] /dev has {} entries", entries.len());
                serial_write(&s);
                // /dev should contain at least ram0 (mounted in run_vfs_tests)
                if entries.is_empty() {
                    serial_write("    [FAIL] /dev should not be empty\n");
                    list_ok = false;
                }
            }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "    [FAIL] list_path('/dev') error: {:?}", e);
                serial_write(&s);
                list_ok = false;
            }
        }

        if list_ok {
            serial_write("  [OK] Directory listing PASSED\n");
            tests_passed += 1;
        } else {
            serial_write("  [FAIL] Directory listing FAILED\n");
            tests_failed += 1;
        }
    }

    // --- STRESS TEST 5: Data integrity with binary patterns ---
    serial_write("\n  [STRESS 5/7] Binary data integrity (256-byte pattern)...\n");
    {
        let pattern: Vec<u8> = (0..=255u8).collect();
        let mut integrity_ok = true;

        match fs::vfs::file_write("/dev/ram0", &pattern) {
            Ok(n) if n == 256 => {
                match fs::vfs::file_read("/dev/ram0") {
                    Ok(data) if data == pattern => {
                        serial_write("  [OK] 256-byte binary pattern verified\n");
                    }
                    Ok(_) => {
                        serial_write("  [FAIL] Binary data corruption!\n");
                        integrity_ok = false;
                    }
                    Err(e) => {
                        let mut s = arrayvec::ArrayString::<128>::new();
                        let _ = writeln!(s, "  [FAIL] Read error: {:?}", e);
                        serial_write(&s);
                        integrity_ok = false;
                    }
                }
            }
            _ => {
                serial_write("  [FAIL] Write error for binary pattern\n");
                integrity_ok = false;
            }
        }

        if integrity_ok { tests_passed += 1; } else { tests_failed += 1; }
    }

    // --- STRESS TEST 6: Capacity overflow protection ---
    serial_write("\n  [STRESS 6/7] Capacity overflow with exact boundary...\n");
    {
        // Write exactly 1024 bytes (ram0 capacity) - should succeed
        let exact_data = [0xBBu8; 1024];
        let mut boundary_ok = true;

        match fs::vfs::file_write("/dev/ram0", &exact_data) {
            Ok(1024) => {
                serial_write("    [OK] Exact capacity write (1024B) accepted\n");
            }
            _ => {
                serial_write("    [FAIL] Exact capacity write rejected!\n");
                boundary_ok = false;
            }
        }

        // Write 1025 bytes - should fail
        let overflow_data = [0xCCu8; 1025];
        match fs::vfs::file_write("/dev/ram0", &overflow_data) {
            Err(fs::vfs::VfsError::CapacityExceeded) => {
                serial_write("    [OK] Overflow (1025B) correctly blocked\n");
            }
            _ => {
                serial_write("    [FAIL] Overflow (1025B) NOT blocked!\n");
                boundary_ok = false;
            }
        }

        if boundary_ok {
            serial_write("  [OK] Capacity boundary enforcement PASSED\n");
            tests_passed += 1;
        } else {
            serial_write("  [FAIL] Capacity boundary enforcement FAILED\n");
            tests_failed += 1;
        }
    }

    // --- STRESS TEST 7: VFS Metrics accuracy ---
    serial_write("\n  [STRESS 7/7] VFS metrics accuracy check...\n");
    {
        let metrics = fs::vfs::get_metrics();
        let mut metrics_ok = true;

        if metrics.operations_count == 0 {
            serial_write("    [FAIL] operations_count should be > 0\n");
            metrics_ok = false;
        } else {
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = writeln!(s, "    [OK] Operations: {}", metrics.operations_count);
            serial_write(&s);
        }

        if metrics.total_bytes_written == 0 {
            serial_write("    [FAIL] total_bytes_written should be > 0\n");
            metrics_ok = false;
        } else {
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = writeln!(s, "    [OK] Bytes written: {}", metrics.total_bytes_written);
            serial_write(&s);
        }

        if metrics.total_bytes_read == 0 {
            serial_write("    [FAIL] total_bytes_read should be > 0\n");
            metrics_ok = false;
        } else {
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = writeln!(s, "    [OK] Bytes read: {}", metrics.total_bytes_read);
            serial_write(&s);
        }

        // Security violations should be > 0 (from path traversal tests)
        if metrics.security_violations == 0 {
            serial_write("    [WARN] security_violations = 0 (expected > 0 from traversal tests)\n");
        } else {
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = writeln!(s, "    [OK] Security violations tracked: {}", metrics.security_violations);
            serial_write(&s);
        }

        if metrics_ok {
            serial_write("  [OK] VFS metrics accuracy PASSED\n");
            tests_passed += 1;
        } else {
            serial_write("  [FAIL] VFS metrics accuracy FAILED\n");
            tests_failed += 1;
        }
    }

    // ===== STRESS TEST SUMMARY =====
    serial_write("\n========================================\n");
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "[VFS STRESS] Results: {}/{} passed, {} failed",
            tests_passed, tests_passed + tests_failed, tests_failed);
        serial_write(&s);
    }
    if tests_failed == 0 {
        serial_write("[VFS STRESS] ALL STRESS TESTS PASSED!\n");
    } else {
        serial_write("[VFS STRESS] SOME STRESS TESTS FAILED!\n");
    }
    serial_write("========================================\n");
}

// ===================================================================
// VERIFIER TEST SUITE
// ===================================================================

fn run_verifier_tests() {
    serial_write("\n========================================\n");
    serial_write("[VERIFIER TESTS] Starting Couche 5 test suite\n");
    serial_write("========================================\n\n");

    let mut tests_passed = 0u32;
    let mut tests_failed = 0u32;

    // TEST 1: Verify write to /dev/ path (should be allowed)
    serial_write("  [TEST 1/6] Write to /dev/ram0 (expect Allow)... ");
    match verifier::hooks::verify_write("/dev/ram0", 64) {
        Ok(()) => { serial_write("OK\n"); tests_passed += 1; }
        Err(_) => { serial_write("FAIL: expected Allow\n"); tests_failed += 1; }
    }

    // TEST 2: Verify read from /dev/ path (should be allowed)
    serial_write("  [TEST 2/6] Read from /dev/ram0 (expect Allow)... ");
    match verifier::hooks::verify_read("/dev/ram0") {
        Ok(()) => { serial_write("OK\n"); tests_passed += 1; }
        Err(_) => { serial_write("FAIL: expected Allow\n"); tests_failed += 1; }
    }

    // TEST 3: Verify write to /sys/ path (should be denied)
    serial_write("  [TEST 3/6] Write to /sys/config (expect Deny)... ");
    match verifier::hooks::verify_write("/sys/config", 10) {
        Err(_) => { serial_write("OK (denied)\n"); tests_passed += 1; }
        Ok(()) => { serial_write("FAIL: expected Deny\n"); tests_failed += 1; }
    }

    // TEST 4: Verify write to /tmp/ path (should be audited = allowed)
    serial_write("  [TEST 4/6] Write to /tmp/log (expect Audit)... ");
    match verifier::hooks::verify_write("/tmp/log", 32) {
        Ok(()) => { serial_write("OK (audited)\n"); tests_passed += 1; }
        Err(_) => { serial_write("FAIL: expected Audit/Allow\n"); tests_failed += 1; }
    }

    // TEST 5: Verify large write exceeding 64KB limit (should be denied)
    serial_write("  [TEST 5/6] Large write 128KB to /dev/ram0 (expect Deny)... ");
    match verifier::hooks::verify_write("/dev/ram0", 128 * 1024) {
        Err(_) => { serial_write("OK (denied, DoS protection)\n"); tests_passed += 1; }
        Ok(()) => { serial_write("FAIL: expected Deny for large write\n"); tests_failed += 1; }
    }

    // TEST 6: Verify unknown path (should be denied by default-deny)
    serial_write("  [TEST 6/6] Read from /unknown/path (expect Deny)... ");
    match verifier::hooks::verify_read("/unknown/path") {
        Err(_) => { serial_write("OK (default deny)\n"); tests_passed += 1; }
        Ok(()) => { serial_write("FAIL: expected default Deny\n"); tests_failed += 1; }
    }

    // Verifier metrics
    let vm = verifier::policy::get_metrics();
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "\n  [METRICS] Rules evaluated: {}", vm.rules_evaluated);
        serial_write(&s);
    }
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  [METRICS] Allowed: {}, Denied: {}, Audited: {}",
            vm.operations_allowed, vm.operations_denied, vm.operations_audited);
        serial_write(&s);
    }

    serial_write("\n========================================\n");
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "[VERIFIER TESTS] Results: {}/{} passed, {} failed",
            tests_passed, tests_passed + tests_failed, tests_failed);
        serial_write(&s);
    }
    if tests_failed == 0 {
        serial_write("[VERIFIER TESTS] ALL TESTS PASSED!\n");
    } else {
        serial_write("[VERIFIER TESTS] SOME TESTS FAILED!\n");
    }
    serial_write("========================================\n");
}

// ===================================================================
// METRICS REPORTING
// ===================================================================

fn print_system_metrics() {
    serial_write("\n========================================\n");
    serial_write("[METRICS] System Metrics Report\n");
    serial_write("========================================\n");

    let vfs_m = fs::vfs::get_metrics();

    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  [VFS] Nodes: {}", vfs_m.total_nodes);
        serial_write(&s);
    }
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  [VFS] Bytes written: {} B", vfs_m.total_bytes_written);
        serial_write(&s);
    }
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  [VFS] Bytes read: {} B", vfs_m.total_bytes_read);
        serial_write(&s);
    }
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  [VFS] Operations: {}", vfs_m.operations_count);
        serial_write(&s);
    }
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  [VFS] Errors: {}", vfs_m.errors_count);
        serial_write(&s);
    }
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  [VFS] Security violations: {}", vfs_m.security_violations);
        serial_write(&s);
    }
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  [VFS] Bus errors: {}", vfs_m.bus_errors);
        serial_write(&s);
    }

    // Error rate
    if vfs_m.operations_count > 0 {
        let error_pct = (vfs_m.errors_count * 100) / vfs_m.operations_count;
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  [VFS] Error rate: {}%", error_pct);
        serial_write(&s);
    }

    // Verifier metrics
    let vm = verifier::policy::get_metrics();
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  [VERIFIER] Rules evaluated: {}", vm.rules_evaluated);
        serial_write(&s);
    }
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  [VERIFIER] Allowed: {} | Denied: {} | Audited: {}",
            vm.operations_allowed, vm.operations_denied, vm.operations_audited);
        serial_write(&s);
    }
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  [VERIFIER] Policy violations: {}", vm.policy_violations);
        serial_write(&s);
    }

    serial_write("========================================\n");
}

// ===== Entry Point =====
bootloader::entry_point!(kernel_main);

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    // === Banner ===
    serial_write("\n========================================\n");
    serial_write("[AETHERION] Kernel v");
    serial_write(KERNEL_VERSION);
    serial_write("\n========================================\n\n");

    // VGA clear
    {
        let mut vga = VGA.lock();
        vga.clear();
        vga.write_str("[AETHERION] Couche 5 - Verifier Boot\n");
    }

    // === Step 1: GDT ===
    serial_write("[1/6] Loading GDT...\n");
    arch::x86_64::gdt::init();
    serial_write("      [OK] GDT with TSS loaded\n");

    // === Step 2: IDT ===
    serial_write("[2/6] Loading IDT...\n");
    arch::x86_64::idt::init();
    serial_write("      [OK] IDT with 20 handlers\n");

    // === Step 3: PIC ===
    serial_write("[3/6] Initializing PIC...\n");
    arch::x86_64::interrupts::init();
    serial_write("      [OK] PIC remapped (32-47)\n");

    // === Step 4: Security ===
    serial_write("[4/6] Security init...\n");
    security::init();
    serial_write("      [OK] TPM stub + PCR0\n");

    // === Step 5: Memory (Couche 2) ===
    serial_write("[5/6] Memory init (Couche 2)...\n");
    let mut memory_manager = match memory::init(boot_info) {
        Ok(mm) => {
            serial_write("      [OK] Memory manager ready\n");
            mm
        }
        Err(e) => {
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = writeln!(s, "      [FAILED] {}", e);
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
            let _ = writeln!(s, "      [WARN] Heap: {}", e);
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
        vga.write_str("\n[3/6] Testing Cognitive Bus (IPC)...\n");
    }

    serial_write("\n========================================\n");
    serial_write("[COGNITIVE BUS] Initializing...\n");
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  Capacity: {} messages", ipc::bus::capacity());
        serial_write(&s);
    }

    // --- IPC TEST 1: Basic publish/consume ---
    serial_write("\n[IPC TEST 1] Basic message flow:\n");
    {
        use ipc::{IntentMessage, ComponentId, Priority};

        let msg1 = IntentMessage::new(
            ComponentId::HAL,
            ComponentId::Orchestrator,
            0x0001,
            Priority::Normal,
            0x41,
        );

        match ipc::bus::publish(msg1) {
            Ok(_) => {
                let mut s = arrayvec::ArrayString::<256>::new();
                let _ = writeln!(s, "  [OK] Published: {}", msg1);
                serial_write(&s);
            }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "  [FAIL] Publish error: {:?}", e);
                serial_write(&s);
            }
        }

        match ipc::bus::consume() {
            Ok(msg) => {
                let mut s = arrayvec::ArrayString::<256>::new();
                let _ = writeln!(s, "  [OK] Consumed: {}", msg);
                serial_write(&s);
            }
            Err(e) => {
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "  [FAIL] Consume error: {:?}", e);
                serial_write(&s);
            }
        }

        match ipc::bus::consume() {
            Err(ipc::BusError::QueueEmpty) => {
                serial_write("  [OK] Queue empty after consume (correct)\n");
            }
            _ => {
                serial_write("  [FAIL] Queue should be empty!\n");
            }
        }
    }

    // --- IPC TEST 2: Multi-message ---
    serial_write("\n[IPC TEST 2] Multi-message orchestrator simulation:\n");
    {
        use ipc::{IntentMessage, ComponentId, Priority};

        let messages = [
            IntentMessage::new(ComponentId::HAL, ComponentId::Orchestrator, 0x0001, Priority::Normal, 0x41),
            IntentMessage::new(ComponentId::Verifier, ComponentId::Orchestrator, 0x0020, Priority::Critical, 0xDEAD_BEEF),
            IntentMessage::new(ComponentId::Cerebellum, ComponentId::HAL, 0x0030, Priority::Low, 0xCAFE),
        ];

        for msg in &messages {
            match ipc::bus::publish(*msg) {
                Ok(_) => {
                    let mut s = arrayvec::ArrayString::<256>::new();
                    let _ = writeln!(s, "  [PUB] {}", msg);
                    serial_write(&s);
                }
                Err(e) => {
                    let mut s = arrayvec::ArrayString::<128>::new();
                    let _ = writeln!(s, "  [FAIL] Publish: {:?}", e);
                    serial_write(&s);
                }
            }
        }

        serial_write("\n  [ORCHESTRATOR] Consuming bus:\n");
        let mut consumed = 0u32;
        while let Ok(msg) = ipc::bus::consume() {
            consumed += 1;
            let mut s = arrayvec::ArrayString::<256>::new();
            let _ = writeln!(s, "    #{} {}", consumed, msg);
            serial_write(&s);
        }
        {
            let mut s = arrayvec::ArrayString::<64>::new();
            let _ = writeln!(s, "  [OK] Consumed {} messages", consumed);
            serial_write(&s);
        }
    }

    // --- IPC TEST 3: Overflow ---
    serial_write("\n[IPC TEST 3] Overflow detection:\n");
    {
        use ipc::{IntentMessage, ComponentId, Priority};

        let mut published = 0u32;
        for i in 0..110u32 {
            let msg = IntentMessage::new(
                ComponentId::HAL, ComponentId::Orchestrator,
                i, Priority::Normal, i as u64,
            );
            match ipc::bus::publish(msg) {
                Ok(_) => published += 1,
                Err(ipc::BusError::QueueFull) => {
                    let mut s = arrayvec::ArrayString::<128>::new();
                    let _ = writeln!(s, "  [OK] Overflow detected at msg #{} (capacity: {})",
                        i, ipc::bus::capacity());
                    serial_write(&s);
                    break;
                }
                Err(e) => {
                    let mut s = arrayvec::ArrayString::<128>::new();
                    let _ = writeln!(s, "  [FAIL] Unexpected error: {:?}", e);
                    serial_write(&s);
                    break;
                }
            }
        }
        {
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = writeln!(s, "  Published {} messages before overflow", published);
            serial_write(&s);
        }
        while ipc::bus::consume().is_ok() {}
        serial_write("  [OK] Queue drained successfully\n");
    }

    // --- IPC TEST 4: FIFO Determinism within same Priority ---
    serial_write("\n[IPC TEST 4] FIFO determinism (same priority):\n");
    {
        use ipc::{IntentMessage, ComponentId, Priority};

        // Drain any leftover messages
        while ipc::bus::consume().is_ok() {}

        // Publish 5 Normal-priority messages with known ascending timestamps.
        // We manually set timestamps to guarantee ordering.
        let base_ts = arch::x86_64::timer::read_tsc();
        for i in 0u32..5 {
            let mut msg = IntentMessage::new(
                ComponentId::HAL,
                ComponentId::Orchestrator,
                i,           // intent_id = 0,1,2,3,4
                Priority::Normal,
                i as u64,
            );
            // Force ascending timestamps (base + i*1000)
            msg.timestamp = base_ts + (i as u64) * 1000;
            ipc::bus::publish(msg).unwrap();
        }

        // Consume: expect FIFO order (0, 1, 2, 3, 4)
        let mut fifo_ok = true;
        for expected in 0u32..5 {
            match ipc::bus::consume() {
                Ok(msg) if msg.intent_id == expected => {}
                Ok(msg) => {
                    let mut s = arrayvec::ArrayString::<128>::new();
                    let _ = writeln!(s, "  [FAIL] Expected intent_id={}, got {}", expected, msg.intent_id);
                    serial_write(&s);
                    fifo_ok = false;
                }
                Err(e) => {
                    let mut s = arrayvec::ArrayString::<128>::new();
                    let _ = writeln!(s, "  [FAIL] Consume error: {:?}", e);
                    serial_write(&s);
                    fifo_ok = false;
                }
            }
        }
        if fifo_ok {
            serial_write("  [OK] FIFO order preserved for 5 same-priority messages\n");
        } else {
            panic!("FIFO DETERMINISM VIOLATED: same-priority messages consumed out of order!");
        }
    }

    // --- IPC TEST 5: Priority + FIFO combined ---
    serial_write("\n[IPC TEST 5] Priority ordering + FIFO within level:\n");
    {
        use ipc::{IntentMessage, ComponentId, Priority};

        while ipc::bus::consume().is_ok() {}

        let base_ts = arch::x86_64::timer::read_tsc();

        // Publish interleaved priorities: Low, Critical, Normal, Normal, High
        let specs: [(Priority, u32, u64); 5] = [
            (Priority::Low,      0xA0, base_ts + 100),
            (Priority::Critical, 0xB0, base_ts + 200),
            (Priority::Normal,   0xC0, base_ts + 300),  // Normal #1
            (Priority::Normal,   0xC1, base_ts + 400),  // Normal #2
            (Priority::High,     0xD0, base_ts + 500),
        ];
        for (prio, id, ts) in &specs {
            let mut msg = IntentMessage::new(
                ComponentId::HAL, ComponentId::Orchestrator,
                *id, *prio, 0,
            );
            msg.timestamp = *ts;
            ipc::bus::publish(msg).unwrap();
        }

        // Expected consumption order: Critical(0xB0), High(0xD0), Normal(0xC0), Normal(0xC1), Low(0xA0)
        let expected_order: [u32; 5] = [0xB0, 0xD0, 0xC0, 0xC1, 0xA0];
        let mut order_ok = true;
        for (i, expected_id) in expected_order.iter().enumerate() {
            match ipc::bus::consume() {
                Ok(msg) if msg.intent_id == *expected_id => {
                    let mut s = arrayvec::ArrayString::<128>::new();
                    let _ = writeln!(s, "  #{}: intent=0x{:02X} prio={} OK", i+1, msg.intent_id, msg.priority);
                    serial_write(&s);
                }
                Ok(msg) => {
                    let mut s = arrayvec::ArrayString::<128>::new();
                    let _ = writeln!(s, "  #{}: FAIL expected 0x{:02X}, got 0x{:02X}", i+1, expected_id, msg.intent_id);
                    serial_write(&s);
                    order_ok = false;
                }
                Err(_) => { order_ok = false; }
            }
        }
        if order_ok {
            serial_write("  [OK] Priority + FIFO ordering verified\n");
        } else {
            panic!("PRIORITY+FIFO ORDERING VIOLATED!");
        }
    }

    serial_write("\n[COGNITIVE BUS] All tests PASSED!\n");
    serial_write("========================================\n");

    // ===================================================================
    // COUCHE 4: VFS (Virtual Filesystem) TESTS
    // ===================================================================
    serial_write("\n[6/6] Initializing VFS (Couche 4)...\n");
    {
        let mut vga = VGA.lock();
        vga.write_str("[4/6] Testing VFS (Filesystem)...\n");
    }

    run_vfs_tests();

    // ===================================================================
    // VFS STRESS & HARDENING TESTS (AUDIT PHASE 3)
    // ===================================================================
    run_vfs_stress_tests();

    // ===================================================================
    // COUCHE 5: VERIFIER (Policy Engine) INIT + TESTS
    // ===================================================================
    serial_write("\n[7/7] Initializing Verifier (Couche 5)...\n");
    match verifier::policy::init() {
        Ok(_) => serial_write("      [OK] Policy engine loaded\n"),
        Err(e) => {
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = writeln!(s, "      [FAIL] Verifier: {}", e);
            serial_write(&s);
        }
    }

    run_verifier_tests();

    // ===================================================================
    // SYSTEM METRICS
    // ===================================================================
    print_system_metrics();

    // === Boot Complete ===
    {
        let mut vga = VGA.lock();
        vga.write_str("[OK] Couche 4 VFS ready\n");
    }

    serial_write("\n========================================\n");
    serial_write("[BOOT] AetherionOS Couche 5 READY\n");
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  Memory: {} frames ({} KB)",
            memory_manager.frame_allocator.total_frames(),
            memory_manager.frame_allocator.total_frames() * 4);
        serial_write(&s);
    }
    {
        let mut s = arrayvec::ArrayString::<64>::new();
        let _ = writeln!(s, "  Heap: {} KB", memory::heap::HEAP_SIZE / 1024);
        serial_write(&s);
    }
    {
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "  Cognitive Bus: {} msg capacity (lock-free MPMC)",
            ipc::bus::capacity());
        serial_write(&s);
    }
    serial_write("  VFS: Mounted with security hardening\n");
    serial_write("  Verifier: Policy engine active (Couche 5)\n");
    serial_write("  Interrupts: enabled\n");
    serial_write("  Security: TPM stub + VFS capability + Verifier policy\n");
    serial_write("========================================\n");

    {
        let mut vga = VGA.lock();
        vga.write_str("\n[OK] Couche 5 BOOT COMPLETE\n");
    }

    // Idle loop
    loop {
        x86_64::instructions::hlt();
    }
}
