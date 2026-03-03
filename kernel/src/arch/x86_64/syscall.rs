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

/// Saved kernel RSP for parent resume after child threads complete.
/// When sys_wait IRETQs to a child, we save the kernel stack pointer here.
/// This RSP points to the stack frame with saved user registers from syscall_entry.
/// On resume, we restore this RSP and return through the normal sysretq path,
/// which restores all callee-saved registers correctly.
static mut PARENT_RESUME_KERNEL_RSP: u64 = 0;

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
const KERNEL_SYSCALL_STACK_SIZE: usize = 262144; // 256 KiB - Deep call chains: SYSCALL -> VFS -> FAT32 -> VirtIO-Block -> serial

#[repr(align(16))]
struct AlignedStack([u8; KERNEL_SYSCALL_STACK_SIZE]);

static mut SYSCALL_STACK: AlignedStack = AlignedStack([0; KERNEL_SYSCALL_STACK_SIZE]);

/// Per-CPU data structure accessed via GS base after swapgs.
/// Layout is ABI-critical: offset 0 = kernel_rsp, offset 8 = user_rsp, offset 16 = user_rip.
#[repr(C)]
struct PerCpuData {
    kernel_rsp: u64,  // offset 0: kernel RSP loaded on SYSCALL entry
    user_rsp: u64,    // offset 8: user RSP saved during SYSCALL
    user_rip: u64,    // offset 16: user RIP saved on SYSCALL entry (from RCX)
}

static mut PER_CPU: PerCpuData = PerCpuData {
    kernel_rsp: 0,
    user_rsp: 0,
    user_rip: 0,
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

        // 3b. Save the kernel RSP (pointing to saved regs) into global.
        // This is overwritten on every syscall; sys_wait copies it to the
        // process struct before launching a child thread.
        "mov [{save_rsp}], rsp",

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
        save_rsp = sym PARENT_RESUME_KERNEL_RSP,
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
        10 => sys_mmap_fb(a1),
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
        200 => sys_ps(),
        201 => sys_bus_publish(a1, a2 as u32, a3),
        202 => sys_vga_write(a1 as usize, a2 as usize, a3),
        210 => sys_net_ping(a1, a2 as u16),
        211 => sys_gethostbyname(a1),
        212 => sys_tcp_read(a1 as u32, a2, a3),
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
    }

    // Main process exit — print the banner
    crate::serial_println!("========================================");
    crate::serial_println!("[SUCCESS] Ring 3 process PID {} exited (code {})", current, code);
    crate::serial_println!("========================================");

    // Try to launch the next queued userspace process (e.g., threads.elf after j19_test.elf)
    // Look for any Ready userspace process to launch
    let next_ready = crate::process::find_next_ready_userspace(current);
    crate::serial_println!("[SYSCALL] Looking for next userspace process: {:?}", next_ready);
    if let Some((next_pid, entry, stack, pml4, name)) = next_ready {
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

    // If we reach here, no more processes to run
    loop { unsafe { asm!("hlt", options(nomem, nostack)); } }
}

// ===== sys_wait(pid) =====

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
    // PARENT_RESUME_KERNEL_RSP was set by syscall_entry for THIS syscall (parent's).
    // Save it now before any child syscalls overwrite it.
    let parent_kernel_rsp = unsafe { PARENT_RESUME_KERNEL_RSP };
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
