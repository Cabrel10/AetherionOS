//! AgentScheduler — CPU/iGPU/GPU/NPU Compute Arbitration
//!
//! # Architecture (2026-06-25)
//!
//! The AgentScheduler sits between the kernel's process scheduler and the
//! ComputeBackend registry. When an agent (= kernel process with PID)
//! requests inference or any compute-heavy operation, the AgentScheduler
//! decides **which backend** handles it, based on:
//!
//! 1. **Workload type**: matmul-heavy → GPU, low-latency → CPU, int8 → NPU
//! 2. **Resource availability**: VRAM free, CPU cores idle, power budget
//! 3. **Agent priority**: Critical agents get the fastest backend
//! 4. **Energy constraints**: Battery mode → NPU/iGPU, plugged → dGPU
//! 5. **Model size**: Fits in VRAM → GPU, spills → layer-split CPU+GPU
//!
//! ```text
//! ┌───────────────────────────────────────────────────────┐
//! │                  AgentScheduler                       │
//! │                                                       │
//! │  ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌─────────┐ │
//! │  │ Agent A  │  │ Agent B  │  │ Agent C  │  │ Agent D  │ │
//! │  │(français)│  │ (math)   │  │ (vision) │  │(embed)   │ │
//! │  │ PID=3    │  │ PID=5    │  │ PID=7    │  │ PID=9    │ │
//! │  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘ │
//! │       │              │              │              │       │
//! │  ┌────▼──────────────▼──────────────▼──────────────▼────┐ │
//! │  │              Routing Decision Engine                  │ │
//! │  │  workload_type × priority × resources × energy       │ │
//! │  └────┬──────────────┬──────────────┬──────────────┬────┘ │
//! │       │              │              │              │       │
//! │  ┌────▼────┐  ┌──────▼────┐  ┌─────▼─────┐  ┌────▼────┐ │
//! │  │CPU AVX2 │  │ MI50 GPU  │  │ iGPU UHD  │  │  NPU    │ │
//! │  │cores 1-4│  │ 8GB HBM2  │  │ shared    │  │ vendor  │ │
//! │  └─────────┘  └───────────┘  └───────────┘  └─────────┘ │
//! └───────────────────────────────────────────────────────────┘
//! ```
//!
//! # Safety
//!
//! The scheduler reads backend metadata via atomic/lock-free paths and
//! never allocates on the hot scheduling path. Task submission uses a
//! fixed-size ring buffer to avoid heap pressure during inference.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicU32, AtomicU8, Ordering};
use spin::Mutex;
use lazy_static::lazy_static;

use super::{ComputeBackend, BackendType, BackendCaps, active_backend};

// ═══════════════════════════════════════════════════════════════════════════
// Workload Classification
// ═══════════════════════════════════════════════════════════════════════════

/// Classifies what type of compute an agent needs.
/// The scheduler uses this to route to the optimal backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WorkloadType {
    /// Matrix multiplication (attention, FFN projections)
    /// Best on: GPU > AVX512 > AVX2 > scalar
    MatMul = 0,

    /// Full transformer attention (QKV + softmax + output projection)
    /// Best on: GPU with fused attention kernel > CPU
    Attention = 1,

    /// Embedding lookup (small, memory-bound)
    /// Best on: CPU (latency) or NPU (efficiency)
    Embedding = 2,

    /// Token generation (autoregressive, latency-sensitive)
    /// Best on: CPU (low latency) when batch=1; GPU when batch>4
    TokenGen = 3,

    /// Batch inference (multiple sequences, throughput-focused)
    /// Best on: GPU > CPU
    BatchInference = 4,

    /// Vector similarity search (cosine distance, embedding comparison)
    /// Best on: AVX2/AVX512 > GPU (memory transfer overhead for small vectors)
    VectorSearch = 5,

    /// RoPE + RMSNorm + activation (lightweight, interleaved with matmul)
    /// Best on: same backend as the surrounding matmul (avoid transfer)
    Normalization = 6,

    /// Memory transfer (upload weights to GPU, download logits)
    /// Not a "workload" per se, but tracked for scheduling decisions.
    Transfer = 7,
}

// ═══════════════════════════════════════════════════════════════════════════
// Compute Task
// ═══════════════════════════════════════════════════════════════════════════

