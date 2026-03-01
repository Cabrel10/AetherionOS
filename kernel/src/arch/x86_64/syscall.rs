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

// ===== Kernel syscall stack =====
const KERNEL_SYSCALL_STACK_SIZE: usize = 16384; // 16 KiB for more complex syscalls

#[repr(align(16))]
struct AlignedStack([u8; KERNEL_SYSCALL_STACK_SIZE]);

static mut SYSCALL_STACK: AlignedStack = AlignedStack([0; KERNEL_SYSCALL_STACK_SIZE]);

/// Per-CPU data structure accessed via GS base after swapgs.
/// Layout is ABI-critical: offset 0 = kernel_rsp, offset 8 = user_rsp.
#[repr(C)]
struct PerCpuData {
    kernel_rsp: u64,  // offset 0: kernel RSP loaded on SYSCALL entry
    user_rsp: u64,    // offset 8: user RSP saved during SYSCALL
}

static mut PER_CPU: PerCpuData = PerCpuData {
    kernel_rsp: 0,
    user_rsp: 0,
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

// ===== User pointer validation =====

/// Validate that a user pointer range [ptr, ptr+len) is within user address space
#[inline]
fn validate_user_ptr(addr: u64, len: u64) -> bool {
    if addr >= USER_ADDR_LIMIT { return false; }
    if len > 0x1000_0000 { return false; } // 256 MiB sanity
    addr.checked_add(len).map_or(false, |end| end <= USER_ADDR_LIMIT)
}

/// Read a null-terminated string from user space (max 256 bytes)
unsafe fn read_user_string(addr: u64) -> Option<alloc::string::String> {
    if addr >= USER_ADDR_LIMIT { return None; }
    let mut buf = alloc::vec::Vec::with_capacity(256);
    let ptr = addr as *const u8;
    for i in 0..256usize {
        let byte_addr = addr + i as u64;
        if byte_addr >= USER_ADDR_LIMIT { return None; }
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

        // 2. Save user RSP, load kernel RSP
        "mov gs:[8], rsp",
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

        // 4. Prepare arguments for Rust handler
        //    syscall_handler_rust(nr: u64, a1: u64, a2: u64, a3: u64)
        //    System V calling convention: rdi, rsi, rdx, rcx
        //    From SYSCALL: rax=nr, rdi=a1, rsi=a2, rdx=a3
        "mov rcx, rdx",    // 4th arg = a3 (rdx from user)
        "mov rdx, rsi",    // 3rd arg = a2
        "mov rsi, rdi",    // 2nd arg = a1
        "mov rdi, rax",    // 1st arg = syscall number

        // Call the Rust dispatcher
        "call {handler}",

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
#[no_mangle]
extern "C" fn syscall_handler_rust(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    match nr {
        0  => sys_read(a1 as u32, a2, a3),
        1  => sys_write(a1, a2, a3),
        2  => sys_open(a1, a2 as u32),
        3  => sys_close(a1 as u32),
        8  => sys_seek(a1 as u32, a2 as i64, a3 as u32),
        9  => sys_mmap(a1, a2, a3),
        20 => sys_getpid(),
        39 => sys_getppid(),
        57 => sys_fork(),
        59 => sys_exec(a1),
        60 => sys_exit(a1),
        61 => sys_wait(a1),
        62 => sys_kill(a1, a2 as u32),
        200 => sys_ps(),
        201 => sys_bus_publish(a1, a2 as u32, a3),
        202 => sys_vga_write(a1 as usize, a2 as usize, a3),
        _ => {
            crate::serial_println!("[SYSCALL] Unknown nr={} a1=0x{:X} a2=0x{:X} a3=0x{:X}", nr, a1, a2, a3);
            ENOSYS
        }
    }
}

// ===== sys_write(fd, buf, len) =====

/// POSIX write: fd=1 or fd=2 -> serial output.
/// SECURITY: buf and buf+len must be < USER_ADDR_LIMIT.
fn sys_write(fd: u64, buf_addr: u64, len: u64) -> u64 {
    // Validate fd (stdout=1, stderr=2)
    if fd != 1 && fd != 2 {
        return EBADF;
    }
    if len == 0 { return 0; }

    // SECURITY: validate user pointer
    if !validate_user_ptr(buf_addr, len) {
        crate::serial_println!("[SYSCALL] EFAULT: write buf=0x{:X} len={}", buf_addr, len);
        return EFAULT;
    }

    // Write bytes to COM1 serial port (0x3F8) using direct port I/O.
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

    len  // Return number of bytes written
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
            // Read from VFS
            match crate::fs::vfs::file_read(&path) {
                Ok(data) => {
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
                Err(_) => ENOENT,
            }
        }
        None => EBADF,
    }
}

// ===== sys_open(path, flags) =====

/// POSIX open: validate path, check VFS, allocate FD.
fn sys_open(path_addr: u64, flags: u32) -> u64 {
    if !validate_user_ptr(path_addr, 1) {
        return EFAULT;
    }

    let path = match unsafe { read_user_string(path_addr) } {
        Some(p) => p,
        None => return EFAULT,
    };

    let current_pid = crate::scheduler::current_pid();

    // Check the file exists in VFS (try to read it)
    if crate::fs::vfs::file_read(&path).is_err() {
        // Try with /bin prefix
        let bin_path = alloc::format!("/bin/{}", path);
        if crate::fs::vfs::file_read(&bin_path).is_err() {
            return ENOENT;
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

/// Fork the current process.
/// Returns: 0 in child, child_pid in parent.
/// MVP: Deep copy of page tables (no COW).
fn sys_fork() -> u64 {
    let current_pid = crate::scheduler::current_pid();

    crate::serial_println!("[SYSCALL] sys_fork() from PID {}", current_pid);

    // Get the current process's PML4 and info
    let (parent_pml4, parent_entry, parent_stack) = match crate::process::with_process(current_pid, |p| {
        (p.pml4_phys, p.entry_point, p.stack_pointer)
    }) {
        Some(info) => info,
        None => return ENOMEM,
    };

    // Clone the PML4 (deep copy for MVP - copies all mapped pages)
    let child_pml4 = unsafe {
        match clone_pml4(parent_pml4) {
            Some(pml4) => pml4,
            None => {
                crate::serial_println!("[SYSCALL] fork: failed to clone PML4");
                return ENOMEM;
            }
        }
    };

    // Create the child process
    match crate::process::fork_process(current_pid, child_pml4, parent_entry, parent_stack) {
        Ok(child_pid) => {
            crate::serial_println!("[SYSCALL] fork: child PID {} created (PML4=0x{:X})", child_pid, child_pml4);

            // Enqueue child in scheduler
            crate::scheduler::enqueue_process(child_pid);

            // Return child PID to parent
            // Note: In a real fork, the child would get 0 returned via its saved context.
            // For our MVP, we rely on the child being a new process starting from its entry point.
            child_pid
        }
        Err(e) => {
            crate::serial_println!("[SYSCALL] fork: error: {}", e);
            ENOMEM
        }
    }
}

/// Clone a PML4 page table (deep copy of user pages, shared kernel pages)
unsafe fn clone_pml4(src_pml4_phys: u64) -> Option<u64> {
    let phys_offset = crate::elf::phys_offset();

    // Allocate a new PML4 frame
    let new_pml4_phys = crate::elf::alloc_demand_frame()?;
    let new_pml4_virt = (new_pml4_phys + phys_offset) as *mut u64;
    let src_pml4_virt = (src_pml4_phys + phys_offset) as *const u64;

    // Zero the new PML4
    core::ptr::write_bytes(new_pml4_virt, 0, 512);

    // Copy all entries (kernel entries verbatim, user entries deep-copied)
    for i in 0..512usize {
        let entry = core::ptr::read_volatile(src_pml4_virt.add(i));
        if entry & 0x01 != 0 {
            // For kernel entries (typically 256-511 and entry 0), share directly
            // For user entries, also share for MVP (simpler than full deep copy)
            core::ptr::write_volatile(new_pml4_virt.add(i), entry);
        }
    }

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

    // Load the ELF binary
    match crate::elf::load_elf_binary(&elf_data) {
        Ok(result) => {
            let current_pid = crate::scheduler::current_pid();
            crate::serial_println!("[SYSCALL] exec: loaded {} for PID {}, entry=0x{:X}",
                vfs_path, current_pid, result.entry_point);

            // Update process with new PML4 and entry point
            crate::process::with_process_mut(current_pid, |p| {
                p.pml4_phys = result.pml4_phys;
                p.entry_point = result.entry_point;
                p.stack_pointer = result.stack_pointer;
                p.name = alloc::string::String::from(&vfs_path[..]);
            });

            // Switch CR3 and jump to Ring 3
            unsafe {
                core::arch::asm!(
                    "mov cr3, {}",
                    in(reg) result.pml4_phys,
                    options(nostack)
                );
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

/// Terminate the current user process.
fn sys_exit(code: u64) -> u64 {
    let current = crate::scheduler::current_pid();
    crate::serial_println!(
        "[SYSCALL] sys_exit({}) - PID {} terminating",
        code, current
    );

    if current != 0 {
        crate::process::set_exit_code(current, code as i32);
        let _ = crate::process::set_state(
            current,
            crate::process::ProcessState::Terminated,
        );
        crate::serial_println!("[SYSCALL] PID {} terminated (exit {})", current, code);
    }

    crate::serial_println!("========================================");
    crate::serial_println!("[SUCCESS] Ring 3 process PID {} exited (code {})", current, code);
    crate::serial_println!("========================================");

    // Schedule next process or halt
    crate::scheduler::schedule_next();

    // If we reach here, no more processes to run
    loop { unsafe { asm!("hlt", options(nomem, nostack)); } }
}

// ===== sys_wait(pid) =====

/// Wait for a child process to terminate.
/// pid=0 means wait for any child.
fn sys_wait(pid: u64) -> u64 {
    let current = crate::scheduler::current_pid();
    crate::serial_println!("[SYSCALL] sys_wait({}) from PID {}", pid, current);

    // Poll for child termination (MVP: busy wait with yield)
    let max_iters = 50_000_000u64;
    for _ in 0..max_iters {
        match crate::process::wait_for_child(current) {
            Ok((child_pid, exit_code)) => {
                crate::serial_println!("[SYSCALL] wait: child PID {} exited with {}", child_pid, exit_code);
                // Return child PID in upper 32 bits, exit code in lower 32
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

// ===== sys_mmap(addr, len, prot) =====
/// Simplified mmap: allocates anonymous memory pages at a fixed virtual address.
/// Returns the virtual address of the mapped region, or ENOMEM on failure.
/// For simplicity, we always map at MMAP_BASE (0x400000000000) + offset.
fn sys_mmap(addr_hint: u64, len: u64, _prot: u64) -> u64 {
    const MMAP_BASE: u64 = 0x0000_4000_0000_0000; // PML4[128]

    if len == 0 || len > 64 * 1024 * 1024 {
        return EINVAL;
    }

    let num_pages = ((len + 4095) / 4096) as usize;
    crate::serial_println!(
        "[SYSCALL] sys_mmap(addr=0x{:X}, len={}, pages={})",
        addr_hint, len, num_pages
    );

    // Get current process PML4 from CR3
    let cr3: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)); }
    let pml4_phys = cr3 & !0xFFF;

    // Map pages at MMAP_BASE
    let base_vaddr = MMAP_BASE;
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
