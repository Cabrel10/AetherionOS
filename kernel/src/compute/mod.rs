//! Compute subsystem — unified CPU/GPU/NPU backend abstraction.
//!
//! # Architecture (2026-06-25 Expansion)
//!
//! The ComputeBackend trait is the **universal seam** between the LLM/AI runtime
//! and whatever hardware actually executes the math. Every backend — CPU scalar,
//! AVX2/FMA, AVX-512, Vulkan, CUDA, OpenCL, ROCm, NPU — implements the same
//! trait, so the inference loop never special-cases hardware.
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │              ComputeBackend Trait                    │
//! │  matmul_q8 | matmul_f32 | attention | softmax       │
//! │  rmsnorm | silu | rope | embedding_lookup           │
//! ├────────┬────────┬─────────┬──────────┬──────────────┤
//! │CPU     │AVX2    │AVX-512  │Vulkan    │ NPU          │
//! │Scalar  │FMA     │VNNI     │SPIR-V    │ Vendor SDK   │
//! ├────────┴────────┴─────────┼──────────┤              │
//! │    x86_64 host paths      │CUDA/ROCm │              │
//! │    (Ring 0, no_std)       │(PCIe BAR)│              │
//! └───────────────────────────┴──────────┴──────────────┘
//! ```
//!
//! # Backend Selection
//!
//! At boot, `init_backend()` probes CPUID/PCI and selects the best available
//! backend. The `BACKEND_REGISTRY` holds all detected backends, and the
//! `ACTIVE_BACKEND` points to the one currently in use. The AgentScheduler
//! (see `agent_scheduler` module) can dynamically switch backends per-task
//! based on workload characteristics and energy constraints.
//!
//! # Safety
//!
//! All SIMD backends require that the corresponding CPU feature was enabled
//! at boot (CR4.OSXSAVE, XCR0 bits). GPU backends require PCI BAR mapping.
//! The `init_backend()` function handles all safety preconditions.

pub mod avx2;
pub mod agent_scheduler;
pub mod agentic_runtime;
pub mod persistent_memory;
pub mod layer_scheduler;
pub mod moe_runtime;
pub mod compute_graph;
pub mod kv_cache;
pub mod multi_model;
pub mod energy_planner;

use alloc::vec::Vec;
use alloc::string::String;
use alloc::format;
use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

// ═══════════════════════════════════════════════════════════════════════════
// Backend Capability Flags
// ═══════════════════════════════════════════════════════════════════════════

/// Bitmask of capabilities a backend exposes.
/// Used by the AgentScheduler to route workloads to the best backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct BackendCaps(pub u32);

impl BackendCaps {
    pub const NONE: Self = Self(0);
    /// Can execute f32 matrix multiply
    pub const MATMUL_F32: Self = Self(1 << 0);
    /// Can execute Q8_0 quantized matmul directly (no dequant needed)
    pub const MATMUL_Q8: Self = Self(1 << 1);
    /// Can execute Q4_0 quantized matmul directly
    pub const MATMUL_Q4: Self = Self(1 << 2);
    /// Has fused attention kernel (QKV + softmax + output in one pass)
    pub const FUSED_ATTENTION: Self = Self(1 << 3);
    /// Has fused RMSNorm kernel
    pub const FUSED_RMSNORM: Self = Self(1 << 4);
    /// Has fused SiLU activation
    pub const FUSED_SILU: Self = Self(1 << 5);
    /// Can perform RoPE rotation in hardware/SIMD
    pub const ROPE: Self = Self(1 << 6);
    /// Supports batched operations (multiple sequences simultaneously)
    pub const BATCHED: Self = Self(1 << 7);
    /// Has dedicated memory (VRAM) — not shared with host RAM
    pub const DEDICATED_MEMORY: Self = Self(1 << 8);
    /// Supports async/DMA transfers (can overlap compute + memory)
    pub const ASYNC_TRANSFER: Self = Self(1 << 9);
    /// AVX-512 VNNI (Vector Neural Network Instructions)
    pub const VNNI: Self = Self(1 << 10);
    /// BF16 (bfloat16) native support
    pub const BF16: Self = Self(1 << 11);
    /// FP16 native support
    pub const FP16: Self = Self(1 << 12);
    /// INT8 dot product acceleration
    pub const INT8_DOT: Self = Self(1 << 13);

