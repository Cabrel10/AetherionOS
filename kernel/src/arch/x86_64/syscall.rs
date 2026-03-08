// arch/x86_64/syscall.rs - Couche 13: Complete POSIX Syscalls + Multi-Processing
//
// Implements a proper SYSCALL entry point with:
//   - swapgs to access kernel per-CPU data (kernel RSP)
//   - Full register save/restore on kernel stack
//   - Rust-level syscall dispatch with full POSIX routing
//   - User pointer validation (EFAULT if buffer in kernel space)
//   - sysretq to return to Ring 3
//
// Syscall Table (Linux x86_64 ABI):
//   0  sys_read(fd, buf, len)
//   1  sys_write(fd, buf, len)
//   2  sys_open(path, flags, mode)
//   3  sys_close(fd)
//   8  sys_seek(fd, offset, whence)
//  20  sys_getpid()
//  39  sys_getppid()
//  57  sys_fork()
//  59  sys_exec(path, argv, envp)
//  60  sys_exit(code)
//  61  sys_wait(pid)
//  62  sys_kill(pid, signal)
// 200  sys_ps() - custom: list processes
//
// SECURITY:
//   - User pointers validated: must be < 0x0000_8000_0000_0000
//   - Kernel stack is separate from user stack (swapgs-based switch)
//   - RFLAGS.IF masked on entry (SFMASK) — no interrupt reentrancy
//
// GDT layout (from gdt.rs):
//   0x08  Kernel Code (Ring 0)
//   0x10  Kernel Data (Ring 0)
//   0x18  User Data   (Ring 3)
//   0x20  User Code   (Ring 3)

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

// PARENT_RESUME_KERNEL_RSP is now stored in PER_CPU.saved_kernel_rsp (gs:[24])
// to avoid R_X86_64_32S relocations in the naked syscall_entry function.

// ===== MSR addresses =====
const IA32_EFER: u32       = 0xC000_0080;
const IA32_STAR: u32       = 0xC000_0081;
const IA32_LSTAR: u32      = 0xC000_0082;
const IA32_FMASK: u32      = 0xC000_0084;
const IA32_GS_BASE: u32    = 0xC000_0101;
const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;

/// EFER.SCE bit
const EFER_SCE: u64 = 1 << 0;

/// RFLAGS bits to mask on SYSCALL: IF(9), TF(8), DF(10)
const SFMASK_VALUE: u64 = (1 << 9) | (1 << 8) | (1 << 10);

/// Maximum valid user-space address
const USER_ADDR_LIMIT: u64 = 0x0000_8000_0000_0000;

/// POSIX error codes (negative, as unsigned)
const ENOSYS: u64 = (-38i64) as u64;
const EFAULT: u64 = (-14i64) as u64;
const EBADF: u64  = (-9i64) as u64;
const EAGAIN: u64 = (-11i64) as u64;
const ENOMEM: u64 = (-12i64) as u64;
const EINVAL: u64 = (-22i64) as u64;
const ECHILD: u64 = (-10i64) as u64;
const ENOENT: u64 = (-2i64) as u64;
const EMFILE: u64 = (-24i64) as u64;
const ENOSPC: u64 = (-28i64) as u64;
const EEXIST: u64 = (-17i64) as u64;

// POSIX open flags
const O_RDONLY: u32 = 0;
const O_WRONLY: u32 = 1;
const O_RDWR: u32   = 2;
const O_CREAT: u32  = 0o100;  // 64
const O_TRUNC: u32  = 0o1000; // 512

// ===== Kernel syscall stack =====
const KERNEL_SYSCALL_STACK_SIZE: usize = 1048576; // 1 MiB - Deep call chains: SYSCALL -> VFS -> FAT32 -> VirtIO-Block -> serial

#[repr(align(16))]
struct AlignedStack([u8; KERNEL_SYSCALL_STACK_SIZE]);

static mut SYSCALL_STACK: AlignedStack = AlignedStack([0; KERNEL_SYSCALL_STACK_SIZE]);

/// Per-CPU data structure accessed via GS base after swapgs.
/// Layout is ABI-critical: offset 0 = kernel_rsp, offset 8 = user_rsp, offset 16 = user_rip,
/// offset 24 = saved_kernel_rsp (for sys_wait parent resume).
#[repr(C)]
struct PerCpuData {
    kernel_rsp: u64,        // offset 0: kernel RSP loaded on SYSCALL entry
    user_rsp: u64,          // offset 8: user RSP saved during SYSCALL
    user_rip: u64,          // offset 16: user RIP saved on SYSCALL entry (from RCX)
    saved_kernel_rsp: u64,  // offset 24: snapshot of kernel RSP after pushes (for sys_wait)
}

static mut PER_CPU: PerCpuData = PerCpuData {
    kernel_rsp: 0,
    user_rsp: 0,
    user_rip: 0,
    saved_kernel_rsp: 0,
};

// ===== MSR helpers =====

#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    asm!("rdmsr",
        in("ecx") msr,
        out("eax") lo,
        out("edx") hi,
        options(nomem, nostack));
    ((hi as u64) << 32) | (lo as u64)
}

#[inline]
unsafe fn wrmsr(msr: u32, value: u64) {
    let lo = value as u32;
    let hi = (value >> 32) as u32;
    asm!("wrmsr",
        in("ecx") msr,
        in("eax") lo,
        in("edx") hi,
        options(nomem, nostack));
}

// ===== Helpers to get saved user state (for thread context save/restore) =====

/// Return the user-mode RIP that was saved on SYSCALL entry (from RCX).
#[inline]
fn saved_user_rip() -> u64 {
    unsafe { PER_CPU.user_rip }
}

/// Return the user-mode RSP that was saved on SYSCALL entry.
#[inline]
fn saved_user_rsp() -> u64 {
    unsafe { PER_CPU.user_rsp }
}

// ===== User pointer validation =====

/// Validate that a user pointer range [ptr, ptr+len) is within user address space
#[inline]
fn validate_user_ptr(addr: u64, len: u64) -> bool {
    // Accept both lower-half (<0x8000_0000_0000) and userspace ELF region (0x80_0000_0000+)
    if addr == 0 { return false; }
    if len > 0x1000_0000 { return false; } // 256 MiB sanity
    // Standard user-space: below canonical hole
    if addr < USER_ADDR_LIMIT {
        return addr.checked_add(len).map_or(false, |end| end <= USER_ADDR_LIMIT);
    }
    // User ELF region: 0x80_0000_0000 .. 0x80_1000_0000 (4 GiB window)
    if addr >= 0x80_0000_0000 && addr < 0x80_1000_0000 {
        return addr.checked_add(len).map_or(false, |end| end <= 0x80_1000_0000);
    }
    false
}

/// Read a null-terminated string from user space (max 256 bytes)
unsafe fn read_user_string(addr: u64) -> Option<alloc::string::String> {
    if !validate_user_ptr(addr, 1) { return None; }
    let mut buf = alloc::vec::Vec::with_capacity(256);
    let ptr = addr as *const u8;
    for i in 0..256usize {
        if !validate_user_ptr(addr + i as u64, 1) { return None; }
        let byte = core::ptr::read_volatile(ptr.add(i));
        if byte == 0 { break; }
        buf.push(byte);
    }
    alloc::string::String::from_utf8(buf).ok()
}

// ===== SYSCALL entry point (naked, assembly) =====

#[naked]
unsafe extern "C" fn syscall_entry() {
    asm!(
        // 1. Switch to kernel GS
        "swapgs",

        // 2. Save user RSP and RIP, load kernel RSP
        "mov gs:[8], rsp",
        "mov gs:[16], rcx",   // save user RIP (RCX holds return addr from SYSCALL)
        "mov rsp, gs:[0]",

        // 3. Build a stack frame with all user state
        "push rcx",     // user RIP (saved by SYSCALL)
        "push r11",     // user RFLAGS (saved by SYSCALL)
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",

        // 3b. Save the kernel RSP (pointing to saved regs) into per-CPU data.
        // This is overwritten on every syscall; sys_wait copies it to the
        // process struct before launching a child thread.
        // Uses GS-relative access (gs:[24]) to avoid R_X86_64_32S relocations.
        "mov gs:[24], rsp",

        // 4. Prepare arguments for Rust handler
        //    syscall_handler_rust(nr: u64, a1: u64, a2: u64, a3: u64)
        //    System V calling convention: rdi, rsi, rdx, rcx
        //    From SYSCALL: rax=nr, rdi=a1, rsi=a2, rdx=a3
        "mov rcx, rdx",    // 4th arg = a3 (rdx from user)
        "mov rdx, rsi",    // 3rd arg = a2
        "mov rsi, rdi",    // 2nd arg = a1
        "mov rdi, rax",    // 1st arg = syscall number

        // Align RSP to 16 bytes before calling Rust (ABI requirement)
        "mov r15, rsp",          // save current RSP in r15 (already pushed)
        "and rsp, -16",          // align to 16-byte boundary
        // Call the Rust dispatcher
        "call {handler}",
        "mov rsp, r15",          // restore RSP (r15 will be popped next)

        // RAX = return value (set by Rust handler)

        // 5. Restore callee-saved registers
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "pop r11",     // user RFLAGS
        "pop rcx",     // user RIP

        // 6. Restore user RSP
        "mov rsp, gs:[8]",

        // 7. Swap back to user GS
        "swapgs",

        // 8. Return to Ring 3
        "sysretq",

        handler = sym syscall_handler_rust,
        options(noreturn),
    );
}

// ===== Rust syscall dispatcher =====

/// Route syscall by number (Linux x86_64 ABI).
/// Returns result in RAX.
///
/// Jalon 33: The kernel is compiled with -sse,+soft-float so it NEVER
/// touches XMM/YMM registers.  User FPU state therefore survives every
/// syscall automatically — no fxsave/fxrstor needed in the fast path.
#[no_mangle]
extern "C" fn syscall_handler_rust(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    syscall_dispatch(nr, a1, a2, a3)
}

/// Internal syscall dispatch (separated for FPU save/restore wrapper).
fn syscall_dispatch(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    match nr {
        0  => sys_read(a1 as u32, a2, a3),
        1  => sys_write(a1, a2, a3),
        2  => sys_open(a1, a2 as u32),
        3  => sys_close(a1 as u32),
        8  => sys_seek(a1 as u32, a2 as i64, a3 as u32),
        9  => sys_mmap(a1, a2, a3),
        10 => sys_mmap_fb(a1),
        12 => sys_brk(a1),             // Jalon 27: brk(new_break) for userspace malloc
        20 => sys_getpid(),
        24 => sys_yield(),
        39 => sys_getppid(),
        41 => sys_socket(a1 as u32, a2 as u32, a3 as u32),
        42 => sys_tcp_connect(a1 as u32, a2, a3),
        44 => sys_sendto(a1 as u32, a2, a3),
        45 => sys_recvfrom(a1 as u32, a2, a3),
        47 => sys_tcp_shutdown_syscall(a1 as u32),
        49 => sys_bind(a1 as u32, a2 as u16),
        56 => sys_clone(a1),
        57 => sys_fork(),
        59 => sys_exec(a1),
        60 => sys_exit(a1),
        61 => sys_wait(a1),
        62 => sys_kill(a1, a2 as u32),
        22 => sys_pipe(a1),             // Jalon 24: pipe(int pipefd[2])
        33 => sys_dup2(a1 as u32, a2 as u32), // Jalon 24: dup2(oldfd, newfd)
        78 => sys_getdents(a1 as u32, a2, a3), // Jalon 24: getdents(fd, buf, len)
        200 => sys_ps(),
        201 => sys_bus_publish(a1, a2 as u32, a3),
        202 => sys_vga_write(a1 as usize, a2 as usize, a3),
        210 => sys_net_ping(a1, a2 as u16),
        211 => sys_gethostbyname(a1),
        212 => sys_tcp_read(a1 as u32, a2, a3),
        11 => sys_poll_hid(),                     // Jalon 38: HID event polling
        220 => sys_fb_fill_rect(a1, a2, a3),      // Jalon 39: Framebuffer fill rect
        221 => sys_fb_draw_char(a1, a2),           // Jalon 39: Framebuffer draw char
        222 => sys_fb_draw_string(a1, a2, a3),     // Jalon 39: Framebuffer draw string
        223 => sys_fb_get_info(a1),                // Jalon 39: Framebuffer get info
        230 => sys_rdtsc(),                        // Jalon 40: Read TSC
        _ => {
            crate::serial_println!("[SYSCALL] Unknown nr={} a1=0x{:X} a2=0x{:X} a3=0x{:X}", nr, a1, a2, a3);
            ENOSYS
        }
    }
}

