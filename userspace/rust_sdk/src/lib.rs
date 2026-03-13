//! AetherionOS Rust Userspace SDK v1.0
//!
//! Provides:
//! - Raw syscall wrappers (sys_write, sys_brk, sys_exit, sys_getpid, sys_bus_publish, sys_yield)
//! - A global heap allocator backed by `sys_brk` + `linked_list_allocator`
//! - Panic handler that writes to stderr and exits
//! - `_start` entry point that calls user's `main()`
//!
//! # Usage
//! ```rust
//! #![no_std]
//! #![no_main]
//! extern crate alloc;
//! use aetherion_sdk::*;
//!
//! #[no_mangle]
//! pub extern "C" fn main() -> i64 {
//!     let v = alloc::vec![1, 2, 3];
//!     sys_write(1, b"Hello from Rust!\n");
//!     0
//! }
//! ```

#![no_std]
#![feature(alloc_error_handler)]
#![feature(naked_functions)]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use linked_list_allocator::Heap;

// ============================================================
// Compiler built-in memory functions required by no_std
// ============================================================
// The Rust compiler emits calls to these for array init, copy, etc.
// We provide them here in the SDK so every agent gets them automatically.
// IMPORTANT: Do NOT define these in individual agents — it causes
// "symbol multiply defined" errors.
// IMPORTANT: Do NOT use -Z build-std-features=compiler-builtins-mem
// with these definitions — it would also cause duplicate symbols.

#[no_mangle]
pub unsafe extern "C" fn memset(dest: *mut u8, c: i32, n: usize) -> *mut u8 {
    let mut i = 0;
    while i < n {
        *dest.add(i) = c as u8;
        i += 1;
    }
    dest
}

#[no_mangle]
pub unsafe extern "C" fn memcpy(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    let mut i = 0;
    while i < n {
        *dest.add(i) = *src.add(i);
        i += 1;
    }
    dest
}

#[no_mangle]
pub unsafe extern "C" fn memmove(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if (dest as usize) < (src as usize) {
        memcpy(dest, src, n)
    } else {
        let mut i = n;
        while i > 0 {
            i -= 1;
            *dest.add(i) = *src.add(i);
        }
        dest
    }
}

#[no_mangle]
pub unsafe extern "C" fn memcmp(s1: *const u8, s2: *const u8, n: usize) -> i32 {
    let mut i = 0;
    while i < n {
        let a = *s1.add(i);
        let b = *s2.add(i);
        if a != b {
            return a as i32 - b as i32;
        }
        i += 1;
    }
    0
}

// ============================================================
// Syscall Numbers (Linux x86_64 ABI, AetherionOS compatible)
// ============================================================
pub const SYS_READ:         u64 = 0;
pub const SYS_WRITE:        u64 = 1;
pub const SYS_OPEN:         u64 = 2;
pub const SYS_CLOSE:        u64 = 3;
pub const SYS_MMAP:         u64 = 9;
pub const SYS_BRK:          u64 = 12;
pub const SYS_GETPID:       u64 = 20;
pub const SYS_YIELD:        u64 = 24;
pub const SYS_CLONE:        u64 = 56;
pub const SYS_EXIT:         u64 = 60;
pub const SYS_WAIT:         u64 = 61;
pub const SYS_BUS_PUBLISH:  u64 = 201;
pub const SYS_BUS_CONSUME:  u64 = 203;
pub const SYS_VGA_WRITE:    u64 = 202;
pub const SYS_MMAP_FILE:    u64 = 240;
pub const SYS_NET_PING:     u64 = 210;
pub const SYS_GETHOSTBYNAME:u64 = 211;
pub const SYS_SOCKET:       u64 = 41;
pub const SYS_TCP_CONNECT:  u64 = 42;
pub const SYS_SENDTO:       u64 = 44;
pub const SYS_RECVFROM:     u64 = 45;
pub const SYS_TCP_SHUTDOWN: u64 = 47;
pub const SYS_TCP_READ:     u64 = 212;
pub const SYS_MMAP_FB:      u64 = 10;
pub const SYS_POLL_HID:     u64 = 11;
pub const SYS_FB_FILL_RECT: u64 = 220;
pub const SYS_FB_DRAW_CHAR: u64 = 221;
pub const SYS_FB_DRAW_STR:  u64 = 222;
pub const SYS_FB_GET_INFO:  u64 = 223;
pub const SYS_RDTSC:        u64 = 230;