    #[inline]
    pub fn has(self, flag: Self) -> bool {
        (self.0 & flag.0) == flag.0
    }

    #[inline]
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Backend Type Identifier
// ═══════════════════════════════════════════════════════════════════════════

/// Enumerates all possible backend types. Used for routing and logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BackendType {
    /// Pure scalar CPU (no SIMD)
    CpuScalar = 0,
    /// x86_64 SSE4.1/SSE4.2
    CpuSse4 = 1,
    /// x86_64 AVX2 + FMA
    CpuAvx2 = 2,
    /// x86_64 AVX-512 (Foundation + VNNI if available)
    CpuAvx512 = 3,
    /// Intel integrated GPU (Gen9/Gen11/Gen12, via direct PCI BAR)
    IntelIGpu = 4,
    /// AMD discrete GPU (ROCm/HIP via PCIe BAR, e.g. MI50 gfx906)
    AmdGpu = 5,
    /// NVIDIA discrete GPU (CUDA via PCIe BAR)
    NvidiaGpu = 6,
    /// Vulkan compute (generic GPU path via SPIR-V)
    Vulkan = 7,
    /// OpenCL compute (generic accelerator path)
    OpenCL = 8,
    /// Neural Processing Unit (vendor-specific: Intel Movidius, Qualcomm Hexagon, etc.)
    Npu = 9,
    /// Remote/networked compute node (for distributed inference)
    Remote = 10,
}