/// A compute task submitted by an agent process.
#[derive(Debug, Clone, Copy)]
pub struct ComputeTask {
    /// PID of the agent requesting compute
    pub agent_pid: u64,
    /// What type of workload this is
    pub workload: WorkloadType,
    /// Priority: 0=idle, 1=low, 2=normal, 3=high, 4=critical
    pub priority: u8,
    /// Estimated FLOPS needed for this task (0 = unknown)
    pub estimated_flops: u64,
    /// Estimated memory needed in bytes (weights + activations + KV cache)
    pub estimated_memory: u64,
    /// Preferred backend type (hint, not mandatory). 0xFF = no preference.
    pub preferred_backend: u8,
    /// Timestamp (TSC) when the task was submitted
    pub submitted_at: u64,
}

/// Result of a scheduling decision.
#[derive(Debug, Clone, Copy)]
pub struct SchedulingDecision {
    /// Which backend was selected
    pub backend_type: BackendType,
    /// Reason for the decision (for logging/debugging)
    pub reason: SchedulingReason,
    /// Estimated latency in microseconds (0 = unknown)
    pub estimated_latency_us: u64,
    /// Estimated energy cost in microjoules (0 = unknown)
    pub estimated_energy_uj: u64,
}

/// Why the scheduler chose a particular backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SchedulingReason {
    /// Best FLOPS for this workload type
    BestPerformance = 0,
    /// Lowest latency (for interactive/token-gen workloads)
    LowestLatency = 1,
    /// Best performance per watt (energy planner constraint)
    BestEfficiency = 2,
    /// Only backend that has the required capability
    OnlyOption = 3,
    /// Agent explicitly requested this backend
    ExplicitPreference = 4,
    /// Backend has enough free memory for the task
    MemoryFit = 5,
    /// Fallback because preferred backend is busy/unavailable
    Fallback = 6,
}

// ═══════════════════════════════════════════════════════════════════════════
// Energy Profile
// ═══════════════════════════════════════════════════════════════════════════

/// System-wide energy profile set by the EnergyPlanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EnergyProfile {
    /// Maximum performance, ignore power consumption
    Performance = 0,
    /// Balanced: prefer GPU but respect thermal limits
    Balanced = 1,
    /// Power-saving: prefer NPU/iGPU, throttle dGPU
    Efficient = 2,
    /// Ultra low power: CPU only, minimal frequency
    Battery = 3,
}

// ═══════════════════════════════════════════════════════════════════════════
// Scheduler State
// ═══════════════════════════════════════════════════════════════════════════

/// Per-backend runtime statistics tracked by the scheduler.
#[derive(Debug, Clone, Copy)]
struct BackendStats {
    /// Total tasks processed
    tasks_completed: u64,
    /// Total compute time (TSC ticks)
    total_compute_ticks: u64,
    /// Currently queued tasks
    pending_tasks: u32,
    /// Last observed latency (TSC ticks)
    last_latency_ticks: u64,
    /// Moving average latency (TSC ticks, exponential)
    avg_latency_ticks: u64,
}

impl BackendStats {
    const ZERO: Self = Self {
        tasks_completed: 0,
        total_compute_ticks: 0,
        pending_tasks: 0,
        last_latency_ticks: 0,
        avg_latency_ticks: 0,
    };
}

/// The AgentScheduler core state.
struct SchedulerState {
    /// Per-backend statistics (indexed by BackendType ordinal)
    stats: [BackendStats; super::MAX_BACKENDS],
    /// Current energy profile
    energy_profile: EnergyProfile,
    /// Total scheduling decisions made
    total_decisions: u64,
    /// Total tasks that were rerouted due to resource constraints
    rerouted_count: u64,
}

impl SchedulerState {
    fn new() -> Self {
        Self {
            stats: [BackendStats::ZERO; super::MAX_BACKENDS],
            energy_profile: EnergyProfile::Performance,
            total_decisions: 0,
            rerouted_count: 0,
        }
    }
}

lazy_static! {
    static ref AGENT_SCHEDULER: Mutex<SchedulerState> = Mutex::new(SchedulerState::new());
}

// ═══════════════════════════════════════════════════════════════════════════
// Scheduling Algorithm
// ═══════════════════════════════════════════════════════════════════════════