// POSIX open flags
pub const O_RDONLY: u32 = 0;
pub const O_WRONLY: u32 = 1;
pub const O_RDWR: u32   = 2;
pub const O_CREAT: u32  = 0o100;  // 64
pub const O_TRUNC: u32  = 0o1000; // 512

// ============================================================
// Raw Syscall Primitives
//
// CRITICAL: clobber list includes ALL caller-saved registers
// because AetherionOS kernel syscall_entry does not preserve them.
// ============================================================

#[inline(always)]
pub fn syscall0(nr: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") nr => ret,
            out("rcx") _,
            out("r11") _,
            out("r8") _,
            out("r9") _,
            out("r10") _,
            lateout("rdi") _,
            lateout("rsi") _,
            lateout("rdx") _,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
pub fn syscall1(nr: u64, a1: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a1,
            out("rcx") _,
            out("r11") _,
            out("r8") _,
            out("r9") _,
            out("r10") _,
            lateout("rsi") _,
            lateout("rdx") _,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
pub fn syscall2(nr: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a1,
            in("rsi") a2,
            out("rcx") _,
            out("r11") _,
            out("r8") _,
            out("r9") _,
            out("r10") _,
            lateout("rdx") _,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
pub fn syscall3(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") nr => ret,
            in("rdi") a1,
            in("rsi") a2,
            in("rdx") a3,
            out("rcx") _,
            out("r11") _,
            out("r8") _,
            out("r9") _,
            out("r10") _,
            options(nostack),
        );
    }
    ret
}

// ============================================================
// High-Level Syscall Wrappers
// ============================================================

/// Write bytes to a file descriptor. Returns bytes written.
pub fn sys_write(fd: u32, buf: &[u8]) -> i64 {
    syscall3(SYS_WRITE, fd as u64, buf.as_ptr() as u64, buf.len() as u64) as i64
}

/// Read bytes from a file descriptor. Returns bytes read.
pub fn sys_read(fd: u32, buf: &mut [u8]) -> i64 {
    syscall3(SYS_READ, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) as i64
}

/// Set/get the program break.
/// - `brk(0)` returns the current break address.
/// - `brk(addr)` sets the break, allocating pages as needed.
pub fn sys_brk(addr: u64) -> u64 {
    syscall1(SYS_BRK, addr)
}

/// Terminate the current process with exit code.
pub fn sys_exit(code: i32) -> ! {
    syscall1(SYS_EXIT, code as u64);
    // Unreachable safety net
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
    }
}

/// Get the current process PID.
pub fn sys_getpid() -> u64 {
    syscall1(SYS_GETPID, 0)
}

/// Yield the CPU to the scheduler.
pub fn sys_yield() {
    syscall1(SYS_YIELD, 0);
}

/// Publish an intent to the Cognitive Bus.
/// Returns 0 on success.
pub fn sys_bus_publish(intent: u64, priority: u32, data: u64) -> i64 {
    syscall3(SYS_BUS_PUBLISH, intent, priority as u64, data) as i64
}

/// Consume a message from the Cognitive Bus (Jalon 71).
/// msg_buf: pointer to a 48-byte buffer to receive the IntentMessage.
/// 
/// Buffer layout (C struct compatible):
///   offset 0:  u32 source (ComponentId)
///   offset 4:  u32 destination (ComponentId)
///   offset 8:  u32 intent_id
///   offset 12: u32 priority
///   offset 16: u64 payload
///   offset 24: u64 timestamp
///
/// Returns:
///   0 on success (message copied to buffer)
///   -EAGAIN (-11) if bus is empty
///   -EFAULT (-14) if buffer address is invalid
pub fn sys_bus_consume(msg_buf: &mut [u64; 6]) -> i64 {
    syscall1(SYS_BUS_CONSUME, msg_buf.as_mut_ptr() as u64) as i64
}

