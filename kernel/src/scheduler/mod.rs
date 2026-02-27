// scheduler/mod.rs - Couche 7: Priority Scheduler
//
// PriorityScheduler with 5 queues: Critical, High, Normal, Low, Idle
// Matriarch = High, SubMatriarch = Normal, Worker = Low
// Connected to PIT timer via scheduler::tick()

use alloc::collections::VecDeque;
use spin::Mutex;
use lazy_static::lazy_static;
use core::sync::atomic::{AtomicU64, AtomicBool, Ordering};

use crate::process::{self, AgentRole};

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

// ===== Scheduler State =====

struct PriorityScheduler {
    /// Queues indexed by SchedPriority ordinal [Idle=0, Low=1, Normal=2, High=3, Critical=4]
    queues: [VecDeque<u64>; 5],
    /// PID of the currently running process (0 = none)
    current_pid: u64,
    /// Total ticks since scheduler start
    total_ticks: u64,
    /// Count of context switches
    context_switches: u64,
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
        // Scan from highest (Critical=4) to lowest (Idle=0)
        for idx in (0..5).rev() {
            if let Some(pid) = self.queues[idx].pop_front() {
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
        None
    }

    /// Perform a scheduler tick: preempt current, pick next
    fn tick(&mut self) -> TickResult {
        self.total_ticks += 1;

        // Re-enqueue the current process if still alive
        let old_pid = self.current_pid;
        if old_pid != 0 {
            if let Some((role, _prio)) = process::get_role_priority(old_pid) {
                let sched_prio = role_to_priority(role);
                self.enqueue(old_pid, sched_prio);
            }
        }

        // Pick the next process
        if let Some((next_pid, next_prio)) = self.dequeue_next() {
            let switched = next_pid != old_pid;
            if switched {
                self.context_switches += 1;
            }
            self.current_pid = next_pid;
            TickResult {
                old_pid,
                new_pid: next_pid,
                new_priority: next_prio,
                switched,
                tick_number: self.total_ticks,
            }
        } else {
            // No ready processes; stay idle
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
    crate::serial_println!("[SCHEDULER] Initialized with {} processes", process::active_count());
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
    }
}

/// Get current running PID
pub fn current_pid() -> u64 {
    SCHEDULER.lock().current_pid
}

#[derive(Debug, Clone, Copy)]
pub struct SchedulerMetrics {
    pub total_ticks: u64,
    pub context_switches: u64,
    pub current_pid: u64,
    pub queue_lengths: [usize; 5],
}
