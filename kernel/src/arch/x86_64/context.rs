// arch/x86_64/context.rs - Couche 9: Real Context Switch (Assembly)
//
// Saves and restores the full set of general-purpose registers AND
// FPU/SSE state (via fxsave/fxrstor) for cooperative/preemptive
// context switching.
//
// TaskContext layout (offsets used by asm):
//   0x00  rsp
//   0x08  rbp
//   0x10  rbx
//   0x18  r12
//   0x20  r13
//   0x28  r14
//   0x30  r15
//   0x38  rflags
//   0x40  rip  (return address / entry point)
//
// FpuState: 512-byte fxsave area, 16-byte aligned, stored separately
// in Process struct for FPU/SSE register preservation across switches.

use core::arch::asm;

/// 512-byte FPU/SSE save area for fxsave/fxrstor.
/// Must be 16-byte aligned per Intel specification.
#[derive(Clone, Copy)]
#[repr(C, align(16))]
pub struct FpuState {
    pub data: [u8; 512],
}

impl FpuState {
    /// Create a zeroed FPU state (default x87/SSE registers)
    pub const fn zero() -> Self {
        FpuState { data: [0u8; 512] }
    }
}

impl core::fmt::Debug for FpuState {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "FpuState([..512 bytes..])")
    }
}

/// CPU register context saved on a context switch.
/// Stored inside every `Process` struct so each task has its own snapshot.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TaskContext {
    pub rsp: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rflags: u64,
    pub rip: u64,
}

impl TaskContext {
    /// Create a zeroed context (used by kernel_idle and freshly spawned tasks)
    pub const fn zero() -> Self {
        TaskContext {
            rsp: 0,
            rbp: 0,
            rbx: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rflags: 0x200, // IF=1 (interrupts enabled)
            rip: 0,
        }
    }

    /// Create a context with a given stack pointer and entry point
    pub const fn new(stack_top: u64, entry_point: u64) -> Self {
        TaskContext {
            rsp: stack_top,
            rbp: stack_top,
            rbx: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rflags: 0x200,
            rip: entry_point,
        }
    }
}

impl Default for TaskContext {
    fn default() -> Self {
        Self::zero()
    }
}

/// Enable SSE/SSE2 support in CR0 and CR4.
/// Must be called once during kernel initialization before any
/// floating-point or SSE instructions are used.
///
/// Sets: CR0.MP=1, CR0.EM=0, CR4.OSFXSR=1, CR4.OSXMMEXCPT=1
pub unsafe fn enable_sse() {
    asm!(
        // CR0: clear EM (bit 2), set MP (bit 1)
        "mov rax, cr0",
        "and ax, 0xFFFB",   // clear EM (bit 2)
        "or  ax, 0x0002",   // set MP (bit 1)
        "mov cr0, rax",
        // CR4: set OSFXSR (bit 9) and OSXMMEXCPT (bit 10)
        "mov rax, cr4",
        "or  ax, 0x0600",   // bits 9 and 10
        "mov cr4, rax",
        out("rax") _,
        options(nostack, nomem),
    );
}

/// Check if the CPU supports AVX via CPUID.
/// Returns true if the AVX bit is reported in CPUID.1:ECX.
pub fn cpu_has_avx() -> bool {
    let ecx: u32;
    unsafe {
        // CPUID clobbers EAX, EBX, ECX, EDX. RBX is callee-saved and used by LLVM,
        // so we save/restore it manually.
        asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "pop rbx",
            out("ecx") ecx,
            out("eax") _,
            out("edx") _,
            options(nomem),
        );
    }
    // ECX bit 28 = AVX
    (ecx & (1 << 28)) != 0
}

/// Enable AVX support: set CR4.OSXSAVE, then configure XCR0
/// to save/restore both SSE (XMM) and AVX (YMM) state.
///
/// Prerequisites: SSE must already be enabled via enable_sse().
/// Returns true if AVX was successfully enabled.
pub unsafe fn enable_avx() -> bool {
    if !cpu_has_avx() {
        return false;
    }

    // Set CR4.OSXSAVE (bit 18)
    asm!(
        "mov rax, cr4",
        "or eax, (1 << 18)",  // OSXSAVE
        "mov cr4, rax",
        out("rax") _,
        options(nostack, nomem),
    );

    // Set XCR0: bit 0 = x87, bit 1 = SSE (XMM), bit 2 = AVX (YMM)
    asm!(
        "xor ecx, ecx",      // XCR0
        "xgetbv",
        "or eax, 0x07",       // x87 + SSE + AVX
        "xsetbv",
        out("eax") _,
        out("ecx") _,
        out("edx") _,
        options(nostack, nomem),
    );

    true
}

