//! EnergyPlanner — Performance/Watt Scheduling & Thermal Management
//!
//! # Architecture (2026-06-25)
//!
//! The EnergyPlanner is the final piece of the 10-component compute subsystem.
//! It monitors power draw, thermal state, and workload demand to dynamically
//! adjust scheduling decisions for maximum perf/watt efficiency.
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │                    EnergyPlanner                         │
//! │                                                          │
//! │  ┌──────────┐  ┌──────────────┐  ┌──────────────────┐  │
//! │  │ Sensors  │  │ Policy Engine│  │ Backend Governor  │  │
//! │  │          │→ │              │→ │                   │  │
//! │  │ temp_cpu │  │ profile      │  │ freq scaling      │  │
//! │  │ temp_gpu │  │ throttle     │  │ backend migration │  │
//! │  │ power_mw │  │ budget_split │  │ sleep/wake        │  │
//! │  │ fan_rpm  │  │              │  │                   │  │
//! │  └──────────┘  └──────────────┘  └──────────────────┘  │
//! │                                                          │
//! │  ┌──────────────────────────────────────────────────┐   │
//! │  │               Power History Ring                  │   │
//! │  │  [t-0: 45W] [t-1: 42W] [t-2: 48W] ... [t-63]   │   │
//! │  └──────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! # Energy Profiles
//!
//! | Profile       | TDP Budget | GPU Policy   | CPU Policy   | Use Case          |
//! |---------------|------------|--------------|--------------|-------------------|
//! | MaxPerf       | Unlimited  | Full boost   | All-core max | Benchmarks, burst |
//! | Balanced      | 65W total  | P-state auto | Turbo if fit | Default server    |
//! | Efficient     | 35W total  | Low-power    | Eco cores    | Edge / battery    |
//! | Silent        | 15W total  | Off / idle   | Min freq     | Night / idle      |
//! | Emergency     | 5W total   | Off          | Single core  | Thermal critical  |
//!
//! # Integration Points
//!
//! - **AgentScheduler** queries `current_power_budget()` before assigning backends
//! - **LayerScheduler** checks `should_prefetch()` — no prefetch in Silent/Emergency
//! - **MoE Runtime** reduces active experts in low-power profiles
//! - **ComputeBackend** reports `estimated_power_mw()` per backend
//! - **Kernel scheduler** can reduce tick rate in Silent mode

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use spin::Mutex;
use lazy_static::lazy_static;

// ============================================================================
// TYPES
// ============================================================================

/// Energy profile — governs the overall power envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EnergyProfile {
    /// No power limit — all backends at maximum. Use for burst inference.
    MaxPerf = 0,
    /// Default — balanced between performance and power draw.
    Balanced = 1,
    /// Low-power — prefer CPU eco-cores and iGPU over dGPU.
    Efficient = 2,
    /// Minimal activity — single core, GPU off, reduced tick rate.
    Silent = 3,
    /// Thermal emergency — hard throttle everything.
    Emergency = 4,
}

impl EnergyProfile {
    /// Total power budget in milliwatts for this profile.
    pub fn budget_mw(self) -> u32 {
        match self {
            EnergyProfile::MaxPerf   => u32::MAX,  // unlimited
            EnergyProfile::Balanced  => 65_000,     // 65W
            EnergyProfile::Efficient => 35_000,     // 35W
            EnergyProfile::Silent    => 15_000,     // 15W
            EnergyProfile::Emergency => 5_000,      // 5W
        }
    }

    /// Maximum number of active backends allowed.
    pub fn max_active_backends(self) -> u8 {
        match self {
            EnergyProfile::MaxPerf   => 8,
            EnergyProfile::Balanced  => 4,
            EnergyProfile::Efficient => 2,
            EnergyProfile::Silent    => 1,
            EnergyProfile::Emergency => 1,
        }
    }

    /// Whether GPU backends should be powered on.
    pub fn allow_gpu(self) -> bool {
        match self {
            EnergyProfile::MaxPerf   => true,
            EnergyProfile::Balanced  => true,
            EnergyProfile::Efficient => true,  // iGPU only (scheduler decides)
            EnergyProfile::Silent    => false,
            EnergyProfile::Emergency => false,
        }
    }

