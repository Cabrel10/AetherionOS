//! Linux ABI Compatibility Layer for AetherionOS
//!
//! Provides a Linuxulator-style translation layer that enables Linux x86_64
//! static ELF binaries (e.g., busybox, Kali coreutils) to run natively on
//! AetherionOS without emulation overhead.
//!
//! Architecture:
//!   - Process ABI tag (Abi::Linux vs Abi::AetherionOS) set at ELF load time
//!   - Syscall dispatch checks ABI tag and routes to Linux-specific handlers
//!   - Linux-compatible struct layouts (#[repr(C)]) for stat, utsname, etc.
//!   - Error code translation (already POSIX-compatible, minimal differences)
//!
//! Detection:
//!   - EI_OSABI == 0x03 (ELFOSABI_LINUX) in ELF header
//!   - PT_INTERP segment pointing to /lib/ld-linux-x86-64.so.2
//!   - PT_NOTE with GNU ABI tag
//!   - Explicit user flag (future: `exec --linux <binary>`)
//!
//! References:
//!   - FreeBSD Linuxulator: docs.freebsd.org/en/books/handbook/linuxemu/
//!   - Asterinas: github.com/asterinas/asterinas
//!   - Linux syscall table: arch/x86/entry/syscalls/syscall_64.tbl

use alloc::string::String;

// ═══════════════════════════════════════════════════════════
// ABI Enum — stored in Process struct to route syscalls
// ═══════════════════════════════════════════════════════════

/// Application Binary Interface tag for a process.
/// Determines how syscall arguments are interpreted and which
/// struct layouts / error codes are used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abi {
    /// Native AetherionOS ABI (default)
    AetherionOS,
    /// Linux x86_64 ABI (Linuxulator compatibility)
    Linux,
}

impl Default for Abi {
    fn default() -> Self { Abi::AetherionOS }
}

// ═══════════════════════════════════════════════════════════
// Linux ELF Detection
// ═══════════════════════════════════════════════════════════

/// ELF OS/ABI values (from e_ident[EI_OSABI])
pub const ELFOSABI_NONE: u8    = 0;   // UNIX System V (default for Linux)
pub const ELFOSABI_LINUX: u8   = 3;   // Linux
pub const ELFOSABI_FREEBSD: u8 = 9;   // FreeBSD

/// EI_OSABI offset in ELF header
pub const EI_OSABI: usize = 7;

/// PT_INTERP program header type
pub const PT_INTERP: u32 = 3;