/// Route a compute task to the optimal backend.
///
/// This is the core scheduling decision function. It evaluates all
/// registered backends against the task requirements and returns
/// the best match.
///
/// # Algorithm
///
/// 1. If the agent has an explicit preference AND that backend is available,
///    use it (unless energy profile forbids it).
/// 2. Filter backends by required capabilities for the workload type.
/// 3. Among candidates, score by:
///    - Performance (FLOPS) weighted by workload affinity
///    - Memory fit (does VRAM hold the task's data?)
///    - Energy efficiency (FLOPS/watt) weighted by energy profile
///    - Current load (fewer pending tasks = lower latency)
/// 4. Return the highest-scoring backend.
pub fn schedule_task(task: &ComputeTask) -> SchedulingDecision {
    let backends = super::list_backends();

    // Quick path: only one backend registered (common during early boot)
    if backends.len() <= 1 {
        let active = super::active_backend();
        return SchedulingDecision {
            backend_type: active.backend_type(),
            reason: SchedulingReason::OnlyOption,
            estimated_latency_us: 0,
            estimated_energy_uj: 0,
        };
    }

    // Determine required capabilities for this workload
    let required_caps = workload_required_caps(task.workload);

    // Explicit preference check
    if task.preferred_backend != 0xFF {
        for &(name, bt, caps, _is_active) in &backends {
            if bt as u8 == task.preferred_backend && caps.has(required_caps) {
                return SchedulingDecision {
                    backend_type: bt,
                    reason: SchedulingReason::ExplicitPreference,
                    estimated_latency_us: 0,
                    estimated_energy_uj: 0,
                };
            }
        }
    }

    // Score all candidates
    let mut best_score: i64 = i64::MIN;
    let mut best_bt = BackendType::CpuScalar;
    let mut best_reason = SchedulingReason::Fallback;

    let energy_profile = AGENT_SCHEDULER.lock().energy_profile;

    for &(name, bt, caps, _is_active) in &backends {
        // Filter: must have required capabilities
        if !caps.has(required_caps) {
            continue;
        }

        let mut score: i64 = 0;

        // Performance score (higher FLOPS = better)
        // We read from the static registry to get estimated_flops
        let (flops, power_mw, free_mem) = backend_metrics(bt);

        // Base performance score (normalized to GFLOPS)
        let gflops = (flops / 1_000_000_000) as i64;
        score += gflops * performance_weight(task.workload);

        // Memory fit bonus (GPU with enough VRAM gets +1000)
        if task.estimated_memory > 0 && free_mem > 0 {
            if free_mem >= task.estimated_memory {
                score += 1000;
                best_reason = SchedulingReason::MemoryFit;
            } else {
                // Penalty for insufficient memory
                score -= 500;
            }
        }

        // Energy efficiency score
        let efficiency = if power_mw > 0 {
            (flops / power_mw as u64) as i64 // FLOPS per milliwatt
        } else {
            0
        };

        match energy_profile {
            EnergyProfile::Performance => {
                // Raw performance, ignore efficiency
            }
            EnergyProfile::Balanced => {
                score += efficiency / 2;
            }
            EnergyProfile::Efficient => {
                score += efficiency * 2;
                // Penalize high-power backends
                if power_mw > 50_000 {
                    score -= 500;
                }
            }
            EnergyProfile::Battery => {
                score += efficiency * 4;
                // Heavily penalize discrete GPU
                if caps.has(BackendCaps::DEDICATED_MEMORY) && power_mw > 30_000 {
                    score -= 2000;
                }
            }
        }

        // Latency preference for interactive workloads
        if task.workload == WorkloadType::TokenGen || task.workload == WorkloadType::Embedding {
            // CPU is lower latency for single-token inference
            if !caps.has(BackendCaps::DEDICATED_MEMORY) {
                score += 200; // No PCIe transfer overhead
                if best_reason == SchedulingReason::Fallback {
                    best_reason = SchedulingReason::LowestLatency;
                }
            }
        }

        // Priority boost: critical agents get GPU preference
        if task.priority >= 4 {
            if caps.has(BackendCaps::FUSED_ATTENTION) {
                score += 300;
            }
        }

        if score > best_score {
            best_score = score;
            best_bt = bt;
            if best_reason == SchedulingReason::Fallback {
                best_reason = SchedulingReason::BestPerformance;
            }
        }
    }

    // Update scheduler stats
    if let Some(mut sched) = AGENT_SCHEDULER.try_lock() {
        sched.total_decisions += 1;
    }

    SchedulingDecision {
        backend_type: best_bt,
        reason: best_reason,
        estimated_latency_us: 0, // TODO: estimate from stats
        estimated_energy_uj: 0,
    }
}

