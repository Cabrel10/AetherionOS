// scheduler/mod.rs - Couche 7+9: Priority Scheduler with Anti-Starvation Aging
//
// PriorityScheduler with 5 queues: Critical, High, Normal, Low, Idle
// Matriarch = High, SubMatriarch = Normal, Worker = Low
// Connected to PIT timer via scheduler::tick()
//
// Anti-starvation: apply_aging() boosts processes that have waited > 100 ticks.

use alloc::collections::VecDeque;
use spin::Mutex;
use lazy_static::lazy_static;
use core::sync::atomic::{AtomicU64, AtomicBool, Ordering};

use crate::process::{self, AgentRole};

// ===== Per-Core Current PID Tracking (SMP-safe) =====
const MAX_CORES: usize = 16;
static CORE_CURRENT_PID: [AtomicU64; MAX_CORES] = {
    const INIT: AtomicU64 = AtomicU64::new(0);
    [INIT; MAX_CORES]
};

/// Get the APIC ID of the current core using CPUID leaf 1 (register-only, safe anywhere).
#[inline]
fn current_core_index() -> usize {
    let id: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "shr ebx, 24",
            "mov {0:e}, ebx",
            "pop rbx",
            out(reg) id,
            out("eax") _,
            out("ecx") _,
            out("edx") _,
            options(nomem),
        );
    }
    (id as usize) & (MAX_CORES - 1)
}

// ===== Priority Levels for Scheduler Queues =====

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SchedPriority {
    Idle = 0,
    Low = 1,
    Normal = 2,
    High = 3,
    Critical = 4,
}

impl core::fmt::Display for SchedPriority {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::Idle => write!(f, "IDLE"),
            Self::Low => write!(f, "LOW"),
            Self::Normal => write!(f, "NORMAL"),
            Self::High => write!(f, "HIGH"),
            Self::Critical => write!(f, "CRITICAL"),
        }
    }
}

/// Map AgentRole to scheduler priority queue
pub fn role_to_priority(role: AgentRole) -> SchedPriority {
    match role {
        AgentRole::Matriarch => SchedPriority::High,
        AgentRole::SubMatriarch => SchedPriority::Normal,
        AgentRole::Worker => SchedPriority::Low,
        AgentRole::KernelThread => SchedPriority::Critical,
    }
}

/// The number of wait ticks before a process gets an aging boost
const AGING_THRESHOLD: u64 = 100;

// ===== Scheduler State =====

struct PriorityScheduler {
    /// Queues indexed by SchedPriority ordinal [Idle=0, Low=1, Normal=2, High=3, Critical=4]
    queues: [VecDeque<u64>; 5],
    /// Per-PID wait tick counters (indexed by position in the queue)
    /// We track wait_ticks per PID in the process table itself.
    /// PID of the currently running process (0 = none)
    current_pid: u64,
    /// Total ticks since scheduler start
    total_ticks: u64,
    /// Count of context switches
    context_switches: u64,
    /// Number of aging boosts performed
    aging_boosts: u64,
}

impl PriorityScheduler {
    fn new() -> Self {
        PriorityScheduler {
            queues: [
                VecDeque::new(), // Idle
                VecDeque::new(), // Low
                VecDeque::new(), // Normal
                VecDeque::new(), // High
                VecDeque::new(), // Critical
            ],
            current_pid: 0,
            total_ticks: 0,
            context_switches: 0,
            aging_boosts: 0,
        }
    }

    /// Enqueue a PID into the appropriate priority queue
    fn enqueue(&mut self, pid: u64, priority: SchedPriority) {
        let idx = priority as usize;
        if idx < 5 {
            self.queues[idx].push_back(pid);
        }
    }

    /// Dequeue the highest-priority ready PID (strict priority: starves lower queues)
    fn dequeue_next(&mut self) -> Option<(u64, SchedPriority)> {
        self.dequeue_next_for_core(0xFF) // 0xFF = any core (legacy behavior)
    }