    /// Whether prefetch operations are allowed (LayerScheduler integration).
    pub fn allow_prefetch(self) -> bool {
        match self {
            EnergyProfile::MaxPerf   => true,
            EnergyProfile::Balanced  => true,
            EnergyProfile::Efficient => true,
            EnergyProfile::Silent    => false,
            EnergyProfile::Emergency => false,
        }
    }

    /// Maximum MoE experts to keep loaded simultaneously.
    pub fn max_moe_experts(self) -> usize {
        match self {
            EnergyProfile::MaxPerf   => 16,
            EnergyProfile::Balanced  => 8,
            EnergyProfile::Efficient => 4,
            EnergyProfile::Silent    => 2,
            EnergyProfile::Emergency => 1,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => EnergyProfile::MaxPerf,
            1 => EnergyProfile::Balanced,
            2 => EnergyProfile::Efficient,
            3 => EnergyProfile::Silent,
            4 => EnergyProfile::Emergency,
            _ => EnergyProfile::Balanced,
        }
    }
}

/// Thermal zone — maps to a physical sensor location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ThermalZone {
    CpuPackage = 0,
    CpuCore0   = 1,
    CpuCore1   = 2,
    CpuCore2   = 3,
    CpuCore3   = 4,
    Gpu        = 5,
    Vrm        = 6,
    Ambient    = 7,
    Ssd        = 8,
}

/// A single thermal reading — temperature in millidegrees Celsius (1°C = 1000).
#[derive(Debug, Clone, Copy)]
pub struct ThermalReading {
    pub zone: ThermalZone,
    /// Temperature in millidegrees Celsius (e.g., 65500 = 65.5°C).
    pub temp_mc: u32,
    /// Timestamp in kernel ticks.
    pub tick: u64,
}

/// Thermal threshold configuration per zone.
#[derive(Debug, Clone, Copy)]
pub struct ThermalThreshold {
    /// Temperature above which we enter Efficient mode (in millidegrees).
    pub warn_mc: u32,
    /// Temperature above which we enter Silent mode.
    pub throttle_mc: u32,
    /// Temperature above which we enter Emergency mode.
    pub critical_mc: u32,
    /// Temperature above which we initiate kernel panic / hardware shutdown.
    pub shutdown_mc: u32,
}

impl ThermalThreshold {
    /// Default thresholds for CPU package.
    pub const CPU_DEFAULT: Self = ThermalThreshold {
        warn_mc:     70_000,  // 70°C
        throttle_mc: 85_000,  // 85°C
        critical_mc: 95_000,  // 95°C
        shutdown_mc: 105_000, // 105°C
    };

    /// Default thresholds for GPU.
    pub const GPU_DEFAULT: Self = ThermalThreshold {
        warn_mc:     75_000,  // 75°C
        throttle_mc: 90_000,  // 90°C
        critical_mc: 100_000, // 100°C
        shutdown_mc: 110_000, // 110°C
    };

    /// Default thresholds for SSD.
    pub const SSD_DEFAULT: Self = ThermalThreshold {
        warn_mc:     50_000,  // 50°C
        throttle_mc: 60_000,  // 60°C
        critical_mc: 70_000,  // 70°C
        shutdown_mc: 75_000,  // 75°C
    };

    /// Conservative default for unknown zones.
    pub const GENERIC: Self = ThermalThreshold {
        warn_mc:     65_000,
        throttle_mc: 80_000,
        critical_mc: 90_000,
        shutdown_mc: 100_000,
    };
}

/// Power consumption snapshot — aggregated from all backends.
#[derive(Debug, Clone, Copy)]
pub struct PowerSnapshot {
    /// Total estimated power draw in milliwatts.
    pub total_mw: u32,
    /// CPU power draw in milliwatts.
    pub cpu_mw: u32,
    /// GPU power draw in milliwatts.
    pub gpu_mw: u32,
    /// Other (NPU, SSD, fans) in milliwatts.
    pub other_mw: u32,
    /// Timestamp in kernel ticks.
    pub tick: u64,
}

/// Throttle action — what the policy engine decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleAction {
    /// No change needed.
    None,
    /// Switch to a lower energy profile.
    DowngradeProfile(EnergyProfile),
    /// Switch to a higher energy profile (conditions improved).
    UpgradeProfile(EnergyProfile),
    /// Migrate workload from one backend to another.
    MigrateWorkload { from_backend_idx: u8, to_backend_idx: u8 },
    /// Put a backend to sleep.
    SleepBackend(u8),
    /// Wake a sleeping backend.
    WakeBackend(u8),
    /// Emergency: halt all non-critical compute.
    EmergencyHalt,
}