// ===== sys_write(fd, buf, len) =====

/// POSIX write: fd=1 or fd=2 -> serial output. fd>=3 -> VFS/FAT32 file write.
/// SECURITY: buf and buf+len must be < USER_ADDR_LIMIT.
fn sys_write(fd: u64, buf_addr: u64, len: u64) -> u64 {
    if len == 0 { return 0; }

    // SECURITY: validate user pointer
    if !validate_user_ptr(buf_addr, len) {
        // For serial output (fd 1/2), try to write with clamped length
        if (fd == 1 || fd == 2) && validate_user_ptr(buf_addr, 1) {
            // Clamp len to a safe maximum and write what we can
            let safe_len = core::cmp::min(len, 4096) as usize;
            let ptr = buf_addr as *const u8;
            for i in 0..safe_len {
                if !validate_user_ptr(buf_addr + i as u64, 1) { break; }
                let byte = unsafe { core::ptr::read_volatile(ptr.add(i)) };
                if byte == 0 { break; }
                unsafe {
                    while (x86_64::instructions::port::Port::<u8>::new(0x3FD).read() & 0x20) == 0 {}
                    x86_64::instructions::port::Port::<u8>::new(0x3F8).write(byte);
                }
            }
            return safe_len as u64;
        }
        return EFAULT;
    }

    // stdout/stderr -> serial output
    if fd == 1 || fd == 2 {
        unsafe {
            let buf = buf_addr as *const u8;
            for i in 0..len as usize {
                let byte = core::ptr::read_volatile(buf.add(i));
                // Wait for THR empty (LSR bit 5)
                loop {
                    let lsr: u8;
                    asm!("in al, dx", out("al") lsr, in("dx") 0x3FDu16,
                         options(nomem, nostack));
                    if lsr & 0x20 != 0 { break; }
                }
                // Send byte
                asm!("out dx, al", in("al") byte, in("dx") 0x3F8u16,
                     options(nomem, nostack));
            }
        }
        return len;
    }

    // fd >= 3: write to a file via FD table
    let current_pid = crate::scheduler::current_pid();

    // Get file path and flags from FD table
    let fd_info = crate::process::with_fd_table(current_pid, |fd_table| {
        if let Some(entry) = fd_table.get(fd as usize) {
            Some((entry.path.clone(), entry.flags, entry.offset))
        } else {
            None
        }
    }).flatten();

    match fd_info {
        Some((path, flags, offset)) => {
            // Check write permission (O_WRONLY or O_RDWR)
            let access_mode = flags & 0x3;
            if access_mode != O_WRONLY && access_mode != O_RDWR {
                crate::serial_println!("[SYSCALL] sys_write: FD {} not opened for writing (flags=0x{:X})", fd, flags);
                return EBADF;
            }

            // Copy user data to kernel buffer
            let user_data = unsafe {
                let buf = buf_addr as *const u8;
                let mut data = alloc::vec::Vec::with_capacity(len as usize);
                for i in 0..len as usize {
                    data.push(core::ptr::read_volatile(buf.add(i)));
                }
                data
            };

            // Route to FAT32 if path starts with /disk/
            if path.starts_with("/disk/") {
                let disk_path = &path[6..]; // strip "/disk/"
                crate::serial_println!("[SYSCALL] sys_write: FAT32 write to '{}' ({} bytes)", disk_path, user_data.len());

                // For FAT32, we do a full-file write (read-modify-write if appending)
                // Since FAT32 write_file does a complete overwrite, we handle offset-based writes
                // by reading existing data, inserting new data, and writing back
                let success = if offset > 0 {
                    // Append/overwrite at offset: read existing, extend, write
                    let mut existing = crate::fs::fat32::read_file_path(disk_path)
                        .unwrap_or_else(alloc::vec::Vec::new);
                    let off = offset as usize;
                    if off > existing.len() {
                        existing.resize(off, 0); // pad with zeros
                    }
                    if off + user_data.len() > existing.len() {
                        existing.resize(off + user_data.len(), 0);
                    }
                    existing[off..off + user_data.len()].copy_from_slice(&user_data);
                    crate::fs::fat32::write_file(disk_path, &existing)
                } else {
                    // Simple overwrite from start
                    crate::fs::fat32::write_file(disk_path, &user_data)
                };

                if success {
                    // Update offset
                    crate::process::with_fd_table_mut(current_pid, |fd_table| {
                        if let Some(entry) = fd_table.get_mut(fd as usize) {
                            entry.offset += len;
                        }
                    });
                    crate::serial_println!("[SYSCALL] sys_write: FAT32 write OK, {} bytes", len);
                    len
                } else {
                    crate::serial_println!("[SYSCALL] sys_write: FAT32 write FAILED");
                    ENOSPC
                }
            } else {
                // Write to VFS in-memory file
                match crate::fs::vfs::file_write(&path, &user_data) {
                    Ok(n) => {
                        crate::process::with_fd_table_mut(current_pid, |fd_table| {
                            if let Some(entry) = fd_table.get_mut(fd as usize) {
                                entry.offset += n as u64;
                            }
                        });
                        n as u64
                    }
                    Err(_) => EBADF,
                }
            }
        }
        None => EBADF,
    }
}

// ===== sys_read(fd, buf, len) =====

/// POSIX read: fd=0 -> keyboard input, other fds -> VFS read.
fn sys_read(fd: u32, buf_addr: u64, len: u64) -> u64 {
    if len == 0 { return 0; }
    if !validate_user_ptr(buf_addr, len) {
        return EFAULT;
    }

    let current_pid = crate::scheduler::current_pid();

    if fd == 0 {
        // Read from stdin = keyboard buffer
        // Non-blocking read: return whatever is available now
        let mut temp_buf = [0u8; 256];
        let max_read = core::cmp::min(len as usize, temp_buf.len());
        let bytes_read = crate::process::kbd_read(&mut temp_buf, max_read);
        if bytes_read > 0 {
            // Copy to user buffer
            unsafe {
                let dst = buf_addr as *mut u8;
                for i in 0..bytes_read {
                    core::ptr::write_volatile(dst.add(i), temp_buf[i]);
                }
            }
            return bytes_read as u64;
        }
        // No data available - return 0 (non-blocking)
        return 0;
    }

    // Read from VFS file via FD table
    let path_and_offset = crate::process::with_fd_table(current_pid, |fd_table| {
        if let Some(entry) = fd_table.get(fd as usize) {
            Some((entry.path.clone(), entry.offset))
        } else {
            None
        }
    }).flatten();

    match path_and_offset {
        Some((path, offset)) => {
            // Route /disk/ paths directly to FAT32 for fresh reads
            // Jalon 52: Use chunked read to avoid OOM on large files
            let is_disk = path.starts_with("/disk/");
            if is_disk {
                let disk_path = &path[6..];
                crate::serial_println!("[SYSCALL] sys_read: FAT32 chunked read '{}' offset={} len={}", disk_path, offset, len);
                match crate::fs::fat32::read_file_path_chunk(disk_path, offset, len) {
                    Some(chunk) => {
                        let to_copy = chunk.len();
                        if to_copy == 0 {
                            return 0; // EOF
                        }
                        // Copy to user buffer
                        unsafe {
                            let dst = buf_addr as *mut u8;
                            for i in 0..to_copy {
                                core::ptr::write_volatile(dst.add(i), chunk[i]);
                            }
                        }
                        // Update offset
                        crate::process::with_fd_table_mut(current_pid, |fd_table| {
                            if let Some(entry) = fd_table.get_mut(fd as usize) {
                                entry.offset += to_copy as u64;
                            }
                        });
                        return to_copy as u64;
                    }
                    None => {
                        // Fallback to VFS
                        crate::serial_println!("[SYSCALL] sys_read: FAT32 chunk read failed, fallback to VFS");
                    }
                }
            }

            // VFS path (or FAT32 fallback)
            let data = match crate::fs::vfs::file_read(&path) {
                Ok(d) => d,
                Err(_) => return ENOENT,
            };

            let start = offset as usize;
            if start >= data.len() {
                return 0; // EOF
            }
            let avail = data.len() - start;
            let to_copy = core::cmp::min(avail, len as usize);
            // Copy to user buffer
            unsafe {
                let dst = buf_addr as *mut u8;
                for i in 0..to_copy {
                    core::ptr::write_volatile(dst.add(i), data[start + i]);
                }
            }
            // Update offset
            crate::process::with_fd_table_mut(current_pid, |fd_table| {
                if let Some(entry) = fd_table.get_mut(fd as usize) {
                    entry.offset += to_copy as u64;
                }
            });
            to_copy as u64
        }
        None => EBADF,
    }
}

// ===== sys_open(path, flags) =====

/// POSIX open: validate path, check VFS, allocate FD.
/// Supports O_CREAT for creating files on /disk/ (FAT32).
fn sys_open(path_addr: u64, flags: u32) -> u64 {
    if !validate_user_ptr(path_addr, 1) {
        return EFAULT;
    }

    let path = match unsafe { read_user_string(path_addr) } {
        Some(p) => p,
        None => return EFAULT,
    };

    let current_pid = crate::scheduler::current_pid();

    crate::serial_println!("[SYSCALL] sys_open(\"{}\", flags=0x{:X}) from PID {}", path, flags, current_pid);

    // For /disk/ paths with O_CREAT, we allow creating files that don't exist yet
    if path.starts_with("/disk/") && (flags & O_CREAT) != 0 {
        // FIX #17: Use file_exists() instead of read_file_path() to avoid OOM
        let disk_path = &path[6..]; // strip "/disk/"
        let exists = crate::fs::fat32::file_exists(disk_path).is_some();

        if !exists {
            // Create an empty file on FAT32
            crate::serial_println!("[SYSCALL] sys_open: O_CREAT, creating empty file '{}'", disk_path);
            if !crate::fs::fat32::write_file(disk_path, &[]) {
                crate::serial_println!("[SYSCALL] sys_open: Failed to create file '{}'", disk_path);
                // Don't fail — the file might be writable but the directory doesn't exist
                // We'll still allocate the FD and let write handle it
            }
        }

        // Also register in VFS for FD tracking (as an empty file placeholder)
        // We need the VFS to have the path so file_read doesn't fail during sys_open check
        {
            let mut root = crate::fs::vfs::lock_root();
            // Navigate/create intermediate directories in VFS
            let parts: alloc::vec::Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            let mut current = &mut *root;
            for (i, comp) in parts.iter().enumerate() {
                if i == parts.len() - 1 {
                    // Create the file node if it doesn't exist
                    current.entry(alloc::string::String::from(*comp))
                        .or_insert_with(|| crate::fs::vfs::VfsNode::File(alloc::vec::Vec::new()));
                    break;
                }
                current.entry(alloc::string::String::from(*comp))
                    .or_insert_with(|| crate::fs::vfs::VfsNode::Directory(
                        alloc::collections::BTreeMap::new()
                    ));
                if let Some(crate::fs::vfs::VfsNode::Directory(ref mut children)) = current.get_mut(*comp) {
                    current = children;
                } else {
                    break;
                }
            }
        }

        // Allocate FD
        match crate::process::with_fd_table_mut(current_pid, |fd_table| {
            fd_table.alloc_fd(&path, flags)
        }) {
            Some(Some(fd)) => {
                crate::serial_println!("[SYSCALL] sys_open(\"{}\") = FD {} (O_CREAT)", path, fd);
                return fd as u64;
            }
            _ => return EMFILE,
        }
    }

    // Standard open: check the file exists in VFS
    if crate::fs::vfs::file_read(&path).is_err() {
        // Try with /bin prefix
        let bin_path = alloc::format!("/bin/{}", path);
        if crate::fs::vfs::file_read(&bin_path).is_err() {
            // For /disk/ paths, try checking existence on FAT32 directly
            // FIX #17: Use file_exists() to avoid loading 2GB files into kernel heap
            if path.starts_with("/disk/") {
                let disk_path = &path[6..];
                match crate::fs::fat32::file_exists(disk_path) {
                    None => {
                        crate::serial_println!("[SYSCALL] sys_open: not found '{}'", path);
                        return ENOENT;
                    }
                    Some(file_size) => {
                        crate::serial_println!("[SYSCALL] sys_open: '{}' exists on FAT32 ({} bytes), registering FD (lazy load)", disk_path, file_size);
                        // Register a placeholder in VFS — actual data read via sys_read with chunked read
                        {
                            let mut root = crate::fs::vfs::lock_root();
                            let parts: alloc::vec::Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
                            let mut current = &mut *root;
                            for (i, comp) in parts.iter().enumerate() {
                                if i == parts.len() - 1 {
                                    // Insert an EMPTY placeholder — actual reads go through FAT32 chunked read
                                    current.entry(alloc::string::String::from(*comp))
                                        .or_insert_with(|| crate::fs::vfs::VfsNode::File(alloc::vec::Vec::new()));
                                    break;
                                }
                                current.entry(alloc::string::String::from(*comp))
                                    .or_insert_with(|| crate::fs::vfs::VfsNode::Directory(
                                        alloc::collections::BTreeMap::new()
                                    ));
                                if let Some(crate::fs::vfs::VfsNode::Directory(ref mut children)) = current.get_mut(*comp) {
                                    current = children;
                                } else {
                                    break;
                                }
                            }
                        }
                    }
                }
            } else {
                return ENOENT;
            }
        }
    }

    // Allocate FD in process table
    match crate::process::with_fd_table_mut(current_pid, |fd_table| {
        fd_table.alloc_fd(&path, flags)
    }) {
        Some(Some(fd)) => {
            crate::serial_println!("[SYSCALL] sys_open(\"{}\") = FD {}", path, fd);
            fd as u64
        }
        _ => EMFILE,
    }
}

