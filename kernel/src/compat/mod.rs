//! AetherionOS Linux ABI Compatibility Layer (Linuxulator)
//!
//! Inspired by FreeBSD's Linuxulator (sys/compat/linux/) and Asterinas.
//! Provides binary compatibility with Linux x86_64 ELF binaries by:
//!   1. Detecting Linux ELF binaries via EI_OSABI or PT_NOTE
//!   2. Translating Linux syscall numbers to AetherionOS handlers
//!   3. Providing Linux-compatible struct layouts (stat, utsname, etc.)
//!   4. Emulating /proc, /sys pseudo-filesystems for Linux userspace
//!
//! Architecture: Linuxulator (native execution, no emulation overhead)
//!   - Syscall dispatch: single match table (already Linux ABI numbers)
//!   - Struct translation: #[repr(C)] Linux-compatible layouts
//!   - Signal translation: Linux signal numbers → AetherionOS signals
//!   - Error translation: Linux errno values (already POSIX-compatible)
//!
//! Phase 1: Static binary support (busybox, coreutils)
//!   - ~80 core syscalls implemented
//!   - /proc/self/exe, /proc/self/maps stubs
//!   - uname returns "Linux 6.1.0-aetherion"
//!
//! Phase 2: Dynamic binary support (glibc/musl)
//!   - ld-linux-x86-64.so.2 interpreter loading
//!   - mmap/mprotect for shared library loading
//!   - futex for threading (already implemented)
//!
//! References:
//!   - FreeBSD Linuxulator: docs.freebsd.org/en/books/handbook/linuxemu/
//!   - Asterinas (Rust Linux ABI): github.com/asterinas/asterinas
//!   - Linux syscall table: filippo.io/linux-syscall-table/

pub mod linux_abi;