/// Backend power state tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BackendPowerState {
    /// Active — processing work.
    Active = 0,
    /// Idle — powered on but no work (auto-sleep timer running).
    Idle = 1,
    /// Sleep — clock gated, minimal power, fast wake.
    Sleep = 2,
    /// Off — powered down completely, slow wake.
    Off = 3,
}

/// Per-backend energy tracking.
#[derive(Debug, Clone)]
pub struct BackendEnergyState {
    /// Backend index in BACKEND_REGISTRY.
    pub backend_idx: u8,
    /// Current power state.
    pub power_state: BackendPowerState,
    /// Last known power draw in milliwatts.
    pub current_mw: u32,
    /// Cumulative energy in milliwatt-seconds since boot.
    pub cumulative_mws: u64,
    /// Ticks since last work was assigned (for auto-sleep).
    pub idle_ticks: u32,
    /// Number of operations completed (for perf/watt calculation).
    pub ops_completed: u64,
    /// Auto-sleep threshold: go to sleep after this many idle ticks.
    pub auto_sleep_ticks: u32,
}

impl BackendEnergyState {
    /// Performance per watt: ops / cumulative watt-seconds.
    /// Returns 0 if no energy consumed yet.
    pub fn perf_per_watt(&self) -> u64 {
        if self.cumulative_mws == 0 {
            return 0;
        }
        // ops_completed * 1000 / cumulative_mws (to avoid float)
        self.ops_completed.saturating_mul(1000) / self.cumulative_mws
    }
}

// ============================================================================
// POWER HISTORY RING BUFFER
// ============================================================================

const POWER_HISTORY_SIZE: usize = 64;

struct PowerHistory {
    snapshots: [PowerSnapshot; POWER_HISTORY_SIZE],
    write_idx: usize,
    count: usize,
}

impl PowerHistory {
    const fn new() -> Self {
        PowerHistory {
            snapshots: [PowerSnapshot {
                total_mw: 0,
                cpu_mw: 0,
                gpu_mw: 0,
                other_mw: 0,
                tick: 0,
            }; POWER_HISTORY_SIZE],
            write_idx: 0,
            count: 0,
        }
    }

    fn push(&mut self, snap: PowerSnapshot) {
        self.snapshots[self.write_idx] = snap;
        self.write_idx = (self.write_idx + 1) % POWER_HISTORY_SIZE;
        if self.count < POWER_HISTORY_SIZE {
            self.count += 1;
        }
    }

    /// Average power over the last N samples.
    fn average_mw(&self, n: usize) -> u32 {
        if self.count == 0 {
            return 0;
        }
        let samples = core::cmp::min(n, self.count);
        let mut sum: u64 = 0;
        let mut idx = if self.write_idx >= samples {
            self.write_idx - samples
        } else {
            POWER_HISTORY_SIZE - (samples - self.write_idx)
        };
        for _ in 0..samples {
            sum += self.snapshots[idx].total_mw as u64;
            idx = (idx + 1) % POWER_HISTORY_SIZE;
        }
        (sum / samples as u64) as u32
    }

    /// Peak power in the last N samples.
    fn peak_mw(&self, n: usize) -> u32 {
        if self.count == 0 {
            return 0;
        }
        let samples = core::cmp::min(n, self.count);
        let mut peak: u32 = 0;
        let mut idx = if self.write_idx >= samples {
            self.write_idx - samples
        } else {
            POWER_HISTORY_SIZE - (samples - self.write_idx)
        };
        for _ in 0..samples {
            if self.snapshots[idx].total_mw > peak {
                peak = self.snapshots[idx].total_mw;
            }
            idx = (idx + 1) % POWER_HISTORY_SIZE;
        }
        peak
    }

    /// Power trend: positive = increasing, negative = decreasing.
    /// Returns delta in milliwatts between newest and oldest in window.
    fn trend_mw(&self, n: usize) -> i32 {
        if self.count < 2 {
            return 0;
        }
        let samples = core::cmp::min(n, self.count);
        let newest_idx = if self.write_idx == 0 {
            POWER_HISTORY_SIZE - 1
        } else {
            self.write_idx - 1
        };
        let oldest_idx = if self.write_idx >= samples {
            self.write_idx - samples
        } else {
            POWER_HISTORY_SIZE - (samples - self.write_idx)
        };
        self.snapshots[newest_idx].total_mw as i32
            - self.snapshots[oldest_idx].total_mw as i32
    }
}