// ===== sys_close(fd) =====

fn sys_close(fd: u32) -> u64 {
    // Don't allow closing stdin/stdout/stderr
    if fd < 3 {
        return EBADF;
    }

    let current_pid = crate::scheduler::current_pid();
    match crate::process::with_fd_table_mut(current_pid, |fd_table| {
        fd_table.close_fd(fd as usize)
    }) {
        Some(true) => 0,
        _ => EBADF,
    }
}

// ===== sys_seek(fd, offset, whence) =====

/// POSIX lseek: update FD offset
/// whence: 0=SEEK_SET, 1=SEEK_CUR, 2=SEEK_END
fn sys_seek(fd: u32, offset: i64, whence: u32) -> u64 {
    let current_pid = crate::scheduler::current_pid();

    match crate::process::with_fd_table_mut(current_pid, |fd_table| {
        if let Some(entry) = fd_table.get_mut(fd as usize) {
            let new_offset = match whence {
                0 => offset as u64,   // SEEK_SET
                1 => (entry.offset as i64 + offset) as u64, // SEEK_CUR
                // SEEK_END not supported without file size
                _ => return EINVAL,
            };
            entry.offset = new_offset;
            new_offset
        } else {
            EBADF
        }
    }) {
        Some(result) => result,
        None => EBADF,
    }
}

// ===== sys_getpid() =====

fn sys_getpid() -> u64 {
    crate::scheduler::current_pid()
}

// ===== sys_getppid() =====

fn sys_getppid() -> u64 {
    let pid = crate::scheduler::current_pid();
    crate::process::get_ppid(pid).unwrap_or(0)
}

// ===== sys_fork() =====

// ===== sys_clone(child_stack) =====

/// Create a lightweight thread sharing the parent's address space.
/// Unlike fork, the child reuses the same PML4 — true shared memory threading.
/// child_stack: top of a pre-allocated stack for the new thread.
///   The stack must have the function pointer at (child_stack - 8).
/// Returns: child_pid to the parent, 0 to the child thread.
fn sys_clone(child_stack: u64) -> u64 {
    let current_pid = crate::scheduler::current_pid();

    crate::serial_println!("[SYSCALL] sys_clone(stack=0x{:X}) from PID {}", child_stack, current_pid);

    if child_stack == 0 {
        crate::serial_println!("[SYSCALL] clone: invalid child_stack 0x{:X}", child_stack);
        return EINVAL;
    }

    // Read the function pointer from (child_stack - 8)
    // The C wrapper writes: *(stack_top - 8) = (uint64_t)start_routine;
    let fn_ptr = unsafe { core::ptr::read_volatile((child_stack - 8) as *const u64) };
    crate::serial_println!("[SYSCALL] clone: fn_ptr=0x{:X}", fn_ptr);

    // Create the child thread (shares PML4 = shared address space)
    match crate::process::clone_thread(current_pid, child_stack - 16, fn_ptr) {
        Ok(child_pid) => {
            // Enqueue the child in the scheduler
            crate::scheduler::enqueue_process(child_pid);
            crate::serial_println!(
                "[SYSCALL] clone: thread PID {} created (shared PML4, stack=0x{:X}, fn=0x{:X})",
                child_pid, child_stack - 16, fn_ptr
            );
            child_pid
        }
        Err(e) => {
            crate::serial_println!("[SYSCALL] clone: error: {}", e);
            ENOMEM
        }
    }
}

// ===== sys_yield() =====

/// Voluntarily yield the CPU to another ready process.
/// Essential for cooperative multitasking and userspace spinlocks/mutexes.
fn sys_yield() -> u64 {
    // Re-enqueue current process and pick the next one
    crate::scheduler::schedule_next();
    // Pause briefly
    unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    0 // Success
}

/// Fork the current process (Jalon 25a - REAL UNIX FORK).
/// Deep-copies the entire user address space (PML4[1..255]).
/// Returns: child_pid in parent, 0 in child (via saved register state).
fn sys_fork() -> u64 {
    let current_pid = crate::scheduler::current_pid();

    let (pool_used, pool_max) = crate::elf::pool_stats();
    crate::serial_println!("[SYSCALL] sys_fork() from PID {} (frame pool {}/{} used)", current_pid, pool_used, pool_max);

    // Get the current process's PML4 and info
    let parent_info = match crate::process::with_process(current_pid, |p| {
        (p.pml4_phys, p.entry_point, p.stack_pointer)
    }) {
        Some(info) => info,
        None => return ENOMEM,
    };
    let (parent_pml4, parent_entry, parent_stack) = parent_info;

    // Deep-copy the PML4 (user pages get fresh frames with copied data)
    let child_pml4 = unsafe {
        match clone_pml4_deep(parent_pml4) {
            Some(pml4) => pml4,
            None => {
                crate::serial_println!("[SYSCALL] fork: failed to deep-clone PML4");
                return ENOMEM;
            }
        }
    };

    // Capture parent's current user-mode return state from this syscall.
    // syscall_entry saved: user RIP in RCX -> gs:[16], user RSP -> gs:[8].
    // The kernel stack has: [r15, r14, r13, r12, rbx, rbp, r11(RFLAGS), rcx(RIP)]
    let parent_rip = saved_user_rip();
    let parent_rsp = saved_user_rsp();
    let parent_kernel_rsp = unsafe { PER_CPU.saved_kernel_rsp };
    let saved_regs: [u64; 8] = if parent_kernel_rsp != 0 {
        let ptr = parent_kernel_rsp as *const u64;
        unsafe {
            [
                core::ptr::read_volatile(ptr),          // r15
                core::ptr::read_volatile(ptr.add(1)),   // r14
                core::ptr::read_volatile(ptr.add(2)),   // r13
                core::ptr::read_volatile(ptr.add(3)),   // r12
                core::ptr::read_volatile(ptr.add(4)),   // rbx
                core::ptr::read_volatile(ptr.add(5)),   // rbp
                core::ptr::read_volatile(ptr.add(6)),   // r11 (RFLAGS)
                core::ptr::read_volatile(ptr.add(7)),   // rcx (RIP)
            ]
        }
    } else {
        [0; 8]
    };

    // Create the child process with deep-copied PML4
    match crate::process::fork_process(current_pid, child_pml4, parent_entry, parent_stack) {
        Ok(child_pid) => {
            crate::serial_println!(
                "[SYSCALL] fork: child PID {} created (PML4=0x{:X}, RIP=0x{:X}, RSP=0x{:X})",
                child_pid, child_pml4, parent_rip, parent_rsp
            );

            // Store the parent's exact register state into the child's process struct.
            // When the child is scheduled, it will resume at this RIP with these regs,
            // but with RAX=0 (the fork return value for the child).
            crate::process::with_process_mut(child_pid, |child| {
                child.saved_user_rip = parent_rip;
                child.saved_user_rsp = parent_rsp;
                child.saved_syscall_regs = saved_regs;
                child.is_forked = true;  // Flag: resume via sysretq with RAX=0
            });

            // Enqueue child in scheduler
            crate::scheduler::enqueue_process(child_pid);

            // Return child PID to parent (RAX = child_pid)
            child_pid
        }
        Err(e) => {
            crate::serial_println!("[SYSCALL] fork: error: {}", e);
            ENOMEM
        }
    }
}

/// Deep-copy a PML4 page table (Jalon 25a).
/// Kernel entries (PML4[0] and PML4[256..511]) are shared verbatim.
/// User entries (PML4[1..255]) are walked to the leaf PT level:
/// for each mapped 4K page, a NEW physical frame is allocated and the
/// 4096 bytes of data are copied byte-for-byte.
unsafe fn clone_pml4_deep(src_pml4_phys: u64) -> Option<u64> {
    let phys_offset = crate::elf::phys_offset();

    // Allocate a new PML4 frame
    let new_pml4_phys = crate::elf::alloc_demand_frame()?;
    let new_pml4 = (new_pml4_phys + phys_offset) as *mut u64;
    let src_pml4 = (src_pml4_phys + phys_offset) as *const u64;

    // Zero the new PML4
    core::ptr::write_bytes(new_pml4, 0, 512);

    let mut user_pages_copied = 0usize;

    for pml4_i in 0..512usize {
        let pml4_entry = core::ptr::read_volatile(src_pml4.add(pml4_i));
        if pml4_entry & 0x01 == 0 { continue; } // not present

        // Kernel entries: PML4[0], PML4[256..511], or any entry WITHOUT USER_ACCESSIBLE — share verbatim
        // CRITICAL: The kernel heap at 0x4444_4444_0000 maps to PML4[136] which must be shared,
        // not deep-copied, so child processes see the same kernel data structures (PROCESS_TABLE etc.)
        if pml4_i == 0 || pml4_i >= 256 || (pml4_entry & 0x04) == 0 {
            core::ptr::write_volatile(new_pml4.add(pml4_i), pml4_entry);
            continue;
        }

        // User entry (PML4[1..255]) — deep copy
        let src_pdpt_phys = pml4_entry & 0x000F_FFFF_FFFF_F000;
        let src_pdpt = (src_pdpt_phys + phys_offset) as *const u64;

        // Allocate new PDPT
        let new_pdpt_phys = crate::elf::alloc_demand_frame()?;
        let new_pdpt = (new_pdpt_phys + phys_offset) as *mut u64;
        core::ptr::write_bytes(new_pdpt, 0, 512);

        for pdpt_i in 0..512usize {
            let pdpt_entry = core::ptr::read_volatile(src_pdpt.add(pdpt_i));
            if pdpt_entry & 0x01 == 0 { continue; }
            // 1G huge page check (bit 7) — unlikely but skip
            if pdpt_entry & 0x80 != 0 {
                core::ptr::write_volatile(new_pdpt.add(pdpt_i), pdpt_entry);
                continue;
            }

            let src_pd_phys = pdpt_entry & 0x000F_FFFF_FFFF_F000;
            let src_pd = (src_pd_phys + phys_offset) as *const u64;

            // Allocate new PD
            let new_pd_phys = crate::elf::alloc_demand_frame()?;
            let new_pd = (new_pd_phys + phys_offset) as *mut u64;
            core::ptr::write_bytes(new_pd, 0, 512);

            for pd_i in 0..512usize {
                let pd_entry = core::ptr::read_volatile(src_pd.add(pd_i));
                if pd_entry & 0x01 == 0 { continue; }
                // 2M huge page check (bit 7) — unlikely but skip
                if pd_entry & 0x80 != 0 {
                    core::ptr::write_volatile(new_pd.add(pd_i), pd_entry);
                    continue;
                }

                let src_pt_phys = pd_entry & 0x000F_FFFF_FFFF_F000;
                let src_pt = (src_pt_phys + phys_offset) as *const u64;

                // Allocate new PT
                let new_pt_phys = crate::elf::alloc_demand_frame()?;
                let new_pt = (new_pt_phys + phys_offset) as *mut u64;
                core::ptr::write_bytes(new_pt, 0, 512);

                for pt_i in 0..512usize {
                    let pt_entry = core::ptr::read_volatile(src_pt.add(pt_i));
                    if pt_entry & 0x01 == 0 { continue; }

                    // Extract physical address (bits 12-51) and flags
                    let src_frame_phys = pt_entry & 0x000F_FFFF_FFFF_F000;
                    // Preserve all flag bits (lower 12 + NX bit 63)
                    let flag_bits = pt_entry & 0x8000_0000_0000_0FFF;

                    // Allocate a NEW physical frame and copy 4096 bytes
                    let new_frame_phys = crate::elf::alloc_demand_frame()?;
                    let src_data = (src_frame_phys.wrapping_add(phys_offset)) as *const u8;
                    let dst_data = (new_frame_phys.wrapping_add(phys_offset)) as *mut u8;
                    core::ptr::copy_nonoverlapping(src_data, dst_data, 4096);

                    // Map with same flags
                    core::ptr::write_volatile(new_pt.add(pt_i), new_frame_phys | flag_bits);
                    user_pages_copied += 1;
                }

                // Map new PT in new PD with same flags
                let pd_flags = pd_entry & 0x8000_0000_0000_0FFF;
                core::ptr::write_volatile(new_pd.add(pd_i), new_pt_phys | pd_flags);
            }

            // Map new PD in new PDPT with same flags
            let pdpt_flags = pdpt_entry & 0x8000_0000_0000_0FFF;
            core::ptr::write_volatile(new_pdpt.add(pdpt_i), new_pd_phys | pdpt_flags);
        }

        // Map new PDPT in new PML4 with same flags
        let pml4_flags = pml4_entry & 0x8000_0000_0000_0FFF;
        core::ptr::write_volatile(new_pml4.add(pml4_i), new_pdpt_phys | pml4_flags);
    }

    crate::serial_println!(
        "[FORK] Deep-copied PML4: src=0x{:X} -> dst=0x{:X}, {} user pages copied ({} KB)",
        src_pml4_phys, new_pml4_phys, user_pages_copied, user_pages_copied * 4
    );

    Some(new_pml4_phys)
}