    /// Jalon 97: Dequeue highest-priority PID that matches the given core affinity.
    /// A process with cpu_affinity == 0xFF runs on any core.
    /// A process with cpu_affinity == N runs only on core N.
    /// The `core_id` parameter is the ID of the currently executing core.
    ///
    /// Two-pass strategy for SMP compatibility:
    ///   Pass 1: Prefer processes whose affinity matches this core (or 0xFF)
    ///   Pass 2: If no match, schedule ANY ready process (prevents starvation
    ///           when AP cores aren't fully running, e.g., QEMU single-thread)
    fn dequeue_next_for_core(&mut self, core_id: u32) -> Option<(u64, SchedPriority)> {
        // Jalon 102: Strict SMP CPU affinity enforcement.
        //
        // When SMP is active (AP alive):
        //   - Core 0 (BSP) runs: affinity==0 or affinity==0xFF (any-core)
        //   - Core 1+ (AP)  runs: ONLY affinity matching that exact core
        //     AP cores do NOT steal 0xFF tasks (BSP handles those)
        //
        // When single-core (core_id==0xFF): run everything (no affinity filter)
        let smp_active = crate::arch::x86_64::apic::ap_is_alive();

        // Pass 1: Affinity-aware dequeue
        for idx in (0..5).rev() {
            let mut i = 0;
            while i < self.queues[idx].len() {
                let pid = self.queues[idx][i];
                if let Some(state) = process::get_state(pid) {
                    if state == process::ProcessState::Terminated {
                        self.queues[idx].remove(i); // Clean up dead process
                        continue;
                    }
                    if state == process::ProcessState::Blocked {
                        i += 1;
                        continue;
                    }
                }
                let affinity = process::get_cpu_affinity(pid);

                let matches = if core_id == 0xFF {
                    // Single-core fallback: run everything
                    true
                } else if core_id == 0 {
                    // BSP (Core 0): run affinity==0 or affinity==0xFF
                    // When SMP is active, do NOT steal affinity==1 tasks
                    if smp_active {
                        affinity == 0 || affinity == 0xFF
                    } else {
                        // No AP alive: BSP runs everything
                        true
                    }
                } else {
                    // AP core (Core 1+): ONLY run processes pinned to this exact core
                    affinity == core_id as u8
                };

                if matches {
                    let pid = self.queues[idx].remove(i).unwrap();
                    let prio = match idx {
                        4 => SchedPriority::Critical,
                        3 => SchedPriority::High,
                        2 => SchedPriority::Normal,
                        1 => SchedPriority::Low,
                        _ => SchedPriority::Idle,
                    };
                    return Some((pid, prio));
                }
                i += 1;
            }
        }

        // Pass 2: Fallback — only when single-core or BSP with no AP alive.
        // When SMP is active, do NOT use fallback (strict isolation).
        if !smp_active || core_id == 0xFF {
            for idx in (0..5).rev() {
                let mut i = 0;
                while i < self.queues[idx].len() {
                    let pid = self.queues[idx][i];
                    if let Some(state) = process::get_state(pid) {
                        if state == process::ProcessState::Terminated {
                            self.queues[idx].remove(i);
                            continue;
                        }
                        if state == process::ProcessState::Blocked {
                            i += 1;
                            continue;
                        }
                    }
                    let pid = self.queues[idx].remove(i).unwrap();
                    let prio = match idx {
                        4 => SchedPriority::Critical,
                        3 => SchedPriority::High,
                        2 => SchedPriority::Normal,
                        1 => SchedPriority::Low,
                        _ => SchedPriority::Idle,
                    };
                    return Some((pid, prio));
                }
            }
        }
        None
    }