/// Write a colored character to VGA text buffer.
pub fn sys_vga_write(row: u32, col: u32, color_char: u64) -> i64 {
    syscall3(SYS_VGA_WRITE, row as u64, col as u64, color_char) as i64
}

/// Open a file. Returns a file descriptor (>= 0) or negative error.
/// `path` must be a null-terminated string.
pub fn sys_open(path: &[u8], flags: u32) -> i64 {
    syscall2(SYS_OPEN, path.as_ptr() as u64, flags as u64) as i64
}

/// Close a file descriptor. Returns 0 on success.
pub fn sys_close(fd: u32) -> i64 {
    syscall1(SYS_CLOSE, fd as u64) as i64
}

/// Write bytes to a file descriptor (by fd number). Returns bytes written.
pub fn sys_write_fd(fd: u32, buf: &[u8]) -> i64 {
    syscall3(SYS_WRITE, fd as u64, buf.as_ptr() as u64, buf.len() as u64) as i64
}

/// Read bytes from a file descriptor (by fd number). Returns bytes read.
pub fn sys_read_fd(fd: u32, buf: &mut [u8]) -> i64 {
    syscall3(SYS_READ, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) as i64
}

// ============================================================
// ACHA Episodic Memory Structures (Jalon 31)
// ============================================================

/// Saga: a single episodic memory record.
///
/// Represents one completed action/intent with its outcome.
/// Designed for binary serialization to disk (fixed 16 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Saga {
    pub timestamp: u64,    // Monotonic counter or TSC value
    pub intent_id: u32,    // Cognitive Bus intent that was executed
    pub success: u8,       // 1 = OK, 0 = FAIL
    pub _padding: u8,      // Alignment padding
    pub data_hash: u16,    // Truncated hash of result data
}

impl Saga {
    pub const SIZE: usize = 16;

    pub fn new(timestamp: u64, intent_id: u32, success: bool, data_hash: u16) -> Self {
        Saga {
            timestamp,
            intent_id,
            success: if success { 1 } else { 0 },
            _padding: 0,
            data_hash,
        }
    }

    /// Serialize to a byte array for writing to disk.
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        let ts = self.timestamp.to_le_bytes();
        buf[0..8].copy_from_slice(&ts);
        let id = self.intent_id.to_le_bytes();
        buf[8..12].copy_from_slice(&id);
        buf[12] = self.success;
        buf[13] = self._padding;
        let dh = self.data_hash.to_le_bytes();
        buf[14..16].copy_from_slice(&dh);
        buf
    }

    /// Deserialize from a byte array.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE {
            return None;
        }
        Some(Saga {
            timestamp: u64::from_le_bytes([buf[0], buf[1], buf[2], buf[3],
                                           buf[4], buf[5], buf[6], buf[7]]),
            intent_id: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            success: buf[12],
            _padding: buf[13],
            data_hash: u16::from_le_bytes([buf[14], buf[15]]),
        })
    }

    /// MAGIC header for saga files (4 bytes): "SAGA"
    pub const MAGIC: [u8; 4] = [b'S', b'A', b'G', b'A'];
    pub const VERSION: u8 = 1;
}

/// AlmanacEntry: registry entry for a known agent.
///
/// Fixed 12-byte record tracking agent identity and trust.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AlmanacEntry {
    pub agent_id: u32,     // Unique agent identifier
    pub trust_score: u8,   // 0-255 trust level
    pub _padding: [u8; 3], // Alignment
    pub memory_kb: u32,    // Memory usage in KiB
}

impl AlmanacEntry {
    pub const SIZE: usize = 12;

    pub fn new(agent_id: u32, trust_score: u8, memory_kb: u32) -> Self {
        AlmanacEntry {
            agent_id,
            trust_score,
            _padding: [0; 3],
            memory_kb,
        }
    }