// ===== sys_exec(path) =====

/// Execute a new ELF binary, replacing the current process.
fn sys_exec(path_addr: u64) -> u64 {
    if !validate_user_ptr(path_addr, 1) {
        return EFAULT;
    }

    let path = match unsafe { read_user_string(path_addr) } {
        Some(p) => p,
        None => return EFAULT,
    };

    crate::serial_println!("[SYSCALL] sys_exec(\"{}\")", path);

    // Try to load from VFS
    let vfs_path = if path.starts_with('/') {
        path.clone()
    } else {
        alloc::format!("/bin/{}", path)
    };

    // Read ELF data from VFS
    let elf_data = match crate::fs::vfs::file_read(&vfs_path) {
        Ok(data) => data,
        Err(_) => {
            crate::serial_println!("[SYSCALL] exec: file not found: {}", vfs_path);
            return ENOENT;
        }
    };

    // Load the ELF binary (Jalon 25b - real sys_exec)
    match crate::elf::load_elf_binary(&elf_data) {
        Ok(result) => {
            let current_pid = crate::scheduler::current_pid();
            crate::serial_println!("[SYSCALL] exec: loaded {} for PID {}, entry=0x{:X}",
                vfs_path, current_pid, result.entry_point);

            // Replace the current process's address space and state
            crate::process::with_process_mut(current_pid, |p| {
                p.pml4_phys = result.pml4_phys;
                p.entry_point = result.entry_point;
                p.stack_pointer = result.stack_pointer;
                p.name = alloc::string::String::from(&vfs_path[..]);
                // Reset FD table to just stdio (exec replaces everything)
                p.fd_table = crate::process::FdTable::new_with_stdio();
                // Clear saved state
                p.saved_user_rip = 0;
                p.saved_user_rsp = 0;
                p.saved_syscall_regs = [0; 8];
                p.is_forked = false;
            });

            // Switch CR3 and jump to Ring 3
            unsafe {
                core::arch::asm!(
                    "mov cr3, {}",
                    in(reg) result.pml4_phys,
                    options(nostack)
                );
                // swapgs before IRETQ: we're inside a syscall handler (GS=PER_CPU)
                // Must restore user GS (=0) before returning to Ring 3
                core::arch::asm!("swapgs", options(nomem, nostack));
                crate::elf::jump_to_ring3(result.entry_point, result.stack_pointer);
            }
        }
        Err(e) => {
            crate::serial_println!("[SYSCALL] exec: ELF load failed: {}", e);
            ENOENT
        }
    }
}

// ===== sys_exit(code) =====

/// Terminate the current user process or thread.
fn sys_exit(code: u64) -> u64 {
    let current = crate::scheduler::current_pid();
    let is_thread = crate::process::with_process(current, |p| p.is_thread).unwrap_or(false);

    crate::serial_println!(
        "[SYSCALL] sys_exit({}) - {} {} terminating",
        code, if is_thread { "Thread" } else { "Process" }, current
    );

    if current != 0 {
        crate::process::set_exit_code(current, code as i32);
        let _ = crate::process::set_state(
            current,
            crate::process::ProcessState::Terminated,
        );
        crate::serial_println!("[SYSCALL] PID {} terminated (exit {})", current, code);
    }

    if is_thread {
        // Thread exit: find the parent and check for more siblings
        let parent_pid = crate::process::with_process(current, |p| p.ppid).unwrap_or(0);
        if parent_pid != 0 {
            crate::serial_println!("[SYSCALL] Thread {} done, checking siblings for parent PID {}", current, parent_pid);

            // Check if parent has more child threads to run
            if let Some((next_child, child_entry, child_stack, child_pml4)) =
                crate::process::find_ready_child_thread(parent_pid)
            {
                // Launch next sibling thread
                crate::serial_println!(
                    "[SYSCALL] Launching next thread PID {} (entry=0x{:X}, stack=0x{:X})",
                    next_child, child_entry, child_stack
                );
                let _ = crate::process::set_state(next_child, crate::process::ProcessState::Running);
                crate::scheduler::set_current_pid(next_child);
                unsafe {
                    core::arch::asm!("mov cr3, {}", in(reg) child_pml4, options(nostack));
                    core::arch::asm!(
                        "swapgs",           // Restore user GS before Ring 3
                        "push 0x1B",        // SS
                        "push {stack}",     // RSP
                        "push 0x202",       // RFLAGS
                        "push 0x23",        // CS
                        "push {entry}",     // RIP
                        "iretq",
                        stack = in(reg) child_stack,
                        entry = in(reg) child_entry,
                        options(noreturn),
                    );
                }
            }

            // No more child threads — resume parent at its saved context.
            // The parent's full register state was saved on the kernel syscall stack
            // by syscall_entry. We restore the kernel RSP to that point and let the
            // normal pop + sysretq path run, which correctly restores all user registers.
            let parent_info = crate::process::with_process(parent_pid, |p| {
                (p.saved_user_rip, p.saved_user_rsp, p.pml4_phys, p.saved_kernel_rsp, p.saved_syscall_regs)
            });

            if let Some((saved_rip, saved_rsp, pml4, saved_kernel_rsp, saved_regs)) = parent_info {
                crate::serial_println!(
                    "[SYSCALL] All threads done, resuming parent PID {} at RIP=0x{:X} RSP=0x{:X}",
                    parent_pid, saved_rip, saved_rsp
                );

                // Set scheduler to parent
                crate::scheduler::set_current_pid(parent_pid);
                let _ = crate::process::set_state(parent_pid, crate::process::ProcessState::Running);

                // Switch CR3 to parent PML4
                unsafe {
                    core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack));
                }

                // Resume parent via sysretq through the normal syscall_entry return path.
                // This restores all user registers (r15-r12, rbx, rbp, r11=RFLAGS, rcx=RIP)
                // that were saved by syscall_entry when the parent called sys_wait.
                let wait_result = ((current & 0xFFFF) << 16) | (code & 0xFFFF);
                crate::serial_println!(
                    "[SYSCALL] sysretq to parent: RAX=0x{:X}, regs saved={}",
                    wait_result, saved_regs[7] != 0
                );

                if saved_regs[7] != 0 {
                    // Restore the full register state from saved_regs and return via sysretq.
                    // saved_regs: [r15, r14, r13, r12, rbx, rbp, r11(RFLAGS), rcx(RIP)]
                    let r15 = saved_regs[0];
                    let r14 = saved_regs[1];
                    let r13 = saved_regs[2];
                    let r12 = saved_regs[3];
                    let rbx = saved_regs[4];
                    let rbp = saved_regs[5];
                    let r11 = saved_regs[6]; // user RFLAGS
                    let rcx = saved_regs[7]; // user RIP

                    // Write parent's user RSP into PER_CPU.user_rsp (gs:[8])
                    unsafe { PER_CPU.user_rsp = saved_rsp; }

                    crate::serial_println!(
                        "[SYSCALL] Restoring regs: RCX(RIP)=0x{:X} R11(RFLAGS)=0x{:X} RBP=0x{:X} RBX=0x{:X}",
                        rcx, r11, rbp, rbx
                    );

                    unsafe {
                        core::arch::asm!(
                            "mov r15, {v_r15}",
                            "mov r14, {v_r14}",
                            "mov r13, {v_r13}",
                            "mov r12, {v_r12}",
                            "mov rbx, {v_rbx}",
                            "mov rbp, {v_rbp}",
                            "mov r11, {v_r11}",
                            "mov rcx, {v_rcx}",
                            "mov rax, {result}",
                            "mov rsp, gs:[8]",      // Restore user RSP
                            "swapgs",               // Swap back to user GS
                            "sysretq",              // Return to Ring 3
                            v_r15 = in(reg) r15,
                            v_r14 = in(reg) r14,
                            v_r13 = in(reg) r13,
                            v_r12 = in(reg) r12,
                            v_rbx = in(reg) rbx,
                            v_rbp = in(reg) rbp,
                            v_r11 = in(reg) r11,
                            v_rcx = in(reg) rcx,
                            result = in(reg) wait_result,
                            options(noreturn),
                        );
                    }
                } else if saved_rip != 0 && saved_rsp != 0 {
                    // Fallback: kernel_rsp not saved, use IRETQ (may lose callee-saved regs)
                    crate::serial_println!(
                        "[SYSCALL] Fallback IRETQ to parent: RIP=0x{:X}, RSP=0x{:X}",
                        saved_rip, saved_rsp
                    );
                    unsafe {
                        core::arch::asm!(
                            "mov rax, {result}",
                            "swapgs",
                            "push 0x1B",
                            "push {stack}",
                            "push 0x202",
                            "push 0x23",
                            "push {entry}",
                            "iretq",
                            result = in(reg) wait_result,
                            stack = in(reg) saved_rsp,
                            entry = in(reg) saved_rip,
                            options(noreturn),
                        );
                    }
                } else {
                    // Fallback: restart parent from entry_point
                    let entry = crate::process::with_process(parent_pid, |p| p.entry_point).unwrap_or(0);
                    let stack = crate::process::with_process(parent_pid, |p| p.stack_pointer).unwrap_or(0);
                    crate::serial_println!(
                        "[SYSCALL] Fallback: restarting parent at entry=0x{:X}",
                        entry
                    );
                    unsafe {
                        core::arch::asm!(
                            "swapgs",           // Restore user GS before Ring 3
                            "push 0x1B",
                            "push {stack}",
                            "push 0x202",
                            "push 0x23",
                            "push {entry}",
                            "iretq",
                            stack = in(reg) stack,
                            entry = in(reg) entry,
                            options(noreturn),
                        );
                    }
                }
            }
        }
    } else {
        // ===== Forked child exit: resume blocked parent (Jalon 25 - CRITICAL FIX) =====
        // When a forked child (not a thread) exits, the parent is Blocked in sys_wait.
        // We must unblock the parent and restore its context so it receives the wait result.
        let parent_pid_opt = crate::process::with_process(current, |p| p.ppid);
        let parent_pid = parent_pid_opt.unwrap_or(0);
        crate::serial_println!("[SYSCALL] Forked child exit: current={} ppid_opt={:?} ppid={}", current, parent_pid_opt, parent_pid);

        if parent_pid != 0 {
            let parent_blocked = crate::process::with_process(parent_pid, |p| {
                p.state == crate::process::ProcessState::Blocked
            });
            crate::serial_println!("[SYSCALL] parent_blocked={:?}", parent_blocked);

            if parent_blocked == Some(true) {
                crate::serial_println!(
                    "[SYSCALL] Forked child PID {} exited (code {}), resuming parent PID {}",
                    current, code, parent_pid
                );

                let parent_ctx = crate::process::with_process(parent_pid, |p| {
                    (p.saved_user_rip, p.saved_user_rsp, p.saved_kernel_rsp,
                     p.saved_syscall_regs, p.pml4_phys)
                });

                if let Some((saved_rip, saved_rsp, _parent_kernel_rsp, saved_regs, pml4)) = parent_ctx {
                    if saved_regs[7] != 0 {
                        // Set parent back to Running
                        let _ = crate::process::set_state(
                            parent_pid,
                            crate::process::ProcessState::Running,
                        );
                        crate::scheduler::set_current_pid(parent_pid);

                        // Build wait result: (child_pid << 16) | exit_code
                        let wait_result = ((current & 0xFFFF) << 16) | (code & 0xFFFF);

                        let r15 = saved_regs[0];
                        let r14 = saved_regs[1];
                        let r13 = saved_regs[2];
                        let r12 = saved_regs[3];
                        let rbx = saved_regs[4];
                        let rbp = saved_regs[5];
                        let r11 = saved_regs[6]; // RFLAGS
                        let rcx = saved_regs[7]; // RIP

                        unsafe { PER_CPU.user_rsp = saved_rsp; }

                        crate::serial_println!(
                            "[SYSCALL] Resuming parent PID {} via sysretq: RAX=0x{:X} RIP=0x{:X} RSP=0x{:X}",
                            parent_pid, wait_result, rcx, saved_rsp
                        );

                        unsafe {
                            core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack));
                            core::arch::asm!(
                                "mov r15, {v_r15}",
                                "mov r14, {v_r14}",
                                "mov r13, {v_r13}",
                                "mov r12, {v_r12}",
                                "mov rbx, {v_rbx}",
                                "mov rbp, {v_rbp}",
                                "mov r11, {v_r11}",
                                "mov rcx, {v_rcx}",
                                "mov rax, {result}",
                                "mov rsp, gs:[8]",
                                "swapgs",
                                "sysretq",
                                v_r15 = in(reg) r15,
                                v_r14 = in(reg) r14,
                                v_r13 = in(reg) r13,
                                v_r12 = in(reg) r12,
                                v_rbx = in(reg) rbx,
                                v_rbp = in(reg) rbp,
                                v_r11 = in(reg) r11,
                                v_rcx = in(reg) rcx,
                                result = in(reg) wait_result,
                                options(noreturn),
                            );
                        }
                    } else if saved_rip != 0 && saved_rsp != 0 {
                        // Fallback: IRETQ if no kernel registers saved
                        let _ = crate::process::set_state(
                            parent_pid,
                            crate::process::ProcessState::Running,
                        );
                        crate::scheduler::set_current_pid(parent_pid);

                        let wait_result = ((current & 0xFFFF) << 16) | (code & 0xFFFF);
                        crate::serial_println!(
                            "[SYSCALL] Fallback IRETQ to parent PID {}: RIP=0x{:X}",
                            parent_pid, saved_rip
                        );

                        unsafe {
                            core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack));
                            core::arch::asm!(
                                "mov rax, {result}",
                                "swapgs",
                                "push 0x1B",
                                "push {stack}",
                                "push 0x202",
                                "push 0x23",
                                "push {entry}",
                                "iretq",
                                result = in(reg) wait_result,
                                stack = in(reg) saved_rsp,
                                entry = in(reg) saved_rip,
                                options(noreturn),
                            );
                        }
                    }
                }
            }
        }
        // Fallthrough: parent not blocked or no parent → normal process exit below
    }

    // Main process exit — print the banner
    crate::serial_println!("========================================");
    crate::serial_println!("[SUCCESS] Ring 3 process PID {} exited (code {})", current, code);
    crate::serial_println!("========================================");

    // Try to launch the next queued userspace process
    launch_next_userspace_process(current);

    // If we reach here, no more processes to run
    loop { unsafe { asm!("hlt", options(nomem, nostack)); } }
}