/// Check if an ELF binary should be treated as a Linux binary.
/// Returns true if:
///   1. EI_OSABI == ELFOSABI_LINUX (0x03), or
///   2. EI_OSABI == ELFOSABI_NONE (0x00) AND a PT_INTERP segment exists
///      pointing to a Linux dynamic linker, or
///   3. A PT_NOTE section contains the GNU ABI tag
///
/// Most static Linux binaries compiled with GCC/musl have OSABI=0 (UNIX),
/// so we also check for common Linux linker paths.
pub fn detect_linux_elf(elf_data: &[u8]) -> bool {
    if elf_data.len() < 64 { return false; }

    // Check ELF magic
    if &elf_data[0..4] != b"\x7fELF" { return false; }

    let osabi = elf_data[EI_OSABI];

    // Explicit Linux OSABI
    if osabi == ELFOSABI_LINUX {
        crate::serial_println!("[LINUX-ABI] Detected Linux ELF via EI_OSABI=0x03");
        return true;
    }

    // For OSABI=0 (generic UNIX), check program headers for Linux markers
    if osabi == ELFOSABI_NONE {
        // Parse ELF64 header fields
        let e_phoff = u64::from_le_bytes([
            elf_data[32], elf_data[33], elf_data[34], elf_data[35],
            elf_data[36], elf_data[37], elf_data[38], elf_data[39],
        ]);
        let e_phentsize = u16::from_le_bytes([elf_data[54], elf_data[55]]) as u64;
        let e_phnum = u16::from_le_bytes([elf_data[56], elf_data[57]]) as u64;

        // Scan program headers for PT_INTERP
        for i in 0..e_phnum {
            let ph_off = e_phoff + i * e_phentsize;
            if (ph_off + 56) as usize > elf_data.len() { break; }
            let ph_off = ph_off as usize;

            let p_type = u32::from_le_bytes([
                elf_data[ph_off], elf_data[ph_off+1],
                elf_data[ph_off+2], elf_data[ph_off+3],
            ]);

            if p_type == PT_INTERP {
                // Read the interpreter path
                let p_offset = u64::from_le_bytes([
                    elf_data[ph_off+8], elf_data[ph_off+9],
                    elf_data[ph_off+10], elf_data[ph_off+11],
                    elf_data[ph_off+12], elf_data[ph_off+13],
                    elf_data[ph_off+14], elf_data[ph_off+15],
                ]) as usize;
                let p_filesz = u64::from_le_bytes([
                    elf_data[ph_off+32], elf_data[ph_off+33],
                    elf_data[ph_off+34], elf_data[ph_off+35],
                    elf_data[ph_off+36], elf_data[ph_off+37],
                    elf_data[ph_off+38], elf_data[ph_off+39],
                ]) as usize;

                if p_offset + p_filesz <= elf_data.len() && p_filesz < 256 {
                    let interp = &elf_data[p_offset..p_offset + p_filesz];
                    // Trim null terminator
                    let interp = if interp.last() == Some(&0) {
                        &interp[..interp.len()-1]
                    } else {
                        interp
                    };

                    // Check for common Linux interpreters
                    if interp.starts_with(b"/lib64/ld-linux") ||
                       interp.starts_with(b"/lib/ld-linux") ||
                       interp.starts_with(b"/lib/ld-musl") ||
                       interp.starts_with(b"/lib/x86_64-linux-gnu/ld-linux")
                    {
                        crate::serial_println!("[LINUX-ABI] Detected Linux ELF via PT_INTERP");
                        return true;
                    }
                }
            }

            // Check PT_NOTE for GNU ABI tag
            if p_type == 4 { // PT_NOTE
                let p_offset = u64::from_le_bytes([
                    elf_data[ph_off+8], elf_data[ph_off+9],
                    elf_data[ph_off+10], elf_data[ph_off+11],
                    elf_data[ph_off+12], elf_data[ph_off+13],
                    elf_data[ph_off+14], elf_data[ph_off+15],
                ]) as usize;
                let p_filesz = u64::from_le_bytes([
                    elf_data[ph_off+32], elf_data[ph_off+33],
                    elf_data[ph_off+34], elf_data[ph_off+35],
                    elf_data[ph_off+36], elf_data[ph_off+37],
                    elf_data[ph_off+38], elf_data[ph_off+39],
                ]) as usize;

                if p_offset + p_filesz <= elf_data.len() && p_filesz >= 16 {
                    let note = &elf_data[p_offset..p_offset + p_filesz];
                    // GNU ABI note: namesz=4, name="GNU\0", type=1
                    if note.len() >= 16 && &note[12..16] == b"GNU\0" {
                        crate::serial_println!("[LINUX-ABI] Detected Linux ELF via GNU PT_NOTE");
                        return true;
                    }
                }
            }
        }
    }

    false
}

// ═══════════════════════════════════════════════════════════
// Linux-Compatible Struct Layouts
// ═══════════════════════════════════════════════════════════

/// Linux utsname structure (for uname syscall)
/// Each field is 65 bytes (including null terminator)
/// Total: 5 * 65 = 325 bytes (+ domainname = 390 bytes with NIS extension)
#[repr(C)]
pub struct LinuxUtsname {
    pub sysname:    [u8; 65],
    pub nodename:   [u8; 65],
    pub release:    [u8; 65],
    pub version:    [u8; 65],
    pub machine:    [u8; 65],
    pub domainname: [u8; 65], // Linux extension (NIS domain)
}

impl LinuxUtsname {
    /// Create a Linux-compatible utsname that tricks binaries into thinking
    /// they're running on a real Linux system.
    pub fn linux_default() -> Self {
        let mut u = LinuxUtsname {
            sysname:    [0u8; 65],
            nodename:   [0u8; 65],
            release:    [0u8; 65],
            version:    [0u8; 65],
            machine:    [0u8; 65],
            domainname: [0u8; 65],
        };

        Self::copy_str(&mut u.sysname,    b"Linux");
        Self::copy_str(&mut u.nodename,   b"aetherion");
        Self::copy_str(&mut u.release,    b"6.1.0-aetherion");
        Self::copy_str(&mut u.version,    b"#1 SMP PREEMPT_DYNAMIC AetherionOS");
        Self::copy_str(&mut u.machine,    b"x86_64");
        Self::copy_str(&mut u.domainname, b"(none)");

        u
    }