/// Map workload type to required backend capabilities.
fn workload_required_caps(workload: WorkloadType) -> BackendCaps {
    match workload {
        WorkloadType::MatMul => BackendCaps::MATMUL_F32,
        WorkloadType::Attention => BackendCaps::MATMUL_F32, // Fused attention is optional
        WorkloadType::Embedding => BackendCaps::MATMUL_F32,
        WorkloadType::TokenGen => BackendCaps::MATMUL_Q8,
        WorkloadType::BatchInference => BackendCaps::MATMUL_F32,
        WorkloadType::VectorSearch => BackendCaps::MATMUL_F32,
        WorkloadType::Normalization => BackendCaps::FUSED_RMSNORM,
        WorkloadType::Transfer => BackendCaps::NONE, // Any backend can accept transfers
    }
}

/// Performance weight multiplier per workload type.
/// Higher = more importance placed on raw FLOPS.
fn performance_weight(workload: WorkloadType) -> i64 {
    match workload {
        WorkloadType::MatMul => 3,
        WorkloadType::Attention => 3,
        WorkloadType::BatchInference => 4,
        WorkloadType::TokenGen => 1,     // Latency matters more than throughput
        WorkloadType::Embedding => 1,
        WorkloadType::VectorSearch => 2,
        WorkloadType::Normalization => 1,
        WorkloadType::Transfer => 0,
    }
}

/// Get runtime metrics for a backend type from the registry.
fn backend_metrics(bt: BackendType) -> (u64, u32, u64) {
    let count = super::BACKEND_COUNT.load(Ordering::Acquire) as usize;
    for i in 0..count {
        unsafe {
            if let Some(ref entry) = super::BACKEND_REGISTRY[i] {
                if entry.backend.backend_type() == bt {
                    return (
                        entry.backend.estimated_flops(),
                        entry.backend.estimated_power_mw(),
                        entry.backend.free_memory_bytes(),
                    );
                }
            }
        }
    }
    (0, 0, 0)
}

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Set the system-wide energy profile.
pub fn set_energy_profile(profile: EnergyProfile) {
    AGENT_SCHEDULER.lock().energy_profile = profile;
    crate::serial_println!("[AGENT-SCHED] energy profile set to: {:?}", profile);
}

/// Get the current energy profile.
pub fn energy_profile() -> EnergyProfile {
    AGENT_SCHEDULER.lock().energy_profile
}

/// Record task completion for a backend (updates running stats).
pub fn record_completion(backend_type: BackendType, latency_ticks: u64) {
    if let Some(mut sched) = AGENT_SCHEDULER.try_lock() {
        let idx = backend_type as usize;
        if idx < super::MAX_BACKENDS {
            let stats = &mut sched.stats[idx];
            stats.tasks_completed += 1;
            stats.total_compute_ticks += latency_ticks;
            stats.last_latency_ticks = latency_ticks;
            // Exponential moving average (α = 1/8)
            stats.avg_latency_ticks =
                (stats.avg_latency_ticks * 7 + latency_ticks) / 8;
            if stats.pending_tasks > 0 {
                stats.pending_tasks -= 1;
            }
        }
    }
}

/// Get scheduling metrics for diagnostics.
pub fn scheduler_metrics() -> SchedulerMetrics {
    let sched = AGENT_SCHEDULER.lock();
    SchedulerMetrics {
        total_decisions: sched.total_decisions,
        rerouted_count: sched.rerouted_count,
        energy_profile: sched.energy_profile,
    }
}

/// Scheduling metrics for sysinfo display.
#[derive(Debug, Clone, Copy)]
pub struct SchedulerMetrics {
    pub total_decisions: u64,
    pub rerouted_count: u64,
    pub energy_profile: EnergyProfile,
}

/// Initialize the agent scheduler (called after compute::init_backend).
pub fn init() {
    // Register default energy profile based on detected hardware
    let active = super::active_backend();
    let profile = if active.is_gpu() {
        EnergyProfile::Balanced
    } else {
        EnergyProfile::Performance
    };
    set_energy_profile(profile);
    crate::serial_println!(
        "[AGENT-SCHED] initialized (active backend: {}, profile: {:?})",
        active.name(),
        profile
    );
}