// ===== sys_wait(pid) =====

/// Launch the next ready userspace process (used by sys_exit and sys_wait).
/// Handles both normal processes (IRETQ to entry_point) and forked children
/// (sysretq to parent's saved RIP with RAX=0).
fn launch_next_userspace_process(exclude_pid: u64) {
    let next_ready = crate::process::find_next_ready_userspace(exclude_pid);
    crate::serial_println!("[SYSCALL] Looking for next userspace process: {:?}", next_ready);

    if let Some((next_pid, entry, stack, pml4, name)) = next_ready {
        // Check if this is a forked child that should resume at parent's saved RIP
        let fork_info = crate::process::with_process(next_pid, |p| {
            (p.is_forked, p.saved_user_rip, p.saved_user_rsp, p.saved_syscall_regs)
        });

        if let Some((true, saved_rip, saved_rsp, saved_regs)) = fork_info {
            if saved_regs[7] != 0 {
                // Forked child: resume at parent's exact RIP with RAX=0
                crate::serial_println!(
                    "[SYSCALL] Launching FORKED child PID {} ({}) via sysretq with RAX=0, RIP=0x{:X}",
                    next_pid, name, saved_rip
                );

                crate::scheduler::set_current_pid(next_pid);
                let _ = crate::process::set_state(next_pid, crate::process::ProcessState::Running);
                // Clear the forked flag so it won't re-trigger
                crate::process::with_process_mut(next_pid, |p| { p.is_forked = false; });

                let r15 = saved_regs[0];
                let r14 = saved_regs[1];
                let r13 = saved_regs[2];
                let r12 = saved_regs[3];
                let rbx = saved_regs[4];
                let rbp = saved_regs[5];
                let r11 = saved_regs[6]; // RFLAGS
                let rcx = saved_regs[7]; // RIP

                // Write child's user RSP into PER_CPU
                unsafe { PER_CPU.user_rsp = saved_rsp; }

                unsafe {
                    core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack));
                    core::arch::asm!(
                        "mov r15, {v_r15}",
                        "mov r14, {v_r14}",
                        "mov r13, {v_r13}",
                        "mov r12, {v_r12}",
                        "mov rbx, {v_rbx}",
                        "mov rbp, {v_rbp}",
                        "mov r11, {v_r11}",
                        "mov rcx, {v_rcx}",
                        "xor eax, eax",         // RAX = 0 (fork return for child!)
                        "mov rsp, gs:[8]",      // Restore user RSP
                        "swapgs",               // Swap back to user GS
                        "sysretq",              // Return to Ring 3
                        v_r15 = in(reg) r15,
                        v_r14 = in(reg) r14,
                        v_r13 = in(reg) r13,
                        v_r12 = in(reg) r12,
                        v_rbx = in(reg) rbx,
                        v_rbp = in(reg) rbp,
                        v_r11 = in(reg) r11,
                        v_rcx = in(reg) rcx,
                        options(noreturn),
                    );
                }
            }
        }

        // Normal process launch: IRETQ to entry_point
        crate::serial_println!(
            "[SYSCALL] Launching next process: PID {} ({}) entry=0x{:X}",
            next_pid, name, entry
        );
        crate::scheduler::set_current_pid(next_pid);
        let _ = crate::process::set_state(next_pid, crate::process::ProcessState::Running);
        unsafe {
            core::arch::asm!("mov cr3, {}", in(reg) pml4, options(nostack));
            core::arch::asm!(
                "swapgs",           // Restore user GS before Ring 3
                "push 0x1B",        // SS
                "push {stack}",     // RSP
                "push 0x202",       // RFLAGS
                "push 0x23",        // CS
                "push {entry}",     // RIP
                "iretq",
                stack = in(reg) stack,
                entry = in(reg) entry,
                options(noreturn),
            );
        }
    }
}

/// Wait for a child process/thread to terminate.
/// pid=0 means wait for any child.
/// For threads: launches the child by doing IRETQ to its entry/stack,
/// saves the parent's user context so it can be resumed when all children are done.
fn sys_wait(pid: u64) -> u64 {
    let current = crate::scheduler::current_pid();
    crate::serial_println!("[SYSCALL] sys_wait({}) from PID {}", pid, current);

    // Save the parent's user-mode return address so we can resume after threads
    let parent_rip = saved_user_rip();
    let parent_rsp = saved_user_rsp();
    // PER_CPU.saved_kernel_rsp was set by syscall_entry for THIS syscall (parent's).
    // Save it now before any child syscalls overwrite it.
    let parent_kernel_rsp = unsafe { PER_CPU.saved_kernel_rsp };
    // Copy the 8 saved registers from the kernel stack into the process struct.
    // The shared kernel syscall stack will be overwritten by child thread syscalls.
    // Stack layout (from RSP upward): r15, r14, r13, r12, rbx, rbp, r11(RFLAGS), rcx(RIP)
    let saved_regs: [u64; 8] = if parent_kernel_rsp != 0 {
        let ptr = parent_kernel_rsp as *const u64;
        unsafe {
            [
                core::ptr::read_volatile(ptr),          // r15
                core::ptr::read_volatile(ptr.add(1)),   // r14
                core::ptr::read_volatile(ptr.add(2)),   // r13
                core::ptr::read_volatile(ptr.add(3)),   // r12
                core::ptr::read_volatile(ptr.add(4)),   // rbx
                core::ptr::read_volatile(ptr.add(5)),   // rbp
                core::ptr::read_volatile(ptr.add(6)),   // r11 (RFLAGS)
                core::ptr::read_volatile(ptr.add(7)),   // rcx (RIP)
            ]
        }
    } else {
        [0; 8]
    };
    crate::process::with_process_mut(current, |p| {
        p.saved_user_rip = parent_rip;
        p.saved_user_rsp = parent_rsp;
        p.saved_kernel_rsp = parent_kernel_rsp;
        p.saved_syscall_regs = saved_regs;
    });
    crate::serial_println!(
        "[SYSCALL] wait: saved parent context RIP=0x{:X} RSP=0x{:X} KRSP=0x{:X}",
        parent_rip, parent_rsp, parent_kernel_rsp
    );

    // Check for ready child threads to run
    let child_info = crate::process::find_ready_child_thread(current);

    if let Some((child_pid, child_entry, child_stack, child_pml4)) = child_info {
        // We have a child thread to run!
        crate::serial_println!(
            "[SYSCALL] wait: launching thread PID {} (entry=0x{:X}, stack=0x{:X})",
            child_pid, child_entry, child_stack
        );

        // Mark child as Running, parent as Blocked
        let _ = crate::process::set_state(child_pid, crate::process::ProcessState::Running);
        let _ = crate::process::set_state(current, crate::process::ProcessState::Blocked);
        // Set scheduler's current PID to child
        crate::scheduler::set_current_pid(child_pid);

        // Switch CR3 to child's PML4 (same as parent for threads)
        unsafe {
            core::arch::asm!("mov cr3, {}", in(reg) child_pml4, options(nostack));
        }

        // IRETQ to the child thread!
        unsafe {
            core::arch::asm!(
                "swapgs",           // Restore user GS before Ring 3
                "push 0x1B",        // SS
                "push {stack}",     // RSP
                "push 0x202",       // RFLAGS (IF=1)
                "push 0x23",        // CS
                "push {entry}",     // RIP
                "iretq",
                stack = in(reg) child_stack,
                entry = in(reg) child_entry,
                options(noreturn),
            );
        }
    }

    // No child threads to launch — check for forked children to launch
    // A forked child has is_forked=true and should be launched with sysretq(RAX=0).
    let forked_child = crate::process::find_ready_forked_child(current, pid);
    if let Some((child_pid, child_pml4, child_rip, child_rsp, child_regs)) = forked_child {
        crate::serial_println!(
            "[SYSCALL] wait: launching FORKED child PID {} (RIP=0x{:X}, RSP=0x{:X})",
            child_pid, child_rip, child_rsp
        );
        crate::serial_println!(
            "[FORK-LAUNCH] regs: r15=0x{:X} r14=0x{:X} r13=0x{:X} r12=0x{:X} rbx=0x{:X} rbp=0x{:X} r11=0x{:X} rcx=0x{:X}",
            child_regs[0], child_regs[1], child_regs[2], child_regs[3],
            child_regs[4], child_regs[5], child_regs[6], child_regs[7]
        );

        // Mark child as Running, parent as Blocked
        let _ = crate::process::set_state(child_pid, crate::process::ProcessState::Running);
        let _ = crate::process::set_state(current, crate::process::ProcessState::Blocked);
        crate::scheduler::set_current_pid(child_pid);

        // Clear forked flag
        crate::process::with_process_mut(child_pid, |p| { p.is_forked = false; });

        let r15 = child_regs[0];
        let r14 = child_regs[1];
        let r13 = child_regs[2];
        let r12 = child_regs[3];
        let rbx = child_regs[4];
        let rbp = child_regs[5];
        let r11 = child_regs[6];
        let rcx = child_regs[7];

        unsafe { PER_CPU.user_rsp = child_rsp; }

        unsafe {
            core::arch::asm!("mov cr3, {}", in(reg) child_pml4, options(nostack));
            core::arch::asm!(
                "mov r15, {v_r15}",
                "mov r14, {v_r14}",
                "mov r13, {v_r13}",
                "mov r12, {v_r12}",
                "mov rbx, {v_rbx}",
                "mov rbp, {v_rbp}",
                "mov r11, {v_r11}",
                "mov rcx, {v_rcx}",
                "xor eax, eax",         // RAX = 0 (fork return for child!)
                "mov rsp, gs:[8]",
                "swapgs",
                "sysretq",
                v_r15 = in(reg) r15,
                v_r14 = in(reg) r14,
                v_r13 = in(reg) r13,
                v_r12 = in(reg) r12,
                v_rbx = in(reg) rbx,
                v_rbp = in(reg) rbp,
                v_r11 = in(reg) r11,
                v_rcx = in(reg) rcx,
                options(noreturn),
            );
        }
    }

    // No child threads to launch — poll for already-terminated children
    let max_iters = 50_000_000u64;
    for _ in 0..max_iters {
        match crate::process::wait_for_child(current) {
            Ok((child_pid, exit_code)) => {
                crate::serial_println!("[SYSCALL] wait: child PID {} exited with {}", child_pid, exit_code);
                // Return child PID in upper 16 bits, exit code in lower 16
                return ((child_pid & 0xFFFF) << 16) | (exit_code as u64 & 0xFFFF);
            }
            Err(crate::process::ProcessError::WaitingForChild) => {
                // No child terminated yet, yield
                unsafe { asm!("pause", options(nomem, nostack)); }
            }
            Err(_) => return ECHILD,
        }
    }

    // Timeout
    ECHILD
}

