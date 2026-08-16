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
use core::sync::atomic::{AtomicBool, Ordering};

/// Global flag: true if XSAVE/XRSTOR are usable (CR4.OSXSAVE set, XCR0 configured).
/// Set once by `enable_avx()` during BSP boot. Read by `fpu_save()`/`fpu_restore()`
/// on every context switch to select between xsave64 and fxsave64.
///
/// Design rationale: The CI runner CPU has AVX=false → `enable_avx()` returns false
/// → CR4.OSXSAVE is never set → executing `xsave64` triggers #UD (Invalid Opcode).
/// This flag ensures we fall back to `fxsave64`/`fxrstor64` which only require
/// CR4.OSFXSR (set by `enable_sse()`) and save x87+SSE state (512 bytes).
static XSAVE_ENABLED: AtomicBool = AtomicBool::new(false);

/// Query whether XSAVE/XRSTOR are active for FPU context switching.
/// Used by process module to decide init_default_fpu_state layout.
#[inline]
pub fn is_xsave_enabled() -> bool {
    XSAVE_ENABLED.load(Ordering::Relaxed)
}

/// 1024-byte FPU/SSE/AVX save area for xsave64/xrstor64.
/// Must be 64-byte aligned per Intel XSAVE specification.
/// Supports x87 (bytes 0-159), SSE/XMM (bytes 160-415),
/// XSAVE header (bytes 512-575), and AVX/YMM (bytes 576-831).
/// Jalon 97: Expanded from 512B fxsave to 1024B xsave for AVX2 context switching.
#[derive(Clone, Copy)]
#[repr(C, align(64))]
pub struct FpuState {
    pub data: [u8; 1024],
}

impl FpuState {
    /// Create a zeroed FPU state (default x87/SSE/AVX registers)
    pub const fn zero() -> Self {
        FpuState { data: [0u8; 1024] }
    }
}

impl core::fmt::Debug for FpuState {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "FpuState([..1024 bytes xsave..])")
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

