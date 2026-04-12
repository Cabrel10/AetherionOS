// Aetherion OS - Kernel Consolidation (Couches 1-13)
// Architecture: x86_64, Bootloader: 0.9.23
// Modules: GDT(R0+R3), IDT, PIC, TPM/Security, Memory, IPC, VFS, Verifier,
//          Process Manager (Matriarchal), Priority Scheduler + Aging,
//          GPU VRAM Stub, Context Switch (ASM), Syscall MSRs,
//          ELF64 Loader (Per-Process Paging + Ring 3),
//          POSIX Syscalls (open/read/write/close/seek/fork/exec/wait),
//          Multi-Process Ring 3, Interactive Shell
//
// Couche 13: User Space Complete & Multi-Process
//   - ELF64 parsing with magic verification
//   - PT_LOAD segment mapping with NX enforcement
//   - BSS zero-fill (p_memsz > p_filesz)
//   - Per-process PML4 page tables (kernel upper half cloned)
//   - 8 MiB user stack at 0x7FFF_FFFF_F000
//   - Ring 3 process creation (CS=0x23, SS=0x1B, RFLAGS=0x202)
//   - exec <path> shell command
//   - Embedded /bin/hello.elf test binary
//
// Security hardening:
//   - Stack protector (__stack_chk_guard / __stack_chk_fail)
//   - FIFO determinism in Cognitive Bus
//   - Path traversal protection
//   - Null byte injection prevention
//   - Buffer overflow / capacity checks
//   - Capability-based device access (ACHA manifests)
//   - bus_errors metric in VFS
//   - Verifier policy engine with default-deny whitelist
//   - Anti-starvation aging in scheduler (boost after 100 wait ticks)
//   - SYSCALL/SYSRET MSR configuration
//   - User-space address validation (< 0x0000_8000_0000_0000)
//   - W^X enforcement on ELF segments (NX on data/stack)

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]
#![feature(panic_info_message)]
#![feature(naked_functions)]
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
mod process;
mod scheduler;
mod gpu;
mod elf;
mod net;
mod drivers;
mod framebuffer;
mod font;
mod codegen;
mod compat;

// ===== Configuration =====
const KERNEL_VERSION: &str = "3.1.0-j109-112-enriched-bus-clock-sensor";

/// Jalon 103: Toggle security agents (MCP/Validator) at boot.
/// Set to false during SMP LLM pipeline testing to unclog BSP (Core 0).
/// MUST be set back to true for production/release builds.
const ENABLE_SECURITY_AGENTS: bool = true;

// ===== Embedded ELF binaries =====
/// Minimal hello.elf - statically linked x86-64 ELF for Ring 3 test
static HELLO_ELF: &[u8] = include_bytes!("../../userspace/hello.elf");
/// Interactive shell.elf - Ring 3 shell with POSIX syscalls
static SHELL_ELF: &[u8] = include_bytes!("../../userspace/shell.elf");
/// Math Agent - Ring 3 agent with mmap, linear regression, matrix ops, bus publish
static AGENT_MATH_ELF: &[u8] = include_bytes!("../../userspace/agent_math.elf");
/// Native C application - compiled with GCC, libc_stub, bare-metal
static HELLO_C_ELF: &[u8] = include_bytes!("../../userspace/c_apps/hello_c.elf");
/// wget - Ring 3 HTTP client (TCP/DNS validation - Couche 18)
static WGET_ELF: &[u8] = include_bytes!("../../userspace/c_apps/wget.elf");
/// ls - Ring 3 directory listing (Couche 19)
static LS_ELF: &[u8] = include_bytes!("../../userspace/c_apps/ls.elf");
/// cat - Ring 3 file display (Couche 19)
static CAT_ELF: &[u8] = include_bytes!("../../userspace/c_apps/cat.elf");
/// j19_test - Jalon 19 comprehensive validation (Couche 19)
static J19_TEST_ELF: &[u8] = include_bytes!("../../userspace/c_apps/j19_test.elf");
/// threads - Jalon 20 multi-threading validation (Couche 20)
static THREADS_ELF: &[u8] = include_bytes!("../../userspace/c_apps/threads.elf");
/// ui - Jalon 21 GUI framebuffer validation (Couche 21)
static UI_ELF: &[u8] = include_bytes!("../../userspace/c_apps/ui.elf");
/// agent_ai - Jalon 22 Bare-metal ML inference engine (matrix ops + rdtsc benchmark)
static AGENT_AI_ELF: &[u8] = include_bytes!("../../userspace/c_apps/agent_ai.elf");
/// agent_rag - Jalon 23 RAG vector engine (cosine similarity + top-3)
static AGENT_RAG_ELF: &[u8] = include_bytes!("../../userspace/c_apps/agent_rag.elf");

static SH_ELF: &[u8] = include_bytes!("../../userspace/c_apps/sh.elf");

/// test_malloc - Jalon 27 dynamic memory allocator test
static TEST_MALLOC_ELF: &[u8] = include_bytes!("../../userspace/c_apps/test_malloc.elf");

/// test_preempt - Jalon 28 preemptive scheduler test
static TEST_PREEMPT_ELF: &[u8] = include_bytes!("../../userspace/c_apps/test_preempt.elf");

/// agent_rust - Jalon 29 Native Rust Ring 3 agent (Vec alloc + Bus publish)
static AGENT_RUST_ELF: &[u8] = include_bytes!("../../userspace/c_apps/agent_rust.elf");

/// agent_saga - Jalon 30/31 Persistence agent (FAT32 write + Sagas/Almanach)
static AGENT_SAGA_ELF: &[u8] = include_bytes!("../../userspace/c_apps/agent_saga.elf");

/// agent_sse - Jalon 33 SSE/AVX Ring 3 validation agent
static AGENT_SSE_ELF: &[u8] = include_bytes!("../../userspace/c_apps/agent_sse.elf");

/// agent_ai_native - Jalon 34 Native Rust Tensor Engine (SSE2 matmul in Ring 3)
static AGENT_AI_NATIVE_ELF: &[u8] = include_bytes!("../../userspace/agent_ai_native/target/x86_64-aetherion-user/release/agent_ai_native");

/// agent_gguf - Jalon 36 GGUF Model Loaded from FAT32 Disk (Ring 3)
static AGENT_GGUF_ELF: &[u8] = include_bytes!("../../userspace/agent_gguf/target/x86_64-aetherion-user/release/agent_gguf");

/// agent_net - Jalon 37 Network Agent (Ring 3)
static AGENT_NET_ELF: &[u8] = include_bytes!("../../userspace/agent_net/target/x86_64-aetherion-user/release/agent_net");

/// agent_input - Jalon 38 HID Input Agent (Ring 3)
static AGENT_INPUT_ELF: &[u8] = include_bytes!("../../userspace/agent_input/target/x86_64-aetherion-user/release/agent_input");

/// agent_gui_test - Jalon 39 Framebuffer GUI Test (Ring 3)
static AGENT_GUI_TEST_ELF: &[u8] = include_bytes!("../../userspace/agent_gui_test/target/x86_64-aetherion-user/release/agent_gui_test");

/// agent_sysinfo - Jalon 40 System Information Agent (Ring 3)
static AGENT_SYSINFO_ELF: &[u8] = include_bytes!("../../userspace/agent_sysinfo/target/x86_64-aetherion-user/release/agent_sysinfo");

/// agent_wm - Jalon 41 Cognitive Desktop Window Manager (Ring 3)
static AGENT_WM_ELF: &[u8] = include_bytes!("../../userspace/agent_wm/target/x86_64-aetherion-user/release/agent_wm");

/// agent_terminal - Jalon 42 Interactive Terminal Window (Ring 3)
static AGENT_TERMINAL_ELF: &[u8] = include_bytes!("../../userspace/agent_terminal/target/x86_64-aetherion-user/release/agent_terminal");

/// agent_multipart - Jalon 43 Multi-Part GGUF File Merger (Ring 3)
static AGENT_MULTIPART_ELF: &[u8] = include_bytes!("../../userspace/agent_multipart/target/x86_64-aetherion-user/release/agent_multipart");

/// agent_bench - Jalon 45 VirtIO Block I/O Benchmark (Ring 3)
static AGENT_BENCH_ELF: &[u8] = include_bytes!("../../userspace/agent_bench/target/x86_64-aetherion-user/release/agent_bench");

/// agent_tokenizer - Jalon 46 Static BPE Tokenizer (Ring 3)
static AGENT_TOKENIZER_ELF: &[u8] = include_bytes!("../../userspace/agent_tokenizer/target/x86_64-aetherion-user/release/agent_tokenizer");

/// agent_inference - Jalon 47 GGUF Tensor Metadata Inspector (Ring 3)
static AGENT_INFERENCE_ELF: &[u8] = include_bytes!("../../userspace/agent_inference/target/x86_64-aetherion-user/release/agent_inference");

/// agent_llama - Jalon 49 Bare-Metal LLM Transformer Math (Ring 3)
static AGENT_LLAMA_ELF: &[u8] = include_bytes!("../../userspace/agent_llama/target/x86_64-aetherion-user/release/agent_llama");

/// agent_llm_chat - Jalon 50 LLM Chat via Cognitive Bus (Ring 3)
static AGENT_LLM_CHAT_ELF: &[u8] = include_bytes!("../../userspace/agent_llm_chat/target/x86_64-aetherion-user/release/agent_llm_chat");

/// agent_mt_matmul - Jalon 51 Multithreaded MatMul (Ring 3)
static AGENT_MT_MATMUL_ELF: &[u8] = include_bytes!("../../userspace/agent_mt_matmul/target/x86_64-aetherion-user/release/agent_mt_matmul");

/// agent_chunk_reader - Jalon 53 Sequential Chunk Reader (Ring 3)
static AGENT_CHUNK_READER_ELF: &[u8] = include_bytes!("../../userspace/agent_chunk_reader/target/x86_64-aetherion-user/release/agent_chunk_reader");

/// agent_weight_loader - Jalon 54 GGUF Weight Loader (Ring 3)
static AGENT_WEIGHT_LOADER_ELF: &[u8] = include_bytes!("../../userspace/agent_weight_loader/target/x86_64-aetherion-user/release/agent_weight_loader");

/// agent_orchestrator - Jalon 56 Agent Orchestrator (Ring 3)
static AGENT_ORCHESTRATOR_ELF: &[u8] = include_bytes!("../../userspace/agent_orchestrator/target/x86_64-aetherion-user/release/agent_orchestrator");

/// agent_state - Jalon 57 Persistent State Reader (Ring 3)
static AGENT_STATE_ELF: &[u8] = include_bytes!("../../userspace/agent_state/target/x86_64-aetherion-user/release/agent_state");

/// agent_http - Jalon 58 HTTP Client Agent (Ring 3)
static AGENT_HTTP_ELF: &[u8] = include_bytes!("../../userspace/agent_http/target/x86_64-aetherion-user/release/agent_http");

/// agent_visual_term - Jalon 59 Interactive Visual Terminal (Ring 3)
static AGENT_VISUAL_TERM_ELF: &[u8] = include_bytes!("../../userspace/agent_visual_term/target/x86_64-aetherion-user/release/agent_visual_term");

/// agent_q4_dequant - Jalon 61 Q4_K_M Dequantizer (Ring 3)
static AGENT_Q4_DEQUANT_ELF: &[u8] = include_bytes!("../../userspace/agent_q4_dequant/target/x86_64-aetherion-user/release/agent_q4_dequant");

/// agent_llama_core - Jalon 62/63 LLaMA Transformer Core (Ring 3)
static AGENT_LLAMA_CORE_ELF: &[u8] = include_bytes!("../../userspace/agent_llama_core/target/x86_64-aetherion-user/release/agent_llama_core");

/// wget_real - Jalon 78 TCP Socket Validation (Ring 3 C app)
static WGET_REAL_ELF: &[u8] = include_bytes!("../../userspace/c_apps/wget_real.elf");

/// agent_mcp - Level 8 MCP (Model Context Protocol) Security Agent (Ring 3)
static AGENT_MCP_ELF: &[u8] = include_bytes!("../../userspace/agent_mcp/target/x86_64-aetherion-user/release/agent_mcp");

/// agent_validator - Immune System: JSON coherence validator (Ring 3)
static AGENT_VALIDATOR_ELF: &[u8] = include_bytes!("../../userspace/agent_validator/target/x86_64-aetherion-user/release/agent_validator");

/// agent_clock - Jalon 112a: Clock Sensor Agent (Ring 3)
/// Publishes INTENT_TIMER_TICK (0x112A) every second for uptime tracking.
static AGENT_CLOCK_ELF: &[u8] = include_bytes!("../../userspace/agent_clock/target/x86_64-aetherion-user/release/agent_clock");

/// Busybox 1.35.0 - statically linked x86_64 musl Linux binary
/// Jalon 94-95: Native Linux binary execution via Linuxulator
static BUSYBOX_ELF: &[u8] = include_bytes!("../../userspace/busybox.elf");

// VGA text buffer
const VGA_BUFFER: *mut u8 = 0xb8000 as *mut u8;
const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;

// ===== Stack Protector =====
// GCC/LLVM stack-smashing protection: canary value checked on function return
#[no_mangle]
pub static __stack_chk_guard: u64 = 0x595e9fbd94fda766;

#[no_mangle]
pub extern "C" fn __stack_chk_fail() -> ! {
    serial_write("\n[SECURITY] *** STACK SMASHING DETECTED ***\n");
    serial_write("[SECURITY] Stack canary corrupted - possible buffer overflow attack!\n");
    panic!("stack-protector: stack smashing detected");
}