    fn copy_str(dst: &mut [u8; 65], src: &[u8]) {
        let len = core::cmp::min(src.len(), 64);
        dst[..len].copy_from_slice(&src[..len]);
        // Already zeroed, null terminator implicit
    }
}

/// Linux stat structure (x86_64, 144 bytes)
/// Used by stat(2), fstat(2), lstat(2), newfstatat(2)
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LinuxStat {
    pub st_dev:     u64,  // offset 0
    pub st_ino:     u64,  // offset 8
    pub st_nlink:   u64,  // offset 16
    pub st_mode:    u32,  // offset 24
    pub st_uid:     u32,  // offset 28
    pub st_gid:     u32,  // offset 32
    pub _pad0:      u32,  // offset 36
    pub st_rdev:    u64,  // offset 40
    pub st_size:    i64,  // offset 48
    pub st_blksize: i64,  // offset 56
    pub st_blocks:  i64,  // offset 64
    pub st_atime:   i64,  // offset 72
    pub st_atime_nsec: i64, // offset 80
    pub st_mtime:   i64,  // offset 88
    pub st_mtime_nsec: i64, // offset 96
    pub st_ctime:   i64,  // offset 104
    pub st_ctime_nsec: i64, // offset 112
    pub _reserved:  [i64; 3], // offset 120 (24 bytes padding to 144)
}

impl Default for LinuxStat {
    fn default() -> Self {
        LinuxStat {
            st_dev: 0,
            st_ino: 1,
            st_nlink: 1,
            st_mode: 0o100644,  // S_IFREG | 0644
            st_uid: 1000,
            st_gid: 1000,
            _pad0: 0,
            st_rdev: 0,
            st_size: 0,
            st_blksize: 4096,
            st_blocks: 0,
            st_atime: 0,
            st_atime_nsec: 0,
            st_mtime: 0,
            st_mtime_nsec: 0,
            st_ctime: 0,
            st_ctime_nsec: 0,
            _reserved: [0; 3],
        }
    }
}

// ═══════════════════════════════════════════════════════════
// Linux Syscall Handlers (for processes with Abi::Linux)
// ═══════════════════════════════════════════════════════════

/// arch_prctl(code, addr) — set/get architecture-specific thread state
/// ARCH_SET_FS (0x1002): Set FS base register for TLS (Thread Local Storage)
/// ARCH_GET_FS (0x1001): Get FS base register
/// ARCH_SET_GS (0x1001): Set GS base
/// ARCH_GET_GS (0x1004): Get GS base
pub fn linux_arch_prctl(code: u64, addr: u64) -> u64 {
    const ARCH_SET_GS: u64 = 0x1001;
    const ARCH_SET_FS: u64 = 0x1002;
    const ARCH_GET_FS: u64 = 0x1003;
    const ARCH_GET_GS: u64 = 0x1004;

    match code {
        ARCH_SET_FS => {
            // Set FS base MSR (IA32_FS_BASE = 0xC000_0100)
            // This is critical for TLS (thread-local storage) in glibc/musl
            unsafe {
                let lo = addr as u32;
                let hi = (addr >> 32) as u32;
                core::arch::asm!(
                    "wrmsr",
                    in("ecx") 0xC000_0100u32, // IA32_FS_BASE
                    in("eax") lo,
                    in("edx") hi,
                    options(nomem, nostack)
                );
            }
            crate::serial_println!("[LINUX-ABI] arch_prctl ARCH_SET_FS=0x{:X}", addr);
            0
        }
        ARCH_GET_FS => {
            // Read FS base MSR
            let fs_base: u64 = unsafe {
                let lo: u32;
                let hi: u32;
                core::arch::asm!(
                    "rdmsr",
                    in("ecx") 0xC000_0100u32,
                    out("eax") lo,
                    out("edx") hi,
                    options(nomem, nostack)
                );
                ((hi as u64) << 32) | (lo as u64)
            };
            if addr != 0 && crate::arch::x86_64::syscall::validate_user_ptr_pub(addr, 8) {
                unsafe { core::ptr::write_volatile(addr as *mut u64, fs_base); }
            }
            0
        }
        ARCH_SET_GS => {
            // GS base is managed by the kernel (swapgs), silently accept
            crate::serial_println!("[LINUX-ABI] arch_prctl ARCH_SET_GS=0x{:X} (ignored, kernel-managed)", addr);
            0
        }
        ARCH_GET_GS => {
            if addr != 0 && crate::arch::x86_64::syscall::validate_user_ptr_pub(addr, 8) {
                unsafe { core::ptr::write_volatile(addr as *mut u64, 0); }
            }
            0
        }
        _ => {
            crate::serial_println!("[LINUX-ABI] arch_prctl: unknown code=0x{:X}", code);
            (-22i64) as u64 // EINVAL
        }
    }
}