// ============================================================================
// THERMAL HISTORY
// ============================================================================

const MAX_THERMAL_ZONES: usize = 9;
const THERMAL_HISTORY_DEPTH: usize = 16;

struct ThermalHistory {
    readings: [[u32; THERMAL_HISTORY_DEPTH]; MAX_THERMAL_ZONES],
    write_idx: [usize; MAX_THERMAL_ZONES],
    counts: [usize; MAX_THERMAL_ZONES],
    thresholds: [ThermalThreshold; MAX_THERMAL_ZONES],
}

impl ThermalHistory {
    const fn new() -> Self {
        ThermalHistory {
            readings: [[0u32; THERMAL_HISTORY_DEPTH]; MAX_THERMAL_ZONES],
            write_idx: [0usize; MAX_THERMAL_ZONES],
            counts: [0usize; MAX_THERMAL_ZONES],
            thresholds: [ThermalThreshold::GENERIC; MAX_THERMAL_ZONES],
        }
    }

    fn record(&mut self, zone: ThermalZone, temp_mc: u32) {
        let z = zone as usize;
        if z >= MAX_THERMAL_ZONES {
            return;
        }
        self.readings[z][self.write_idx[z]] = temp_mc;
        self.write_idx[z] = (self.write_idx[z] + 1) % THERMAL_HISTORY_DEPTH;
        if self.counts[z] < THERMAL_HISTORY_DEPTH {
            self.counts[z] += 1;
        }
    }

    /// Current (most recent) temperature for a zone.
    fn current_mc(&self, zone: ThermalZone) -> u32 {
        let z = zone as usize;
        if z >= MAX_THERMAL_ZONES || self.counts[z] == 0 {
            return 0;
        }
        let idx = if self.write_idx[z] == 0 {
            THERMAL_HISTORY_DEPTH - 1
        } else {
            self.write_idx[z] - 1
        };
        self.readings[z][idx]
    }

    /// Average temperature for a zone.
    fn average_mc(&self, zone: ThermalZone) -> u32 {
        let z = zone as usize;
        if z >= MAX_THERMAL_ZONES || self.counts[z] == 0 {
            return 0;
        }
        let mut sum: u64 = 0;
        for i in 0..self.counts[z] {
            sum += self.readings[z][i] as u64;
        }
        (sum / self.counts[z] as u64) as u32
    }