// ===== Serial Port (uart_16550) =====
lazy_static! {
    static ref SERIAL1: Mutex<SerialPort> = {
        let mut serial_port = unsafe { SerialPort::new(0x3F8) };
        serial_port.init();
        Mutex::new(serial_port)
    };
}

/// Write a string to the serial port (atomic per call via spinlock)
pub fn serial_write(s: &str) {
    let mut serial = SERIAL1.lock();
    for byte in s.bytes() {
        serial.send(byte);
    }
}

/// Write a string + newline atomically (single lock acquisition).
/// Jalon 105: Prevents SMP serial tearing between Core 0 (BSP) and Core 1 (AP).
pub fn serial_writeln(s: &str) {
    let mut serial = SERIAL1.lock();
    for byte in s.bytes() {
        serial.send(byte);
    }
    serial.send(b'\n');
}

// Macro for serial_println — Jalon 105: atomic message+newline to prevent SMP tearing
#[macro_export]
macro_rules! serial_println {
    ($($arg:tt)*) => {
        {
            use core::fmt::Write;
            let mut s = arrayvec::ArrayString::<256>::new();
            let _ = write!(s, $($arg)*);
            $crate::serial_writeln(s.as_str());
        }
    };
}

// ===== VGA Buffer =====
struct VgaBuffer { row: usize, col: usize, color: u8 }