// ===== sys_kill(pid, signal) =====

// ===== Jalon 24: sys_pipe, sys_dup2, sys_getdents =====

/// Global pipe buffer storage.
/// Each pipe has a 4KB circular buffer identified by a pipe_id.
/// The pipe_id is stored in the FD path as "pipe:<id>:r" or "pipe:<id>:w".
mod pipe {
    use spin::Mutex;
    
    const PIPE_BUF_SIZE: usize = 4096;
    const MAX_PIPES: usize = 16;
    
    struct PipeBuf {
        data: [u8; PIPE_BUF_SIZE],
        read_pos: usize,
        write_pos: usize,
        count: usize,       // bytes available to read
        writer_closed: bool, // true when write end is closed
        reader_closed: bool, // true when read end is closed
        active: bool,
    }
    
    impl PipeBuf {
        const fn new() -> Self {
            PipeBuf {
                data: [0u8; PIPE_BUF_SIZE],
                read_pos: 0,
                write_pos: 0,
                count: 0,
                writer_closed: false,
                reader_closed: false,
                active: false,
            }
        }
    }
    
    static PIPES: Mutex<[PipeBuf; MAX_PIPES]> = Mutex::new([
        PipeBuf::new(), PipeBuf::new(), PipeBuf::new(), PipeBuf::new(),
        PipeBuf::new(), PipeBuf::new(), PipeBuf::new(), PipeBuf::new(),
        PipeBuf::new(), PipeBuf::new(), PipeBuf::new(), PipeBuf::new(),
        PipeBuf::new(), PipeBuf::new(), PipeBuf::new(), PipeBuf::new(),
    ]);
    
    /// Allocate a new pipe, returns pipe_id or None
    pub fn alloc() -> Option<usize> {
        let mut pipes = PIPES.lock();
        for i in 0..MAX_PIPES {
            if !pipes[i].active {
                pipes[i] = PipeBuf::new();
                pipes[i].active = true;
                return Some(i);
            }
        }
        None
    }
    
    /// Write data to pipe. Returns bytes written.
    pub fn write(pipe_id: usize, data: &[u8]) -> usize {
        let mut pipes = PIPES.lock();
        if pipe_id >= MAX_PIPES || !pipes[pipe_id].active {
            return 0;
        }
        let pipe = &mut pipes[pipe_id];
        let mut written = 0;
        for &byte in data {
            if pipe.count >= PIPE_BUF_SIZE {
                break; // buffer full
            }
            pipe.data[pipe.write_pos] = byte;
            pipe.write_pos = (pipe.write_pos + 1) % PIPE_BUF_SIZE;
            pipe.count += 1;
            written += 1;
        }
        written
    }
    
    /// Read data from pipe. Returns bytes read.
    pub fn read(pipe_id: usize, buf: &mut [u8]) -> usize {
        let mut pipes = PIPES.lock();
        if pipe_id >= MAX_PIPES || !pipes[pipe_id].active {
            return 0;
        }
        let pipe = &mut pipes[pipe_id];
        let mut read_count = 0;
        for slot in buf.iter_mut() {
            if pipe.count == 0 {
                break; // no data
            }
            *slot = pipe.data[pipe.read_pos];
            pipe.read_pos = (pipe.read_pos + 1) % PIPE_BUF_SIZE;
            pipe.count -= 1;
            read_count += 1;
        }
        read_count
    }
    
    /// Close writer end
    pub fn close_writer(pipe_id: usize) {
        let mut pipes = PIPES.lock();
        if pipe_id < MAX_PIPES && pipes[pipe_id].active {
            pipes[pipe_id].writer_closed = true;
            if pipes[pipe_id].reader_closed {
                pipes[pipe_id].active = false;
            }
        }
    }
    
    /// Close reader end
    pub fn close_reader(pipe_id: usize) {
        let mut pipes = PIPES.lock();
        if pipe_id < MAX_PIPES && pipes[pipe_id].active {
            pipes[pipe_id].reader_closed = true;
            if pipes[pipe_id].writer_closed {
                pipes[pipe_id].active = false;
            }
        }
    }
    
    /// Get count of bytes available to read
    pub fn available(pipe_id: usize) -> usize {
        let pipes = PIPES.lock();
        if pipe_id < MAX_PIPES && pipes[pipe_id].active {
            pipes[pipe_id].count
        } else {
            0
        }
    }
}

/// sys_pipe(pipefd_ptr): creates a pipe with a 4KB kernel buffer.
/// Writes two file descriptors to user memory: pipefd[0]=read, pipefd[1]=write.
/// Returns 0 on success, negative on error.
fn sys_pipe(pipefd_ptr: u64) -> u64 {
    crate::serial_println!("[SYSCALL] sys_pipe(pipefd_ptr=0x{:X})", pipefd_ptr);
    
    if pipefd_ptr == 0 || pipefd_ptr >= USER_ADDR_LIMIT {
        return EINVAL;
    }
    
    // Allocate kernel pipe buffer
    let pipe_id = match pipe::alloc() {
        Some(id) => id,
        None => {
            crate::serial_println!("[SYSCALL] pipe: no free pipe slots");
            return ENOMEM;
        }
    };
    
    // Get current process PID to allocate FDs
    let pid = crate::scheduler::current_pid();
    
    // Allocate read FD
    let read_fd = {
        let path = alloc::format!("pipe:{}:r", pipe_id);
        crate::process::alloc_fd(pid, &path, 0) // O_RDONLY
    };
    
    // Allocate write FD
    let write_fd = {
        let path = alloc::format!("pipe:{}:w", pipe_id);
        crate::process::alloc_fd(pid, &path, 1) // O_WRONLY
    };
    
    match (read_fd, write_fd) {
        (Some(rfd), Some(wfd)) => {
            // Write [read_fd, write_fd] to user memory as two i32 values
            let user_ptr = pipefd_ptr as *mut i32;
            unsafe {
                core::ptr::write_volatile(user_ptr, rfd as i32);
                core::ptr::write_volatile(user_ptr.add(1), wfd as i32);
            }
            crate::serial_println!("[SYSCALL] pipe: created pipe_id={}, read_fd={}, write_fd={}", pipe_id, rfd, wfd);
            0
        }
        _ => {
            crate::serial_println!("[SYSCALL] pipe: fd allocation failed");
            ENOMEM
        }
    }
}

/// sys_dup2(oldfd, newfd): duplicate file descriptor.
/// Makes newfd refer to the same file as oldfd.
/// Returns newfd on success, negative on error.
fn sys_dup2(oldfd: u32, newfd: u32) -> u64 {
    crate::serial_println!("[SYSCALL] sys_dup2({}, {})", oldfd, newfd);
    
    let pid = crate::scheduler::current_pid();
    
    // Get the path from the old FD
    let old_path = crate::process::get_fd_path(pid, oldfd as usize);
    
    match old_path {
        Some(path) => {
            // Close newfd if it's open, then set it to the same path
            crate::process::set_fd(pid, newfd as usize, &path, 2); // O_RDWR
            crate::serial_println!("[SYSCALL] dup2: {} -> {} ({})", oldfd, newfd, path);
            newfd as u64
        }
        None => {
            crate::serial_println!("[SYSCALL] dup2: oldfd {} not found", oldfd);
            EBADF
        }
    }
}

/// sys_getdents(fd, buf, bufsize): read directory entries.
/// Currently only supports reading /bin directory listing.
/// Writes entries as newline-separated filenames into buf.
/// Returns total bytes written on success.
fn sys_getdents(fd: u32, buf_ptr: u64, buf_size: u64) -> u64 {
    crate::serial_println!("[SYSCALL] sys_getdents(fd={}, buf=0x{:X}, size={})", fd, buf_ptr, buf_size);
    
    if buf_ptr == 0 || buf_ptr >= USER_ADDR_LIMIT || buf_size == 0 {
        return EINVAL;
    }
    
    // Get the path associated with this FD
    let pid = crate::scheduler::current_pid();
    let path = crate::process::get_fd_path(pid, fd as usize);
    
    let dir_path = match path {
        Some(p) => p,
        None => return EBADF,
    };
    
    crate::serial_println!("[SYSCALL] getdents: listing directory '{}'", dir_path);
    
    // Get directory listing from VFS
    let entries = {
        let root = crate::fs::vfs::lock_root();
        let mut result = alloc::vec::Vec::new();
        
        // Parse path to find directory
        if dir_path == "/bin" || dir_path == "bin" {
            if let Some(crate::fs::vfs::VfsNode::Directory(ref bin_dir)) = root.get("bin") {
                for key in bin_dir.keys() {
                    result.push(key.clone());
                }
            }
        } else if dir_path == "/" || dir_path == "." {
            for key in root.keys() {
                result.push(key.clone());
            }
        }
        result
    };
    
    // Build output buffer: entries separated by newlines
    let mut output = alloc::string::String::new();
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 { output.push('\n'); }
        output.push_str(entry);
    }
    
    let bytes = output.as_bytes();
    let to_copy = core::cmp::min(bytes.len(), buf_size as usize);
    
    if to_copy > 0 {
        let user_buf = buf_ptr as *mut u8;
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), user_buf, to_copy);
        }
    }
    
    crate::serial_println!("[SYSCALL] getdents: returned {} bytes ({} entries)", to_copy, entries.len());
    to_copy as u64
}

fn sys_kill(pid: u64, _signal: u32) -> u64 {
    crate::serial_println!("[SYSCALL] sys_kill({}, {})", pid, _signal);
    match crate::process::kill(pid) {
        Ok(()) => 0,
        Err(_) => EINVAL,
    }
}

// ===== sys_ps() - Custom: list processes =====

fn sys_ps() -> u64 {
    crate::serial_println!("\n[PS] Process Table:");
    crate::serial_println!("  PID  PPID  STATE        ROLE          NAME");
    crate::serial_println!("  ---  ----  -----------  -----------   ----");

    let pids = crate::process::list_active_pids();
    for pid in &pids {
        if let Some(info) = crate::process::get_info(*pid) {
            crate::serial_println!("  {}", info);
        }
    }
    crate::serial_println!("[PS] Total: {} active processes\n", pids.len());
    0
}