/// uname(buf) — Linux-compatible uname that returns "Linux 6.1.0-aetherion"
/// This is critical for Kali tools that check `uname -r` for kernel version.
pub fn linux_uname(buf_addr: u64) -> u64 {
    // struct utsname with NIS domain: 6 * 65 = 390 bytes
    // Standard without domain: 5 * 65 = 325 bytes
    if !crate::arch::x86_64::syscall::validate_user_ptr_pub(buf_addr, 390) {
        return (-14i64) as u64; // EFAULT
    }

    let uname = LinuxUtsname::linux_default();

    unsafe {
        let dst = buf_addr as *mut u8;
        let src = &uname as *const LinuxUtsname as *const u8;
        core::ptr::copy_nonoverlapping(src, dst, 390);
    }

    crate::serial_println!("[LINUX-ABI] uname: sysname=Linux, release=6.1.0-aetherion");
    0
}

/// set_tid_address(tidptr) — store the address of the clear_child_tid value
/// Returns the caller's TID (= PID for single-threaded processes)
pub fn linux_set_tid_address(_tidptr: u64) -> u64 {
    crate::scheduler::current_pid()
}

/// Linux brk(addr) — adjust the program break
/// Returns the current/new program break address
pub fn linux_brk(addr: u64) -> u64 {
    // Route to the existing sys_brk implementation
    // Linux brk returns the new break on success, or the old break on failure
    let current_pid = crate::scheduler::current_pid();
    let current_break = crate::process::with_process(current_pid, |p| p.heap_break)
        .unwrap_or(0x3000_0000_0000);

    if addr == 0 {
        return current_break;
    }

    // Try to set the new break
    let result = crate::arch::x86_64::syscall::sys_brk_pub(addr);
    if result == 0 || (result as i64) < 0 {
        // Failed: return current break
        current_break
    } else {
        result
    }
}

/// Linux getuid/geteuid/getgid/getegid — return sensible defaults
pub fn linux_getuid() -> u64 { 0 }    // root for Kali compatibility
pub fn linux_geteuid() -> u64 { 0 }   // root
pub fn linux_getgid() -> u64 { 0 }    // root group
pub fn linux_getegid() -> u64 { 0 }   // root group

/// Linux getpid — return process ID
pub fn linux_getpid() -> u64 {
    crate::scheduler::current_pid()
}

/// Linux gettid — return thread ID (= PID for main thread)
pub fn linux_gettid() -> u64 {
    crate::scheduler::current_pid()
}

/// Linux set_robust_list — used by glibc/musl for robust futex
/// Stub: accept and return 0
pub fn linux_set_robust_list(_head: u64, _len: u64) -> u64 { 0 }

/// Linux get_robust_list — companion to set_robust_list
pub fn linux_get_robust_list(_pid: u64, _head: u64, _len: u64) -> u64 { 0 }

/// Linux rseq — restartable sequences (glibc 2.35+)
/// Stub: return ENOSYS to gracefully degrade
pub fn linux_rseq(_rseq: u64, _rseq_len: u64, _flags: u64) -> u64 {
    (-38i64) as u64 // ENOSYS
}