    /// Serialize to a byte array for writing to disk.
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        let id = self.agent_id.to_le_bytes();
        buf[0..4].copy_from_slice(&id);
        buf[4] = self.trust_score;
        buf[5..8].copy_from_slice(&self._padding);
        let mem = self.memory_kb.to_le_bytes();
        buf[8..12].copy_from_slice(&mem);
        buf
    }

    /// Deserialize from a byte array.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::SIZE {
            return None;
        }
        Some(AlmanacEntry {
            agent_id: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            trust_score: buf[4],
            _padding: [buf[5], buf[6], buf[7]],
            memory_kb: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
        })
    }

    /// MAGIC header for almanac files (4 bytes): "ALMC"
    pub const MAGIC: [u8; 4] = [b'A', b'L', b'M', b'C'];
    pub const VERSION: u8 = 1;
}

// ============================================================
// Network Syscalls (J37)
// ============================================================

/// Ping an IP address. ip is big-endian packed IPv4 (e.g. 0x08080808 for 8.8.8.8).
/// Returns RTT in microseconds or negative error code.
pub fn sys_net_ping(ip: u32, seq: u16) -> i64 {
    syscall2(SYS_NET_PING, ip as u64, seq as u64) as i64
}

/// DNS query. Returns packed IPv4 in big-endian (network byte order) or 0 on failure.
pub fn sys_gethostbyname(name: &[u8]) -> u32 {
    syscall1(SYS_GETHOSTBYNAME, name.as_ptr() as u64) as u32
}

/// Create a TCP socket. domain=2 (AF_INET), sock_type=1 (SOCK_STREAM).
/// Returns file descriptor or negative error.
pub fn sys_socket(domain: u32, sock_type: u32, protocol: u32) -> i64 {
    syscall3(SYS_SOCKET, domain as u64, sock_type as u64, protocol as u64) as i64
}

/// TCP connect: fd is the socket, ip_packed is big-endian IPv4, port is the port number.
/// ip_packed encodes as (a<<24 | b<<16 | c<<8 | d).
pub fn sys_tcp_connect(fd: u32, ip_packed: u32, port: u16) -> i64 {
    syscall3(SYS_TCP_CONNECT, fd as u64, ip_packed as u64, port as u64) as i64
}

/// Send data over a TCP socket.
/// The kernel expects: first 8 bytes = length (u64 LE), followed by data.
/// Returns bytes sent or negative error.
pub fn sys_tcp_send(fd: u32, data: &[u8]) -> i64 {
    // Build a buffer: [len:u64][data...]
    // We'll use a stack buffer for small sends
    let total = 8 + data.len();
    if total > 4096 { return -1; }
    let mut buf = [0u8; 4096];
    let len_bytes = (data.len() as u64).to_le_bytes();
    buf[0..8].copy_from_slice(&len_bytes);
    buf[8..8+data.len()].copy_from_slice(data);
    syscall3(SYS_SENDTO, fd as u64, buf.as_ptr() as u64, total as u64) as i64
}

/// Read data from a TCP socket.
/// Returns bytes read or negative error.
pub fn sys_tcp_read(fd: u32, buf: &mut [u8]) -> i64 {
    syscall3(SYS_TCP_READ, fd as u64, buf.as_mut_ptr() as u64, buf.len() as u64) as i64
}

/// Shutdown a TCP socket.
pub fn sys_tcp_shutdown(fd: u32) -> i64 {
    syscall1(SYS_TCP_SHUTDOWN, fd as u64) as i64
}

// ============================================================
// Framebuffer Syscalls (J39)
// ============================================================

/// Map the framebuffer into user space. Returns virtual address.
/// info_buf: pointer to 4 u64s [vaddr, width, height, stride]
pub fn sys_mmap_fb(info_buf: &mut [u64; 4]) -> u64 {
    syscall1(SYS_MMAP_FB, info_buf.as_mut_ptr() as u64)
}