// ===== Initialization =====

/// Configure the four SYSCALL MSRs and set up the kernel stack + GS base.
/// Must be called after GDT is loaded.
pub fn init() {
    crate::serial_println!("[SYSCALL] Initializing x86_64 SYSCALL/SYSRET...");

    unsafe {
        // Prepare per-CPU data with kernel stack
        let stack_top = (&SYSCALL_STACK.0 as *const u8 as u64)
            + KERNEL_SYSCALL_STACK_SIZE as u64;
        PER_CPU.kernel_rsp = stack_top;
        PER_CPU.user_rsp = 0;

        crate::serial_println!(
            "[SYSCALL] Kernel syscall stack: top=0x{:X}, size={} bytes",
            stack_top, KERNEL_SYSCALL_STACK_SIZE
        );

        // Set KERNEL_GS_BASE to &PER_CPU (swapped in by swapgs)
        let per_cpu_addr = &PER_CPU as *const PerCpuData as u64;
        wrmsr(IA32_KERNEL_GS_BASE, per_cpu_addr);
        crate::serial_println!("[SYSCALL] KERNEL_GS_BASE = 0x{:X}", per_cpu_addr);

        // User GS base = 0 (no user TLS yet)
        wrmsr(IA32_GS_BASE, 0);

        // 1. EFER.SCE
        let efer = rdmsr(IA32_EFER);
        wrmsr(IA32_EFER, efer | EFER_SCE);
        crate::serial_println!("[SYSCALL] EFER: 0x{:016X} -> 0x{:016X}", efer, efer | EFER_SCE);

        // 2. STAR
        let star: u64 = (0x10u64 << 48) | (0x08u64 << 32);
        wrmsr(IA32_STAR, star);
        crate::serial_println!("[SYSCALL] STAR: 0x{:016X}", star);

        // 3. LSTAR
        let handler_addr = syscall_entry as *const () as u64;
        wrmsr(IA32_LSTAR, handler_addr);
        crate::serial_println!("[SYSCALL] LSTAR: 0x{:016X}", handler_addr);

        // 4. SFMASK
        wrmsr(IA32_FMASK, SFMASK_VALUE);
        crate::serial_println!("[SYSCALL] SFMASK: 0x{:04X}", SFMASK_VALUE);
    }

    crate::serial_println!("[OK] SYSCALL/SYSRET fully configured (16 syscalls registered)");
}

/// Ensure GS_BASE = 0 (user) and KERNEL_GS_BASE = PER_CPU (kernel).
/// Call this right before the initial IRETQ to Ring 3 to guarantee
/// that syscall_entry's first swapgs will work correctly.
pub fn reset_gs_bases() {
    unsafe {
        let per_cpu_addr = &PER_CPU as *const PerCpuData as u64;
        wrmsr(IA32_KERNEL_GS_BASE, per_cpu_addr);
        wrmsr(IA32_GS_BASE, 0);
        crate::serial_println!(
            "[SYSCALL] GS bases reset: GS_BASE=0, KERNEL_GS_BASE=0x{:X}",
            per_cpu_addr
        );
    }
}

// ===== sys_mmap(addr, len, prot) =====
/// Simplified mmap: allocates anonymous memory pages at a fixed virtual address.
/// Returns the virtual address of the mapped region, or ENOMEM on failure.
/// For simplicity, we always map at MMAP_BASE (0x400000000000) + offset.
/// Atomic counter for mmap allocations (each call gets a unique region)
static MMAP_NEXT_OFFSET: AtomicU64 = AtomicU64::new(0);

fn sys_mmap(addr_hint: u64, len: u64, _prot: u64) -> u64 {
    const MMAP_BASE: u64 = 0x0000_4000_0000_0000; // PML4[128]

    if len == 0 || len > 64 * 1024 * 1024 {
        return EINVAL;
    }

    let num_pages = ((len + 4095) / 4096) as usize;

    // Atomically reserve space: each mmap gets a unique address range
    let page_offset = MMAP_NEXT_OFFSET.fetch_add(num_pages as u64, AtomicOrdering::SeqCst);

    crate::serial_println!(
        "[SYSCALL] sys_mmap(addr=0x{:X}, len={}, pages={}, offset={})",
        addr_hint, len, num_pages, page_offset
    );

    // Get current process PML4 from CR3
    let cr3: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)); }
    let pml4_phys = cr3 & !0xFFF;

    // Map pages at unique offset from MMAP_BASE
    let base_vaddr = MMAP_BASE + page_offset * 4096;
    for i in 0..num_pages {
        let vaddr = base_vaddr + (i as u64) * 4096;
        let frame = unsafe { crate::elf::alloc_demand_frame() };
        match frame {
            Some(paddr) => {
                // Zero the frame
                unsafe {
                    let phys_offset = crate::elf::phys_offset();
                    core::ptr::write_bytes(
                        (paddr + phys_offset) as *mut u8,
                        0,
                        4096
                    );
                    // Map with USER | WRITABLE | PRESENT | NX
                    let flags: u64 = 0x01 | 0x02 | 0x04 | (1u64 << 63);
                    if crate::elf::demand_map_user_page(pml4_phys, vaddr, paddr, flags).is_err() {
                        crate::serial_println!("[SYSCALL] mmap: page mapping failed at 0x{:X}", vaddr);
                        return ENOMEM;
                    }
                }
            }
            None => {
                crate::serial_println!("[SYSCALL] mmap: out of frames at page {}", i);
                return ENOMEM;
            }
        }
    }

    // Flush TLB
    unsafe {
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack));
    }

    crate::serial_println!(
        "[SYSCALL] mmap: mapped {} pages ({} KB) at 0x{:X}",
        num_pages, num_pages * 4, base_vaddr
    );
    base_vaddr
}

// ===== sys_mmap_fb(info_buf) =====
/// Map the framebuffer into the calling process's address space.
/// info_buf: user pointer to a buffer of at least 4 u64s where we write:
///   [0] = framebuffer virtual address
///   [1] = width
///   [2] = height
///   [3] = stride (bytes per row)
/// Returns: the virtual address of the framebuffer, or ENOMEM on failure.
fn sys_mmap_fb(info_buf: u64) -> u64 {
    crate::serial_println!("[SYSCALL] sys_mmap_fb(info_buf=0x{:X})", info_buf);

    let fb_info = match crate::framebuffer::get_info() {
        Some(info) => info,
        None => {
            crate::serial_println!("[SYSCALL] mmap_fb: no framebuffer available");
            return ENOENT;
        }
    };

    // Get current process PML4 from CR3
    let cr3: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)); }
    let pml4_phys = cr3 & !0xFFF;

    // Map framebuffer into user address space
    let fb_vaddr = match crate::framebuffer::map_fb_for_user(pml4_phys) {
        Some(addr) => addr,
        None => {
            crate::serial_println!("[SYSCALL] mmap_fb: mapping failed");
            return ENOMEM;
        }
    };

    // Write info to user buffer if provided
    if info_buf != 0 && validate_user_ptr(info_buf, 32) {
        unsafe {
            let buf = info_buf as *mut u64;
            core::ptr::write_volatile(buf, fb_vaddr);
            core::ptr::write_volatile(buf.add(1), fb_info.width as u64);
            core::ptr::write_volatile(buf.add(2), fb_info.height as u64);
            core::ptr::write_volatile(buf.add(3), fb_info.stride as u64);
        }
        crate::serial_println!(
            "[SYSCALL] mmap_fb: wrote info to user buf: vaddr=0x{:X} {}x{} stride={}",
            fb_vaddr, fb_info.width, fb_info.height, fb_info.stride
        );
    }

    fb_vaddr
}

// ===== sys_bus_publish(intent, priority, data) =====
/// Publish a message to the Cognitive Bus from userspace.
/// intent: 16-bit intent code
/// priority: 0=Low, 1=Normal, 2=High, 3=Critical
/// data: 64-bit payload
fn sys_bus_publish(intent: u64, priority: u32, data: u64) -> u64 {
    use crate::ipc::{IntentMessage, ComponentId, Priority};

    let prio = match priority {
        0 => Priority::Low,
        1 => Priority::Normal,
        2 => Priority::High,
        _ => Priority::Critical,
    };

    let msg = IntentMessage::new(
        ComponentId::Worker,
        ComponentId::Orchestrator,
        intent as u32,
        prio,
        data,
    );

    match crate::ipc::bus::publish(msg) {
        Ok(()) => {
            crate::serial_println!(
                "[SYSCALL] bus_publish: intent=0x{:X}, prio={}, data=0x{:X}",
                intent, priority, data
            );
            0
        }
        Err(_) => {
            crate::serial_println!("[SYSCALL] bus_publish: queue full");
            EAGAIN
        }
    }
}

// ===== sys_vga_write(row, col, color_char) =====
/// Write a colored character to the VGA text buffer.
/// color_char: upper 8 bits = attribute, lower 8 bits = character
fn sys_vga_write(row: usize, col: usize, color_char: u64) -> u64 {
    const VGA_BUFFER: *mut u8 = 0xb8000 as *mut u8;
    const VGA_WIDTH: usize = 80;
    const VGA_HEIGHT: usize = 25;

    if row >= VGA_HEIGHT || col >= VGA_WIDTH {
        return EINVAL;
    }

    let ch = (color_char & 0xFF) as u8;
    let attr = ((color_char >> 8) & 0xFF) as u8;
    let offset = (row * VGA_WIDTH + col) * 2;

    unsafe {
        core::ptr::write_volatile(VGA_BUFFER.add(offset), ch);
        core::ptr::write_volatile(VGA_BUFFER.add(offset + 1), attr);
    }

    crate::serial_println!(
        "[SYSCALL] vga_write: row={}, col={}, char=0x{:02X}, attr=0x{:02X}",
        row, col, ch, attr
    );
    0
}

// ===== Network Syscalls (Couche 17) =====

/// sys_socket(domain, type, protocol) -> fd
fn sys_socket(domain: u32, sock_type: u32, protocol: u32) -> u64 {
    crate::serial_println!("[SYSCALL] sys_socket(domain={}, type={}, proto={})", domain, sock_type, protocol);
    crate::net::socket::sys_socket(domain, sock_type, protocol)
}

/// sys_sendto(fd, buf_addr, encoded_dest) -> bytes_sent
/// For UDP: encoded_dest has IP in upper 32 bits and port in lower 16
/// For TCP: encoded_dest == 0, uses connected remote; length in first 8 bytes of buf
fn sys_sendto(fd: u32, buf_addr: u64, encoded: u64) -> u64 {
    // Check if this is a TCP socket (encoded == 0 means use connected remote)
    {
        let is_tcp = {
            let table = crate::net::socket::SOCKET_TABLE.lock();
            table.get(&fd).map(|s| s.sock_type == crate::net::socket::SOCK_STREAM).unwrap_or(false)
        };

        if is_tcp {
            // TCP send: read length prefix from buf, then send data
            let len = unsafe {
                let ptr = buf_addr as *const u64;
                core::ptr::read_volatile(ptr)
            };
            crate::serial_println!("[SYSCALL] sys_sendto/TCP(fd={}, len={})", fd, len);
            return crate::net::socket::sys_tcp_send(fd, buf_addr + 8, len);
        }
    }

    // UDP/ICMP: decode dest from a3
    let ip_u32 = (encoded >> 16) as u32;
    let port = (encoded & 0xFFFF) as u16;
    let ip = crate::net::ipv4::Ipv4Addr([
        ((ip_u32 >> 24) & 0xFF) as u8,
        ((ip_u32 >> 16) & 0xFF) as u8,
        ((ip_u32 >> 8) & 0xFF) as u8,
        (ip_u32 & 0xFF) as u8,
    ]);

    // Read length from user (first 8 bytes of buf contain length)
    let len = unsafe {
        let ptr = buf_addr as *const u64;
        core::ptr::read_volatile(ptr)
    };

    crate::serial_println!("[SYSCALL] sys_sendto(fd={}, buf=0x{:X}, len={}, dst={}:{})",
        fd, buf_addr + 8, len, ip, port);

    crate::net::socket::sys_sendto(fd, buf_addr + 8, len, 0, ip, port)
}

