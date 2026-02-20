// src/lib.rs - Library interface for Aetherion Kernel
// Permet l'utilisation en tant que lib pour les tests

#![no_std]
#![cfg_attr(test, no_main)]
#![feature(abi_x86_interrupt)]
#![feature(custom_test_frameworks)]
#![feature(alloc_error_handler)]
#![test_runner(crate::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

// Re-exports pour les tests
pub mod arch;
pub mod security;
pub mod memory;

pub mod tests;

#[cfg(test)]
use core::panic::PanicInfo;

/// Test runner pour cargo test
pub fn test_runner(tests: &[&dyn Fn()]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test();
    }
    serial_println!("[TEST] All tests passed!");
}

/// Panic handler pour les tests
#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("[FAILED] {}", info);
    loop {}
}

/// Point d'entrée pour tests
#[cfg(test)]
#[no_mangle]
pub extern "C" fn _start() -> ! {
    test_main();
    loop {}
}

// ===== Macros =====

/// Macro println! pour output serial
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        {
            use core::fmt::Write;
            let mut s = $crate::util::ArrayWriter::<256>::new();
            let _ = core::write!(&mut s, $($arg)*);
            $crate::serial_write(s.as_str());
        }
    };
}

#[macro_export]
macro_rules! serial_println {
    () => {
        $crate::serial_write("\n")
    };
    ($($arg:tt)*) => {
        {
            use core::fmt::Write;
            let mut s = $crate::util::ArrayWriter::<256>::new();
            let _ = core::write!(&mut s, $($arg)*);
            $crate::serial_write(s.as_str());
            $crate::serial_write("\n");
        }
    };
}

/// Test case macro
#[macro_export]
macro_rules! test_case {
    ($name:ident) => {
        #[test_case]
        fn $name() {
            serial_print!("[TEST] {} ... ", stringify!($name));
            $name();
            serial_println!("[OK]");
        }
    };
}

// ===== Utility =====

pub mod util {
    use core::fmt;
    
    /// Writer vers buffer fixe pour no_std
    pub struct ArrayWriter<const N: usize> {
        buffer: [u8; N],
        pos: usize,
    }
    
    impl<const N: usize> ArrayWriter<N> {
        pub const fn new() -> Self {
            Self { buffer: [0; N], pos: 0 }
        }
        
        pub fn as_str(&self) -> &str {
            core::str::from_utf8(&self.buffer[..self.pos]).unwrap_or("")
        }
    }
    
    impl<const N: usize> fmt::Write for ArrayWriter<N> {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let bytes = s.as_bytes();
            let remaining = N.saturating_sub(self.pos);
            let to_write = bytes.len().min(remaining);
            self.buffer[self.pos..self.pos + to_write].copy_from_slice(&bytes[..to_write]);
            self.pos += to_write;
            Ok(())
        }
    }
}

/// Fonction stub pour serial_write (sera définie dans main.rs)
#[no_mangle]
pub extern "C" fn serial_write(_s: &str) {
    // Stub - sera remplacé par main.rs
}

#[test_case]
fn test_library_compiles() {
    // Test basique que la lib compile
    assert_eq!(2 + 2, 4);
}