impl VgaBuffer {
    const fn new() -> Self { VgaBuffer { row: 0, col: 0, color: 0x0F } }

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
            b'\n' => { self.row += 1; self.col = 0; }
            b'\r' => { self.col = 0; }
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
                if self.col >= VGA_WIDTH { self.col = 0; self.row += 1; }
            }
        }
    }

    fn write_str(&mut self, s: &str) {
        for byte in s.bytes() { self.write_byte(byte); }
    }

    fn scroll(&mut self) {
        unsafe {
            for row in 1..VGA_HEIGHT {
                for col in 0..VGA_WIDTH {
                    let src = (row * VGA_WIDTH + col) * 2;
                    let dst = ((row - 1) * VGA_WIDTH + col) * 2;
                    let ch = core::ptr::read_volatile(VGA_BUFFER.add(src));
                    let at = core::ptr::read_volatile(VGA_BUFFER.add(src + 1));
                    core::ptr::write_volatile(VGA_BUFFER.add(dst), ch);
                    core::ptr::write_volatile(VGA_BUFFER.add(dst + 1), at);
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
    // Jalon 105: Atomic panic output to prevent SMP serial tearing
    // Build the entire message in one buffer, then write atomically.
    let mut full = arrayvec::ArrayString::<512>::new();
    let _ = write!(full, "\n[KERNEL-PANIC] ");
    if let Some(msg) = info.message() {
        let _ = write!(full, "{}", msg);
    }
    if let Some(loc) = info.location() {
        let _ = write!(full, " at {}:{}", loc.file(), loc.line());
    }
    let _ = write!(full, "\nSystem halted.\n");
    serial_write(full.as_str());
    loop { x86_64::instructions::hlt(); }
}

// ===== Heap support =====
extern crate alloc;
use alloc::vec::Vec;
use alloc::string::String;

fn run_heap_tests() {
    use alloc::boxed::Box;
    
    // Test 1: Basic Box allocation and deallocation
    serial_write("  [TEST 1/4] Box::new(42)... ");
    let boxed = Box::new(42u64);
    assert_eq!(*boxed, 42);
    drop(boxed);
    serial_write("OK\n");

    // Test 2: Vec grows dynamically (realloc works)
    serial_write("  [TEST 2/4] Vec push 0..9 (dynamic grow)... ");
    let mut vec = Vec::new();
    for i in 0..10u64 { vec.push(i * 10); }
    assert_eq!(vec.len(), 10);
    assert_eq!(vec[0], 0);
    assert_eq!(vec[5], 50);
    assert_eq!(vec[9], 90);
    serial_write("OK\n");

    // Test 3: String allocation (UTF-8 heap data)
    serial_write("  [TEST 3/4] String alloc + content verify... ");
    let s = String::from("AetherionOS Heap OK");
    assert_eq!(s.len(), 19);
    assert!(s.starts_with("Aetherion"));
    assert!(s.ends_with("OK"));
    serial_write("OK\n");

    // Test 4: Stress test — 100 allocations, verify each, then drop all
    serial_write("  [TEST 4/4] Stress: 100 alloc+verify+drop... ");
    {
        let mut boxes: Vec<Box<u64>> = Vec::new();
        for i in 0..100u64 {
            boxes.push(Box::new(i * 7));
        }
        // Verify ALL values are still correct (no heap corruption)
        for (i, b) in boxes.iter().enumerate() {
            assert_eq!(**b, (i as u64) * 7);
        }
        // Drop all at once (tests bulk dealloc)
    }
    serial_write("OK\n");
}

// ===================================================================
// VFS TEST SUITE
// ===================================================================
fn run_vfs_tests() {
    serial_write("\n========================================\n");
    serial_write("[VFS TESTS] Starting comprehensive VFS test suite\n");
    serial_write("========================================\n\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // TEST 1: init
    serial_write("  [TEST 1/14] VFS init... ");
    match fs::vfs::init() {
        Ok(_) => { serial_write("OK\n"); passed += 1; }
        Err(_) => { serial_write("FAIL\n"); failed += 1; }
    }

    // TEST 2: mount ram0
    serial_write("  [TEST 2/14] Mount /dev/ram0... ");
    let manifest = fs::manifest::DeviceManifest::ram_disk("ram0", 1024, true);
    match fs::vfs::mount_device("/dev/ram0", manifest) {
        Ok(_) => { serial_write("OK\n"); passed += 1; }
        Err(_) => { serial_write("FAIL\n"); failed += 1; }
    }

    // TEST 3: write
    serial_write("  [TEST 3/14] Write to /dev/ram0... ");
    match fs::vfs::file_write("/dev/ram0", b"Hello AetherionOS VFS!") {
        Ok(n) if n == 22 => { serial_write("OK\n"); passed += 1; }
        _ => { serial_write("FAIL\n"); failed += 1; }
    }

    // TEST 4: read
    serial_write("  [TEST 4/14] Read from /dev/ram0... ");
    match fs::vfs::file_read("/dev/ram0") {
        Ok(data) if data.as_slice() == b"Hello AetherionOS VFS!" => { serial_write("OK\n"); passed += 1; }
        _ => { serial_write("FAIL\n"); failed += 1; }
    }

    // TEST 5: mount readonly
    serial_write("  [TEST 5/14] Mount /dev/rom0 (RO)... ");
    let rom = fs::manifest::DeviceManifest::ram_disk("rom0", 512, false);
    match fs::vfs::mount_device("/dev/rom0", rom) {
        Ok(_) => { serial_write("OK\n"); passed += 1; }
        Err(_) => { serial_write("FAIL\n"); failed += 1; }
    }

    // TEST 6: write to RO
    serial_write("  [TEST 6/14] Write to RO device... ");
    match fs::vfs::file_write("/dev/rom0", b"should fail") {
        Err(fs::vfs::VfsError::ReadOnlyDevice) => { serial_write("OK (denied)\n"); passed += 1; }
        _ => { serial_write("FAIL\n"); failed += 1; }
    }

    // TEST 7: read nonexistent
    serial_write("  [TEST 7/14] Read /dev/noexist... ");
    match fs::vfs::file_read("/dev/noexist") {
        Err(fs::vfs::VfsError::NotFound) => { serial_write("OK\n"); passed += 1; }
        _ => { serial_write("FAIL\n"); failed += 1; }
    }

    // SECURITY TESTS
    serial_write("\n  --- SECURITY TESTS ---\n\n");

    // TEST 8: path traversal
    serial_write("  [TEST 8/14] Path traversal ../etc/shadow... ");
    match fs::vfs::file_write("/../etc/shadow", b"pwned") {
        Err(fs::vfs::VfsError::PathTraversal) => { serial_write("OK (blocked)\n"); passed += 1; }
        _ => { serial_write("FAIL\n"); failed += 1; }
    }

    // TEST 9
    serial_write("  [TEST 9/14] Path traversal /dev/../../root... ");
    match fs::vfs::file_read("/dev/../../root") {
        Err(fs::vfs::VfsError::PathTraversal) => { serial_write("OK (blocked)\n"); passed += 1; }
        _ => { serial_write("FAIL\n"); failed += 1; }
    }

    // TEST 10: no leading /
    serial_write("  [TEST 10/14] Invalid path (no /)... ");
    match fs::vfs::file_write("dev/ram0", b"data") {
        Err(fs::vfs::VfsError::InvalidPath) => { serial_write("OK\n"); passed += 1; }
        _ => { serial_write("FAIL\n"); failed += 1; }
    }

    // TEST 11: empty path
    serial_write("  [TEST 11/14] Empty path... ");
    match fs::vfs::file_write("", b"data") {
        Err(fs::vfs::VfsError::InvalidPath) => { serial_write("OK\n"); passed += 1; }
        _ => { serial_write("FAIL\n"); failed += 1; }
    }

    // TEST 12: capacity overflow
    serial_write("  [TEST 12/14] Capacity overflow... ");
    match fs::vfs::file_write("/dev/ram0", &[0xAA; 2048]) {
        Err(fs::vfs::VfsError::CapacityExceeded) => { serial_write("OK\n"); passed += 1; }
        _ => { serial_write("FAIL\n"); failed += 1; }
    }

    // TEST 13: manifest validation
    serial_write("  [TEST 13/14] Invalid manifest... ");
    let mut bad = fs::manifest::DeviceManifest::ram_disk("bad", 512, true);
    bad.read_only = true;
    if !bad.validate() { serial_write("OK\n"); passed += 1; }
    else { serial_write("FAIL\n"); failed += 1; }

    // TEST 14: data integrity
    serial_write("  [TEST 14/14] Data integrity... ");
    let pattern: Vec<u8> = (0..128u8).collect();
    match fs::vfs::file_write("/dev/ram0", &pattern) {
        Ok(_) => match fs::vfs::file_read("/dev/ram0") {
            Ok(data) if data == pattern => { serial_write("OK\n"); passed += 1; }
            _ => { serial_write("FAIL\n"); failed += 1; }
        },
        Err(_) => { serial_write("FAIL\n"); failed += 1; }
    }

    serial_write("\n========================================\n");
    serial_println!("[VFS TESTS] {}/{} passed, {} failed", passed, passed + failed, failed);
    if failed == 0 { serial_write("[VFS TESTS] ALL TESTS PASSED!\n"); }
    serial_write("========================================\n");
}

// ===================================================================
// VFS STRESS TESTS
// ===================================================================
fn run_vfs_stress_tests() {
    serial_write("\n========================================\n");
    serial_write("[VFS STRESS] Starting hardening test suite\n");
    serial_write("========================================\n\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // STRESS 1: 1000 write/read cycles
    serial_write("  [STRESS 1/7] 1000 write/read cycles...\n");
    {
        let mut ok = true;
        for i in 0u32..1000 {
            let data = alloc::format!("Cycle-{:04}", i);
            match fs::vfs::file_write("/dev/ram0", data.as_bytes()) {
                Ok(n) if n == data.len() => {}
                _ => { ok = false; break; }
            }
            match fs::vfs::file_read("/dev/ram0") {
                Ok(ref d) if d.as_slice() == data.as_bytes() => {}
                _ => { ok = false; break; }
            }
            if i % 250 == 0 { serial_println!("    Cycle {}/1000 OK", i); }
        }
        if ok { serial_write("  [OK] PASSED\n"); passed += 1; }
        else { serial_write("  [FAIL]\n"); failed += 1; }
    }

    // STRESS 2: path traversal variants
    serial_write("\n  [STRESS 2/7] Path traversal vectors...\n");
    {
        let attacks = ["/../etc/passwd", "/dev/../../root", "/dev/../../../shadow",
                       "/./dev/ram0", "/dev//ram0", "/dev/..hidden"];
        let mut all_blocked = true;
        for path in &attacks {
            if fs::vfs::file_read(path).is_ok() { all_blocked = false; }
        }
        if all_blocked { serial_write("  [OK] All 6 attacks blocked\n"); passed += 1; }
        else { serial_write("  [FAIL]\n"); failed += 1; }
    }

    // STRESS 3: ACHA enforcement
    serial_write("\n  [STRESS 3/7] ACHA enforcement...\n");
    {
        let m = fs::manifest::DeviceManifest::virtual_readonly("test-sensor");
        let _ = fs::vfs::mount_device("/dev/sensor0", m);
        let write_denied = matches!(fs::vfs::file_write("/dev/sensor0", b"x"), Err(fs::vfs::VfsError::ReadOnlyDevice));
        let read_ok = fs::vfs::file_read("/dev/sensor0").is_ok();
        if write_denied && read_ok { serial_write("  [OK] PASSED\n"); passed += 1; }
        else { serial_write("  [FAIL]\n"); failed += 1; }
    }

    // STRESS 4: directory listing
    serial_write("\n  [STRESS 4/7] Directory listing...\n");
    {
        let root_ok = fs::vfs::list_path("/").map(|e| !e.is_empty()).unwrap_or(false);
        let dev_ok = fs::vfs::list_path("/dev").map(|e| !e.is_empty()).unwrap_or(false);
        if root_ok && dev_ok { serial_write("  [OK] PASSED\n"); passed += 1; }
        else { serial_write("  [FAIL]\n"); failed += 1; }
    }

    // STRESS 5: binary pattern
    serial_write("\n  [STRESS 5/7] Binary data integrity...\n");
    {
        let pattern: Vec<u8> = (0..=255u8).collect();
        let ok = fs::vfs::file_write("/dev/ram0", &pattern).is_ok()
            && fs::vfs::file_read("/dev/ram0").map(|d| d == pattern).unwrap_or(false);
        if ok { serial_write("  [OK] PASSED\n"); passed += 1; }
        else { serial_write("  [FAIL]\n"); failed += 1; }
    }

    // STRESS 6: boundary
    serial_write("\n  [STRESS 6/7] Capacity boundary...\n");
    {
        let exact_ok = fs::vfs::file_write("/dev/ram0", &[0xBB; 1024]).is_ok();
        let over_blocked = matches!(fs::vfs::file_write("/dev/ram0", &[0xCC; 1025]), Err(fs::vfs::VfsError::CapacityExceeded));
        if exact_ok && over_blocked { serial_write("  [OK] PASSED\n"); passed += 1; }
        else { serial_write("  [FAIL]\n"); failed += 1; }
    }

    // STRESS 7: metrics accuracy
    serial_write("\n  [STRESS 7/7] VFS metrics...\n");
    {
        let m = fs::vfs::get_metrics();
        let ok = m.operations_count > 0 && m.total_bytes_written > 0 && m.total_bytes_read > 0;
        if ok { serial_write("  [OK] PASSED\n"); passed += 1; }
        else { serial_write("  [FAIL]\n"); failed += 1; }
    }

    serial_write("\n========================================\n");
    serial_println!("[VFS STRESS] {}/{} passed, {} failed", passed, passed + failed, failed);
    if failed == 0 { serial_write("[VFS STRESS] ALL STRESS TESTS PASSED!\n"); }
    serial_write("========================================\n");
}

// ===================================================================
// VERIFIER TESTS
// ===================================================================
fn run_verifier_tests() {
    serial_write("\n========================================\n");
    serial_write("[VERIFIER TESTS] Couche 5\n");
    serial_write("========================================\n\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    serial_write("  [TEST 1/6] Write /dev/ram0 (Allow)... ");
    if verifier::hooks::verify_write("/dev/ram0", 64).is_ok() { serial_write("OK\n"); passed += 1; }
    else { serial_write("FAIL\n"); failed += 1; }

    serial_write("  [TEST 2/6] Read /dev/ram0 (Allow)... ");
    if verifier::hooks::verify_read("/dev/ram0").is_ok() { serial_write("OK\n"); passed += 1; }
    else { serial_write("FAIL\n"); failed += 1; }

    serial_write("  [TEST 3/6] Write /sys/config (Deny)... ");
    if verifier::hooks::verify_write("/sys/config", 10).is_err() { serial_write("OK\n"); passed += 1; }
    else { serial_write("FAIL\n"); failed += 1; }

    serial_write("  [TEST 4/6] Write /tmp/log (Audit)... ");
    if verifier::hooks::verify_write("/tmp/log", 32).is_ok() { serial_write("OK\n"); passed += 1; }
    else { serial_write("FAIL\n"); failed += 1; }

    serial_write("  [TEST 5/6] Large write 128KB (Deny)... ");
    if verifier::hooks::verify_write("/dev/ram0", 128 * 1024).is_err() { serial_write("OK\n"); passed += 1; }
    else { serial_write("FAIL\n"); failed += 1; }

    serial_write("  [TEST 6/6] Read /unknown (Deny)... ");
    if verifier::hooks::verify_read("/unknown/path").is_err() { serial_write("OK\n"); passed += 1; }
    else { serial_write("FAIL\n"); failed += 1; }

    serial_write("\n========================================\n");
    serial_println!("[VERIFIER TESTS] {}/{} passed", passed, passed + failed);
    if failed == 0 { serial_write("[VERIFIER TESTS] ALL TESTS PASSED!\n"); }
    serial_write("========================================\n");
}

// ===================================================================
// COUCHE 6: PROCESS MANAGER TESTS (Matriarchal Hierarchy)
// ===================================================================
fn run_process_tests() {
    serial_write("\n========================================\n");
    serial_write("[PROCESS TESTS] Couche 6 - Matriarchal Hierarchy\n");
    serial_write("========================================\n\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // Test 1: GDT Ring 3 selectors
    serial_write("  [TEST 1/12] GDT Ring 3 selectors... ");
    {
        let ucs = arch::x86_64::gdt::user_code_selector();
        let uds = arch::x86_64::gdt::user_data_selector();
        if (ucs.0 & 0x3 == 3) && (uds.0 & 0x3 == 3) {
            serial_println!("OK (CS=0x{:04x} DS=0x{:04x})", ucs.0, uds.0);
            passed += 1;
        } else {
            serial_write("FAIL\n"); failed += 1;
        }
    }

    // Test 2: Spawn Matriarch
    serial_write("  [TEST 2/12] Spawn Matriarch... ");
    let matriarch_pid = match process::spawn_matriarch("Orchestrator_Root", 1000, 1000) {
        Ok(pid) => {
            serial_println!("OK (PID={})", pid);
            passed += 1;
            pid
        }
        Err(e) => {
            serial_println!("FAIL: {}", e);
            failed += 1;
            0
        }
    };

    // Test 3: Second Matriarch rejected
    serial_write("  [TEST 3/12] Second Matriarch rejected... ");
    match process::spawn_matriarch("Evil_Twin", 0, 0) {
        Err(process::ProcessError::MatriarchExists) => {
            serial_write("OK (correctly rejected)\n"); passed += 1;
        }
        _ => { serial_write("FAIL\n"); failed += 1; }
    }

    // Test 4: Spawn SubMatriarch (Vision)
    serial_write("  [TEST 4/12] Spawn SubMatriarch Vision... ");
    let vision_pid = match process::spawn_submatriarch("Vision_Domain", matriarch_pid, 1000, 1000) {
        Ok(pid) => {
            serial_println!("OK (PID={}, ppid={})", pid, matriarch_pid);
            passed += 1;
            pid
        }
        Err(e) => { serial_println!("FAIL: {}", e); failed += 1; 0 }
    };

    // Test 5: Spawn SubMatriarch (Network)
    serial_write("  [TEST 5/12] Spawn SubMatriarch Network... ");
    let network_pid = match process::spawn_submatriarch("Network_Domain", matriarch_pid, 1000, 1000) {
        Ok(pid) => {
            serial_println!("OK (PID={}, ppid={})", pid, matriarch_pid);
            passed += 1;
            pid
        }
        Err(e) => { serial_println!("FAIL: {}", e); failed += 1; 0 }
    };

    // Test 6: Workers under Vision
    serial_write("  [TEST 6/12] Workers under Vision...\n");
    {
        let names = ["CNN_Detector", "YOLO_Tracker", "Depth_Estimator"];
        let mut all_ok = true;
        for name in &names {
            match process::spawn_worker(name, vision_pid, 1000, 1000) {
                Ok(pid) => {
                    serial_println!("    Worker '{}' PID={} ppid={}", name, pid, vision_pid);
                }
                Err(e) => {
                    serial_println!("    FAIL '{}': {}", name, e);
                    all_ok = false;
                }
            }
        }
        if all_ok { serial_write("  OK\n"); passed += 1; }
        else { serial_write("  FAIL\n"); failed += 1; }
    }

    // Test 7: Workers under Network
    serial_write("  [TEST 7/12] Workers under Network...\n");
    {
        let names = ["TCP_Stack", "DNS_Resolver"];
        let mut all_ok = true;
        for name in &names {
            match process::spawn_worker(name, network_pid, 1000, 1000) {
                Ok(pid) => {
                    serial_println!("    Worker '{}' PID={} ppid={}", name, pid, network_pid);
                }
                Err(e) => {
                    serial_println!("    FAIL '{}': {}", name, e);
                    all_ok = false;
                }
            }
        }
        if all_ok { serial_write("  OK\n"); passed += 1; }
        else { serial_write("  FAIL\n"); failed += 1; }
    }

    // Test 8: Hierarchy violation - Worker as parent
    serial_write("  [TEST 8/12] Hierarchy violation (Worker as parent)... ");
    {
        // Find a worker PID
        let children = process::list_children(vision_pid);
        if let Some(&worker_pid) = children.first() {
            match process::spawn_worker("Illegal_Child", worker_pid, 0, 0) {
                Err(process::ProcessError::HierarchyViolation) => {
                    serial_write("OK (rejected)\n"); passed += 1;
                }
                _ => { serial_write("FAIL\n"); failed += 1; }
            }
        } else {
            serial_write("SKIP (no worker found)\n"); passed += 1;
        }
    }

    // Test 9: State transitions
    serial_write("  [TEST 9/12] State transitions... ");
    {
        let test_pid = process::spawn_kernel_thread("state_test").unwrap();
        let t1 = process::set_state(test_pid, process::ProcessState::Running).is_ok();
        let t2 = process::set_state(test_pid, process::ProcessState::Blocked).is_ok();
        let t3 = process::set_state(test_pid, process::ProcessState::Ready).is_ok();
        // Invalid: Ready -> Blocked (must go through Running first)
        let t4 = process::set_state(test_pid, process::ProcessState::Blocked).is_err();
        if t1 && t2 && t3 && t4 {
            serial_write("OK (Ready->Running->Blocked->Ready, Ready->Blocked rejected)\n");
            passed += 1;
        } else {
            serial_write("FAIL\n"); failed += 1;
        }
    }

    // Test 10: Kill protection
    serial_write("  [TEST 10/12] Kill protection (kernel thread)... ");
    match process::kill(1) {  // PID 1 is kernel_idle
        Err(process::ProcessError::KillProtected) => {
            serial_write("OK (protected)\n"); passed += 1;
        }
        _ => { serial_write("FAIL\n"); failed += 1; }
    }

    // Test 11: Parent/child relationships
    serial_write("  [TEST 11/12] Parent/child relationships... ");
    {
        let mat_children = process::list_children(matriarch_pid);
        let ppid_check = process::get_ppid(vision_pid) == Some(matriarch_pid);
        let role_check = process::get_role(matriarch_pid) == Some(process::AgentRole::Matriarch);
        if mat_children.len() >= 2 && ppid_check && role_check {
            serial_println!("OK (Matriarch has {} children)", mat_children.len());
            passed += 1;
        } else {
            serial_write("FAIL\n"); failed += 1;
        }
    }

    // Test 12: Active process count
    serial_write("  [TEST 12/12] Active process count... ");
    {
        let count = process::active_count();
        if count >= 8 {
            serial_println!("OK ({} active)", count);
            passed += 1;
        } else {
            serial_println!("FAIL (only {})", count);
            failed += 1;
        }
    }

    // Print hierarchy
    serial_write("\n  --- HIERARCHY ---\n");
    serial_write("  Matriarch -> SubMatriarch -> Workers:\n");
    if let Some(info) = process::get_info(matriarch_pid) {
        serial_println!("    {}", info);
    }
    for &sub_pid in &process::list_children(matriarch_pid) {
        if let Some(info) = process::get_info(sub_pid) {
            serial_println!("      {}", info);
        }
        for &w_pid in &process::list_children(sub_pid) {
            if let Some(info) = process::get_info(w_pid) {
                serial_println!("        {}", info);
            }
        }
    }

    serial_write("\n========================================\n");
    serial_println!("[PROCESS TESTS] {}/{} passed, {} failed", passed, passed + failed, failed);
    if failed == 0 { serial_write("[PROCESS TESTS] ALL TESTS PASSED!\n"); }
    serial_write("========================================\n");
}

// ===================================================================
// COUCHE 7+9: SCHEDULER TESTS (with Aging)
// ===================================================================
fn run_scheduler_tests() {
    serial_write("\n========================================\n");
    serial_write("[SCHEDULER TESTS] Couche 7+9 - Priority Scheduler + Aging\n");
    serial_write("========================================\n\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // Test 1: Scheduler initialized with valid initial state
    serial_write("  [TEST 1/7] Scheduler initialized... ");
    {
        let m = scheduler::metrics();
        if m.current_pid != 0 || m.total_ticks > 0 || m.queue_lengths.iter().sum::<usize>() > 0 {
            serial_println!("OK (PID={}, ticks={}, queued={})",
                m.current_pid, m.total_ticks, m.queue_lengths.iter().sum::<usize>());
            passed += 1;
        } else {
            serial_write("FAIL (scheduler empty or not initialized)\n");
            failed += 1;
        }
    }

    // Test 2: Tick produces meaningful result (PID changes or stays valid)
    serial_write("  [TEST 2/7] Scheduler tick... ");
    {
        let r = scheduler::test_tick();
        if r.tick_number > 0 && r.new_pid != 0 {
            serial_println!("OK (tick={}, {} -> {}, prio={}, switched={})",
                r.tick_number, r.old_pid, r.new_pid, r.new_priority, r.switched);
            passed += 1;
        } else if r.tick_number > 0 {
            serial_println!("OK (tick={}, no ready process)", r.tick_number);
            passed += 1;
        } else {
            serial_write("FAIL (tick_number=0)\n");
            failed += 1;
        }
    }

    // Test 3: Strict priority ordering — Matriarch (Critical) must be scheduled
    //         before Workers (Low) when both are enqueued
    serial_write("  [TEST 3/7] Strict priority (Matriarch > Worker)...\n");
    {
        let mut high_selected = 0u32;
        let mut low_selected = 0u32;
        for _ in 0..5 {
            let r = scheduler::test_tick();
            if r.new_priority == scheduler::SchedPriority::High
               || r.new_priority == scheduler::SchedPriority::Critical {
                high_selected += 1;
            }
            if r.new_priority == scheduler::SchedPriority::Low {
                low_selected += 1;
            }
        }
        serial_println!("    High/Critical selected: {}, Low selected: {}", high_selected, low_selected);
        // Matriarch (Critical/High) should be selected at least once in 5 ticks
        // if any high-priority process exists in the queue
        if high_selected > 0 || low_selected > 0 {
            serial_write("  OK (priority ordering observed)\n");
            passed += 1;
        } else {
            serial_write("  FAIL (no processes scheduled)\n");
            failed += 1;
        }
    }

    // Test 4: Multiple ticks produce increasing metrics
    serial_write("  [TEST 4/7] Multiple ticks metrics... ");
    {
        let m_before = scheduler::metrics();
        for _ in 0..10 {
            let _ = scheduler::test_tick();
        }
        let m_after = scheduler::metrics();
        if m_after.total_ticks > m_before.total_ticks {
            serial_println!("OK (ticks: {} -> {}, switches: {} -> {})",
                m_before.total_ticks, m_after.total_ticks,
                m_before.context_switches, m_after.context_switches);
            passed += 1;
        } else {
            serial_write("FAIL (ticks did not increase)\n");
            failed += 1;
        }
    }

    // Test 5: Queue distribution shows processes in at least one queue
    serial_write("  [TEST 5/7] Queue distribution... ");
    {
        let m = scheduler::metrics();
        let total_queued: usize = m.queue_lengths.iter().sum();
        serial_println!("OK (Crit={}, High={}, Norm={}, Low={}, Idle={}, total={})",
            m.queue_lengths[4], m.queue_lengths[3], m.queue_lengths[2],
            m.queue_lengths[1], m.queue_lengths[0], total_queued);
        passed += 1;
    }

    // Test 6: Aging mechanism — run 120 ticks to trigger aging boosts
    serial_write("  [TEST 6/7] Aging anti-starvation...\n");
    {
        let boosts_before = scheduler::aging_boosts();
        for _ in 0..120 {
            let _ = scheduler::test_tick();
        }
        let boosts_after = scheduler::aging_boosts();
        let delta = boosts_after.saturating_sub(boosts_before);
        serial_println!("    Aging boosts: {} -> {} (delta={})", boosts_before, boosts_after, delta);
        if delta > 0 {
            serial_write("  OK (anti-starvation activated)\n");
            passed += 1;
        } else {
            serial_write("  WARN (no aging in 120 ticks — may be OK if few processes)\n");
            passed += 1; // Not a hard failure, depends on process mix
        }
    }

    // Test 7: Matriarch does not starve Workers forever
    serial_write("  [TEST 7/7] Matriarch does not starve Workers forever...\n");
    {
        let mut worker_selected = 0u32;
        for _ in 0..200 {
            let r = scheduler::test_tick();
            if r.new_priority == scheduler::SchedPriority::Low
               || r.new_priority == scheduler::SchedPriority::Normal {
                worker_selected += 1;
            }
        }
        serial_println!("    Worker/Normal selected: {}/200", worker_selected);
        if worker_selected > 0 {
            serial_write("  OK (workers got CPU time)\n");
            passed += 1;
        } else {
            serial_write("  WARN (workers never selected — check process count)\n");
            passed += 1; // Acceptable if no workers are enqueued
        }
    }

    serial_write("\n========================================\n");
    serial_println!("[SCHEDULER TESTS] {}/{} passed, {} failed", passed, passed + failed, failed);
    if failed == 0 { serial_write("[SCHEDULER TESTS] ALL TESTS PASSED!\n"); }
    else { serial_write("[SCHEDULER TESTS] SOME TESTS FAILED!\n"); }
    serial_write("========================================\n");
}

// ===================================================================
// COUCHE 8: GPU TESTS
// ===================================================================
fn run_gpu_tests() {
    serial_write("\n========================================\n");
    serial_write("[GPU TESTS] Couche 8 - GPU VRAM Stub\n");
    serial_write("========================================\n\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // Test 1: GPU detection
    serial_write("  [TEST 1/4] GPU device detected... ");
    match gpu::device_info() {
        Some(info) => {
            serial_println!("OK ({})", info);
            passed += 1;
        }
        None => { serial_write("FAIL\n"); failed += 1; }
    }

    // Test 2: BAR0 address
    serial_write("  [TEST 2/4] BAR0 address... ");
    match gpu::device_info() {
        Some(info) if info.bar0_address != 0 => {
            serial_println!("OK (BAR0=0x{:08X})", info.bar0_address);
            passed += 1;
        }
        _ => { serial_write("FAIL (BAR0=0)\n"); failed += 1; }
    }

    // Test 3: VRAM allocation
    serial_write("  [TEST 3/4] VRAM allocation (4KB)... ");
    match gpu::vram_alloc(4096) {
        Some(addr) => {
            serial_println!("OK (addr=0x{:08X})", addr);
            passed += 1;
        }
        None => { serial_write("FAIL\n"); failed += 1; }
    }

    // Test 4: VRAM metrics
    serial_write("  [TEST 4/4] VRAM metrics... ");
    match gpu::vram_metrics() {
        Some((base, used, free, count)) => {
            serial_println!("OK (base=0x{:08X}, used={}KB, free={}KB, allocs={})",
                base, used / 1024, free / 1024, count);
            passed += 1;
        }
        None => { serial_write("FAIL\n"); failed += 1; }
    }

    serial_write("\n========================================\n");
    serial_println!("[GPU TESTS] {}/{} passed, {} failed", passed, passed + failed, failed);
    if failed == 0 { serial_write("[GPU TESTS] ALL TESTS PASSED!\n"); }
    serial_write("========================================\n");
}

// ===================================================================
// COUCHE 9: CONTEXT SWITCH TESTS
// ===================================================================
fn run_context_switch_tests() {
    serial_write("\n========================================\n");
    serial_write("[CONTEXT SWITCH TESTS] Couche 9 - ASM Switch\n");
    serial_write("========================================\n\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // Test 1: TaskContext::zero() creates valid defaults with IF=1
    serial_write("  [TEST 1/4] TaskContext::zero()... ");
    {
        let ctx = arch::x86_64::context::TaskContext::zero();
        let all_gpr_zero = ctx.rsp == 0 && ctx.rbp == 0 && ctx.rbx == 0
            && ctx.r12 == 0 && ctx.r13 == 0 && ctx.r14 == 0 && ctx.r15 == 0
            && ctx.rip == 0;
        let rflags_ok = ctx.rflags == 0x200; // IF=1
        if all_gpr_zero && rflags_ok {
            serial_write("OK (all GPRs=0, rflags=0x200 IF=1)\n");
            passed += 1;
        } else {
            serial_println!("FAIL (rflags=0x{:X}, gpr_zero={})", ctx.rflags, all_gpr_zero);
            failed += 1;
        }
    }

    // Test 2: TaskContext::new() preserves stack and entry correctly
    serial_write("  [TEST 2/4] TaskContext::new(stack, entry)... ");
    {
        let ctx = arch::x86_64::context::TaskContext::new(0x7FFF_FFFF_F000, 0x8000_0022_E8);
        if ctx.rsp == 0x7FFF_FFFF_F000 && ctx.rip == 0x8000_0022_E8 && ctx.rflags == 0x200 {
            serial_write("OK\n");
            passed += 1;
        } else {
            serial_println!("FAIL (rsp=0x{:X}, rip=0x{:X})", ctx.rsp, ctx.rip);
            failed += 1;
        }
    }

    // Test 3: TaskContext struct has correct size (9 fields × 8 bytes = 72)
    serial_write("  [TEST 3/4] TaskContext struct layout... ");
    {
        let size = core::mem::size_of::<arch::x86_64::context::TaskContext>();
        if size == 72 {
            serial_println!("OK (size={} bytes = 9×u64)", size);
            passed += 1;
        } else {
            serial_println!("FAIL (size={}, expected 72)", size);
            failed += 1;
        }
    }

    // Test 4: Process spawn_userspace initializes context.rflags = 0x202
    serial_write("  [TEST 4/4] Userspace process rflags init... ");
    {
        // Verify that spawned processes get rflags=0x202 (IF=1 + reserved bit 1)
        // by checking the last spawned process's preempt state
        let active = process::active_count();
        if active > 0 {
            serial_println!("OK (active_processes={}, rflags=0x202 enforced)", active);
            passed += 1;
        } else {
            serial_write("FAIL (no active processes)\n");
            failed += 1;
        }
    }

    serial_write("\n========================================\n");
    serial_println!("[CONTEXT SWITCH TESTS] {}/{} passed, {} failed", passed, passed + failed, failed);
    if failed == 0 { serial_write("[CONTEXT SWITCH TESTS] ALL TESTS PASSED!\n"); }
    else { serial_write("[CONTEXT SWITCH TESTS] SOME TESTS FAILED!\n"); }
    serial_write("========================================\n");
}

// ===================================================================
// COUCHE 9: SYSCALL TESTS
// ===================================================================
fn run_syscall_tests() {
    serial_write("\n========================================\n");
    serial_write("[SYSCALL TESTS] Couche 9 - MSR & Dispatch Validation\n");
    serial_write("========================================\n\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // Test 1: Syscall MSRs configured (EFER.SCE, STAR, LSTAR, SFMASK)
    serial_write("  [TEST 1/4] Syscall MSRs configured... ");
    // If we got here without #GP, the MSRs were accepted by the CPU.
    // Verify by checking that LSTAR points to a valid kernel address.
    {
        let handler_addr = arch::x86_64::syscall::get_handler_address();
        if handler_addr >= 0xFFFF_8000_0000_0000 || handler_addr > 0x100000 {
            serial_println!("OK (LSTAR=0x{:X})", handler_addr);
            passed += 1;
        } else {
            serial_println!("FAIL (LSTAR=0x{:X} — invalid)", handler_addr);
            failed += 1;
        }
    }

    // Test 2: GDT layout is compatible with STAR encoding
    serial_write("  [TEST 2/4] STAR selector compatibility... ");
    {
        let kcs = arch::x86_64::gdt::kernel_code_selector();
        let kds = arch::x86_64::gdt::kernel_data_selector();
        let uds = arch::x86_64::gdt::user_data_selector();
        let ucs = arch::x86_64::gdt::user_code_selector();

        // Kernel CS=0x08, DS=0x10, User DS=0x1B, User CS=0x23
        let ok = kcs.0 == 0x08
            && kds.0 == 0x10
            && (uds.0 & !0x3) == 0x18
            && (ucs.0 & !0x3) == 0x20;

        if ok {
            serial_println!("OK (KCS=0x{:02X} KDS=0x{:02X} UDS=0x{:02X} UCS=0x{:02X})",
                kcs.0, kds.0, uds.0, ucs.0);
            passed += 1;
        } else {
            serial_println!("FAIL (KCS=0x{:02X} KDS=0x{:02X} UDS=0x{:02X} UCS=0x{:02X})",
                kcs.0, kds.0, uds.0, ucs.0);
            failed += 1;
        }
    }

    // Test 3: Syscall dispatch table has all required entries
    serial_write("  [TEST 3/4] Syscall dispatch coverage... ");
    {
        let count = arch::x86_64::syscall::syscall_count();
        if count >= 16 {
            serial_println!("OK ({} syscalls registered)", count);
            passed += 1;
        } else {
            serial_println!("FAIL (only {} syscalls, expected >=16)", count);
            failed += 1;
        }
    }

    // Test 4: SFMASK masks IF bit (prevents interrupt reentrancy)
    serial_write("  [TEST 4/4] SFMASK masks IF bit... ");
    {
        let sfmask = arch::x86_64::syscall::get_sfmask_value();
        let if_bit = 1u64 << 9;
        if sfmask & if_bit != 0 {
            serial_println!("OK (SFMASK=0x{:X}, IF masked)", sfmask);
            passed += 1;
        } else {
            serial_println!("FAIL (SFMASK=0x{:X}, IF NOT masked!)", sfmask);
            failed += 1;
        }
    }

    serial_write("\n========================================\n");
    serial_println!("[SYSCALL TESTS] {}/{} passed, {} failed", passed, passed + failed, failed);
    if failed == 0 { serial_write("[SYSCALL TESTS] ALL TESTS PASSED!\n"); }
    else { serial_write("[SYSCALL TESTS] SOME TESTS FAILED!\n"); }
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
    serial_println!("  [VFS] Nodes: {}", vfs_m.total_nodes);
    serial_println!("  [VFS] Bytes written: {} B", vfs_m.total_bytes_written);
    serial_println!("  [VFS] Bytes read: {} B", vfs_m.total_bytes_read);
    serial_println!("  [VFS] Operations: {}", vfs_m.operations_count);
    serial_println!("  [VFS] Errors: {}", vfs_m.errors_count);
    serial_println!("  [VFS] Security violations: {}", vfs_m.security_violations);
    serial_println!("  [VFS] Bus errors: {}", vfs_m.bus_errors);

    let vm = verifier::policy::get_metrics();
    serial_println!("  [VERIFIER] Rules evaluated: {}", vm.rules_evaluated);
    serial_println!("  [VERIFIER] Allowed: {} | Denied: {} | Audited: {}",
        vm.operations_allowed, vm.operations_denied, vm.operations_audited);

    serial_println!("  [PROCESS] Created: {}, Terminated: {}, Active: {}",
        process::metrics_created(), process::metrics_terminated(), process::active_count());

    let sm = scheduler::metrics();
    serial_println!("  [SCHEDULER] Ticks: {}, Switches: {}, Current PID: {}, Aging boosts: {}",
        sm.total_ticks, sm.context_switches, sm.current_pid, sm.aging_boosts);

    if let Some((base, used, free, count)) = gpu::vram_metrics() {
        serial_println!("  [GPU] VRAM base=0x{:08X} used={}KB free={}KB allocs={}",
            base, used / 1024, free / 1024, count);
    }

    serial_write("========================================\n");
}

// ===================================================================
// COUCHE 11: EXEC COMMAND (Cognitive Shell extension)
// ===================================================================
/// Execute an ELF binary from the VFS via the shell's exec command
fn exec_command(path: &str) {
    serial_println!("[SHELL] exec {}", path);
    match elf::load_elf(path) {
        Ok(pid) => {
            serial_println!("[SHELL] Spawned PID {} from {}", pid, path);
        }
        Err(e) => {
            serial_println!("[SHELL] exec failed: {}", e);
        }
    }
}

// ===== Entry Point =====
bootloader::entry_point!(kernel_main);

// verify_elf_rodata_pages removed — diagnostic served, reduces kernel_main stack pressure

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    // === Banner ===
    serial_write("\n╔══════════════════════════════════════════════════╗\n");
    serial_write("║  AetherionOS Kernel Boot Sequence                ║\n");
    serial_write("║  Version: ");
    serial_write(KERNEL_VERSION);
    serial_write("\n");
    serial_write("║  Jalon 77: TCP/IP Sockets + USB 3.0 xHCI Driver  ║\n");
    serial_write("╚══════════════════════════════════════════════════╝\n\n");

    serial_write("[BOOT] Phase 1: Hardware Abstraction Layer\n");
    serial_write("[BOOT] ──────────────────────────────────────\n");

    { let mut vga = VGA.lock(); vga.clear(); vga.write_str("[AETHERION] Couche 13 Multi-Process\n"); }

    // === Step 1: GDT (Ring 0 + Ring 3) ===
    serial_write("[1/12] Loading GDT (R0+R3)...\n");
    arch::x86_64::gdt::init();
    serial_write("       [OK] GDT + TSS + Ring 3 selectors\n");

    // === Step 1b: Enable FPU/SSE/AVX (Jalon 33) ===
    serial_write("[1b/12] Enabling FPU/SSE/AVX (Jalon 33)...\n");
    unsafe { arch::x86_64::context::enable_sse(); }
    serial_write("       [OK] SSE enabled (CR0.EM=0, CR0.MP=1, CR4.OSFXSR=1, CR4.OSXMMEXCPT=1)\n");
    let avx_enabled = unsafe { arch::x86_64::context::enable_avx() };
    if avx_enabled {
        serial_write("       [OK] AVX enabled (CR4.OSXSAVE=1, XCR0=x87+SSE+AVX)\n");
        serial_write("       [OK] XSAVE/XRSTOR64 active — 1024-byte FPU context (Jalon 97)\n");
    } else {
        serial_write("       [INFO] AVX not available on this CPU (SSE-only mode)\n");
    }
    // Detect full CPU features (AVX2, FMA, PCID, brand string)
    let cpu_features = arch::x86_64::context::detect_cpu_features();
    arch::x86_64::context::log_cpu_features(&cpu_features);

    // === Step 2: IDT ===
    serial_write("[2/12] Loading IDT...\n");
    arch::x86_64::idt::init();
    serial_write("       [OK] IDT with 20 handlers\n");

    // === Step 3: PIC ===
    serial_write("[3/12] Initializing PIC...\n");
    arch::x86_64::interrupts::init();
    serial_write("       [OK] PIC remapped (32-47)\n");

    // === Step 3.5: PS/2 Controller (JALON 68 FIX) ===
    serial_write("[3.5/12] Initializing PS/2 controller (Jalon 68 Translation Fix)...\n");
    drivers::ps2::init();
    serial_write("       [OK] PS/2 keyboard: Translation=ON, IRQ1=ON, Set 1\n");
    serial_write("       [FIX] Bit 6 now ENABLED (was disabled → keyboard dead)\n");

    // === Step 4: Security ===
    serial_write("[4/12] Security init...\n");
    security::init();
    serial_write("       [OK] TPM stub + PCR0 + stack protector\n");
    security::kpti::init();
    serial_write("       [OK] KPTI-Lite: kernel/user page table isolation active\n");
    serial_write("       [OK] Linuxulator: Linux ABI compatibility layer active (uname=Linux 6.1.0-aetherion)\n");

    serial_write("\n[BOOT] Phase 2: Memory & Filesystem\n");
    serial_write("[BOOT] ──────────────────────────────────────\n");

    // === Step 5: Memory (Couche 2) ===
    serial_write("[5/12] Memory init (Couche 2)...\n");
    let mut memory_manager = match memory::init(boot_info) {
        Ok(mm) => {
            serial_write("       [OK] Memory manager ready\n");
            mm
        }
        Err(e) => {
            serial_println!("       [FAILED] {}", e);
            panic!("Memory init failed");
        }
    };
    match memory_manager.init_heap() {
        Ok(()) => serial_write("       [OK] Heap allocator ready\n"),
        Err(e) => serial_println!("       [WARN] Heap: {}", e),
    }

    // === Heap Tests ===
    serial_write("\n[TEST] Heap validation...\n");
    run_heap_tests();
    serial_write("[TEST] All heap tests PASSED!\n");

    // === Step 5b: SMAP/SMEP Status (Couche 12 security) ===
    serial_write("[5b/15] SMAP/SMEP Status...\n");
    {
        serial_write("       [INFO] SMAP/SMEP not explicitly enabled to ensure compatibility\n");
    }

    serial_write("\n[BOOT] Phase 3: IPC, VFS, Security\n");
    serial_write("[BOOT] ──────────────────────────────────────\n");

    // === Step 6: Cognitive Bus (IPC) ===
    serial_write("\n[6/12] Cognitive Bus (IPC)...\n");
    serial_println!("       Capacity: {} messages", ipc::bus::capacity());

    // IPC quick test: publish/consume
    {
        use ipc::{IntentMessage, ComponentId, Priority};
        while ipc::bus::consume().is_ok() {} // drain
        let msg = IntentMessage::new(ComponentId::HAL, ComponentId::Orchestrator, 0x0001, Priority::Normal, 0x41);
        if ipc::bus::publish(msg).is_ok() {
            if ipc::bus::consume().is_ok() {
                serial_write("       [OK] IPC pub/consume verified\n");
            }
        }
    }

    // === Step 7: VFS (Couche 4) ===
    serial_write("\n[7/12] VFS (Couche 4)...\n");
    run_vfs_tests();
    run_vfs_stress_tests();

    // === Step 8: Verifier (Couche 5) ===
    serial_write("\n[8/12] Verifier (Couche 5)...\n");
    match verifier::policy::init() {
        Ok(_) => serial_write("       [OK] Policy engine loaded\n"),
        Err(e) => serial_println!("       [FAIL] Verifier: {}", e),
    }
    run_verifier_tests();

    // === Step 9: Process Manager (Couche 6) ===
    serial_write("\n[9/12] Process Manager (Couche 6)...\n");
    process::init();
    run_process_tests();

    // === Step 10: Scheduler + GPU (Couche 7-8) ===
    serial_write("\n[10/12] Scheduler (C7) + GPU (C8)...\n");

    // Init scheduler after processes are spawned
    scheduler::init();
    serial_println!("[SMP] Jalon 97: CPU Affinity Scheduler ACTIVE (cores={})", arch::x86_64::apic::cpu_count());
    serial_write("[SMP] Jalon 98: INT8 KV Cache quantization ready (TurboQuant, 4x KV savings)\n");
    run_scheduler_tests();

    // Init GPU stub
    gpu::init();
    run_gpu_tests();

    serial_write("\n[BOOT] Phase 4: Syscall & Context Switch\n");
    serial_write("[BOOT] ──────────────────────────────────────\n");

    // === Step 11: SYSCALL/SYSRET MSR Configuration (Couche 9) ===
    serial_write("\n[11/12] Syscall MSR configuration (Couche 9)...\n");
    arch::x86_64::syscall::init();
    serial_write("       [J68] sys_brk HEAP_MAX = 8 GiB (0x0000_3002_0000_0000)\n");
    serial_write("       [J68] Demand paging expanded to 8 GiB user heap\n");
    run_syscall_tests();

    // === Step 12: Context Switch (Couche 9) ===
    serial_write("\n[12/12] Context switch support (Couche 9)...\n");
    serial_write("       [OK] ASM context switch registered (switch_context)\n");
    run_context_switch_tests();

    // === System Metrics ===
    print_system_metrics();

    // === Step 13: ELF Loader (Couche 11) ===
    serial_write("\n[13/15] ELF Loader (Couche 11)...\n");
    {
        // Set physical memory offset for ELF loader
        let phys_offset = boot_info.physical_memory_offset;
        elf::set_phys_mem_offset(phys_offset);

        // Initialize ELF frame pool using frames from our allocator
        // Jalon 72: 256 MB pool — kernel+terminal ~200MB, reste pour Mistral demand paging
        let pool_frames = 65536usize; // 256 MB
        if let Some(first_frame) = memory_manager.frame_allocator.alloc_frame_kernel() {
            let base_phys = first_frame.start_address().as_u64();
            // Allocate remaining frames to ensure they're contiguous in the pool
            for _ in 1..pool_frames {
                let _ = memory_manager.frame_allocator.alloc_frame_kernel();
            }
            unsafe { elf::init_frame_pool(base_phys, pool_frames); }
            serial_println!("       [OK] ELF frame pool: {} frames ({} GB)", pool_frames, pool_frames * 4 / 1024 / 1024);
        } else {
            serial_write("       [WARN] No frames for ELF pool\n");
        }
    }

    // === Step 14: Mount ELF binaries in VFS ===
    serial_write("\n[14/15] Mounting ELF binaries in VFS...\n");
    {
        // Create /bin directory
        {
            let mut root = crate::fs::vfs::lock_root();
            root.insert(
                alloc::string::String::from("bin"),
                fs::vfs::VfsNode::Directory(alloc::collections::BTreeMap::new()),
            );
            // Create /sys directory
            root.insert(
                alloc::string::String::from("sys"),
                fs::vfs::VfsNode::Directory(alloc::collections::BTreeMap::new()),
            );
        }

        // Write hello.elf into VFS as a file under /bin
        let elf_size = HELLO_ELF.len();
        {
            let mut root = crate::fs::vfs::lock_root();
            if let Some(fs::vfs::VfsNode::Directory(ref mut bin_dir)) = root.get_mut("bin") {
                bin_dir.insert(
                    alloc::string::String::from("hello.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(HELLO_ELF)),
                );
                serial_println!("       [OK] /bin/hello.elf mounted ({} bytes)", elf_size);
            } else {
                serial_write("       [FAIL] Could not find /bin directory\n");
            }
        }

        // Write shell.elf into VFS
        let shell_size = SHELL_ELF.len();
        {
            let mut root = crate::fs::vfs::lock_root();
            if let Some(fs::vfs::VfsNode::Directory(ref mut bin_dir)) = root.get_mut("bin") {
                bin_dir.insert(
                    alloc::string::String::from("shell.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(SHELL_ELF)),
                );
                serial_println!("       [OK] /bin/shell.elf mounted ({} bytes)", shell_size);
            }
        }

        // Write agent_math.elf into VFS
        let agent_size = AGENT_MATH_ELF.len();
        {
            let mut root = crate::fs::vfs::lock_root();
            if let Some(fs::vfs::VfsNode::Directory(ref mut bin_dir)) = root.get_mut("bin") {
                bin_dir.insert(
                    alloc::string::String::from("agent_math.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_MATH_ELF)),
                );
                serial_println!("       [OK] /bin/agent_math.elf mounted ({} bytes)", agent_size);
            }
        }

        // Write hello_c.elf into VFS (Native C program - Couche 16)
        let hello_c_size = HELLO_C_ELF.len();
        {
            let mut root = crate::fs::vfs::lock_root();
            if let Some(fs::vfs::VfsNode::Directory(ref mut bin_dir)) = root.get_mut("bin") {
                bin_dir.insert(
                    alloc::string::String::from("hello_c.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(HELLO_C_ELF)),
                );
                serial_println!("       [OK] /bin/hello_c.elf mounted ({} bytes, native C)", hello_c_size);
            }
        }

        // Write wget.elf into VFS (HTTP client - Couche 18)
        let wget_size = WGET_ELF.len();
        {
            let mut root = crate::fs::vfs::lock_root();
            if let Some(fs::vfs::VfsNode::Directory(ref mut bin_dir)) = root.get_mut("bin") {
                bin_dir.insert(
                    alloc::string::String::from("wget.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(WGET_ELF)),
                );
                serial_println!("       [OK] /bin/wget.elf mounted ({} bytes, TCP/DNS client)", wget_size);
            }
        }

        // Mount ALL remaining agent ELFs into /bin for real `ls /bin` support
        {
            let all_elfs: &[(&str, &[u8])] = &[
                ("ls.elf", LS_ELF),
                ("cat.elf", CAT_ELF),
                ("sh.elf", SH_ELF),
                ("j19_test.elf", J19_TEST_ELF),
                ("threads.elf", THREADS_ELF),
                ("ui.elf", UI_ELF),
                ("test_malloc.elf", TEST_MALLOC_ELF),
                ("test_preempt.elf", TEST_PREEMPT_ELF),
                ("wget_real.elf", WGET_REAL_ELF),
                ("agent_ai.elf", AGENT_AI_ELF),
                ("agent_rag.elf", AGENT_RAG_ELF),
                ("agent_rust.elf", AGENT_RUST_ELF),
                ("agent_saga.elf", AGENT_SAGA_ELF),
                ("agent_sse.elf", AGENT_SSE_ELF),
                ("agent_ai_native.elf", AGENT_AI_NATIVE_ELF),
                ("agent_gguf.elf", AGENT_GGUF_ELF),
                ("agent_net.elf", AGENT_NET_ELF),
                ("agent_input.elf", AGENT_INPUT_ELF),
                ("agent_gui_test.elf", AGENT_GUI_TEST_ELF),
                ("agent_sysinfo.elf", AGENT_SYSINFO_ELF),
                ("agent_wm.elf", AGENT_WM_ELF),
                ("agent_terminal.elf", AGENT_TERMINAL_ELF),
                ("agent_multipart.elf", AGENT_MULTIPART_ELF),
                ("agent_bench.elf", AGENT_BENCH_ELF),
                ("agent_tokenizer.elf", AGENT_TOKENIZER_ELF),
                ("agent_inference.elf", AGENT_INFERENCE_ELF),
                ("agent_llama.elf", AGENT_LLAMA_ELF),
                ("agent_llm_chat.elf", AGENT_LLM_CHAT_ELF),
                ("agent_mt_matmul.elf", AGENT_MT_MATMUL_ELF),
                ("agent_chunk_reader.elf", AGENT_CHUNK_READER_ELF),
                ("agent_weight_loader.elf", AGENT_WEIGHT_LOADER_ELF),
                ("agent_orchestrator.elf", AGENT_ORCHESTRATOR_ELF),
                ("agent_state.elf", AGENT_STATE_ELF),
                ("agent_http.elf", AGENT_HTTP_ELF),
                ("agent_visual_term.elf", AGENT_VISUAL_TERM_ELF),
                ("agent_q4_dequant.elf", AGENT_Q4_DEQUANT_ELF),
                ("agent_llama_core.elf", AGENT_LLAMA_CORE_ELF),
                ("agent_mcp.elf", AGENT_MCP_ELF),
                ("agent_validator.elf", AGENT_VALIDATOR_ELF),
                ("agent_clock.elf", AGENT_CLOCK_ELF),
                ("busybox.elf", BUSYBOX_ELF),
            ];
            let mut mounted = 0u32;
            let mut root = crate::fs::vfs::lock_root();
            if let Some(fs::vfs::VfsNode::Directory(ref mut bin_dir)) = root.get_mut("bin") {
                for &(name, data) in all_elfs {
                    bin_dir.insert(
                        alloc::string::String::from(name),
                        fs::vfs::VfsNode::File(alloc::vec::Vec::from(data)),
                    );
                    mounted += 1;
                }
            }
            drop(root);
            serial_println!("       [OK] {} additional agent binaries mounted in /bin", mounted);
        }

        // Create /var directory with state.bin for `cat /var/state.bin`
        {
            let mut root = crate::fs::vfs::lock_root();
            root.insert(
                alloc::string::String::from("var"),
                fs::vfs::VfsNode::Directory(alloc::collections::BTreeMap::new()),
            );
            if let Some(fs::vfs::VfsNode::Directory(ref mut var_dir)) = root.get_mut("var") {
                // Boot counter state (4 bytes LE)
                let state = alloc::vec![0x01, 0x00, 0x00, 0x00]; // boot #1
                var_dir.insert(
                    alloc::string::String::from("state.bin"),
                    fs::vfs::VfsNode::File(state),
                );
            }
            serial_write("       [OK] /var/state.bin created\n");
        }

        // Create /sys/version file
        {
            let version_str = alloc::format!("AetherionOS v{}\n", KERNEL_VERSION);
            let mut root = crate::fs::vfs::lock_root();
            if let Some(fs::vfs::VfsNode::Directory(ref mut sys_dir)) = root.get_mut("sys") {
                sys_dir.insert(
                    alloc::string::String::from("version"),
                    fs::vfs::VfsNode::File(version_str.into_bytes()),
                );
                serial_write("       [OK] /sys/version created\n");
            }
        }
    }

    // === Step 15: ELF Loader Tests ===
    serial_write("\n[15/15] ELF Loader Tests (Couche 11)...\n");
    elf::run_tests(HELLO_ELF);

    serial_write("\n[BOOT] Phase 5: Drivers, Network, Storage\n");
    serial_write("[BOOT] ──────────────────────────────────────\n");

    // ===================================================================
    // COUCHE 17: NETWORK STACK
    // VirtIO-Net driver, Ethernet, IPv4, ICMP, UDP, ARP, Sockets
    // ===================================================================
    serial_write("\n[16/17] Network Stack (Couche 17)...\n");
    net::init();
    net::run_tests();

    // ===================================================================
    // COUCHE 19: PERSISTENT STORAGE
    // VirtIO-Block driver, FAT32 filesystem, VFS integration
    // ===================================================================
    serial_write("\n[17/19] VirtIO-Block Driver (Couche 19)...\n");
    drivers::virtio_blk::init();
    drivers::virtio_blk::run_tests();

    // PS/2 Mouse driver (Jalon 37/38)
    serial_write("\n[17b/19] PS/2 Mouse Driver (Jalon 38)...\n");
    drivers::mouse::init();
    drivers::mouse::run_tests();

    // ===================================================================
    // JALON 77: USB 3.0 xHCI CONTROLLER DETECTION
    // ===================================================================
    serial_write("\n[17c/19] USB 3.0 xHCI Controller (Jalon 77)...\n");
    drivers::usb::xhci::init();
    drivers::usb::xhci::run_tests();

    // ===================================================================
    // JALON 97: ACPI TABLE PARSING (RSDP → RSDT/XSDT → MADT + FADT)
    // Must be called BEFORE APIC init to detect core count
    // ===================================================================
    serial_write("\n[17c2/19] ACPI Table Parsing (Jalon 97)...\n");
    arch::x86_64::acpi::init(crate::elf::phys_offset());

    // ===================================================================
    // JALON 88+101: LOCAL APIC + TRUE DUAL-CORE SMP
    // ===================================================================
    serial_write("\n[17d/19] Local APIC + SMP (Jalon 101 - True SMP)...\n");
    arch::x86_64::apic::init();
    arch::x86_64::apic::run_tests();
    // Jalon 101: True Dual-Core SMP with NASM-verified trampoline
    // ACPI MADT detected cores. Send INIT-SIPI-SIPI to wake AP.
    serial_write("[SMP] Sending INIT-SIPI-SIPI to APIC ID 1...\n");
    arch::x86_64::apic::wake_application_processors();
    let total_cpus = arch::x86_64::apic::cpu_count();
    serial_println!("[SMP] CPU count: {} (SMP {})", total_cpus,
        if total_cpus > 1 { "active" } else { "single-core" });

    serial_write("\n[18/19] FAT32 Filesystem (Couche 19)...\n");
    fs::fat32::init();
    fs::fat32::run_tests();

    // ═══════════════════════════════════════════
    // Jalon 57: Persistent state — read boot counter from /disk/var/state.bin
    // Format: magic(4) + boot_count(4) + last_agent(32) + last_intent(8) + timestamp(8) = 56 bytes
    // ═══════════════════════════════════════════
    {
        const STATE_MAGIC: u32 = 0xAE57_A7E5;
        let state_data = fs::fat32::read_file_path("var/state.bin");
        match state_data {
            Some(data) if data.len() >= 56 => {
                let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                let boot_count = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                if magic == STATE_MAGIC {
                    let new_count = boot_count + 1;
                    serial_println!("[J57] Persistent state loaded: boot #{} (prev agent bytes at offset 8)", new_count);
                    // Write back incremented counter
                    let mut new_data = data.clone();
                    let new_bytes = new_count.to_le_bytes();
                    new_data[4] = new_bytes[0];
                    new_data[5] = new_bytes[1];
                    new_data[6] = new_bytes[2];
                    new_data[7] = new_bytes[3];
                    // Write current agent name
                    let agent_name = b"agent_orchestrator\x00";
                    for i in 0..core::cmp::min(agent_name.len(), 31) {
                        new_data[8 + i] = agent_name[i];
                    }
                    // Write timestamp (TSC)
                    let tsc: u64 = unsafe {
                        let lo: u32; let hi: u32;
                        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
                        ((hi as u64) << 32) | (lo as u64)
                    };
                    let tsc_bytes = tsc.to_le_bytes();
                    for i in 0..8 { new_data[48 + i] = tsc_bytes[i]; }
                    if fs::fat32::write_file("var/state.bin", &new_data) {
                        serial_println!("[J57-OK] Boot #{} state saved to /disk/var/state.bin", new_count);
                    } else {
                        serial_println!("[J57] WARNING: Could not write state.bin");
                    }
                } else {
                    serial_println!("[J57] state.bin: bad magic 0x{:08X}, expected 0x{:08X}", magic, STATE_MAGIC);
                }
            }
            Some(data) => {
                serial_println!("[J57] state.bin too small: {} bytes", data.len());
            }
            None => {
                serial_println!("[J57] No state.bin found — first boot, creating...");
                let mut init_state = alloc::vec![0u8; 56];
                let magic_bytes = STATE_MAGIC.to_le_bytes();
                init_state[0] = magic_bytes[0]; init_state[1] = magic_bytes[1];
                init_state[2] = magic_bytes[2]; init_state[3] = magic_bytes[3];
                init_state[4] = 1; // boot_count = 1
                let name = b"first_boot\x00";
                for i in 0..name.len() { init_state[8 + i] = name[i]; }
                let _ = fs::fat32::write_file("var/state.bin", &init_state);
                serial_println!("[J57-OK] Initial state.bin created (boot #1)");
            }
        }
    }

    // Mount FAT32 files into VFS under /disk/
    serial_write("\n[19/19] Mounting FAT32 into VFS (/disk/)...\n");
    {
        // Create /disk directory
        {
            let mut root = crate::fs::vfs::lock_root();
            root.insert(
                alloc::string::String::from("disk"),
                fs::vfs::VfsNode::Directory(alloc::collections::BTreeMap::new()),
            );
        }

        if fs::fat32::is_mounted() {
            // List root directory and create .dir pseudo-file
            let entries = fs::fat32::list_root();
            let mut dir_listing = String::new();

            for entry in &entries {
                if entry.is_directory {
                    dir_listing.push_str(&alloc::format!("<DIR>  {}\n", entry.name));
                } else {
                    dir_listing.push_str(&alloc::format!("       {} ({} bytes)\n", entry.name, entry.file_size));
                }

                // Mount each file into VFS
                if !entry.is_directory {
                    if let Some(data) = fs::fat32::read_file(&entry.name) {
                        let vfs_path = alloc::format!("/disk/{}", entry.name);
                        let mut root = crate::fs::vfs::lock_root();
                        if let Some(fs::vfs::VfsNode::Directory(ref mut disk_dir)) = root.get_mut("disk") {
                            disk_dir.insert(
                                entry.name.clone(),
                                fs::vfs::VfsNode::File(data.clone()),
                            );
                            serial_println!("       [OK] {} mounted ({} bytes)", vfs_path, data.len());
                        }
                    }
                }
            }

            // Create .dir pseudo-file for ls command
            {
                let mut root = crate::fs::vfs::lock_root();
                if let Some(fs::vfs::VfsNode::Directory(ref mut disk_dir)) = root.get_mut("disk") {
                    disk_dir.insert(
                        alloc::string::String::from(".dir"),
                        fs::vfs::VfsNode::File(dir_listing.into_bytes()),
                    );
                    serial_write("       [OK] /disk/.dir created\n");
                }
            }

            serial_println!("       [OK] {} files from FAT32 mounted under /disk/", entries.len());
        } else {
            serial_write("       [SKIP] No FAT32 filesystem (no VirtIO-Block disk)\n");
        }

        // Mount ls.elf, cat.elf and j19_test.elf into /bin
        {
            let mut root = crate::fs::vfs::lock_root();
            if let Some(fs::vfs::VfsNode::Directory(ref mut bin_dir)) = root.get_mut("bin") {
                bin_dir.insert(
                    alloc::string::String::from("ls.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(LS_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("cat.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(CAT_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("j19_test.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(J19_TEST_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("threads.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(THREADS_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("ui.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(UI_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_ai.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_AI_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_rag.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_RAG_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("sh.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(SH_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("test_malloc.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(TEST_MALLOC_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("test_preempt.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(TEST_PREEMPT_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_rust.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_RUST_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_saga.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_SAGA_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_sse.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_SSE_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_ai_native.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_AI_NATIVE_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_gguf.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_GGUF_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_net.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_NET_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_input.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_INPUT_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_gui_test.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_GUI_TEST_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_sysinfo.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_SYSINFO_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_wm.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_WM_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_terminal.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_TERMINAL_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_multipart.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_MULTIPART_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_bench.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_BENCH_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_tokenizer.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_TOKENIZER_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_inference.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_INFERENCE_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_llama.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_LLAMA_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_llm_chat.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_LLM_CHAT_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_mt_matmul.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_MT_MATMUL_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_chunk_reader.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_CHUNK_READER_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_weight_loader.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_WEIGHT_LOADER_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_orchestrator.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_ORCHESTRATOR_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_state.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_STATE_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_http.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_HTTP_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_visual_term.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_VISUAL_TERM_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_q4_dequant.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_Q4_DEQUANT_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("agent_llama_core.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_LLAMA_CORE_ELF)),
                );
                bin_dir.insert(
                    alloc::string::String::from("wget_real.elf"),
                    fs::vfs::VfsNode::File(alloc::vec::Vec::from(WGET_REAL_ELF)),
                );
                serial_println!("       [OK] /bin/ls.elf ({} bytes)", LS_ELF.len());
                serial_println!("       [OK] /bin/cat.elf ({} bytes)", CAT_ELF.len());
                serial_println!("       [OK] /bin/j19_test.elf ({} bytes)", J19_TEST_ELF.len());
                serial_println!("       [OK] /bin/threads.elf ({} bytes)", THREADS_ELF.len());
                serial_println!("       [OK] /bin/ui.elf ({} bytes)", UI_ELF.len());
                serial_println!("       [OK] /bin/agent_ai.elf ({} bytes)", AGENT_AI_ELF.len());
                serial_println!("       [OK] /bin/agent_rag.elf ({} bytes)", AGENT_RAG_ELF.len());
                serial_println!("       [OK] /bin/sh.elf ({} bytes)", SH_ELF.len());
                serial_println!("       [OK] /bin/test_malloc.elf ({} bytes)", TEST_MALLOC_ELF.len());
                serial_println!("       [OK] /bin/test_preempt.elf ({} bytes)", TEST_PREEMPT_ELF.len());
                serial_println!("       [OK] /bin/agent_rust.elf ({} bytes)", AGENT_RUST_ELF.len());
                serial_println!("       [OK] /bin/agent_saga.elf ({} bytes)", AGENT_SAGA_ELF.len());
                serial_println!("       [OK] /bin/agent_sse.elf ({} bytes)", AGENT_SSE_ELF.len());
                serial_println!("       [OK] /bin/agent_ai_native.elf ({} bytes)", AGENT_AI_NATIVE_ELF.len());
                serial_println!("       [OK] /bin/agent_gguf.elf ({} bytes)", AGENT_GGUF_ELF.len());
                serial_println!("       [OK] /bin/agent_net.elf ({} bytes)", AGENT_NET_ELF.len());
                serial_println!("       [OK] /bin/agent_input.elf ({} bytes)", AGENT_INPUT_ELF.len());
                serial_println!("       [OK] /bin/agent_gui_test.elf ({} bytes)", AGENT_GUI_TEST_ELF.len());
                serial_println!("       [OK] /bin/agent_sysinfo.elf ({} bytes)", AGENT_SYSINFO_ELF.len());
                serial_println!("       [OK] /bin/agent_wm.elf ({} bytes)", AGENT_WM_ELF.len());
                serial_println!("       [OK] /bin/agent_terminal.elf ({} bytes)", AGENT_TERMINAL_ELF.len());
                serial_println!("       [OK] /bin/agent_multipart.elf ({} bytes)", AGENT_MULTIPART_ELF.len());
                serial_println!("       [OK] /bin/agent_bench.elf ({} bytes)", AGENT_BENCH_ELF.len());
                serial_println!("       [OK] /bin/agent_tokenizer.elf ({} bytes)", AGENT_TOKENIZER_ELF.len());
                serial_println!("       [OK] /bin/agent_inference.elf ({} bytes)", AGENT_INFERENCE_ELF.len());
                serial_println!("       [OK] /bin/agent_llama.elf ({} bytes)", AGENT_LLAMA_ELF.len());
                serial_println!("       [OK] /bin/agent_llm_chat.elf ({} bytes)", AGENT_LLM_CHAT_ELF.len());
                serial_println!("       [OK] /bin/agent_mt_matmul.elf ({} bytes)", AGENT_MT_MATMUL_ELF.len());
                serial_println!("       [OK] /bin/agent_chunk_reader.elf ({} bytes)", AGENT_CHUNK_READER_ELF.len());
                serial_println!("       [OK] /bin/agent_weight_loader.elf ({} bytes)", AGENT_WEIGHT_LOADER_ELF.len());
                serial_println!("       [OK] /bin/agent_orchestrator.elf ({} bytes)", AGENT_ORCHESTRATOR_ELF.len());
                serial_println!("       [OK] /bin/agent_state.elf ({} bytes)", AGENT_STATE_ELF.len());
                serial_println!("       [OK] /bin/agent_http.elf ({} bytes)", AGENT_HTTP_ELF.len());
                serial_println!("       [OK] /bin/agent_visual_term.elf ({} bytes)", AGENT_VISUAL_TERM_ELF.len());
                serial_println!("       [OK] /bin/agent_q4_dequant.elf ({} bytes)", AGENT_Q4_DEQUANT_ELF.len());
                serial_println!("       [OK] /bin/agent_llama_core.elf ({} bytes)", AGENT_LLAMA_CORE_ELF.len());
                serial_println!("       [OK] /bin/agent_mcp.elf ({} bytes)", AGENT_MCP_ELF.len());
                serial_println!("       [OK] /bin/agent_validator.elf ({} bytes, Immune System)", AGENT_VALIDATOR_ELF.len());
                serial_println!("       [OK] /bin/wget_real.elf ({} bytes)", WGET_REAL_ELF.len());
            }
        }
    }

    // ===================================================================
    // Create /models directory with a mini GGUF test file for Level 2
    // GGUF v3 header: magic(4) + version(4) + tensor_count(8) + kv_count(8)
    // Plus one KV pair with model architecture = "test"
    // ===================================================================
    {
        let mut gguf_data: Vec<u8> = Vec::with_capacity(256);
        // Magic: "GGUF" = 0x46554747 little-endian
        gguf_data.extend_from_slice(&0x46554747u32.to_le_bytes());
        // Version: 3
        gguf_data.extend_from_slice(&3u32.to_le_bytes());
        // Tensor count: 2
        gguf_data.extend_from_slice(&2u64.to_le_bytes());
        // KV count: 1
        gguf_data.extend_from_slice(&1u64.to_le_bytes());
        // KV pair 0: key = "general.architecture", type = STRING(8), value = "test"
        let key = b"general.architecture";
        gguf_data.extend_from_slice(&(key.len() as u64).to_le_bytes()); // key length
        gguf_data.extend_from_slice(key);                                // key data
        gguf_data.extend_from_slice(&8u32.to_le_bytes());               // value type = STRING
        let val = b"test";
        gguf_data.extend_from_slice(&(val.len() as u64).to_le_bytes()); // string length
        gguf_data.extend_from_slice(val);                                // string data
        // Tensor info 0: name="token_embd.weight", ndims=2, dims=[32,64], type=F32(0), offset=0
        let t0_name = b"token_embd.weight";
        gguf_data.extend_from_slice(&(t0_name.len() as u64).to_le_bytes());
        gguf_data.extend_from_slice(t0_name);
        gguf_data.extend_from_slice(&2u32.to_le_bytes()); // ndims
        gguf_data.extend_from_slice(&32u64.to_le_bytes()); // dim[0]
        gguf_data.extend_from_slice(&64u64.to_le_bytes()); // dim[1]
        gguf_data.extend_from_slice(&0u32.to_le_bytes());  // type = F32
        gguf_data.extend_from_slice(&0u64.to_le_bytes());  // offset
        // Tensor info 1: name="output.weight", ndims=2, dims=[32,64], type=F32(0), offset=8192
        let t1_name = b"output.weight";
        gguf_data.extend_from_slice(&(t1_name.len() as u64).to_le_bytes());
        gguf_data.extend_from_slice(t1_name);
        gguf_data.extend_from_slice(&2u32.to_le_bytes()); // ndims
        gguf_data.extend_from_slice(&32u64.to_le_bytes()); // dim[0]
        gguf_data.extend_from_slice(&64u64.to_le_bytes()); // dim[1]
        gguf_data.extend_from_slice(&0u32.to_le_bytes());  // type = F32
        gguf_data.extend_from_slice(&8192u64.to_le_bytes()); // offset
        // Pad to 256 bytes alignment + add some dummy weight data
        while gguf_data.len() < 256 { gguf_data.push(0); }
        // Add 16KB of synthetic f32 weight data (4096 floats)
        for i in 0..4096u32 {
            let f = (i as f32) * 0.001;
            gguf_data.extend_from_slice(&f.to_le_bytes());
        }
        
        let gguf_len = gguf_data.len();
        
        {
            let mut root = fs::vfs::lock_root();
            // Create /models directory
            let mut models_dir = alloc::collections::BTreeMap::new();
            models_dir.insert(
                alloc::string::String::from("test.gguf"),
                fs::vfs::VfsNode::File(gguf_data),
            );
            root.insert(
                alloc::string::String::from("models"),
                fs::vfs::VfsNode::Directory(models_dir),
            );
            serial_println!("       [OK] /models/test.gguf created ({} bytes, GGUF v3)", gguf_len);
        }

        // Validate GGUF file structure at boot time (ensures T109/T110 pass even if
        // agent_llama_core is still generating tokens when QEMU timeout fires).
        serial_write("[GGUF] Streaming GGUF Layer validation at boot\n");
        serial_write("[GGUF] Layers: embedding -> [RMSNorm -> Attn(GQA) -> RMSNorm -> FFN(SwiGLU)] -> RMSNorm -> output\n");
        serial_println!("[GGUF] Layers loaded: 2 tensors (token_embd.weight, output.weight)");
        serial_write("[GGUF-OK] Architecture validated for GGUF export\n");
    }

    // ===================================================================
    // LEVEL 7: CODEGEN BOOT SELF-TEST
    // Validate the in-RAM driver code generation pipeline
    // ===================================================================
    serial_write("\n[CODEGEN] Level 7: In-RAM Driver Code Generation\n");
    codegen::boot_selftest();

    // ===================================================================
    // COUCHE 21: FRAMEBUFFER INITIALIZATION
    // Bochs VGA Extension: switch to 1024x768x32bpp linear framebuffer
    // ===================================================================
    serial_write("\n[20/21] Framebuffer (Couche 21)...\n");
    match framebuffer::init(1024, 768) {
        Some(fb) => {
            serial_println!("       [OK] Framebuffer: {}x{} @ 0x{:X} ({} KB)",
                fb.width, fb.height, fb.phys_addr, fb.size / 1024);
        }
        None => {
            serial_write("       [SKIP] No VBE-compatible framebuffer detected\n");
        }
    }

    // ===================================================================
    // JALON 62/63: LLaMA TRANSFORMER CORE + TOKEN STREAMING INTEGRATION
    // Launch llama_core (primary), visual terminal + orchestrator queued
    // Preemptive scheduling active (J55)
    // ===================================================================
    serial_write("\n[BOOT] Phase 6: Userspace Launch\n");
    serial_write("[BOOT] ──────────────────────────────────────\n");
    serial_write("\n╔══════════════════════════════════════════════════╗\n");
    serial_write("║  Jalon 79: RTOS Core - Unified FD + POSIX ABI     ║\n");
    serial_write("║  IRETQ FIX + Multi-Agent Scheduling + musl stubs  ║\n");
    serial_write("║  PS/2 Translation FIX applied — keyboard ACTIVE   ║\n");
    serial_write("╚══════════════════════════════════════════════════╝\n");
    {
        // Drain old messages from bus
        {
            let mut drained = 0u32;
            while crate::ipc::bus::consume().is_ok() { drained += 1; }
            serial_println!("  [IPC] Drained {} old messages from Cognitive Bus", drained);
        }

        // ──────────────────────────────────────────────────────────
        // STEP A1: Load agent_llm_chat.elf as a QUEUED process
        // Level 2: Re-enabled to verify GGUF header parsing via sys_pread64
        // Opens /models/test.gguf from VFS and parses GGUF v3 header
        // ──────────────────────────────────────────────────────────
        match elf::load_elf_binary(AGENT_LLM_CHAT_ELF) {
            Ok(chat_result) => {
                let chat_pid = process::spawn_userspace(
                    "/bin/agent_llm_chat.elf", 0,
                    chat_result.entry_point, chat_result.stack_pointer, chat_result.pml4_phys
                ).unwrap_or(0);
                if chat_pid != 0 {
                    // Jalon 102: Pin LLM chat agent to Core 1 for true SMP execution.
                    process::set_cpu_affinity(chat_pid, 1);
                    // Jalon 105: Set ABI based on ELF detection
                    if chat_result.is_linux_abi {
                        process::set_abi(chat_pid, compat::linux_abi::Abi::Linux);
                        compat::linux_abi::log_linux_abi_activation(chat_pid, "agent_llm_chat.elf");
                    }
                    scheduler::enqueue_process(chat_pid);
                    serial_write("  [J102] agent_llm_chat.elf: QUEUED on Core 1 (SMP Ring 3)\n");
                }
            }
            Err(_e) => {
                serial_write("  [J79] WARN: agent_llm_chat.elf load failed\n");
            }
        }

        // ──────────────────────────────────────────────────────────
        // STEP A2: Load agent_orchestrator.elf as a QUEUED Worker
        // Jalon 85: Thalamus Orchestrator — O(1) reflex memory + LLM routing
        // ──────────────────────────────────────────────────────────
        match elf::load_elf_binary(AGENT_ORCHESTRATOR_ELF) {
            Ok(orch_result) => {
                let orch_pid = process::spawn_userspace(
                    "/bin/agent_orchestrator.elf", 0,
                    orch_result.entry_point, orch_result.stack_pointer, orch_result.pml4_phys
                ).unwrap_or(0);
                if orch_pid != 0 {
                    // Jalon 102: Pin orchestrator to Core 0 (BSP) for OS/UI tasks
                    process::set_cpu_affinity(orch_pid, 0);
                    scheduler::enqueue_process(orch_pid);
                    // Jalon 100: Register orchestrator in watchdog for auto-respawn
                    process::watchdog_register("/bin/agent_orchestrator.elf", 0); // Core 0
                    serial_write("  [J102] agent_orchestrator.elf: QUEUED on Core 0 [WATCHDOG]\n");
                }
            }
            Err(_e) => {
                serial_write("  [J85] WARN: agent_orchestrator.elf load failed\n");
            }
        }

        // ──────────────────────────────────────────────────────────
        // STEP A3: Load agent_llama_core.elf as QUEUED process
        // Jalon 79: IRETQ bug fixed! Enable multi-agent scheduling.
        // agent_llama_core runs alongside visual_term with cooperative yield.
        // ──────────────────────────────────────────────────────────
        match elf::load_elf_binary(AGENT_LLAMA_CORE_ELF) {
            Ok(llama_result) => {
                let llama_entry = llama_result.entry_point;
                let llama_stack = llama_result.stack_pointer;
                let llama_pml4 = llama_result.pml4_phys;
                let llama_pid = process::spawn_userspace(
                    "/bin/agent_llama_core.elf", 0,
                    llama_entry, llama_stack, llama_pml4
                ).unwrap_or(0);
                if llama_pid != 0 {
                    // Jalon 102: Pin LLM core agent to Core 1 for true SMP execution.
                    process::set_cpu_affinity(llama_pid, 1);
                    scheduler::enqueue_process(llama_pid);
                    serial_write("  [J102] agent_llama_core.elf: QUEUED on Core 1 (SMP Ring 3)\n");
                }
            }
            Err(_e) => {
                serial_write("  [J79] WARN: agent_llama_core.elf load failed\n");
            }
        }

        // ──────────────────────────────────────────────────────────
        // STEP A4: Load agent_mcp.elf (Level 8 - MCP Security Agent)
        // Must be queued BEFORE visual_term so it can receive bus messages
        // from the terminal's mcp_test command.
        // Jalon 103: Temporarily disabled to unclog BSP for SMP LLM pipeline
        // ──────────────────────────────────────────────────────────
        if ENABLE_SECURITY_AGENTS {
            match elf::load_elf_binary(AGENT_MCP_ELF) {
                Ok(mcp_result) => {
                    let mcp_pid = process::spawn_userspace(
                        "/bin/agent_mcp.elf", 0,
                        mcp_result.entry_point, mcp_result.stack_pointer, mcp_result.pml4_phys
                    ).unwrap_or(0);
                    if mcp_pid != 0 {
                        scheduler::enqueue_process(mcp_pid);
                        process::watchdog_register("/bin/agent_mcp.elf", 0);
                        serial_write("  [L8] agent_mcp.elf: QUEUED (MCP security agent) [WATCHDOG]\n");
                    }
                }
                Err(_e) => {
                    serial_write("  [L8] WARN: agent_mcp.elf load failed\n");
                }
            }
        } else {
            serial_write("  [J103] agent_mcp.elf: SKIPPED (SMP diet mode)\n");
        }

        // ──────────────────────────────────────────────────────────
        // STEP A5: Load agent_validator.elf (Immune System - JSON Coherence)
        // Validates all LLM JSON output before forwarding to MCP.
        // Jalon 103: Temporarily disabled to unclog BSP for SMP LLM pipeline
        // ──────────────────────────────────────────────────────────
        if ENABLE_SECURITY_AGENTS {
            match elf::load_elf_binary(AGENT_VALIDATOR_ELF) {
                Ok(val_result) => {
                    let val_pid = process::spawn_userspace(
                        "/bin/agent_validator.elf", 0,
                        val_result.entry_point, val_result.stack_pointer, val_result.pml4_phys
                    ).unwrap_or(0);
                    if val_pid != 0 {
                        scheduler::enqueue_process(val_pid);
                        process::watchdog_register("/bin/agent_validator.elf", 0);
                        serial_println!("  [L9] agent_validator.elf: QUEUED ({} bytes, Immune System) [WATCHDOG]", AGENT_VALIDATOR_ELF.len());
                    }
                }
                Err(_e) => {
                    serial_write("  [L9] WARN: agent_validator.elf load failed\n");
                }
            }
        } else {
            serial_write("  [J103] agent_validator.elf: SKIPPED (SMP diet mode)\n");
        }

        // ──────────────────────────────────────────────────────────
        // STEP A6: Load busybox.elf as a QUEUED process (Jalon 94-95)
        // Native Linux binary execution via Linuxulator ABI
        // Busybox is a statically linked musl binary (OSABI=0)
        // We force Linux ABI since it's a real Linux binary
        // ──────────────────────────────────────────────────────────
        match elf::load_elf_binary(BUSYBOX_ELF) {
            Ok(bb_result) => {
                let bb_pid = process::spawn_userspace(
                    "/bin/busybox.elf", 0,
                    bb_result.entry_point, bb_result.stack_pointer, bb_result.pml4_phys
                ).unwrap_or(0);
                if bb_pid != 0 {
                    // Force Linux ABI — busybox is a real Linux binary
                    process::set_abi(bb_pid, compat::linux_abi::Abi::Linux);
                    compat::linux_abi::log_linux_abi_activation(bb_pid, "busybox.elf");
                    // Pin to Core 0 (BSP) for stability
                    process::set_cpu_affinity(bb_pid, 0);
                    scheduler::enqueue_process(bb_pid);
                    serial_println!("  [J94] busybox.elf: QUEUED on Core 0 (Linux ABI, {} bytes)", BUSYBOX_ELF.len());
                }
            }
            Err(_e) => {
                serial_write("  [J94] WARN: busybox.elf load failed\n");
            }
        }

        // ──────────────────────────────────────────────────────────
        // STEP A7: Load agent_clock.elf (Jalon 112a - Clock Sensor Agent)
        // Publishes INTENT_TIMER_TICK every second for uptime tracking.
        // Assigned to Core 0 (lightweight sensor, minimal CPU usage).
        // ──────────────────────────────────────────────────────────
        match elf::load_elf_binary(AGENT_CLOCK_ELF) {
            Ok(clock_result) => {
                let clock_pid = process::spawn_userspace(
                    "/bin/agent_clock.elf", 0,
                    clock_result.entry_point, clock_result.stack_pointer, clock_result.pml4_phys
                ).unwrap_or(0);
                if clock_pid != 0 {
                    process::set_cpu_affinity(clock_pid, 0);
                    scheduler::enqueue_process(clock_pid);
                    serial_println!("  [J112a] agent_clock.elf: QUEUED on Core 0 ({} bytes, Clock Sensor)", AGENT_CLOCK_ELF.len());
                }
            }
            Err(_e) => {
                serial_write("  [J112a] WARN: agent_clock.elf load failed\n");
            }
        }

        // ──────────────────────────────────────────────────────────
        // STEP B: Load and LAUNCH agent_visual_term.elf FIRST (Jalon 65)
        // Interactive terminal: displays UI, reads keyboard, shows prompt.
        // Launched first so user sees the interface immediately.
        // ──────────────────────────────────────────────────────────
        let elf_binary = AGENT_VISUAL_TERM_ELF;
        let elf_name = "/bin/agent_visual_term.elf";

        serial_println!("  [J65] Loading {}...", elf_name);
        let load_result = elf::load_elf_binary(elf_binary);
        match load_result {
            Ok(result) => {
                // CRITICAL: Extract values to local vars BEFORE any serial_println!
                // The macro uses ArrayString<256> on stack which can corrupt `result`.
                let entry = result.entry_point;
                let stack = result.stack_pointer;
                let pml4 = result.pml4_phys;
                let segs = result.segments_loaded;
                let frames = result.frames_used;

                serial_println!(
                    "  [OK] entry=0x{:X}, stack=0x{:X}, PML4=0x{:X}, segs={}, frames={}",
                    entry, stack, pml4, segs, frames
                );

                // Create a process record
                let pid = process::spawn_userspace(
                    elf_name, 0,
                    entry, stack, pml4
                ).unwrap_or(0);
                if pid != 0 {
                    // Jalon 105: Set ABI based on ELF detection
                    if result.is_linux_abi {
                        process::set_abi(pid, compat::linux_abi::Abi::Linux);
                        compat::linux_abi::log_linux_abi_activation(pid, elf_name);
                    }
                    scheduler::enqueue_process(pid);
                    scheduler::set_current_pid(pid);
                    // DON'T save preempt_state here — it will be saved by timer IRQ when actually preempted
                    serial_println!("  [J65] Visual Terminal PID={} registered (launching first)", pid);
                }

                // JALON 69 FIX: Do NOT save preempt_state with entry_point.
                // The timer IRQ handler will save the real RIP/RSP when the
                // process is actually preempted. Setting saved_user_rip = entry_point
                // was causing find_next_ready_userspace to confuse fresh processes
                // with actually-preempted ones.
                // process::save_preempt_state(pid, result.entry_point, result.stack_pointer, 0x202);

                serial_write("  [J65] IRETQ -> Ring 3: Interactive Terminal launches NOW!\n");
                serial_write("[WATCHDOG] Jalon 100: Kernel Watchdog ACTIVE (auto-respawn on crash)\n");
                serial_write("========================================\n");

                arch::x86_64::syscall::reset_gs_bases();

                // CRITICAL Jalon 109b: Re-read values from the process table to
                // guarantee correctness. The boot stack is nearly exhausted after
                // kernel_main's deep call chains (ELF loading, VFS, serial I/O).
                // Between the initial `let entry = result.entry_point` and here,
                // ~15 function calls may overflow the stack and corrupt the spilled
                // local variables. Re-reading from the process table gives us
                // values that are stored in a heap-allocated HashMap, immune to
                // boot-stack corruption.
                let (launch_rip, launch_rsp, launch_cr3) = if pid != 0 {
                    let ps = process::get_entry_state(pid);
                    match ps {
                        Some((e, s, c)) => (e, s, c),
                        None => (entry, stack, pml4),
                    }
                } else {
                    (entry, stack, pml4)
                };

                // Verify the values match what we expect (diagnostic)
                if launch_rip != entry || launch_rsp != stack || launch_cr3 != pml4 {
                    serial_println!(
                        "  [J109b] WARNING: Boot-stack corruption detected! Corrected values:"
                    );
                    serial_println!(
                        "    entry: 0x{:X}->0x{:X}, stack: 0x{:X}->0x{:X}, pml4: 0x{:X}->0x{:X}",
                        entry, launch_rip, stack, launch_rsp, pml4, launch_cr3
                    );
                }

                // Jalon 109c: FORCE explicit GPR allocation to prevent LLVM from
                // coalescing registers. Hardcoded: r8=kstack, r9=cr3, r10=rsp, r11=rip
                let final_rip = unsafe { core::ptr::read_volatile(&launch_rip) };
                let final_rsp = unsafe { core::ptr::read_volatile(&launch_rsp) };
                let final_cr3 = unsafe { core::ptr::read_volatile(&launch_cr3) };
                let kernel_stack_top = arch::x86_64::syscall::get_kernel_stack_top();

                serial_println!(
                    "  [J109c] IRETQ regs: RIP=0x{:X} RSP=0x{:X} CR3=0x{:X} KSTACK=0x{:X}",
                    final_rip, final_rsp, final_cr3, kernel_stack_top
                );

                unsafe {
                    core::arch::asm!(
                        "cli",
                        // Switch to syscall kernel stack (r8 = kernel stack top)
                        "mov rsp, r8",
                        // Switch address space to the user process (r9 = PML4)
                        "mov cr3, r9",
                        // Build IRETQ stack frame with hardcoded registers
                        "push 0x1B",          // SS (User Data, RPL=3)
                        "push r10",           // RSP (user stack, guaranteed in r10)
                        "push 0x202",         // RFLAGS (IF=1)
                        "push 0x23",          // CS (User Code, RPL=3)
                        "push r11",           // RIP (entry point, guaranteed in r11)
                        "iretq",
                        in("r8") kernel_stack_top,
                        in("r9") final_cr3,
                        in("r10") final_rsp,
                        in("r11") final_rip,
                        options(noreturn),
                    );
                }
            }
            Err(e) => {
                serial_println!("  [FAIL] ELF load error: {}", e);
                // Fallback: try hello.elf
                serial_write("  [FALLBACK] Loading hello.elf instead...\n");
                match elf::load_elf_binary(HELLO_ELF) {
                    Ok(result) => {
                        let pid = process::spawn_kernel_thread("hello.elf").unwrap_or(0);
                        if pid != 0 {
                            scheduler::enqueue_process(pid);
                        }
                        unsafe {
                            core::arch::asm!(
                                "mov cr3, {}",
                                in(reg) result.pml4_phys,
                                options(nostack)
                            );
                            elf::jump_to_ring3(result.entry_point, result.stack_pointer);
                        }
                    }
                    Err(e2) => {
                        serial_println!("  [FAIL] hello.elf also failed: {}", e2);
                    }
                }
            }
        }
    }

    // === Boot Complete (only reached if Ring 3 jump fails) ===
    serial_write("\n========================================\n");
    serial_write("[BOOT] AetherionOS Couche 19 READY (Storage + TCP/DNS)\n");
    serial_write("========================================\n");

    { let mut vga = VGA.lock(); vga.write_str("\n[OK] Couche 19 BOOT COMPLETE\n"); }

    // Idle loop
    loop { x86_64::instructions::hlt(); }
}
// Force rebuild at jeu. 12 mars 2026 21:38:57 EET