/// sys_recvfrom(fd, buf_addr, len) -> bytes_received
fn sys_recvfrom(fd: u32, buf_addr: u64, len: u64) -> u64 {
    if !validate_user_ptr(buf_addr, len) {
        return EFAULT;
    }
    crate::net::socket::sys_recvfrom(fd, buf_addr, len)
}

/// sys_bind(fd, port) -> 0 or error
fn sys_bind(fd: u32, port: u16) -> u64 {
    crate::serial_println!("[SYSCALL] sys_bind(fd={}, port={})", fd, port);
    crate::net::socket::sys_bind(fd, port)
}

/// sys_net_ping(ip_packed, sequence) -> 0 or error
/// Custom AetherionOS syscall for ICMP ping
/// ip_packed: IP address as (a<<24|b<<16|c<<8|d)
fn sys_net_ping(ip_packed: u64, sequence: u16) -> u64 {
    let ip = crate::net::ipv4::Ipv4Addr([
        ((ip_packed >> 24) & 0xFF) as u8,
        ((ip_packed >> 16) & 0xFF) as u8,
        ((ip_packed >> 8) & 0xFF) as u8,
        (ip_packed & 0xFF) as u8,
    ]);

    crate::serial_println!("[SYSCALL] sys_net_ping({}, seq={})", ip, sequence);

    // Send ping
    if crate::net::send_ping(ip, sequence) {
        // Poll for reply with timeout
        for _ in 0..200_000u32 {
            crate::net::poll();
            if let Some(reply_ip) = crate::net::check_ping_reply(sequence) {
                crate::serial_println!("[SYSCALL] PONG from {} (seq={})", reply_ip, sequence);
                return 0; // Success
            }
            unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
        }
        crate::serial_println!("[SYSCALL] ping timeout (seq={})", sequence);
        (-110i64) as u64 // ETIMEDOUT
    } else {
        (-5i64) as u64 // EIO
    }
}

// ===== TCP Connect Syscall (Couche 18) =====

/// sys_tcp_connect(fd, encoded_ip, port)
/// encoded_ip: a<<24 | b<<16 | c<<8 | d
fn sys_tcp_connect(fd: u32, encoded_ip: u64, port: u64) -> u64 {
    let ip_a = ((encoded_ip >> 24) & 0xFF) as u8;
    let ip_b = ((encoded_ip >> 16) & 0xFF) as u8;
    let ip_c = ((encoded_ip >> 8) & 0xFF) as u8;
    let ip_d = (encoded_ip & 0xFF) as u8;
    crate::serial_println!("[SYSCALL] sys_tcp_connect(fd={}, {}.{}.{}.{}:{})", fd, ip_a, ip_b, ip_c, ip_d, port);
    crate::net::socket::sys_connect(fd, ip_a, ip_b, ip_c, ip_d, port as u16)
}

/// sys_tcp_shutdown(fd)
fn sys_tcp_shutdown_syscall(fd: u32) -> u64 {
    crate::serial_println!("[SYSCALL] sys_tcp_shutdown(fd={})", fd);
    crate::net::socket::sys_tcp_shutdown(fd)
}

/// sys_gethostbyname(name_addr) -> packed IP or negative error
fn sys_gethostbyname(name_addr: u64) -> u64 {
    if !validate_user_ptr(name_addr, 1) {
        return EFAULT;
    }
    crate::serial_println!("[SYSCALL] sys_gethostbyname(addr=0x{:X})", name_addr);
    crate::net::socket::sys_gethostbyname(name_addr)
}

/// sys_tcp_read(fd, buf_addr, len) -> bytes read or negative error
fn sys_tcp_read(fd: u32, buf_addr: u64, len: u64) -> u64 {
    if !validate_user_ptr(buf_addr, len) {
        return EFAULT;
    }
    crate::net::socket::sys_tcp_recv(fd, buf_addr, len)
}

// ===== sys_brk(new_break) - Jalon 27 =====
/// Linux-compatible brk syscall for userspace dynamic memory allocation.
///
/// - brk(0) returns the current program break.
/// - brk(addr) attempts to set the program break to `addr`.
///   If `addr` > current break, new pages are allocated and mapped.
///   If `addr` < current break, the break is moved down (pages NOT freed for simplicity).
///   Returns the new (or current) program break on success, or the old break on failure.
///
/// Heap region: 0x0000_3000_0000_0000 .. 0x0000_3000_1000_0000 (256 MiB max)
fn sys_brk(new_break: u64) -> u64 {
    const HEAP_BASE: u64 = 0x0000_3000_0000_0000;  // PML4[96] user heap
    const HEAP_MAX:  u64 = 0x0000_3000_1000_0000;  // 256 MiB limit

    let current = crate::scheduler::current_pid();
    if current == 0 {
        return 0;
    }

    let old_break = crate::process::get_heap_break(current).unwrap_or(HEAP_BASE);

    crate::serial_println!(
        "[SYSCALL] sys_brk(0x{:X}) PID={} old_break=0x{:X}",
        new_break, current, old_break
    );

    // brk(0) → return current break
    if new_break == 0 {
        return old_break;
    }

    // Validate range
    if new_break < HEAP_BASE || new_break > HEAP_MAX {
        crate::serial_println!(
            "[SYSCALL] brk: REJECTED 0x{:X} outside [{:X}, {:X}]",
            new_break, HEAP_BASE, HEAP_MAX
        );
        return old_break; // refuse out-of-range
    }

    // If growing, allocate new pages
    if new_break > old_break {
        let old_page = (old_break + 4095) / 4096 * 4096;  // round up
        let new_page = (new_break + 4095) / 4096 * 4096;

        if new_page > old_page {
            let pages_needed = ((new_page - old_page) / 4096) as usize;
            let cr3: u64;
            unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)); }
            let pml4_phys = cr3 & !0xFFF;

            for i in 0..pages_needed {
                let vaddr = old_page + (i as u64) * 4096;
                let frame = unsafe { crate::elf::alloc_demand_frame() };
                match frame {
                    Some(paddr) => {
                        unsafe {
                            let phys_offset = crate::elf::phys_offset();
                            core::ptr::write_bytes(
                                (paddr + phys_offset) as *mut u8,
                                0,
                                4096,
                            );
                            // USER | WRITABLE | PRESENT | NX
                            let flags: u64 = 0x01 | 0x02 | 0x04 | (1u64 << 63);
                            if crate::elf::demand_map_user_page(pml4_phys, vaddr, paddr, flags).is_err() {
                                crate::serial_println!("[SYSCALL] brk: mapping failed at 0x{:X}", vaddr);
                                return old_break;
                            }
                        }
                    }
                    None => {
                        crate::serial_println!("[SYSCALL] brk: out of frames at page {}", i);
                        return old_break;
                    }
                }
            }

            // Flush TLB
            unsafe { core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack)); }

            crate::serial_println!(
                "[SYSCALL] sys_brk: PID {} grew heap by {} pages ({} KB), break 0x{:X} -> 0x{:X}",
                current, pages_needed, pages_needed * 4, old_break, new_break
            );
        }
    }

    // Update process heap break
    crate::process::set_heap_break(current, new_break);
    new_break
}

// ===== sys_poll_hid() -> u64 (Jalon 38: HID Event Polling) =====
/// Returns a packed HidEvent (8 bytes) or 0 if no events.
fn sys_poll_hid() -> u64 {
    crate::drivers::mouse::poll_event()
}

// ===== sys_fb_fill_rect(packed_xy, packed_wh, color) -> u64 (Jalon 39) =====
/// Fill a rectangle on the framebuffer.
/// packed_xy = x | (y << 16), packed_wh = w | (h << 16), color = ARGB32
fn sys_fb_fill_rect(packed_xy: u64, packed_wh: u64, color: u64) -> u64 {
    let x = (packed_xy & 0xFFFF) as u32;
    let y = ((packed_xy >> 16) & 0xFFFF) as u32;
    let w = (packed_wh & 0xFFFF) as u32;
    let h = ((packed_wh >> 16) & 0xFFFF) as u32;
    let col = color as u32;

    let info = match crate::framebuffer::get_info() {
        Some(i) => i,
        None => return (-2i64) as u64, // ENOENT
    };

    // Access FB via bootloader physical memory offset mapping
    let fb_vaddr = crate::elf::phys_offset() + info.phys_addr;
    let fb_ptr = fb_vaddr as *mut u32;
    let stride_px = info.stride / 4;

    for row in y..(y + h).min(info.height) {
        for col_x in x..(x + w).min(info.width) {
            let offset = (row * stride_px + col_x) as isize;
            unsafe { fb_ptr.offset(offset).write_volatile(col); }
        }
    }
    0
}

// ===== sys_fb_draw_char(packed, color) -> u64 (Jalon 39) =====
/// Draw a single character at (x, y) in the given color.
/// packed = x | (y << 16) | (ch << 32)
fn sys_fb_draw_char(packed: u64, color: u64) -> u64 {
    let x = (packed & 0xFFFF) as u32;
    let y = ((packed >> 16) & 0xFFFF) as u32;
    let ch = ((packed >> 32) & 0xFF) as u8;
    let col = color as u32;

    let info = match crate::framebuffer::get_info() {
        Some(i) => i,
        None => return (-2i64) as u64,
    };

    draw_char_on_fb(&info, x, y, ch, col);
    0
}

// ===== sys_fb_draw_string(packed_pos, packed_str, color) -> u64 (Jalon 39) =====
/// Draw a string at (x, y). packed_str = ptr | (len << 48)
fn sys_fb_draw_string(packed_pos: u64, packed_str: u64, color: u64) -> u64 {
    let x = (packed_pos & 0xFFFF) as u32;
    let y = ((packed_pos >> 16) & 0xFFFF) as u32;
    let ptr = (packed_str & 0x0000_FFFF_FFFF_FFFF) as *const u8;
    let len = (packed_str >> 48) as usize;
    let col = color as u32;

    if !validate_user_ptr(ptr as u64, len as u64) {
        return (-14i64) as u64; // EFAULT
    }

    let info = match crate::framebuffer::get_info() {
        Some(i) => i,
        None => return (-2i64) as u64,
    };

    let mut cx = x;
    for i in 0..len {
        let ch = unsafe { core::ptr::read_volatile(ptr.add(i)) };
        if ch == b'\n' {
            // Newline handling ignored for simplicity
            continue;
        }
        draw_char_on_fb(&info, cx, y, ch, col);
        cx += 8; // 8 pixel char width
    }
    0
}

// ===== sys_fb_get_info(info_buf) -> u64 (Jalon 39) =====
/// Write framebuffer info to user buffer: [width, height, stride, bpp]
fn sys_fb_get_info(info_buf: u64) -> u64 {
    let info = match crate::framebuffer::get_info() {
        Some(i) => i,
        None => return 0,
    };

    if info_buf != 0 && validate_user_ptr(info_buf, 32) {
        unsafe {
            let buf = info_buf as *mut u64;
            core::ptr::write_volatile(buf, info.width as u64);
            core::ptr::write_volatile(buf.add(1), info.height as u64);
            core::ptr::write_volatile(buf.add(2), info.stride as u64);
            core::ptr::write_volatile(buf.add(3), info.bpp as u64);
        }
    }
    1
}

// ===== sys_rdtsc() -> u64 (Jalon 40) =====
/// Read the Time Stamp Counter.
fn sys_rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    ((hi as u64) << 32) | (lo as u64)
}

// ===== 8x16 bitmap font for framebuffer text rendering =====
/// Minimal 8x16 bitmap font - delegates to kernel/src/font.rs (Jalon 48)
fn get_font_glyph(ch: u8) -> [u8; 16] {
    crate::font::get_font_glyph(ch)
}

/// Draw a single character on the framebuffer
fn draw_char_on_fb(info: &crate::framebuffer::FramebufferInfo, x: u32, y: u32, ch: u8, color: u32) {
    let glyph = get_font_glyph(ch);
    let fb_vaddr = crate::elf::phys_offset() + info.phys_addr;
    let fb_ptr = fb_vaddr as *mut u32;
    let stride_px = info.stride / 4;

    for row in 0..16u32 {
        let py = y + row;
        if py >= info.height { break; }
        let bits = glyph[row as usize];
        for col in 0..8u32 {
            let px = x + col;
            if px >= info.width { break; }
            if bits & (0x80 >> col) != 0 {
                let offset = (py * stride_px + px) as isize;
                unsafe { fb_ptr.offset(offset).write_volatile(color); }
            }
        }
    }
}