    /// Anti-starvation aging: increment wait_ticks for all queued (Ready)
    /// processes and boost those that exceed AGING_THRESHOLD.
    ///
    /// A boosted process is moved one priority level up (Low→Normal,
    /// Normal→High). Critical and High processes are not boosted further.
    /// After boosting, the process's wait_ticks are reset to 0.
    fn apply_aging(&mut self) {
        // We need to collect PIDs to boost, then move them.
        // Iterate from Idle(0) to Normal(2) — only these can be boosted.
        for queue_idx in 0..=2usize {
            let target_idx = queue_idx + 1; // one level up
            if target_idx > 4 { continue; }

            let mut i = 0;
            while i < self.queues[queue_idx].len() {
                let pid = self.queues[queue_idx][i];
                // Read wait_ticks from process table
                let wt = process::get_wait_ticks(pid).unwrap_or(0);
                let new_wt = wt + 1;
                process::set_wait_ticks(pid, new_wt);

                if new_wt > AGING_THRESHOLD {
                    // Boost: remove from current queue, add to higher queue
                    self.queues[queue_idx].remove(i);
                    self.queues[target_idx].push_back(pid);
                    process::set_wait_ticks(pid, 0); // reset after boost

                    self.aging_boosts += 1;
                    // PRODUCTION: AGING logging completely disabled.
                    // Serial I/O at port 0x3F8 is extremely slow on QEMU;
                    // writing millions of boost messages starves agent processes
                    // of CPU time and prevents them from completing inference.
                    // The aging mechanism itself still works — just silently.
                    // don't increment i — removal shifted elements
                } else {
                    i += 1;
                }
            }
        }
    }

    /// Perform a scheduler tick: apply aging, preempt current, pick next
    fn tick(&mut self) -> TickResult {
        self.tick_on_core(0xFF) // Legacy: no affinity filtering
    }

    /// Jalon 97: Affinity-aware scheduler tick.
    /// Only selects processes whose cpu_affinity matches `core_id` (or 0xFF = any).
    fn tick_on_core(&mut self, core_id: u32) -> TickResult {
        self.total_ticks += 1;

        // Apply anti-starvation aging
        self.apply_aging();

        // Re-enqueue the current process if still alive
        let old_pid = self.current_pid;
        if old_pid != 0 {
            if let Some(state) = process::get_state(old_pid) {
                if state != process::ProcessState::Terminated {
                    if let Some((role, _prio)) = process::get_role_priority(old_pid) {
                        let sched_prio = role_to_priority(role);
                        self.enqueue(old_pid, sched_prio);
                    }
                }
            }
        }

        // Pick the next process — respecting CPU affinity
        if let Some((next_pid, next_prio)) = self.dequeue_next_for_core(core_id) {
            let switched = next_pid != old_pid;
            if switched {
                self.context_switches += 1;
            }
            self.current_pid = next_pid;
            process::set_wait_ticks(next_pid, 0);
            TickResult {
                old_pid,
                new_pid: next_pid,
                new_priority: next_prio,
                switched,
                tick_number: self.total_ticks,
            }
        } else {
            // No ready processes for this core; stay idle
            self.current_pid = 0;
            TickResult {
                old_pid,
                new_pid: 0,
                new_priority: SchedPriority::Idle,
                switched: old_pid != 0,
                tick_number: self.total_ticks,
            }
        }
    }
}

/// Result of a scheduler tick
#[derive(Debug, Clone, Copy)]
pub struct TickResult {
    pub old_pid: u64,
    pub new_pid: u64,
    pub new_priority: SchedPriority,
    pub switched: bool,
    pub tick_number: u64,
}

// ===== Global Scheduler =====

lazy_static! {
    static ref SCHEDULER: Mutex<PriorityScheduler> = Mutex::new(PriorityScheduler::new());
}

/// Atomic flag: is the scheduler initialized and ready to tick?
static SCHEDULER_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Tick counter for logging throttle (only log every N ticks)
static TICK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// How many ticks have occurred
pub fn total_ticks() -> u64 {
    TICK_COUNTER.load(Ordering::Relaxed)
}

// ===== Public API =====