/// Fill a rectangle on the framebuffer.
/// Args packed: x | (y << 16) | (w << 32) | (h << 48) and color as ARGB.
pub fn sys_fb_fill_rect(x: u32, y: u32, w: u32, h: u32, color: u32) -> i64 {
    let packed_xy = (x as u64) | ((y as u64) << 16);
    let packed_wh = (w as u64) | ((h as u64) << 16);
    syscall3(SYS_FB_FILL_RECT, packed_xy, packed_wh, color as u64) as i64
}

/// Draw a character at (x, y) with color.
pub fn sys_fb_draw_char(x: u32, y: u32, ch: u8, color: u32) -> i64 {
    let packed = (x as u64) | ((y as u64) << 16) | ((ch as u64) << 32);
    syscall2(SYS_FB_DRAW_CHAR, packed, color as u64) as i64
}

/// Draw a string at (x, y) with color.
pub fn sys_fb_draw_string(x: u32, y: u32, s: &[u8], color: u32) -> i64 {
    let packed_pos = (x as u64) | ((y as u64) << 16);
    let packed_str = (s.as_ptr() as u64) | ((s.len() as u64) << 48);
    syscall3(SYS_FB_DRAW_STR, packed_pos, packed_str, color as u64) as i64
}

/// Get framebuffer info. Returns 0 if no FB, or fills info_buf.
pub fn sys_fb_get_info(info_buf: &mut [u64; 4]) -> u64 {
    syscall1(SYS_FB_GET_INFO, info_buf.as_mut_ptr() as u64)
}

// ============================================================
// HID Syscalls (J38)
// ============================================================

/// Poll one HID event. Returns packed u64:
/// [type: u8, buttons: u8, dx: i16, dy: i16, scancode: u8, _pad: u8]
/// Returns 0 if no event available.
pub fn sys_poll_hid() -> u64 {
    syscall1(SYS_POLL_HID, 0)
}

// ============================================================
// Misc Syscalls (J40)
// ============================================================

/// Read TSC (timestamp counter). Returns 64-bit cycle count.
pub fn sys_rdtsc() -> u64 {
    syscall0(SYS_RDTSC)
}

/// Anonymous mmap: allocate `len` bytes of virtual memory.
/// Returns the virtual address of the mapped region.
pub fn sys_mmap(len: usize) -> u64 {
    syscall3(SYS_MMAP, 0, len as u64, 0x3) // PROT_READ|PROT_WRITE
}

/// File-backed mmap: creates a virtual memory mapping backed by a file.
/// Pages are loaded on-demand by the kernel's page fault handler.
/// fd: file descriptor (must be open on a /disk/ file)
/// length: mapping size in bytes
/// offset: starting offset into the file
/// Returns the virtual address, or negative error code.
pub fn sys_mmap_file(fd: u32, length: u64, offset: u64) -> u64 {
    syscall3(SYS_MMAP_FILE, fd as u64, length, offset)
}

/// Seek a file descriptor to a given offset. Returns new offset.
/// whence: 0=SEEK_SET, 1=SEEK_CUR
pub fn sys_lseek(fd: u32, offset: i64, whence: u32) -> i64 {
    syscall3(8, fd as u64, offset as u64, whence as u64) as i64
}

/// Clone (create a lightweight thread sharing the parent's address space).
/// stack_ptr: top of a pre-allocated stack for the new thread.
/// The function pointer must be written at (stack_ptr - 8) before calling.
/// Returns: child PID to parent, 0 to child.
pub fn sys_clone(stack_ptr: u64) -> i64 {
    syscall1(SYS_CLONE, stack_ptr) as i64
}

/// Wait for a child process/thread to terminate.
/// pid: child PID to wait for (0 = wait for any child).
/// Returns: exit code of the child, or negative error.
pub fn sys_wait(pid: u64) -> i64 {
    syscall1(SYS_WAIT, pid) as i64
}

/// Yield the CPU to another ready process/thread.
pub fn sys_yield_cpu() -> i64 {
    syscall0(SYS_YIELD) as i64
}

// ============================================================
// Print Utilities
// ============================================================

/// Print a string to stdout (fd 1).
pub fn print(s: &str) {
    sys_write(1, s.as_bytes());
}

