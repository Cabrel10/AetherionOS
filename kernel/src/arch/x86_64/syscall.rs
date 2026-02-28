// arch/x86_64/syscall.rs - Couche 12: Real SYSCALL/SYSRET with POSIX routing
//
// Implements a proper SYSCALL entry point with:
//   - swapgs to access kernel per-CPU data (kernel RSP)
//   - Full register save/restore on kernel stack
//   - Rust-level syscall dispatch: sys_write(1) and sys_exit(60)
//   - User pointer validation (EFAULT if buffer in kernel space)
//   - sysretq to return to Ring 3
//
// SECURITY:
//   - User pointers validated: must be < 0x0000_8000_0000_0000
//   - Kernel stack is separate from user stack (swapgs-based switch)
//   - RFLAGS.IF masked on entry (SFMASK) — no interrupt reentrancy
//   - SMEP prevents kernel from executing user pages (CR4 bit 20)
//
// GDT layout (from gdt.rs):
//   0x08  Kernel Code (Ring 0)
//   0x10  Kernel Data (Ring 0)
//   0x18  User Data   (Ring 3)
//   0x20  User Code   (Ring 3)
//
// STAR encoding:
//   [47:32] = 0x08  (kernel CS)   -> SS = 0x10
//   [63:48] = 0x10  (user base)   -> SYSRET CS = 0x10+16 = 0x20|RPL3 = 0x23
//                                     SYSRET SS = 0x10+8  = 0x18|RPL3 = 0x1B
//
// References:
//   AMD64 APM Vol. 2, section 6.1 (SYSCALL/SYSRET)
//   Intel SDM Vol. 3, section 5.8.8

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

// ===== Kernel syscall stack =====
const KERNEL_SYSCALL_STACK_SIZE: usize = 8192;

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

// ===== SYSCALL entry point (naked, assembly) =====
//
// On SYSCALL entry (hardware sets):
//   RCX = user RIP (return address)
//   R11 = user RFLAGS
//   RAX = syscall number
//   RDI, RSI, RDX, R10, R8, R9 = arguments (Linux ABI)
//
// Flow:
//   1. swapgs -> GS.base = &PER_CPU
//   2. Save user RSP to PER_CPU.user_rsp
//   3. Load kernel RSP from PER_CPU.kernel_rsp
//   4. Push all registers
//   5. Call syscall_handler_rust(nr, a1, a2, a3)
//   6. Restore all registers
//   7. Restore user RSP
//   8. swapgs -> restore user GS
//   9. sysretq

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
        1  => sys_write(a1, a2, a3),
        60 => sys_exit(a1),
        _ => {
            crate::serial_println!("[SYSCALL] Unknown nr={}", nr);
            ENOSYS
        }
    }
}

// ===== sys_write(fd, buf, len) =====

/// POSIX write: fd=1 or fd=2 -> serial output.
/// SECURITY: buf and buf+len must be < USER_ADDR_LIMIT.
fn sys_write(fd: u64, buf_addr: u64, len: u64) -> u64 {
    // Validate fd
    if fd != 1 && fd != 2 {
        return EBADF;
    }
    if len == 0 { return 0; }
    if len > 0x1000_0000 { return EFAULT; }  // 256 MiB sanity

    // SECURITY: validate user pointer
    if buf_addr >= USER_ADDR_LIMIT {
        crate::serial_println!("[SYSCALL] EFAULT: buf=0x{:X} >= limit", buf_addr);
        return EFAULT;
    }
    if buf_addr.checked_add(len).map_or(true, |end| end > USER_ADDR_LIMIT) {
        crate::serial_println!("[SYSCALL] EFAULT: buf+len overflow", );
        return EFAULT;
    }

    // Write bytes to COM1 serial port (0x3F8) using direct port I/O.
    // The user pages are mapped in the current CR3, so user virtual
    // addresses are directly readable from Ring 0.
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

// ===== sys_exit(code) =====

/// Terminate the current user process. Never returns.
fn sys_exit(code: u64) -> u64 {
    crate::serial_println!(
        "[SYSCALL] sys_exit({}) - user process terminated cleanly",
        code
    );

    let current = crate::scheduler::current_pid();
    if current != 0 {
        let _ = crate::process::set_state(
            current,
            crate::process::ProcessState::Terminated,
        );
        crate::serial_println!("[SYSCALL] PID {} terminated (exit {})", current, code);
    }

    crate::serial_println!("========================================");
    crate::serial_println!("[SUCCESS] Ring 3 user process completed!");
    crate::serial_println!("========================================");

    // Halt — we don't have a second process to schedule
    loop { unsafe { asm!("hlt", options(nomem, nostack)); } }
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

    crate::serial_println!("[OK] SYSCALL/SYSRET fully configured with kernel stack + swapgs");
}
