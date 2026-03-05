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

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use linked_list_allocator::Heap;

// ============================================================
// Syscall Numbers (Linux x86_64 ABI, AetherionOS compatible)
// ============================================================
pub const SYS_READ:         u64 = 0;
pub const SYS_WRITE:        u64 = 1;
pub const SYS_MMAP:         u64 = 9;
pub const SYS_BRK:          u64 = 12;
pub const SYS_GETPID:       u64 = 20;
pub const SYS_YIELD:        u64 = 24;
pub const SYS_CLONE:        u64 = 56;
pub const SYS_EXIT:         u64 = 60;
pub const SYS_WAIT:         u64 = 61;
pub const SYS_BUS_PUBLISH:  u64 = 201;
pub const SYS_VGA_WRITE:    u64 = 202;

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

/// Write a colored character to VGA text buffer.
pub fn sys_vga_write(row: u32, col: u32, color_char: u64) -> i64 {
    syscall3(SYS_VGA_WRITE, row as u64, col as u64, color_char) as i64
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

/// Initial heap size requested from the kernel (64 KiB).
const INITIAL_HEAP_SIZE: usize = 64 * 1024;

/// Maximum heap growth per extension (128 KiB).
const HEAP_GROW_SIZE: usize = 128 * 1024;

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
// ============================================================

extern "C" {
    fn main() -> i64;
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let code = unsafe { main() };
    sys_exit(code as i32);
}