/// Initialize a default SSE/FPU state with proper MXCSR.
/// This sets MXCSR to the Intel default (0x1F80) which masks
/// all SIMD exceptions for safe Ring 3 execution.
pub fn init_default_fpu_state(state: &mut FpuState) {
    state.data = [0u8; 512];
    // MXCSR is at offset 24 in the FXSAVE area
    // Default value: 0x1F80 (all exceptions masked)
    let mxcsr: u32 = 0x1F80;
    state.data[24..28].copy_from_slice(&mxcsr.to_le_bytes());
    // x87 FPU Control Word at offset 0: 0x037F (default)
    let fcw: u16 = 0x037F;
    state.data[0..2].copy_from_slice(&fcw.to_le_bytes());
}

/// Save FPU/SSE state to the given 512-byte aligned buffer.
/// # Safety: `area` must point to a valid 16-byte aligned 512-byte buffer.
#[inline(always)]
pub unsafe fn fpu_save(area: *mut FpuState) {
    asm!(
        "fxsave [{}]",
        in(reg) area,
        options(nostack),
    );
}

/// Restore FPU/SSE state from the given 512-byte aligned buffer.
/// # Safety: `area` must point to a valid 16-byte aligned 512-byte buffer.
#[inline(always)]
pub unsafe fn fpu_restore(area: *const FpuState) {
    asm!(
        "fxrstor [{}]",
        in(reg) area,
        options(nostack),
    );
}

/// Perform a context switch from `old` to `new`.
///
/// # Safety
/// Both pointers must be valid, aligned `TaskContext` structs that
/// reside in memory for the entire duration of the switch.
///
/// This function saves the current CPU state into `*old` and loads
/// the state from `*new`, effectively resuming execution wherever
/// `new` was previously saved.
///
/// NOTE: FPU/SSE state is saved/restored separately via fpu_save/fpu_restore
/// at the process manager level (caller responsibility).
#[inline(never)]
pub unsafe fn switch_context(old: *mut TaskContext, new: *const TaskContext) {
    // Save callee-saved registers into *old, then load from *new.
    // The `ret` at the end jumps to the rip stored in *new.
    asm!(
        // ---- save current context into old (rdi) ----
        "mov [rdi + 0x00], rsp",
        "mov [rdi + 0x08], rbp",
        "mov [rdi + 0x10], rbx",
        "mov [rdi + 0x18], r12",
        "mov [rdi + 0x20], r13",
        "mov [rdi + 0x28], r14",
        "mov [rdi + 0x30], r15",
        "pushfq",
        "pop  rax",
        "mov  [rdi + 0x38], rax",       // save rflags
        "lea  rax, [rip + 2f]",         // return address = label 2
        "mov  [rdi + 0x40], rax",       // save rip

        // ---- restore context from new (rsi) ----
        "mov rsp, [rsi + 0x00]",
        "mov rbp, [rsi + 0x08]",
        "mov rbx, [rsi + 0x10]",
        "mov r12, [rsi + 0x18]",
        "mov r13, [rsi + 0x20]",
        "mov r14, [rsi + 0x28]",
        "mov r15, [rsi + 0x30]",
        "mov rax, [rsi + 0x38]",
        "push rax",
        "popfq",                        // restore rflags
        "jmp [rsi + 0x40]",            // jump to saved rip

        "2:",                           // old context resumes here
        in("rdi") old,
        in("rsi") new,
        // clobbers: rax is used as scratch; all callee-saved are
        // explicitly handled above so we mark caller-saved as clobbered.
        out("rax") _,
        out("rcx") _,
        out("rdx") _,
        out("r8")  _,
        out("r9")  _,
        out("r10") _,
        out("r11") _,
        options(nostack),
    );
}
