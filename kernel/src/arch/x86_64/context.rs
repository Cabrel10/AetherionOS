// arch/x86_64/context.rs - Couche 9: Real Context Switch (Assembly)
//
// Saves and restores the full set of general-purpose registers for
// cooperative/preemptive context switching.  The `switch_context`
// routine is written in AT&T-syntax inline assembly and follows the
// System V x86-64 ABI (callee-saved: rbx, rbp, r12-r15).
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

use core::arch::asm;

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
/// NOTE: In the current kernel all scheduling is still *logical*
/// (the scheduler picks PIDs but does not actually switch stacks).
/// This function is provided so that a future preemptive scheduler
/// can call it from the timer ISR once per-task kernel stacks are
/// allocated.  For now we expose it and test it with a
/// round-trip self-switch that proves the assembly is correct.
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