/// Print a string to stdout followed by newline.
pub fn println(s: &str) {
    print(s);
    print("\n");
}

/// Print an unsigned integer to stdout.
pub fn print_u64(val: u64) {
    if val == 0 {
        print("0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 20;
    let mut v = val;
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    sys_write(1, &buf[i..20]);
}

/// Print a hex value to stdout.
pub fn print_hex(val: u64) {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut buf = [b'0'; 18]; // "0x" + 16 hex digits
    buf[0] = b'0';
    buf[1] = b'x';
    for i in 0..16 {
        buf[2 + i] = HEX[((val >> (60 - i * 4)) & 0xF) as usize];
    }
    sys_write(1, &buf);
}

// ============================================================
// Global Heap Allocator backed by sys_brk
// ============================================================

/// Initial heap size requested from the kernel.
/// 4 MiB is sufficient for all agents. The allocator grows on demand
/// via sys_brk in 4 MiB increments. The LLM agent uses streaming
/// (layer-by-layer from disk) and never loads the full model in RAM.
const INITIAL_HEAP_SIZE: usize = 4 * 1024 * 1024;

/// Heap growth per extension (4 MiB).
/// On-demand growth avoids pre-allocating huge regions that OOM the kernel.
const HEAP_GROW_SIZE: usize = 4 * 1024 * 1024;

/// The AetherionOS global allocator.
/// Uses `linked_list_allocator::Heap` internally, backed by `sys_brk`.
struct AetherionAllocator {
    heap: spin::Mutex<Heap>,
    initialized: AtomicBool,
    heap_end: AtomicUsize,
}

// We need a simple spinlock. Let's implement it inline since we can't
// depend on the `spin` crate in this context.
mod spin {
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicBool, Ordering};

    pub struct Mutex<T> {
        locked: AtomicBool,
        data: UnsafeCell<T>,
    }

    unsafe impl<T: Send> Send for Mutex<T> {}
    unsafe impl<T: Send> Sync for Mutex<T> {}

    impl<T> Mutex<T> {
        pub const fn new(data: T) -> Self {
            Self {
                locked: AtomicBool::new(false),
                data: UnsafeCell::new(data),
            }
        }

        pub fn lock(&self) -> MutexGuard<'_, T> {
            while self.locked.compare_exchange_weak(
                false, true, Ordering::Acquire, Ordering::Relaxed
            ).is_err() {
                core::hint::spin_loop();
            }
            MutexGuard { mutex: self }
        }
    }

    pub struct MutexGuard<'a, T> {
        mutex: &'a Mutex<T>,
    }

    impl<'a, T> core::ops::Deref for MutexGuard<'a, T> {
        type Target = T;
        fn deref(&self) -> &T {
            unsafe { &*self.mutex.data.get() }
        }
    }

    impl<'a, T> core::ops::DerefMut for MutexGuard<'a, T> {
        fn deref_mut(&mut self) -> &mut T {
            unsafe { &mut *self.mutex.data.get() }
        }
    }

    impl<'a, T> Drop for MutexGuard<'a, T> {
        fn drop(&mut self) {
            self.mutex.locked.store(false, Ordering::Release);
        }
    }
}

#[global_allocator]
static ALLOCATOR: AetherionAllocator = AetherionAllocator {
    heap: spin::Mutex::new(Heap::empty()),
    initialized: AtomicBool::new(false),
    heap_end: AtomicUsize::new(0),
};

impl AetherionAllocator {
    /// Initialize the heap by requesting memory from the kernel via sys_brk.
    fn ensure_init(&self) {
        if self.initialized.load(Ordering::Relaxed) {
            return;
        }
        // Get current break
        let base = sys_brk(0);
        if base == 0 {
            return; // brk failed
        }
        // Extend the break by INITIAL_HEAP_SIZE
        let new_end = sys_brk(base + INITIAL_HEAP_SIZE as u64);
        if new_end <= base {
            return; // extension failed
        }
        let size = (new_end - base) as usize;
        unsafe {
            self.heap.lock().init(base as *mut u8, size);
        }
        self.heap_end.store(new_end as usize, Ordering::Release);
        self.initialized.store(true, Ordering::Release);
    }