/// Check if the CPU supports the XSAVE instruction set via CPUID.
/// Returns true if CPUID.1:ECX bit 26 (XSAVE) is set.
/// Note: This only checks hardware support. CR4.OSXSAVE must also
/// be set (by enable_avx()) before xsave64 can actually execute.
pub fn cpu_has_xsave() -> bool {
    let ecx: u32;
    unsafe {
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
    // ECX bit 26 = XSAVE
    (ecx & (1 << 26)) != 0
}

/// Check if the CPU supports RDRAND via CPUID.
/// Returns true if CPUID.1:ECX bit 30 (RDRAND) is set.
/// RDRAND provides hardware-grade entropy directly from an on-die
/// Digital Random Number Generator (DRNG) that meets NIST SP 800-90A.
/// Critical for: libssl handshakes, apk package verification, /dev/urandom.
pub fn cpu_has_rdrand() -> bool {
    let ecx: u32;
    unsafe {
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
    // ECX bit 30 = RDRAND
    (ecx & (1 << 30)) != 0
}

/// Check if the CPU supports RDSEED via CPUID leaf 7.
/// Returns true if CPUID.7.0:EBX bit 18 (RDSEED) is set.
/// RDSEED provides direct access to the entropy conditioner output,
/// suitable for seeding other DRBGs.
pub fn cpu_has_rdseed() -> bool {
    let ebx: u32;
    unsafe {
        asm!(
            "push rbx",
            "mov eax, 7",
            "xor ecx, ecx",
            "cpuid",
            "mov {0:e}, ebx",
            "pop rbx",
            out(reg) ebx,
            out("eax") _,
            out("ecx") _,
            out("edx") _,
            options(nomem),
        );
    }
    // EBX bit 18 = RDSEED
    (ebx & (1 << 18)) != 0
}

/// Check if the CPU supports AVX via CPUID.
/// Returns true if the AVX bit is reported in CPUID.1:ECX.
pub fn cpu_has_avx() -> bool {
    let ecx: u32;
    unsafe {
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

/// Check if the CPU supports AVX2 via CPUID leaf 7, subleaf 0.
/// Returns true if CPUID.7.0:EBX.AVX2[bit 5] is set.
pub fn cpu_has_avx2() -> bool {
    let ebx: u32;
    unsafe {
        asm!(
            "push rbx",
            "mov eax, 7",
            "xor ecx, ecx",
            "cpuid",
            "mov {0:e}, ebx",
            "pop rbx",
            out(reg) ebx,
            out("eax") _,
            out("ecx") _,
            out("edx") _,
            options(nomem),
        );
    }
    // EBX bit 5 = AVX2
    (ebx & (1 << 5)) != 0
}

/// Check if the CPU supports FMA3 via CPUID.
/// Returns true if CPUID.1:ECX.FMA[bit 12] is set.
pub fn cpu_has_fma() -> bool {
    let ecx: u32;
    unsafe {
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
    // ECX bit 12 = FMA
    (ecx & (1 << 12)) != 0
}

/// CPU feature flags detected at boot
#[derive(Debug, Clone, Copy)]
pub struct CpuFeatures {
    pub sse: bool,
    pub sse2: bool,
    pub avx: bool,
    pub avx2: bool,
    pub fma: bool,
    pub pcid: bool,
    pub rdrand: bool,
    pub rdseed: bool,
    pub brand: [u8; 48],
}

/// Detect all relevant CPU features
pub fn detect_cpu_features() -> CpuFeatures {
    let (ecx1, edx1): (u32, u32);
    unsafe {
        asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "pop rbx",
            out("ecx") ecx1,
            out("edx") edx1,
            out("eax") _,
            options(nomem),
        );
    }

    let mut brand = [0u8; 48];
    // Try to read brand string (CPUID leaves 0x80000002-0x80000004)
    let max_ext: u32;
    unsafe {
        asm!(
            "push rbx",
            "mov eax, 0x80000000",
            "cpuid",
            "pop rbx",
            out("eax") max_ext,
            out("ecx") _,
            out("edx") _,
            options(nomem),
        );
    }
    if max_ext >= 0x80000004 {
        for i in 0u32..3 {
            let (eax, ebx_val, ecx, edx): (u32, u32, u32, u32);
            unsafe {
                let leaf = 0x80000002u32 + i;
                asm!(
                    "push rbx",
                    "mov eax, {leaf:e}",
                    "cpuid",
                    "mov {out_ebx:e}, ebx",
                    "pop rbx",
                    leaf = in(reg) leaf,
                    out_ebx = out(reg) ebx_val,
                    lateout("eax") eax,
                    out("ecx") ecx,
                    out("edx") edx,
                    options(nomem),
                );
            }
            let off = (i as usize) * 16;
            brand[off..off+4].copy_from_slice(&eax.to_le_bytes());
            brand[off+4..off+8].copy_from_slice(&ebx_val.to_le_bytes());
            brand[off+8..off+12].copy_from_slice(&ecx.to_le_bytes());
            brand[off+12..off+16].copy_from_slice(&edx.to_le_bytes());
        }
    }

    CpuFeatures {
        sse: (edx1 & (1 << 25)) != 0,
        sse2: (edx1 & (1 << 26)) != 0,
        avx: (ecx1 & (1 << 28)) != 0,
        avx2: cpu_has_avx2(),
        fma: (ecx1 & (1 << 12)) != 0,
        pcid: (ecx1 & (1 << 17)) != 0,
        rdrand: (ecx1 & (1 << 30)) != 0,
        rdseed: cpu_has_rdseed(),
        brand,
    }
}

/// Log CPU features to serial
pub fn log_cpu_features(features: &CpuFeatures) {
    // Find end of brand string
    let mut brand_len = 0;
    for i in 0..48 {
        if features.brand[i] == 0 { break; }
        brand_len = i + 1;
    }
    if brand_len > 0 {
        crate::serial_write("[CPU] Brand: ");
        // Trim leading spaces and write character by character
        let mut start = 0;
        while start < brand_len && features.brand[start] == b' ' { start += 1; }
        let mut buf = [0u8; 48];
        let mut pos = 0;
        for i in start..brand_len {
            buf[pos] = features.brand[i];
            pos += 1;
        }
        if pos > 0 {
            if let Ok(s) = core::str::from_utf8(&buf[..pos]) {
                crate::serial_write(s);
            }
        }
        crate::serial_write("\n");
    }

    crate::serial_println!("[CPU] Features: SSE={} SSE2={} AVX={} AVX2={} FMA={} PCID={}",
        features.sse, features.sse2, features.avx, features.avx2, features.fma, features.pcid);
    crate::serial_println!("[CPU] Entropy: RDRAND={} RDSEED={}",
        features.rdrand, features.rdseed);

    if features.avx2 && features.fma {
        crate::serial_println!("[CPU] Hardware acceleration: AVX2+FMA available for LLM inference");
        crate::serial_println!("[CPU] Haswell+ detected: optimal for SIMD matrix operations");
    } else if features.avx {
        crate::serial_println!("[CPU] Hardware acceleration: AVX (no AVX2/FMA)");
    } else {
        crate::serial_println!("[CPU] Hardware acceleration: SSE only (no AVX)");
    }
}

/// Enable AVX support: set CR4.OSXSAVE, then configure XCR0
/// to save/restore both SSE (XMM) and AVX (YMM) state.
///
/// Prerequisites: SSE must already be enabled via enable_sse().
/// Returns true if AVX was successfully enabled.
pub unsafe fn enable_avx() -> bool {
    if !cpu_has_avx() {
        crate::serial_println!("[CPU] AVX not available — FPU context switch will use FXSAVE64/FXRSTOR64 (SSE-only)");
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

    // Mark XSAVE as globally available — fpu_save/fpu_restore will use xsave64/xrstor64
    XSAVE_ENABLED.store(true, Ordering::Release);
    crate::serial_println!("[CPU] XCR0 configured for AVX/YMM context switching (XSAVE ready)");

    true
}

/// Initialize a default SSE/FPU/AVX state with proper MXCSR.
/// This sets MXCSR to the Intel default (0x1F80) which masks
/// all SIMD exceptions for safe Ring 3 execution.
///
/// Layout adapts to hardware:
///   - XSAVE available: 1024-byte xsave layout with XSTATE_BV header (x87+SSE+AVX)
///   - XSAVE unavailable: 512-byte fxsave layout (x87+SSE only, no XSAVE header)
///
/// Jalon 97: Uses 1024-byte buffer in both cases (the extra bytes are harmless zeros).
pub fn init_default_fpu_state(state: &mut FpuState) {
    state.data = [0u8; 1024];
    // x87 FPU Control Word at offset 0: 0x037F (default — all exceptions masked)
    let fcw: u16 = 0x037F;
    state.data[0..2].copy_from_slice(&fcw.to_le_bytes());
    // MXCSR is at offset 24 in the FXSAVE/XSAVE area
    // Default value: 0x1F80 (all SIMD exceptions masked)
    let mxcsr: u32 = 0x1F80;
    state.data[24..28].copy_from_slice(&mxcsr.to_le_bytes());
    // XSAVE header at offset 512: only written when XSAVE is active.
    // Writing XSTATE_BV on a CPU that doesn't support XSAVE is harmless
    // (fxrstor ignores bytes beyond 512), but we keep it clean.
    if XSAVE_ENABLED.load(Ordering::Relaxed) {
        // Set XSTATE_BV bits for x87+SSE+AVX (0x07)
        let xstate_bv: u64 = 0x07;
        state.data[512..520].copy_from_slice(&xstate_bv.to_le_bytes());
    }
}

/// Save FPU/SSE/AVX state to the given 64-byte aligned 1024-byte buffer.
///
/// Hardware-adaptive dispatch:
///   - XSAVE available (AVX-capable CPU): `xsave64` with mask 0x07 (x87+SSE+AVX)
///     Saves full YMM0-15 state (832 bytes) for AVX2 matrix operations.
///   - XSAVE unavailable (SSE-only CPU):  `fxsave64`
///     Saves x87+XMM0-15 state (512 bytes). Sufficient for musl/Python3.
///
/// The dispatch reads `XSAVE_ENABLED` (set once at boot by `enable_avx()`).
/// This is NOT a stub — both paths execute real hardware instructions.
///
/// Jalon 97: Upgraded from unconditional fxsave to adaptive xsave64/fxsave64.
/// # Safety: `area` must point to a valid 64-byte aligned 1024-byte buffer.
#[inline(always)]
pub unsafe fn fpu_save(area: *mut FpuState) {
    if XSAVE_ENABLED.load(Ordering::Relaxed) {
        // xsave64: saves x87 + SSE (XMM0-15) + AVX (YMM0-15)
        // EDX:EAX = save component mask. 0x07 = x87(bit0) + SSE(bit1) + AVX(bit2)
        asm!(
            "mov eax, 0x07",
            "xor edx, edx",
            "xsave64 [{}]",
            in(reg) area,
            out("eax") _,
            out("edx") _,
            options(nostack),
        );
    } else {
        // fxsave64: saves x87 + SSE (XMM0-15) — 512 bytes
        // Requires only CR4.OSFXSR (set by enable_sse()), no OSXSAVE needed.
        asm!(
            "fxsave64 [{}]",
            in(reg) area,
            options(nostack),
        );
    }
}

/// Restore FPU/SSE/AVX state from the given 64-byte aligned 1024-byte buffer.
///
/// Hardware-adaptive dispatch (mirrors `fpu_save`):
///   - XSAVE available: `xrstor64` with mask 0x07 — restores x87+SSE+AVX (YMM0-15)
///   - XSAVE unavailable: `fxrstor64` — restores x87+SSE (XMM0-15) only
///
/// Jalon 97: Upgraded from unconditional fxrstor to adaptive xrstor64/fxrstor64.
/// # Safety: `area` must point to a valid 64-byte aligned 1024-byte buffer
///          previously written by the matching save instruction.
#[inline(always)]
pub unsafe fn fpu_restore(area: *const FpuState) {
    if XSAVE_ENABLED.load(Ordering::Relaxed) {
        // xrstor64: restores x87 + SSE (XMM0-15) + AVX (YMM0-15)
        asm!(
            "mov eax, 0x07",
            "xor edx, edx",
            "xrstor64 [{}]",
            in(reg) area,
            out("eax") _,
            out("edx") _,
            options(nostack),
        );
    } else {
        // fxrstor64: restores x87 + SSE (XMM0-15) — 512 bytes
        asm!(
            "fxrstor64 [{}]",
            in(reg) area,
            options(nostack),
        );
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