    /// Highest thermal severity across all zones.
    fn worst_severity(&self) -> ThermalSeverity {
        let mut worst = ThermalSeverity::Normal;
        for z in 0..MAX_THERMAL_ZONES {
            if self.counts[z] == 0 {
                continue;
            }
            let temp = self.current_mc(unsafe { core::mem::transmute(z as u8) });
            let thresh = &self.thresholds[z];
            let sev = if temp >= thresh.shutdown_mc {
                ThermalSeverity::Shutdown
            } else if temp >= thresh.critical_mc {
                ThermalSeverity::Critical
            } else if temp >= thresh.throttle_mc {
                ThermalSeverity::Throttle
            } else if temp >= thresh.warn_mc {
                ThermalSeverity::Warning
            } else {
                ThermalSeverity::Normal
            };
            if (sev as u8) > (worst as u8) {
                worst = sev;
            }
        }
        worst
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ThermalSeverity {
    Normal   = 0,
    Warning  = 1,
    Throttle = 2,
    Critical = 3,
    Shutdown = 4,
}

// ============================================================================
// GLOBAL STATE
// ============================================================================

/// Current energy profile (atomic for lock-free reads).
static CURRENT_PROFILE: AtomicU8 = AtomicU8::new(EnergyProfile::Balanced as u8);

/// Total power consumed since boot in milliwatt-ticks (approximation).
static TOTAL_ENERGY_MWT: AtomicU64 = AtomicU64::new(0);

/// Number of profile transitions since boot.
static PROFILE_TRANSITIONS: AtomicU64 = AtomicU64::new(0);

/// Number of throttle events since boot.
static THROTTLE_EVENTS: AtomicU64 = AtomicU64::new(0);

/// Tick counter for the energy planner (incremented by `tick()`).
static PLANNER_TICKS: AtomicU64 = AtomicU64::new(0);

const MAX_TRACKED_BACKENDS: usize = 8;

lazy_static! {
    static ref POWER_HIST: Mutex<PowerHistory> = Mutex::new(PowerHistory::new());
    static ref THERMAL_HIST: Mutex<ThermalHistory> = {
        let mut th = ThermalHistory::new();
        // Set proper thresholds for known zones
        th.thresholds[ThermalZone::CpuPackage as usize] = ThermalThreshold::CPU_DEFAULT;
        th.thresholds[ThermalZone::CpuCore0 as usize]   = ThermalThreshold::CPU_DEFAULT;
        th.thresholds[ThermalZone::CpuCore1 as usize]   = ThermalThreshold::CPU_DEFAULT;
        th.thresholds[ThermalZone::CpuCore2 as usize]   = ThermalThreshold::CPU_DEFAULT;
        th.thresholds[ThermalZone::CpuCore3 as usize]   = ThermalThreshold::CPU_DEFAULT;
        th.thresholds[ThermalZone::Gpu as usize]         = ThermalThreshold::GPU_DEFAULT;
        th.thresholds[ThermalZone::Ssd as usize]         = ThermalThreshold::SSD_DEFAULT;
        Mutex::new(th)
    };
    static ref BACKEND_ENERGY: Mutex<Vec<BackendEnergyState>> = Mutex::new(Vec::new());
}

// ============================================================================
// PUBLIC API
// ============================================================================

/// Initialize the energy planner. Call once at boot after compute backends
/// are registered.
pub fn init(num_backends: usize) {
    let mut states = BACKEND_ENERGY.lock();
    states.clear();
    for i in 0..core::cmp::min(num_backends, MAX_TRACKED_BACKENDS) {
        states.push(BackendEnergyState {
            backend_idx: i as u8,
            power_state: BackendPowerState::Idle,
            current_mw: 0,
            cumulative_mws: 0,
            idle_ticks: 0,
            ops_completed: 0,
            auto_sleep_ticks: 500,  // ~5 seconds at 100Hz tick
        });
    }
    crate::serial_println!(
        "[ENERGY] Planner initialized: {} backends tracked, profile=Balanced",
        states.len()
    );
}

/// Get the current energy profile.
pub fn current_profile() -> EnergyProfile {
    EnergyProfile::from_u8(CURRENT_PROFILE.load(Ordering::Acquire))
}

/// Get the current power budget in milliwatts.
pub fn current_power_budget() -> u32 {
    current_profile().budget_mw()
}

/// Manually set the energy profile (e.g., from user command or agent request).
pub fn set_profile(profile: EnergyProfile) {
    let old = CURRENT_PROFILE.swap(profile as u8, Ordering::Release);
    if old != profile as u8 {
        PROFILE_TRANSITIONS.fetch_add(1, Ordering::Relaxed);
        crate::serial_println!(
            "[ENERGY] Profile changed: {:?} -> {:?}",
            EnergyProfile::from_u8(old),
            profile
        );
    }
}

/// Report a thermal reading. Called by hardware monitoring code.
pub fn report_thermal(zone: ThermalZone, temp_mc: u32) {
    let mut thermal = THERMAL_HIST.lock();
    thermal.record(zone, temp_mc);
}

/// Report backend power consumption. Called periodically by each backend
/// or estimated by the scheduler.
pub fn report_backend_power(backend_idx: u8, power_mw: u32) {
    let mut states = BACKEND_ENERGY.lock();
    if let Some(state) = states.iter_mut().find(|s| s.backend_idx == backend_idx) {
        state.current_mw = power_mw;
    }
}

/// Report that a backend completed work (for perf/watt tracking).
pub fn report_backend_ops(backend_idx: u8, ops: u64) {
    let mut states = BACKEND_ENERGY.lock();
    if let Some(state) = states.iter_mut().find(|s| s.backend_idx == backend_idx) {
        state.ops_completed = state.ops_completed.saturating_add(ops);
        state.idle_ticks = 0;
        state.power_state = BackendPowerState::Active;
    }
}

/// Called every scheduler tick (~100Hz). This is the main policy loop.
/// Returns a list of throttle actions the kernel should execute.
pub fn tick() -> Vec<ThrottleAction> {
    let tick_num = PLANNER_TICKS.fetch_add(1, Ordering::Relaxed) + 1;
    let mut actions = Vec::new();

    // 1. Update backend energy counters
    {
        let mut states = BACKEND_ENERGY.lock();
        let mut total_mw: u32 = 0;
        let mut cpu_mw: u32 = 0;
        let mut gpu_mw: u32 = 0;
        let mut other_mw: u32 = 0;

        for state in states.iter_mut() {
            // Accumulate energy (milliwatt-ticks → approximate milliwatt-seconds at 100Hz)
            state.cumulative_mws = state.cumulative_mws.saturating_add(state.current_mw as u64);
            total_mw = total_mw.saturating_add(state.current_mw);

            // Categorize power by backend index (0=CPU scalar, 1=AVX2 are CPU)
            if state.backend_idx <= 2 {
                cpu_mw = cpu_mw.saturating_add(state.current_mw);
            } else if state.backend_idx <= 5 {
                gpu_mw = gpu_mw.saturating_add(state.current_mw);
            } else {
                other_mw = other_mw.saturating_add(state.current_mw);
            }

            // Track idle time
            if state.power_state == BackendPowerState::Active {
                // Will be reset by report_backend_ops()
            } else if state.power_state == BackendPowerState::Idle {
                state.idle_ticks = state.idle_ticks.saturating_add(1);
            }
        }

        // Record power snapshot
        TOTAL_ENERGY_MWT.fetch_add(total_mw as u64, Ordering::Relaxed);
        let mut hist = POWER_HIST.lock();
        hist.push(PowerSnapshot {
            total_mw,
            cpu_mw,
            gpu_mw,
            other_mw,
            tick: tick_num,
        });

        // Auto-sleep idle backends
        for state in states.iter_mut() {
            if state.power_state == BackendPowerState::Idle
                && state.idle_ticks >= state.auto_sleep_ticks
            {
                state.power_state = BackendPowerState::Sleep;
                state.current_mw = 0;
                actions.push(ThrottleAction::SleepBackend(state.backend_idx));
                crate::serial_println!(
                    "[ENERGY] Backend {} auto-sleep after {} idle ticks",
                    state.backend_idx,
                    state.idle_ticks
                );
            }
        }
    }

    // 2. Thermal policy — check every 10 ticks (100ms at 100Hz)
    if tick_num % 10 == 0 {
        let thermal = THERMAL_HIST.lock();
        let severity = thermal.worst_severity();
        drop(thermal);

        let current = current_profile();
        let thermal_action = match severity {
            ThermalSeverity::Shutdown => {
                crate::serial_println!("[ENERGY] THERMAL SHUTDOWN — temperature critical!");
                Some(ThrottleAction::EmergencyHalt)
            }
            ThermalSeverity::Critical if current != EnergyProfile::Emergency => {
                THROTTLE_EVENTS.fetch_add(1, Ordering::Relaxed);
                Some(ThrottleAction::DowngradeProfile(EnergyProfile::Emergency))
            }
            ThermalSeverity::Throttle if current == EnergyProfile::MaxPerf
                || current == EnergyProfile::Balanced =>
            {
                THROTTLE_EVENTS.fetch_add(1, Ordering::Relaxed);
                Some(ThrottleAction::DowngradeProfile(EnergyProfile::Efficient))
            }
            ThermalSeverity::Warning if current == EnergyProfile::MaxPerf => {
                Some(ThrottleAction::DowngradeProfile(EnergyProfile::Balanced))
            }
            ThermalSeverity::Normal if current == EnergyProfile::Emergency
                || current == EnergyProfile::Silent =>
            {
                // Temperatures recovered — allow upgrade
                Some(ThrottleAction::UpgradeProfile(EnergyProfile::Balanced))
            }
            _ => None,
        };

        if let Some(action) = thermal_action {
            match action {
                ThrottleAction::DowngradeProfile(p) | ThrottleAction::UpgradeProfile(p) => {
                    set_profile(p);
                }
                _ => {}
            }
            actions.push(action);
        }
    }

    // 3. Budget enforcement — check every 5 ticks (50ms)
    if tick_num % 5 == 0 {
        let hist = POWER_HIST.lock();
        let avg = hist.average_mw(10);  // average over last 10 samples (~1s)
        let budget = current_power_budget();
        drop(hist);

        if budget != u32::MAX && avg > budget {
            let overshoot_pct = ((avg - budget) as u64 * 100) / budget as u64;
            if overshoot_pct > 20 {
                // Significant overshoot — downgrade profile
                let current = current_profile();
                let new_profile = match current {
                    EnergyProfile::MaxPerf  => EnergyProfile::Balanced,
                    EnergyProfile::Balanced => EnergyProfile::Efficient,
                    EnergyProfile::Efficient => EnergyProfile::Silent,
                    _ => current,
                };
                if new_profile != current {
                    crate::serial_println!(
                        "[ENERGY] Budget exceeded: {}mW avg vs {}mW budget ({}% over), downgrading to {:?}",
                        avg, budget, overshoot_pct, new_profile
                    );
                    set_profile(new_profile);
                    THROTTLE_EVENTS.fetch_add(1, Ordering::Relaxed);
                    actions.push(ThrottleAction::DowngradeProfile(new_profile));
                }
            } else if overshoot_pct > 5 {
                // Mild overshoot — try migrating GPU work to CPU
                let states = BACKEND_ENERGY.lock();
                let gpu_active = states.iter().any(|s| {
                    s.backend_idx > 2 && s.power_state == BackendPowerState::Active
                });
                if gpu_active {
                    // Find first active GPU and first idle CPU
                    let gpu_idx = states.iter()
                        .find(|s| s.backend_idx > 2 && s.power_state == BackendPowerState::Active)
                        .map(|s| s.backend_idx);
                    let cpu_idx = states.iter()
                        .find(|s| s.backend_idx <= 2 &&
                            (s.power_state == BackendPowerState::Idle ||
                             s.power_state == BackendPowerState::Active))
                        .map(|s| s.backend_idx);
                    if let (Some(g), Some(c)) = (gpu_idx, cpu_idx) {
                        actions.push(ThrottleAction::MigrateWorkload {
                            from_backend_idx: g,
                            to_backend_idx: c,
                        });
                    }
                }
            }
        }
    }

    actions
}

/// Wake a sleeping backend (called by AgentScheduler when work arrives).
pub fn wake_backend(backend_idx: u8) {
    let mut states = BACKEND_ENERGY.lock();
    if let Some(state) = states.iter_mut().find(|s| s.backend_idx == backend_idx) {
        if state.power_state == BackendPowerState::Sleep
            || state.power_state == BackendPowerState::Off
        {
            crate::serial_println!(
                "[ENERGY] Waking backend {} from {:?}",
                backend_idx, state.power_state
            );
            state.power_state = BackendPowerState::Idle;
            state.idle_ticks = 0;
        }
    }
}

/// Mark a backend as idle (no more work queued).
pub fn mark_idle(backend_idx: u8) {
    let mut states = BACKEND_ENERGY.lock();
    if let Some(state) = states.iter_mut().find(|s| s.backend_idx == backend_idx) {
        if state.power_state == BackendPowerState::Active {
            state.power_state = BackendPowerState::Idle;
            state.idle_ticks = 0;
        }
    }
}

// ============================================================================
// QUERY API (for AgentScheduler, LayerScheduler, MoE Runtime integration)
// ============================================================================

/// Whether the current profile allows GPU usage.
pub fn gpu_allowed() -> bool {
    current_profile().allow_gpu()
}

/// Whether prefetch operations are allowed.
pub fn prefetch_allowed() -> bool {
    current_profile().allow_prefetch()
}

/// Maximum MoE experts to keep warm under current profile.
pub fn max_moe_experts() -> usize {
    current_profile().max_moe_experts()
}

/// Maximum active backends allowed under current profile.
pub fn max_active_backends() -> u8 {
    current_profile().max_active_backends()
}

/// Get average power draw over last N samples (in milliwatts).
pub fn average_power(samples: usize) -> u32 {
    let hist = POWER_HIST.lock();
    hist.average_mw(samples)
}

/// Get peak power draw over last N samples (in milliwatts).
pub fn peak_power(samples: usize) -> u32 {
    let hist = POWER_HIST.lock();
    hist.peak_mw(samples)
}

/// Get power trend (positive = increasing).
pub fn power_trend(samples: usize) -> i32 {
    let hist = POWER_HIST.lock();
    hist.trend_mw(samples)
}

/// Remaining power budget (budget - current average).
/// Returns 0 if over budget.
pub fn remaining_budget() -> u32 {
    let budget = current_power_budget();
    if budget == u32::MAX {
        return u32::MAX;
    }
    let avg = average_power(10);
    budget.saturating_sub(avg)
}

/// Get performance/watt for a specific backend. Higher = better.
pub fn backend_perf_per_watt(backend_idx: u8) -> u64 {
    let states = BACKEND_ENERGY.lock();
    states.iter()
        .find(|s| s.backend_idx == backend_idx)
        .map(|s| s.perf_per_watt())
        .unwrap_or(0)
}

/// Get the most efficient backend (best perf/watt) that is active or idle.
pub fn most_efficient_backend() -> Option<u8> {
    let states = BACKEND_ENERGY.lock();
    states.iter()
        .filter(|s| s.power_state == BackendPowerState::Active
            || s.power_state == BackendPowerState::Idle)
        .max_by_key(|s| s.perf_per_watt())
        .map(|s| s.backend_idx)
}

/// Get current temperature for a thermal zone (in millidegrees Celsius).
pub fn temperature(zone: ThermalZone) -> u32 {
    let thermal = THERMAL_HIST.lock();
    thermal.current_mc(zone)
}

/// Current thermal severity across all zones.
pub fn thermal_severity() -> ThermalSeverity {
    let thermal = THERMAL_HIST.lock();
    thermal.worst_severity()
}

// ============================================================================
// DIAGNOSTICS
// ============================================================================

/// Energy planner status summary for debug/logging.
pub fn status_summary() -> EnergySummary {
    let profile = current_profile();
    let hist = POWER_HIST.lock();
    let avg = hist.average_mw(10);
    let peak = hist.peak_mw(64);
    let trend = hist.trend_mw(10);
    drop(hist);

    let thermal = THERMAL_HIST.lock();
    let cpu_temp = thermal.current_mc(ThermalZone::CpuPackage);
    let gpu_temp = thermal.current_mc(ThermalZone::Gpu);
    let severity = thermal.worst_severity();
    drop(thermal);

    let states = BACKEND_ENERGY.lock();
    let active_backends = states.iter()
        .filter(|s| s.power_state == BackendPowerState::Active)
        .count() as u8;
    let sleeping_backends = states.iter()
        .filter(|s| s.power_state == BackendPowerState::Sleep)
        .count() as u8;
    drop(states);

    EnergySummary {
        profile,
        budget_mw: profile.budget_mw(),
        avg_power_mw: avg,
        peak_power_mw: peak,
        power_trend_mw: trend,
        cpu_temp_mc: cpu_temp,
        gpu_temp_mc: gpu_temp,
        thermal_severity: severity,
        active_backends,
        sleeping_backends,
        total_transitions: PROFILE_TRANSITIONS.load(Ordering::Relaxed),
        total_throttle_events: THROTTLE_EVENTS.load(Ordering::Relaxed),
        uptime_ticks: PLANNER_TICKS.load(Ordering::Relaxed),
    }
}

/// Printable summary of energy state.
#[derive(Debug)]
pub struct EnergySummary {
    pub profile: EnergyProfile,
    pub budget_mw: u32,
    pub avg_power_mw: u32,
    pub peak_power_mw: u32,
    pub power_trend_mw: i32,
    pub cpu_temp_mc: u32,
    pub gpu_temp_mc: u32,
    pub thermal_severity: ThermalSeverity,
    pub active_backends: u8,
    pub sleeping_backends: u8,
    pub total_transitions: u64,
    pub total_throttle_events: u64,
    pub uptime_ticks: u64,
}

impl EnergySummary {
    /// Log the summary to serial.
    pub fn log(&self) {
        crate::serial_println!("[ENERGY STATUS]");
        crate::serial_println!("  Profile:    {:?} (budget: {}mW)", self.profile, self.budget_mw);
        crate::serial_println!(
            "  Power:      avg={}mW  peak={}mW  trend={:+}mW",
            self.avg_power_mw, self.peak_power_mw, self.power_trend_mw
        );
        crate::serial_println!(
            "  Thermal:    CPU={}.{}C  GPU={}.{}C  severity={:?}",
            self.cpu_temp_mc / 1000, (self.cpu_temp_mc % 1000) / 100,
            self.gpu_temp_mc / 1000, (self.gpu_temp_mc % 1000) / 100,
            self.thermal_severity
        );
        crate::serial_println!(
            "  Backends:   {} active, {} sleeping",
            self.active_backends, self.sleeping_backends
        );
        crate::serial_println!(
            "  History:    {} transitions, {} throttle events, {} ticks",
            self.total_transitions, self.total_throttle_events, self.uptime_ticks
        );
    }
}