    /// Grow the heap by requesting more memory from the kernel.
    fn grow_heap(&self, min_size: usize) -> bool {
        let grow = if min_size > HEAP_GROW_SIZE { min_size } else { HEAP_GROW_SIZE };
        // Align grow to page size
        let grow_aligned = (grow + 4095) & !4095;
        let current_end = self.heap_end.load(Ordering::Acquire) as u64;
        let new_end = sys_brk(current_end + grow_aligned as u64);
        if new_end <= current_end {
            return false;
        }
        let actual_grow = (new_end - current_end) as usize;
        unsafe {
            self.heap.lock().extend(actual_grow);
        }
        self.heap_end.store(new_end as usize, Ordering::Release);
        true
    }
}

unsafe impl GlobalAlloc for AetherionAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.ensure_init();
        // Try to allocate
        match self.heap.lock().allocate_first_fit(layout) {
            Ok(ptr) => ptr.as_ptr(),
            Err(_) => {
                // Heap full — grow and retry
                let needed = layout.size() + layout.align();
                if self.grow_heap(needed) {
                    match self.heap.lock().allocate_first_fit(layout) {
                        Ok(ptr) => ptr.as_ptr(),
                        Err(_) => core::ptr::null_mut(),
                    }
                } else {
                    core::ptr::null_mut()
                }
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(nonnull) = core::ptr::NonNull::new(ptr) {
            self.heap.lock().deallocate(nonnull, layout);
        }
    }
}

// ============================================================
// Panic & Alloc Error Handlers
// ============================================================

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    sys_write(2, b"\n[PANIC] Rust agent panic: ");
    if let Some(loc) = info.location() {
        sys_write(2, loc.file().as_bytes());
        sys_write(2, b":");
        print_u64_to_fd(2, loc.line() as u64);
    }
    sys_write(2, b"\n");
    sys_exit(1);
}

#[alloc_error_handler]
fn alloc_error(layout: Layout) -> ! {
    sys_write(2, b"\n[ALLOC ERROR] Failed to allocate ");
    print_u64_to_fd(2, layout.size() as u64);
    sys_write(2, b" bytes\n");
    sys_exit(2);
}

fn print_u64_to_fd(fd: u32, val: u64) {
    if val == 0 {
        sys_write(fd, b"0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 20;
    let mut v = val;
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    sys_write(fd, &buf[i..20]);
}

// ============================================================
// _start Entry Point
// Calls user-defined main() and exits with its return value.
//
// IMPORTANT: _start is the ELF entry point, reached via IRETQ from the
// kernel. RSP is ALREADY 16-byte aligned on entry (page-aligned stack top).
// Unlike a normal function (where RSP = 8 mod 16 after the CALL pushes
// the return address), _start sees RSP = 0 mod 16. We must NOT add any
// extra push before calling main(); the CALL instruction itself will push
// the return address, giving main() the standard ABI alignment (RSP = 8
// mod 16 on entry). Using inline asm ensures the compiler doesn't insert
// an extra push rax that would break stack alignment for movaps.
// ============================================================

extern "C" {
    fn main() -> i64;
}

/// Raw assembly _start: call main, then syscall exit with return value.
/// Ensures no compiler-generated stack realignment that would break
/// the 16-byte alignment invariant required by SSE movaps instructions.
#[naked]
#[no_mangle]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::asm!(
        // RSP is 16-byte aligned here (from IRETQ).
        // CALL pushes 8-byte return address -> RSP becomes 8 mod 16.
        // This is exactly what the x86_64 SysV ABI expects on function entry.
        "call main",
        // main() returned in RAX. Use it as exit code.
        "mov rdi, rax",      // exit code = main() return value
        "mov eax, 60",       // syscall 60 = exit
        "syscall",
        // Unreachable: halt loop as safety net
        "2: hlt",
        "jmp 2b",
        options(noreturn),
    )
}