impl core::fmt::Display for BackendType {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::CpuScalar => write!(f, "cpu-scalar"),
            Self::CpuSse4 => write!(f, "cpu-sse4"),
            Self::CpuAvx2 => write!(f, "cpu-avx2"),
            Self::CpuAvx512 => write!(f, "cpu-avx512"),
            Self::IntelIGpu => write!(f, "intel-igpu"),
            Self::AmdGpu => write!(f, "amd-gpu"),
            Self::NvidiaGpu => write!(f, "nvidia-gpu"),
            Self::Vulkan => write!(f, "vulkan"),
            Self::OpenCL => write!(f, "opencl"),
            Self::Npu => write!(f, "npu"),
            Self::Remote => write!(f, "remote"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Unified ComputeBackend Trait
// ═══════════════════════════════════════════════════════════════════════════

/// **The** unified compute interface for AetherionOS.
///
/// Every hardware backend implements this trait. The inference engine calls
/// these methods without knowing whether the math runs on CPU SIMD, GPU
/// shaders, or a remote TPU. This is the core abstraction that lets
/// AetherionOS "engloutir" (swallow) any model on any hardware.
///
/// # Design Principles
///
/// 1. **Zero-copy where possible**: Methods take `&[u8]` for quantized data
///    and `&[f32]` / `&mut [f32]` for activation vectors. No intermediate
///    format conversions unless the backend truly needs them.
///
/// 2. **Fallback chain**: Every method has a default implementation that
///    calls the scalar CPU path. GPU backends override only the methods
///    they accelerate; everything else falls through to the CPU.
///
/// 3. **Batch-aware**: Methods accept dimension parameters so a single
///    backend can handle variable-size tensors (different models).
///
/// 4. **No allocation in hot path**: All output buffers are caller-provided.
///    The backend never allocates — the caller (inference engine) owns memory.
pub trait ComputeBackend: Sync + Send {
    // ─── Metadata ───────────────────────────────────────────────────────

    /// Human-readable backend name, e.g. `"cpu-avx2"` or `"amd-mi50-rocm"`.
    fn name(&self) -> &'static str;

    /// Backend type for routing decisions.
    fn backend_type(&self) -> BackendType;

    /// True if this backend executes on a GPU/accelerator (not host CPU).
    fn is_gpu(&self) -> bool;

    /// True if the backend can consume Q8_0 quantized weights directly
    /// without prior dequantization to f32.
    fn supports_quantized(&self) -> bool;

    /// Bitmask of all capabilities this backend exposes.
    fn capabilities(&self) -> BackendCaps;

    /// Estimated FLOPS (single-precision) for scheduling decisions.
    /// Returns 0 if unknown. Used by the AgentScheduler for load balancing.
    fn estimated_flops(&self) -> u64 { 0 }

    /// Estimated power consumption in milliwatts for the EnergyPlanner.
    /// Returns 0 if unknown.
    fn estimated_power_mw(&self) -> u32 { 0 }

    /// Amount of dedicated memory in bytes (VRAM for GPUs, 0 for CPU).
    fn dedicated_memory_bytes(&self) -> u64 { 0 }

    /// Amount of currently free dedicated memory in bytes.
    fn free_memory_bytes(&self) -> u64 { 0 }

    // ─── Core Math Operations ───────────────────────────────────────────

    /// Matrix-vector multiply: `out[row] = dot(weight_row, x)` for Q8_0 weights.
    ///
    /// `q8_data` is row-major Q8_0: per block = 2-byte f16 scale + 32 × i8.
    /// `x` is the f32 activation vector of length `d_in`.
    /// `out` is the f32 result vector of length `d_out`.
    ///
    /// Default: delegates to the scalar `llm::matmul::matmul_q8_0`.
    fn matmul_q8(&self, out: &mut [f32], x: &[f32], q8_data: &[u8], d_in: usize, d_out: usize) {
        crate::llm::matmul::matmul_q8_0(out, x, q8_data, d_in, d_out);
    }

    /// Matrix-vector multiply for f32 weights (dequantized or native f32 models).
    ///
    /// Default: delegates to `llm::matmul::matmul_f32_fast`.
    fn matmul_f32(&self, out: &mut [f32], x: &[f32], w: &[f32], d_in: usize, d_out: usize) {
        crate::llm::matmul::matmul_f32_fast(out, x, w, d_in, d_out);
    }

    /// Fused argmax over Q8_0 logit projection: returns (best_token, best_logit).
    ///
    /// This is the single most expensive operation in inference (49152 dot products
    /// for SmolLM2). Backends that support parallel reduction should override this.
    ///
    /// Default: delegates to `llm::inference::argmax_quant_logits`.
    fn argmax_q8(&self, x: &[f32], q8_data: &[u8], dim: usize, vocab_size: usize) -> (u32, f32) {
        crate::llm::inference::argmax_quant_logits(x, q8_data, dim, vocab_size, true)
    }

    /// RMS normalization in-place: `x[i] = x[i] * weight[i] / sqrt(mean(x²) + eps)`
    ///
    /// Default: delegates to `llm::matmul::rmsnorm`.
    fn rmsnorm(&self, x: &mut [f32], weight: &[f32], eps: f32) {
        crate::llm::matmul::rmsnorm(x, weight, eps);
    }

    /// Softmax in-place over `x`.
    ///
    /// Default: delegates to `llm::matmul::softmax`.
    fn softmax(&self, x: &mut [f32]) {
        crate::llm::matmul::softmax(x);
    }

    /// SiLU activation: `x / (1 + exp(-x))`. Applied element-wise.
    ///
    /// Default: scalar loop using `llm::matmul::silu`.
    fn silu_inplace(&self, x: &mut [f32]) {
        for v in x.iter_mut() {
            *v = crate::llm::matmul::silu(*v);
        }
    }

    /// Apply RoPE (Rotary Position Embedding) using precomputed inverse frequencies.
    ///
    /// Default: delegates to `llm::matmul::apply_rope_cached`.
    fn rope_cached(&self, q: &mut [f32], k: &mut [f32], pos: usize, inv_freq: &[f32]) {
        crate::llm::matmul::apply_rope_cached(q, k, pos, inv_freq);
    }

    /// Element-wise multiply-accumulate: `out[i] += a[i] * b[i]`
    ///
    /// Used in FFN gate computation (SiLU gate * up projection).
    /// Default: scalar loop.
    fn elementwise_mul(&self, out: &mut [f32], b: &[f32]) {
        let n = out.len().min(b.len());
        for i in 0..n {
            out[i] *= b[i];
        }
    }

    /// Vector addition in-place: `a[i] += b[i]` (residual connections).
    ///
    /// Default: scalar loop.
    fn vec_add(&self, a: &mut [f32], b: &[f32]) {
        let n = a.len().min(b.len());
        for i in 0..n {
            a[i] += b[i];
        }
    }

    // ─── Memory Transfer (GPU backends) ─────────────────────────────────

    /// Upload data from host RAM to device memory.
    /// Returns a device-side handle/offset, or None if not a GPU backend.
    fn upload(&self, _data: &[u8]) -> Option<u64> { None }

    /// Download data from device memory to host RAM.
    /// Returns bytes read, or 0 if not a GPU backend.
    fn download(&self, _device_offset: u64, _dst: &mut [u8]) -> usize { 0 }

    /// Synchronize: wait for all pending operations to complete.
    /// No-op for synchronous CPU backends.
    fn sync(&self) {}
}

// ═══════════════════════════════════════════════════════════════════════════
// CPU Backend (Scalar)
// ═══════════════════════════════════════════════════════════════════════════

/// CPU compute backend — scalar path (no SIMD). Always available.
pub struct CpuScalarBackend;

impl ComputeBackend for CpuScalarBackend {
    fn name(&self) -> &'static str { "cpu-scalar" }
    fn backend_type(&self) -> BackendType { BackendType::CpuScalar }
    fn is_gpu(&self) -> bool { false }
    fn supports_quantized(&self) -> bool { true }
    fn capabilities(&self) -> BackendCaps {
        BackendCaps::MATMUL_F32
            .union(BackendCaps::MATMUL_Q8)
            .union(BackendCaps::MATMUL_Q4)
            .union(BackendCaps::FUSED_RMSNORM)
            .union(BackendCaps::FUSED_SILU)
            .union(BackendCaps::ROPE)
    }
    // Scalar CPU: ~2 GFLOPS on a modern core (conservative estimate)
    fn estimated_flops(&self) -> u64 { 2_000_000_000 }
    fn estimated_power_mw(&self) -> u32 { 15_000 } // ~15W single core
}

// ═══════════════════════════════════════════════════════════════════════════
// CPU Backend (AVX2 + FMA)
// ═══════════════════════════════════════════════════════════════════════════

/// CPU compute backend with AVX2 + FMA acceleration.
/// 8-wide f32 SIMD on the Q8_0 hot path (~4-8x vs scalar).
pub struct CpuAvx2Backend;

impl ComputeBackend for CpuAvx2Backend {
    fn name(&self) -> &'static str { "cpu-avx2" }
    fn backend_type(&self) -> BackendType { BackendType::CpuAvx2 }
    fn is_gpu(&self) -> bool { false }
    fn supports_quantized(&self) -> bool { true }
    fn capabilities(&self) -> BackendCaps {
        BackendCaps::MATMUL_F32
            .union(BackendCaps::MATMUL_Q8)
            .union(BackendCaps::MATMUL_Q4)
            .union(BackendCaps::FUSED_RMSNORM)
            .union(BackendCaps::FUSED_SILU)
            .union(BackendCaps::ROPE)
    }
    // AVX2: ~64 GFLOPS per core (8-wide FMA, ~2 GHz effective)
    fn estimated_flops(&self) -> u64 { 64_000_000_000 }
    fn estimated_power_mw(&self) -> u32 { 25_000 } // ~25W single core under AVX2 load

    fn matmul_q8(&self, out: &mut [f32], x: &[f32], q8_data: &[u8], d_in: usize, d_out: usize) {
        // Dispatch to AVX2 vectorized path (already gated by has_avx2_fma in matmul_q8_0)
        crate::llm::matmul::matmul_q8_0(out, x, q8_data, d_in, d_out);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// CPU Backend (AVX-512) — Stub, activated when CPUID confirms
// ═══════════════════════════════════════════════════════════════════════════

/// CPU compute backend with AVX-512 acceleration.
/// 16-wide f32 SIMD, VNNI for int8 dot products.
/// This is a stub — actual AVX-512 kernels will be implemented when
/// the target hardware (EPYC/Xeon) is available for testing.
pub struct CpuAvx512Backend {
    pub has_vnni: bool,
}

impl ComputeBackend for CpuAvx512Backend {
    fn name(&self) -> &'static str {
        if self.has_vnni { "cpu-avx512-vnni" } else { "cpu-avx512" }
    }
    fn backend_type(&self) -> BackendType { BackendType::CpuAvx512 }
    fn is_gpu(&self) -> bool { false }
    fn supports_quantized(&self) -> bool { true }
    fn capabilities(&self) -> BackendCaps {
        let mut caps = BackendCaps::MATMUL_F32
            .union(BackendCaps::MATMUL_Q8)
            .union(BackendCaps::MATMUL_Q4)
            .union(BackendCaps::FUSED_RMSNORM)
            .union(BackendCaps::FUSED_SILU)
            .union(BackendCaps::ROPE);
        if self.has_vnni {
            caps = caps.union(BackendCaps::VNNI).union(BackendCaps::INT8_DOT);
        }
        caps
    }
    // AVX-512: ~128 GFLOPS per core (16-wide FMA)
    fn estimated_flops(&self) -> u64 { 128_000_000_000 }
    fn estimated_power_mw(&self) -> u32 { 40_000 } // ~40W under AVX-512 load (frequency throttle)

    // For now, falls through to AVX2/scalar via default implementations.
    // TODO: Implement dedicated avx512 kernels in compute/avx512.rs
}

// ═══════════════════════════════════════════════════════════════════════════
// Vulkan Compute Backend — Stub for GPU inference
// ═══════════════════════════════════════════════════════════════════════════

/// Vulkan compute backend for GPU-accelerated inference.
///
/// This is the bare-metal Vulkan path: AetherionOS talks directly to the
/// GPU via PCI BAR memory-mapped registers, bypassing any OS driver stack.
/// The SPIR-V shaders for matmul/attention are compiled offline and loaded
/// as raw byte arrays in the kernel binary.
///
/// # Hardware Support
/// - Intel iGPU Gen9+ (UHD 620/630, Iris Plus)
/// - AMD RDNA/RDNA2/CDNA (MI50 gfx906, RX 6000 series)
/// - NVIDIA (via Vulkan, not CUDA — more portable)
///
/// # Current State
/// Stub: all operations fall through to CPU. Actual Vulkan implementation
/// requires PCI BAR mapping + command buffer submission + fence synchronization.
pub struct VulkanBackend {
    pub device_name: &'static str,
    pub vendor_id: u16,
    pub device_id: u16,
    pub vram_bytes: u64,
    pub vram_free: u64,
}

impl VulkanBackend {
    pub const fn stub() -> Self {
        Self {
            device_name: "vulkan-stub",
            vendor_id: 0,
            device_id: 0,
            vram_bytes: 0,
            vram_free: 0,
        }
    }
}

impl ComputeBackend for VulkanBackend {
    fn name(&self) -> &'static str { self.device_name }
    fn backend_type(&self) -> BackendType { BackendType::Vulkan }
    fn is_gpu(&self) -> bool { true }
    fn supports_quantized(&self) -> bool { true }
    fn capabilities(&self) -> BackendCaps {
        BackendCaps::MATMUL_F32
            .union(BackendCaps::MATMUL_Q8)
            .union(BackendCaps::FUSED_ATTENTION)
            .union(BackendCaps::DEDICATED_MEMORY)
            .union(BackendCaps::ASYNC_TRANSFER)
            .union(BackendCaps::FP16)
    }
    fn estimated_flops(&self) -> u64 { 0 } // Unknown until probed
    fn estimated_power_mw(&self) -> u32 { 0 }
    fn dedicated_memory_bytes(&self) -> u64 { self.vram_bytes }
    fn free_memory_bytes(&self) -> u64 { self.vram_free }

    // All ops fall through to CPU defaults for now.
    // TODO: Implement SPIR-V shader dispatch
}

// ═══════════════════════════════════════════════════════════════════════════
// CUDA Backend — Stub for NVIDIA GPUs
// ═══════════════════════════════════════════════════════════════════════════

/// CUDA compute backend for NVIDIA GPUs.
///
/// Bare-metal CUDA requires direct manipulation of the GPU's command FIFO
/// via PCI BAR0 (MMIO register space). This is significantly more complex
/// than Vulkan but offers superior performance for matrix operations.
///
/// # Current State: Stub
/// Falls through to CPU. Real implementation requires:
/// 1. PCI BAR0 mapping for NVIDIA GPU registers
/// 2. GPU context creation via MMIO
/// 3. PTX kernel loading and launch
/// 4. Memory allocation on GPU VRAM
pub struct CudaBackend {
    pub device_name: &'static str,
    pub vram_bytes: u64,
    pub compute_capability: (u32, u32), // e.g. (7, 5) for Turing
}

impl CudaBackend {
    pub const fn stub() -> Self {
        Self {
            device_name: "cuda-stub",
            vram_bytes: 0,
            compute_capability: (0, 0),
        }
    }
}

impl ComputeBackend for CudaBackend {
    fn name(&self) -> &'static str { self.device_name }
    fn backend_type(&self) -> BackendType { BackendType::NvidiaGpu }
    fn is_gpu(&self) -> bool { true }
    fn supports_quantized(&self) -> bool { true }
    fn capabilities(&self) -> BackendCaps {
        BackendCaps::MATMUL_F32
            .union(BackendCaps::MATMUL_Q8)
            .union(BackendCaps::MATMUL_Q4)
            .union(BackendCaps::FUSED_ATTENTION)
            .union(BackendCaps::DEDICATED_MEMORY)
            .union(BackendCaps::ASYNC_TRANSFER)
            .union(BackendCaps::FP16)
            .union(BackendCaps::BF16)
            .union(BackendCaps::INT8_DOT)
    }
    fn dedicated_memory_bytes(&self) -> u64 { self.vram_bytes }
}

// ═══════════════════════════════════════════════════════════════════════════
// OpenCL Backend — Stub for generic accelerators
// ═══════════════════════════════════════════════════════════════════════════

/// OpenCL compute backend — the most portable GPU/accelerator path.
///
/// Works with Intel iGPU, AMD GPUs, and even some FPGAs.
/// Lower performance than vendor-specific paths but maximal portability.
pub struct OpenClBackend {
    pub device_name: &'static str,
    pub vendor_id: u16,
}

impl ComputeBackend for OpenClBackend {
    fn name(&self) -> &'static str { self.device_name }
    fn backend_type(&self) -> BackendType { BackendType::OpenCL }
    fn is_gpu(&self) -> bool { true }
    fn supports_quantized(&self) -> bool { false } // OpenCL typically needs f32
    fn capabilities(&self) -> BackendCaps {
        BackendCaps::MATMUL_F32
            .union(BackendCaps::DEDICATED_MEMORY)
            .union(BackendCaps::FP16)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// NPU Backend — Stub for Neural Processing Units
// ═══════════════════════════════════════════════════════════════════════════

/// Neural Processing Unit backend.
///
/// NPUs (Intel Movidius, Qualcomm Hexagon, Apple ANE, AMD XDNA) are
/// ultra-low-power fixed-function accelerators optimized for int8/int4
/// matrix operations. They offer the best performance/watt for inference.
///
/// AetherionOS detects NPUs via ACPI/PCI enumeration and routes
/// low-latency inference tasks to them via the EnergyPlanner.
pub struct NpuBackend {
    pub device_name: &'static str,
    pub tops: u32, // Tera Operations Per Second (int8)
    pub power_mw: u32,
}

impl ComputeBackend for NpuBackend {
    fn name(&self) -> &'static str { self.device_name }
    fn backend_type(&self) -> BackendType { BackendType::Npu }
    fn is_gpu(&self) -> bool { false } // NPU is neither CPU nor GPU
    fn supports_quantized(&self) -> bool { true } // NPUs excel at quantized
    fn capabilities(&self) -> BackendCaps {
        BackendCaps::MATMUL_Q8
            .union(BackendCaps::MATMUL_Q4)
            .union(BackendCaps::INT8_DOT)
            .union(BackendCaps::DEDICATED_MEMORY)
    }
    fn estimated_flops(&self) -> u64 { (self.tops as u64) * 1_000_000_000_000 }
    fn estimated_power_mw(&self) -> u32 { self.power_mw }
}

// ═══════════════════════════════════════════════════════════════════════════
// Backend Registry
// ═══════════════════════════════════════════════════════════════════════════

/// Maximum number of compute backends the kernel can track simultaneously.
pub const MAX_BACKENDS: usize = 8;

/// Entry in the backend registry.
pub struct BackendEntry {
    pub backend: &'static dyn ComputeBackend,
    pub active: bool,
}

/// Global registry of all detected compute backends.
/// Indexed by BackendType ordinal. Populated at boot by `init_backend()`.
///
/// SAFETY: Written once during single-threaded BSP boot, then read-only.
static mut BACKEND_REGISTRY: [Option<BackendEntry>; MAX_BACKENDS] = [
    None, None, None, None, None, None, None, None,
];

/// Count of registered backends.
static BACKEND_COUNT: AtomicU8 = AtomicU8::new(0);

/// The active backend (index into BACKEND_REGISTRY).
static ACTIVE_BACKEND_IDX: AtomicU8 = AtomicU8::new(0);

/// Cumulative operations counter (for metrics/energy planner).
static OPS_COUNTER: AtomicU64 = AtomicU64::new(0);

// ── Legacy compatibility ────────────────────────────────────────────────
// The old code references `compute::BACKEND` as `Option<&'static dyn ComputeBackend>`.
// Keep it for backward compatibility with existing code in llm::parallel etc.
pub static mut BACKEND: Option<&'static dyn ComputeBackend> = None;

// Static backend instances (no heap needed at boot time)
static CPU_SCALAR_STATIC: CpuScalarBackend = CpuScalarBackend;
static CPU_AVX2_STATIC: CpuAvx2Backend = CpuAvx2Backend;
static CPU_AVX512_NO_VNNI: CpuAvx512Backend = CpuAvx512Backend { has_vnni: false };
static CPU_AVX512_VNNI: CpuAvx512Backend = CpuAvx512Backend { has_vnni: true };

// ═══════════════════════════════════════════════════════════════════════════
// Initialization
// ═══════════════════════════════════════════════════════════════════════════

/// Probe AVX-512 Foundation support via CPUID leaf 7, sub-leaf 0.
/// Returns (avx512f, avx512_vnni).
fn probe_avx512() -> (bool, bool) {
    let ebx: u32;
    let ecx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 7",
            "xor ecx, ecx",
            "cpuid",
            "mov {0:e}, ebx",
            "mov {1:e}, ecx",
            "pop rbx",
            out(reg) ebx,
            out(reg) ecx,
            out("eax") _,
            out("edx") _,
        );
    }
    let avx512f = (ebx & (1 << 16)) != 0;
    let vnni = (ecx & (1 << 11)) != 0;
    (avx512f, vnni)
}

/// Initialise the compute subsystem.
///
/// Probes all CPU features (AVX2/FMA, AVX-512, VNNI) and PCI devices
/// (GPU vendor/device IDs), then populates the backend registry and
/// selects the optimal active backend.
///
/// Must be called at boot *after* `arch::x86_64::context::enable_avx()`.
pub fn init_backend() {
    // ── Phase 1: CPU feature detection ──────────────────────────────
    let has_avx2 = avx2::init_caps();
    let (has_avx512, has_vnni) = probe_avx512();

    crate::serial_println!(
        "[COMPUTE] CPU features: avx2={}, avx512={}, vnni={}",
        has_avx2, has_avx512, has_vnni
    );

    // ── Phase 2: Register CPU backends ──────────────────────────────
    // Always register the scalar backend as fallback
    let mut active_backend: &'static dyn ComputeBackend = &CPU_SCALAR_STATIC;
    let mut active_idx: u8 = 0;

    unsafe {
        BACKEND_REGISTRY[0] = Some(BackendEntry {
            backend: &CPU_SCALAR_STATIC,
            active: !has_avx2, // active only if nothing better
        });
    }
    BACKEND_COUNT.store(1, Ordering::Release);

    if has_avx2 {
        unsafe {
            BACKEND_REGISTRY[1] = Some(BackendEntry {
                backend: &CPU_AVX2_STATIC,
                active: !has_avx512,
            });
        }
        active_backend = &CPU_AVX2_STATIC;
        active_idx = 1;
        BACKEND_COUNT.store(2, Ordering::Release);
    }

    if has_avx512 {
        let avx512_backend: &'static CpuAvx512Backend = if has_vnni {
            &CPU_AVX512_VNNI
        } else {
            &CPU_AVX512_NO_VNNI
        };
        unsafe {
            BACKEND_REGISTRY[2] = Some(BackendEntry {
                backend: avx512_backend,
                active: true,
            });
        }
        active_backend = avx512_backend;
        active_idx = 2;
        BACKEND_COUNT.store(3, Ordering::Release);
    }

    // ── Phase 3: GPU/Accelerator detection via PCI ──────────────────
    // This is a placeholder — actual GPU backends require PCI BAR mapping
    // which happens later in the boot sequence. We register stubs here
    // and the GPU init code will upgrade them to real backends.
    // (Handled by gpu::init() which already scans PCI class 0x03)

    // ── Phase 4: Set active backend ─────────────────────────────────
    ACTIVE_BACKEND_IDX.store(active_idx, Ordering::Release);

    // Legacy compatibility
    unsafe {
        BACKEND = Some(active_backend);
    }

    crate::serial_println!(
        "[COMPUTE] backend selected: {} (type={}, gpu={}, quantized={}, caps=0x{:08X})",
        active_backend.name(),
        active_backend.backend_type(),
        active_backend.is_gpu(),
        active_backend.supports_quantized(),
        active_backend.capabilities().0
    );
    crate::serial_println!(
        "[COMPUTE] estimated: {} GFLOPS, {}mW",
        active_backend.estimated_flops() / 1_000_000_000,
        active_backend.estimated_power_mw()
    );
    crate::serial_println!(
        "[COMPUTE] {} backend(s) registered",
        BACKEND_COUNT.load(Ordering::Acquire)
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Get the currently active compute backend.
///
/// Returns the scalar CPU fallback if no backend was initialized yet.
/// This is the primary entry point for the inference engine.
#[inline]
pub fn active_backend() -> &'static dyn ComputeBackend {
    let idx = ACTIVE_BACKEND_IDX.load(Ordering::Acquire) as usize;
    unsafe {
        if let Some(ref entry) = BACKEND_REGISTRY[idx] {
            entry.backend
        } else {
            &CPU_SCALAR_STATIC
        }
    }
}

/// Switch the active backend (used by AgentScheduler for dynamic routing).
///
/// Returns true if the switch succeeded.
pub fn set_active_backend(backend_type: BackendType) -> bool {
    let count = BACKEND_COUNT.load(Ordering::Acquire) as usize;
    for i in 0..count {
        unsafe {
            if let Some(ref entry) = BACKEND_REGISTRY[i] {
                if entry.backend.backend_type() == backend_type {
                    ACTIVE_BACKEND_IDX.store(i as u8, Ordering::Release);
                    BACKEND = Some(entry.backend); // Legacy compat
                    crate::serial_println!(
                        "[COMPUTE] switched active backend to: {}",
                        entry.backend.name()
                    );
                    return true;
                }
            }
        }
    }
    false
}

/// Register a new backend at runtime (e.g., after GPU PCI BAR is mapped).
///
/// Returns the index where it was registered, or None if the registry is full.
pub fn register_backend(backend: &'static dyn ComputeBackend) -> Option<usize> {
    let idx = BACKEND_COUNT.load(Ordering::Acquire) as usize;
    if idx >= MAX_BACKENDS {
        return None;
    }
    unsafe {
        BACKEND_REGISTRY[idx] = Some(BackendEntry {
            backend,
            active: false,
        });
    }
    BACKEND_COUNT.store((idx + 1) as u8, Ordering::Release);
    crate::serial_println!(
        "[COMPUTE] registered backend #{}: {} (type={}, caps=0x{:08X})",
        idx, backend.name(), backend.backend_type(), backend.capabilities().0
    );
    Some(idx)
}

/// List all registered backends (for diagnostics / sysinfo).
pub fn list_backends() -> Vec<(&'static str, BackendType, BackendCaps, bool)> {
    let count = BACKEND_COUNT.load(Ordering::Acquire) as usize;
    let active_idx = ACTIVE_BACKEND_IDX.load(Ordering::Acquire) as usize;
    let mut result = Vec::with_capacity(count);
    for i in 0..count {
        unsafe {
            if let Some(ref entry) = BACKEND_REGISTRY[i] {
                result.push((
                    entry.backend.name(),
                    entry.backend.backend_type(),
                    entry.backend.capabilities(),
                    i == active_idx,
                ));
            }
        }
    }
    result
}

/// Increment the operations counter (called after each matmul/attention pass).
#[inline]
pub fn record_op() {
    OPS_COUNTER.fetch_add(1, Ordering::Relaxed);
}

/// Get total operations count since boot.
#[inline]
pub fn total_ops() -> u64 {
    OPS_COUNTER.load(Ordering::Relaxed)
}