/// Linux prlimit64 — get/set resource limits
pub fn linux_prlimit64(pid: u64, resource: u64, new_rlim: u64, old_rlim: u64) -> u64 {
    // If old_rlim is provided, return generous limits
    if old_rlim != 0 && crate::arch::x86_64::syscall::validate_user_ptr_pub(old_rlim, 16) {
        let infinity: u64 = 0xFFFF_FFFF_FFFF_FFFF;
        unsafe {
            // rlim_cur
            core::ptr::write_volatile(old_rlim as *mut u64, infinity);
            // rlim_max
            core::ptr::write_volatile((old_rlim + 8) as *mut u64, infinity);
        }
    }
    0
}

/// Linux sysinfo — system info (struct sysinfo)
/// Returns memory info, uptime, load averages
pub fn linux_sysinfo(info_addr: u64) -> u64 {
    if !crate::arch::x86_64::syscall::validate_user_ptr_pub(info_addr, 128) {
        return (-14i64) as u64; // EFAULT
    }

    unsafe {
        let dst = info_addr as *mut u8;
        // Zero the struct
        core::ptr::write_bytes(dst, 0, 128);

        // uptime (offset 0): approximate from TSC
        let tsc: u64;
        core::arch::asm!("rdtsc", "shl rdx, 32", "or rax, rdx",
                         out("rax") tsc, out("rdx") _, options(nomem, nostack));
        let uptime = tsc / 2_000_000_000; // ~2 GHz approximation
        core::ptr::write_volatile(info_addr as *mut u64, uptime);

        // totalram (offset 32): report 1 GiB
        core::ptr::write_volatile((info_addr + 32) as *mut u64, 1024 * 1024 * 1024);
        // freeram (offset 40): report 512 MiB
        core::ptr::write_volatile((info_addr + 40) as *mut u64, 512 * 1024 * 1024);
        // mem_unit (offset 104): 1 byte
        core::ptr::write_volatile((info_addr + 104) as *mut u32, 1);
    }

    0
}

// ═══════════════════════════════════════════════════════════
// Linux Syscall Router
// ═══════════════════════════════════════════════════════════

/// Handle a syscall from a process with Abi::Linux.
/// Linux x86_64 syscall convention: rax=nr, rdi=a1, rsi=a2, rdx=a3, r10=a4, r8=a5, r9=a6
///
/// Most syscalls are identical to our existing handlers (since we already
/// use Linux ABI numbers). This function handles the Linux-SPECIFIC ones
/// that differ from our AetherionOS implementations.
///
/// Returns Some(result) if this is a Linux-specific syscall we handle,
/// or None if the caller should fall through to the standard dispatch.
pub fn linux_syscall_override(nr: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> Option<u64> {
    match nr {
        // ── Syscalls that need Linux-specific behavior ──

        // uname: return "Linux" instead of "AetherionOS"
        63 => Some(linux_uname(a1)),

        // arch_prctl: actually set FS base MSR (critical for TLS)
        158 => Some(linux_arch_prctl(a1, a2)),

        // set_tid_address: return TID
        218 => Some(linux_set_tid_address(a1)),

        // brk: Linux-style (returns address, not 0/-1)
        12 => Some(linux_brk(a1)),

        // Identity stubs specific to Linux root expectations
        102 => Some(linux_getuid()),
        104 => Some(linux_getgid()),
        107 => Some(linux_geteuid()),
        108 => Some(linux_getegid()),

        // set_robust_list / get_robust_list (glibc/musl threading)
        273 => Some(linux_set_robust_list(a1, a2)),
        274 => Some(linux_get_robust_list(a1, a2, a3)),

        // rseq (glibc 2.35+ restartable sequences)
        334 => Some(linux_rseq(a1, a2, a3)),

        // prlimit64 with proper old_rlim support
        302 => Some(linux_prlimit64(a1, a2, a3, a4)),

        // sysinfo
        99 => Some(linux_sysinfo(a1)),

        // gettid
        186 => Some(linux_gettid()),

        // ── Everything else: fall through to standard dispatch ──
        _ => None,
    }
}

/// Log a summary of Linux ABI activation for a process
pub fn log_linux_abi_activation(pid: u64, name: &str) {
    crate::serial_println!(
        "[LINUX-ABI] PID {} ({}) activated Linux compatibility layer",
        pid, name
    );
    crate::serial_println!(
        "[LINUX-ABI] uname='Linux 6.1.0-aetherion x86_64', uid=0 (root), TLS via FS base MSR"
    );
}
