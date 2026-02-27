// arch/x86_64/syscall.rs - Couche 9: Syscall / SYSRET MSR Configuration
//
// Enables the SYSCALL/SYSRET fast-path by programming the relevant MSRs:
//   - IA32_EFER   (0xC000_0080) : set SCE (bit 0)
//   - IA32_STAR   (0xC000_0081) : kernel CS/SS in bits 47:32, user CS/SS in bits 63:48
//   - IA32_LSTAR  (0xC000_0082) : RIP loaded on SYSCALL
//   - IA32_FMASK  (0xC000_0084) : RFLAGS bits to clear on SYSCALL (e.g. IF, TF, DF)
//
// On SYSCALL the CPU loads CS from STAR[47:32] and SS = STAR[47:32]+8.
// On SYSRET  the CPU loads CS from STAR[63:48]+16 and SS = STAR[63:48]+8.
//
// GDT layout (from gdt.rs):
//   0x08  Kernel Code (Ring 0)
//   0x10  Kernel Data (Ring 0)
//   0x18  User Data   (Ring 3)
//   0x20  User Code   (Ring 3)
//
// STAR encoding:
//   [47:32] = 0x08  (kernel CS)   → SS inferred as 0x08+8 = 0x10
//   [63:48] = 0x18  (user base)   → SYSRET CS = 0x18+16 = 0x28 ← but we use 0x20
//                                    SYSRET SS = 0x18+8  = 0x20
//   Actually for sysret: CS = STAR[63:48]+16 with RPL forced to 3.
//   So STAR[63:48] should be 0x10 (0x10+16=0x20 | RPL3 = 0x23, SS=0x10+8=0x18|RPL3=0x1B)
//   That matches our GDT: User Code CS=0x23, User Data DS=0x1B ✓
//
// References:
//   AMD64 Architecture Programmer's Manual Vol. 2, §6.1
//   Intel SDM Vol. 3, §5.8.8

use core::arch::asm;

/// MSR addresses
const IA32_EFER: u32  = 0xC000_0080;
const IA32_STAR: u32  = 0xC000_0081;
const IA32_LSTAR: u32 = 0xC000_0082;
const IA32_FMASK: u32 = 0xC000_0084;

/// EFER.SCE bit
const EFER_SCE: u64 = 1 << 0;

/// RFLAGS bits to mask on SYSCALL entry (IF=9, TF=8, DF=10)
const SFMASK_VALUE: u64 = (1 << 9) | (1 << 8) | (1 << 10);

/// Read a 64-bit Model-Specific Register (MSR)
#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    asm!(
        "rdmsr",
        in("ecx") msr,
        out("eax") lo,
        out("edx") hi,
        options(nomem, nostack),
    );
    ((hi as u64) << 32) | (lo as u64)
}

/// Write a 64-bit Model-Specific Register (MSR)
#[inline]
unsafe fn wrmsr(msr: u32, value: u64) {
    let lo = value as u32;
    let hi = (value >> 32) as u32;
    asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") lo,
        in("edx") hi,
        options(nomem, nostack),
    );
}

/// The stub SYSCALL entry point.
///
/// In a full implementation this would save user registers, switch to a
/// kernel stack, dispatch the syscall number, then execute SYSRET.
/// For now it simply returns via `sysretq` — we never actually enter
/// Ring 3 so this code path is unreachable at runtime; the important
/// thing is that the MSR configuration is correct and logged.
#[naked]
unsafe extern "C" fn syscall_entry() {
    asm!(
        // Placeholder: a real handler would:
        //   swapgs
        //   mov [gs:...], rsp   ; save user RSP
        //   mov rsp, [gs:...]   ; load kernel RSP
        //   push rcx            ; user RIP
        //   push r11            ; user RFLAGS
        //   ... dispatch ...
        //   pop  r11
        //   pop  rcx
        //   sysretq
        "sysretq",
        options(noreturn),
    );
}

/// Initialise SYSCALL/SYSRET by programming the four MSRs.
///
/// Must be called after GDT is loaded (needs segment selector values).
/// Safe to call on any x86-64 CPU that supports long mode (all do).
pub fn init() {
    crate::serial_println!("[SYSCALL] Initializing x86_64 MSRs...");

    unsafe {
        // 1. Enable EFER.SCE (System Call Extensions)
        let efer = rdmsr(IA32_EFER);
        wrmsr(IA32_EFER, efer | EFER_SCE);
        crate::serial_println!("[SYSCALL] EFER: 0x{:016X} -> 0x{:016X}", efer, efer | EFER_SCE);

        // 2. STAR: kernel CS/SS in [47:32], user base in [63:48]
        //    Kernel CS = 0x08, User base = 0x10
        //    SYSRET: CS = 0x10+16 = 0x20 | RPL3 = 0x23 ✓
        //            SS = 0x10+8  = 0x18 | RPL3 = 0x1B ✓
        let star: u64 = (0x10u64 << 48) | (0x08u64 << 32);
        wrmsr(IA32_STAR, star);
        crate::serial_println!("[SYSCALL] STAR: 0x{:016X} (kernel=0x08, user_base=0x10)", star);

        // 3. LSTAR: address of the SYSCALL entry point
        let handler_addr = syscall_entry as *const () as u64;
        wrmsr(IA32_LSTAR, handler_addr);
        crate::serial_println!("[SYSCALL] LSTAR: 0x{:016X}", handler_addr);

        // 4. SFMASK: clear IF, TF, DF on SYSCALL entry
        wrmsr(IA32_FMASK, SFMASK_VALUE);
        crate::serial_println!("[SYSCALL] SFMASK: 0x{:04X} (IF+TF+DF masked)", SFMASK_VALUE);
    }

    crate::serial_println!("[OK] SYSCALL/SYSRET instructions enabled");
}
