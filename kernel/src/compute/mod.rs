//! Compute subsystem — unified CPU/GPU backend abstraction.
//!
//! Phase 1 (now):  CPU scalar + AVX2/FMA  (this module)
//! Phase 2 (next): Intel iGPU Gen11 (bare-metal, via PCI BAR + Mesa-less path)
//! Phase 3 (later): AMD ROCm / discrete GPU
//!
//! The [`ComputeBackend`] trait is the seam between the LLM runtime
//! (`llm::inference`) and whatever hardware actually executes the matmuls.
//! For now only a CPU backend exists; GPU backends will implement the same
//! trait so the inference loop never has to special-case hardware.

pub mod avx2;

/// Unified compute backend interface.
///
/// Implementors expose only capability metadata for now; the actual kernel
/// dispatch still happens through `llm::matmul` (which consults
/// [`avx2::has_avx2_fma`]). As GPU backends land, the quantized matmul entry
/// point will move behind this trait.
pub trait ComputeBackend: Sync {
    /// Human-readable backend name, e.g. `"cpu-avx2"` or `"intel-gen11"`.
    fn name(&self) -> &'static str;
    /// True if this backend executes on a GPU rather than the host CPU.
    fn is_gpu(&self) -> bool;
    /// True if the backend can consume quantized (Q8_0/Q4_0) weights directly.
    fn supports_quantized(&self) -> bool;
}

/// CPU compute backend (scalar + optional AVX2/FMA acceleration).
pub struct CpuBackend {
    avx2: bool,
}

impl CpuBackend {
    /// Construct the CPU backend, recording whether AVX2/FMA is live.
    pub const fn new(avx2: bool) -> Self {
        Self { avx2 }
    }

    /// Whether the AVX2 vectorised path is active for this backend.
    pub fn avx2_active(&self) -> bool {
        self.avx2
    }
}

impl ComputeBackend for CpuBackend {
    fn name(&self) -> &'static str {
        if self.avx2 {
            "cpu-avx2"
        } else {
            "cpu-scalar"
        }
    }
    fn is_gpu(&self) -> bool {
        false
    }
    fn supports_quantized(&self) -> bool {
        true
    }
}

/// Globally selected compute backend, chosen once at boot.
///
/// `None` until `init_backend()` runs. Stored as a `&'static dyn` so the
/// inference path can query backend metadata without locking.
pub static mut BACKEND: Option<&'static dyn ComputeBackend> = None;

/// Static CPU backends — one per AVX2 state, so we can hand out a `&'static`
/// without heap allocation before the allocator-dependent runtime is up.
static CPU_AVX2: CpuBackend = CpuBackend::new(true);
static CPU_SCALAR: CpuBackend = CpuBackend::new(false);

/// Initialise the compute subsystem.
///
/// Probes AVX2/FMA (caching it for the matmul hot path) and selects the CPU
/// backend accordingly. Must be called at boot *after*
/// `arch::x86_64::context::enable_avx()`.
pub fn init_backend() {
    let avx2 = avx2::init_caps();
    // SAFETY: called once during single-threaded BSP boot, before any other
    // core reads BACKEND.
    unsafe {
        BACKEND = Some(if avx2 { &CPU_AVX2 } else { &CPU_SCALAR });
        if let Some(b) = BACKEND {
            crate::serial_println!(
                "[COMPUTE] backend selected: {} (gpu={}, quantized={})",
                b.name(),
                b.is_gpu(),
                b.supports_quantized()
            );
        }
    }
}