/// Initialize the scheduler — call after process::init()
pub fn init() {
    // Enqueue all existing alive processes
    let pids = process::list_active_pids();
    let mut sched = SCHEDULER.lock();
    for pid in pids {
        if let Some((role, _)) = process::get_role_priority(pid) {
            let prio = role_to_priority(role);
            sched.enqueue(pid, prio);
        }
    }
    drop(sched);
    SCHEDULER_ACTIVE.store(true, Ordering::SeqCst);
    // crate::serial_println!("[SCHEDULER] Initialized with {} processes (aging threshold: {} ticks)",
    //     process::active_count(), AGING_THRESHOLD);
}

/// Enqueue a newly spawned process
pub fn enqueue_process(pid: u64) {
    if let Some((role, _)) = process::get_role_priority(pid) {
        let prio = role_to_priority(role);
        SCHEDULER.lock().enqueue(pid, prio);
    }
}

/// Scheduler tick — called from the PIT timer interrupt handler.
/// Must be very fast (runs in interrupt context).
pub fn tick() {
    if !SCHEDULER_ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let tick_num = TICK_COUNTER.fetch_add(1, Ordering::Relaxed);
    // Only actually perform scheduling every 10 ticks to avoid overhead
    if tick_num % 10 != 0 {
        return;
    }
    // Try to acquire the lock; if contended, skip this tick
    if let Some(mut sched) = SCHEDULER.try_lock() {
        let _result = sched.tick();
        // Logging is done in test mode, not in hot path
    }
}

/// Manually run one tick and return the result (for tests)
pub fn test_tick() -> TickResult {
    TICK_COUNTER.fetch_add(1, Ordering::Relaxed);
    SCHEDULER.lock().tick()
}

/// Manually trigger a context switch to the next process.
pub fn schedule_next() {
    if SCHEDULER_ACTIVE.load(Ordering::Relaxed) {
        if let Some(mut sched) = SCHEDULER.try_lock() {
            sched.tick();
        }
    }
}

/// Yield from `current` to the next ready process.
/// Returns the PID of the next process (or 0/current if no switch).
/// Jalon 101: Uses try_lock to prevent SMP deadlock. Disables interrupts
/// while holding the scheduler lock to avoid priority inversion.
/// Strict CPU affinity: Core 0 = OS/UI (affinity 0/0xFF), Core 1 = LLM (affinity 1/0xFF).
pub fn yield_to_next(current: u64) -> u64 {
    if !SCHEDULER_ACTIVE.load(Ordering::Relaxed) {
        return current;
    }
    // Disable interrupts while accessing scheduler (prevents deadlock with timer ISR)
    let flags: u64;
    unsafe {
        core::arch::asm!("pushfq; pop {}; cli", out(reg) flags, options(nomem));
    }

    let raw_core = crate::arch::x86_64::apic::current_core();
    // Strict affinity: use exact core ID when SMP is active
    let core_id = if raw_core == 0 && !crate::arch::x86_64::apic::ap_is_alive() {
        0xFF // Single-core fallback: BSP runs everything
    } else {
        raw_core
    };

    // Use try_lock to avoid spinning if another core holds the lock
    let result = if let Some(mut sched) = SCHEDULER.try_lock() {
        // Re-enqueue current if still alive
        if current != 0 {
            if let Some(state) = process::get_state(current) {
                if state != process::ProcessState::Terminated {
                    if let Some((role, _)) = process::get_role_priority(current) {
                        let prio = role_to_priority(role);
                        sched.enqueue(current, prio);
                    }
                }
            }
        }
        // Dequeue next — respecting CPU affinity
        if let Some((next_pid, _next_prio)) = sched.dequeue_next_for_core(core_id) {
            if next_pid != current {
                sched.context_switches += 1;
            }
            sched.current_pid = next_pid;
            process::set_wait_ticks(next_pid, 0);
            next_pid
        } else {
            sched.current_pid = 0;
            0
        }
    } else {
        current // Lock contended, return current (no switch)
    };

    // Restore interrupt flags
    unsafe {
        core::arch::asm!("push {}; popfq", in(reg) flags, options(nomem));
    }
    result
}

/// Get current scheduler metrics
pub fn metrics() -> SchedulerMetrics {
    let sched = SCHEDULER.lock();
    let mut queue_lengths = [0usize; 5];
    for i in 0..5 {
        queue_lengths[i] = sched.queues[i].len();
    }
    SchedulerMetrics {
        total_ticks: sched.total_ticks,
        context_switches: sched.context_switches,
        current_pid: sched.current_pid,
        queue_lengths,
        aging_boosts: sched.aging_boosts,
    }
}

/// Jalon 55: Preemptive tick — returns switch info if a context switch is needed.
/// Called from the timer interrupt handler.
/// Returns Some((old_pid, new_pid, new_rip, new_rsp, new_rflags, new_pml4)) if switch needed.
pub fn tick_preemptive() -> Option<(u64, u64, u64, u64, u64, u64)> {
    if !SCHEDULER_ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    let tick_num = TICK_COUNTER.fetch_add(1, Ordering::Relaxed);
    // Schedule every 3 ticks (~165ms at 18.2 Hz PIT) for responsive preemption
    if tick_num % 3 != 0 {
        return None;
    }
    // Try to acquire the lock; if contended, skip
    let mut sched = SCHEDULER.try_lock()?;
    // Jalon 97: Strict affinity for preemptive scheduling
    let raw_core = crate::arch::x86_64::apic::current_core();
    let core_id = if raw_core == 0 && !crate::arch::x86_64::apic::ap_is_alive() {
        0xFF // Single-core fallback
    } else {
        raw_core
    };
    let result = sched.tick_on_core(core_id);

    if !result.switched || result.new_pid == 0 {
        return None;
    }

    // CRITICAL FIX (Jalon 79): Don't change current_pid here.
    // The actual context switch is DEFERRED (pending). The timer ISR
    // only records a pending switch; the actual switch happens in
    // check_pending_switch() or sys_yield(). Until then, the old process
    // is still running on the CPU, so current_pid must remain the old PID.
    sched.current_pid = result.old_pid;

    // Get the new process's saved user-mode state
    let (new_rip, new_rsp, new_rflags, new_pml4, _new_ctx) =
        process::get_preempt_state(result.new_pid).unwrap_or((0, 0, 0x202, 0, process::SyscallContext::ZERO));

    if new_rip == 0 {
        // Process hasn't been preempted before — it's a fresh process.
        // Use its entry_point and stack_pointer for first launch.
        if let Some((entry, stack, pml4)) = process::get_entry_state(result.new_pid) {
            if entry != 0 && pml4 != 0 {
                return Some((result.old_pid, result.new_pid, entry, stack, 0x202, pml4));
            }
        }
        return None;
    }

    Some((result.old_pid, result.new_pid, new_rip, new_rsp, new_rflags, new_pml4))
}

/// Get current running PID (SMP-safe: reads from per-core atomic array)
pub fn current_pid() -> u64 {
    let core = current_core_index();
    CORE_CURRENT_PID[core].load(Ordering::Relaxed)
}

/// Set the current PID (SMP-safe: writes to per-core atomic array)
pub fn set_current_pid(pid: u64) {
    let core = current_core_index();
    CORE_CURRENT_PID[core].store(pid, Ordering::Relaxed);
    // Also update global scheduler for compatibility with metrics/display
    if let Some(mut s) = SCHEDULER.try_lock() {
        s.current_pid = pid;
    }
}

/// Get the aging boost count
pub fn aging_boosts() -> u64 {
    SCHEDULER.try_lock().map(|s| s.aging_boosts).unwrap_or(0)
}

#[derive(Debug, Clone, Copy)]
pub struct SchedulerMetrics {
    pub total_ticks: u64,
    pub context_switches: u64,
    pub current_pid: u64,
    pub queue_lengths: [usize; 5],
    pub aging_boosts: u64,
}
