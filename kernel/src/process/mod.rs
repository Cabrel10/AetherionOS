// process/mod.rs - Couche 6: Process Manager with Matriarchal Hierarchy
//
// Thread-safe process table (BTreeMap<u64, Process>) protected by spin::Mutex.
// Provides spawn_matriarch, spawn_submatriarch, spawn_worker.
// Enforces hierarchy rules:
//   - Only ONE Matriarch can exist
//   - SubMatriarch must have a Matriarch or SubMatriarch as parent
//   - Worker must have a SubMatriarch as parent
//   - Workers cannot be parents

pub mod task;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use lazy_static::lazy_static;
use core::sync::atomic::{AtomicU64, Ordering};

pub use task::{AgentRole, Process, ProcessState};
pub use crate::arch::x86_64::context::TaskContext;

// ===== Error Type =====

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessError {
    /// A Matriarch already exists
    MatriarchExists,
    /// The specified parent PID was not found
    ParentNotFound,
    /// The parent role does not allow the requested child role
    HierarchyViolation,
    /// Process not found
    NotFound,
    /// Invalid state transition
    InvalidTransition,
    /// Cannot kill kernel threads
    KillProtected,
    /// Maximum process count reached
    LimitReached,
}

impl core::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::MatriarchExists => write!(f, "Matriarch already exists"),
            Self::ParentNotFound => write!(f, "Parent PID not found"),
            Self::HierarchyViolation => write!(f, "Hierarchy violation"),
            Self::NotFound => write!(f, "Process not found"),
            Self::InvalidTransition => write!(f, "Invalid state transition"),
            Self::KillProtected => write!(f, "Cannot kill protected process"),
            Self::LimitReached => write!(f, "Process limit reached"),
        }
    }
}

// ===== Constants =====

const MAX_PROCESSES: usize = 256;

// ===== Metrics =====

static PROCESSES_CREATED: AtomicU64 = AtomicU64::new(0);
static PROCESSES_TERMINATED: AtomicU64 = AtomicU64::new(0);

pub fn metrics_created() -> u64 { PROCESSES_CREATED.load(Ordering::Relaxed) }
pub fn metrics_terminated() -> u64 { PROCESSES_TERMINATED.load(Ordering::Relaxed) }

// ===== Process Table =====

lazy_static! {
    /// Global process table: PID -> Process
    static ref PROCESS_TABLE: Mutex<BTreeMap<u64, Process>> = Mutex::new(BTreeMap::new());
}

// ===== Helpers =====

/// Check if a Matriarch already exists in the table
fn has_matriarch(table: &BTreeMap<u64, Process>) -> bool {
    table.values().any(|p| p.role == AgentRole::Matriarch && p.is_alive())
}

/// Get the Matriarch PID (if any)
pub fn matriarch_pid() -> Option<u64> {
    let table = PROCESS_TABLE.lock();
    table.values()
        .find(|p| p.role == AgentRole::Matriarch && p.is_alive())
        .map(|p| p.pid)
}

// ===== Spawn Functions =====

/// Spawn the unique Matriarch process (root of the hierarchy)
/// Returns the Matriarch's PID or an error if one already exists.
pub fn spawn_matriarch(name: &str, uid: u32, gid: u32) -> Result<u64, ProcessError> {
    let mut table = PROCESS_TABLE.lock();
    if table.len() >= MAX_PROCESSES {
        return Err(ProcessError::LimitReached);
    }
    if has_matriarch(&table) {
        return Err(ProcessError::MatriarchExists);
    }
    let proc = Process::new(name, AgentRole::Matriarch, 0, uid, gid);
    let pid = proc.pid;
    table.insert(pid, proc);
    PROCESSES_CREATED.fetch_add(1, Ordering::Relaxed);
    Ok(pid)
}

/// Spawn a SubMatriarch under a parent (Matriarch or another SubMatriarch).
pub fn spawn_submatriarch(name: &str, parent_pid: u64, uid: u32, gid: u32) -> Result<u64, ProcessError> {
    let mut table = PROCESS_TABLE.lock();
    if table.len() >= MAX_PROCESSES {
        return Err(ProcessError::LimitReached);
    }
    // Validate parent
    let parent_role = table.get(&parent_pid)
        .ok_or(ProcessError::ParentNotFound)?
        .role;
    match parent_role {
        AgentRole::Matriarch | AgentRole::SubMatriarch => {}
        _ => return Err(ProcessError::HierarchyViolation),
    }
    let proc = Process::new(name, AgentRole::SubMatriarch, parent_pid, uid, gid);
    let pid = proc.pid;
    table.insert(pid, proc);
    // Add child to parent
    if let Some(parent) = table.get_mut(&parent_pid) {
        parent.add_child(pid);
    }
    PROCESSES_CREATED.fetch_add(1, Ordering::Relaxed);
    Ok(pid)
}

/// Spawn a Worker under a SubMatriarch.
pub fn spawn_worker(name: &str, parent_pid: u64, uid: u32, gid: u32) -> Result<u64, ProcessError> {
    let mut table = PROCESS_TABLE.lock();
    if table.len() >= MAX_PROCESSES {
        return Err(ProcessError::LimitReached);
    }
    // Validate parent is a SubMatriarch
    let parent_role = table.get(&parent_pid)
        .ok_or(ProcessError::ParentNotFound)?
        .role;
    if parent_role != AgentRole::SubMatriarch {
        return Err(ProcessError::HierarchyViolation);
    }
    let proc = Process::new(name, AgentRole::Worker, parent_pid, uid, gid);
    let pid = proc.pid;
    table.insert(pid, proc);
    if let Some(parent) = table.get_mut(&parent_pid) {
        parent.add_child(pid);
    }
    PROCESSES_CREATED.fetch_add(1, Ordering::Relaxed);
    Ok(pid)
}

/// Spawn a kernel thread (no hierarchy restrictions).
pub fn spawn_kernel_thread(name: &str) -> Result<u64, ProcessError> {
    let mut table = PROCESS_TABLE.lock();
    if table.len() >= MAX_PROCESSES {
        return Err(ProcessError::LimitReached);
    }
    let proc = Process::new_kernel(name);
    let pid = proc.pid;
    table.insert(pid, proc);
    PROCESSES_CREATED.fetch_add(1, Ordering::Relaxed);
    Ok(pid)
}

// ===== State Management =====

/// Set the PML4 physical address for a process by PID
pub fn set_pml4_phys(pid: u64, pml4: u64) -> Result<(), ProcessError> {
    let mut table = PROCESS_TABLE.lock();
    let proc = table.get_mut(&pid).ok_or(ProcessError::NotFound)?;
    proc.set_pml4_phys(pml4);
    Ok(())
}

/// Set the state of a process by PID
pub fn set_state(pid: u64, new_state: ProcessState) -> Result<(), ProcessError> {
    let mut table = PROCESS_TABLE.lock();
    let proc = table.get_mut(&pid).ok_or(ProcessError::NotFound)?;
    if proc.set_state(new_state) {
        if new_state == ProcessState::Terminated {
            PROCESSES_TERMINATED.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    } else {
        Err(ProcessError::InvalidTransition)
    }
}

/// Kill a process (only non-kernel threads with uid != 0)
pub fn kill(pid: u64) -> Result<(), ProcessError> {
    let mut table = PROCESS_TABLE.lock();
    let proc = table.get_mut(&pid).ok_or(ProcessError::NotFound)?;
    if proc.role == AgentRole::KernelThread || proc.uid == 0 {
        return Err(ProcessError::KillProtected);
    }
    proc.state = ProcessState::Terminated;
    PROCESSES_TERMINATED.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

// ===== Queries =====

/// Get process info as a formatted string
pub fn get_info(pid: u64) -> Option<String> {
    let table = PROCESS_TABLE.lock();
    table.get(&pid).map(|p| {
        let mut s = arrayvec::ArrayString::<256>::new();
        let _ = core::fmt::Write::write_fmt(&mut s, format_args!("{}", p));
        String::from(s.as_str())
    })
}

/// Get a snapshot of a process's role and priority for the scheduler
pub fn get_role_priority(pid: u64) -> Option<(AgentRole, u8)> {
    let table = PROCESS_TABLE.lock();
    table.get(&pid).map(|p| (p.role, p.priority))
}

/// Count of active (alive) processes
pub fn active_count() -> usize {
    let table = PROCESS_TABLE.lock();
    table.values().filter(|p| p.is_alive()).count()
}

/// Total count of all processes (including terminated)
pub fn total_count() -> usize {
    PROCESS_TABLE.lock().len()
}

/// List all PIDs of alive processes
pub fn list_active_pids() -> Vec<u64> {
    let table = PROCESS_TABLE.lock();
    table.values().filter(|p| p.is_alive()).map(|p| p.pid).collect()
}

/// List children of a process
pub fn list_children(pid: u64) -> Vec<u64> {
    let table = PROCESS_TABLE.lock();
    table.get(&pid).map(|p| p.children.clone()).unwrap_or_default()
}

/// Get the context and PML4 physical address for a process
pub fn get_context_and_pml4(pid: u64) -> Option<(TaskContext, u64)> {
    let table = PROCESS_TABLE.lock();
    table.get(&pid).map(|p| (p.context, p.pml4_phys))
}

/// Get the parent PID of a process
pub fn get_ppid(pid: u64) -> Option<u64> {
    let table = PROCESS_TABLE.lock();
    table.get(&pid).map(|p| p.ppid)
}

/// Get the PML4 physical address of a process
pub fn get_pml4_phys(pid: u64) -> Option<u64> {
    let table = PROCESS_TABLE.lock();
    table.get(&pid).map(|p| p.pml4_phys)
}

/// Get the role of a process
pub fn get_role(pid: u64) -> Option<AgentRole> {
    let table = PROCESS_TABLE.lock();
    table.get(&pid).map(|p| p.role)
}

/// Get wait_ticks for a process (used by scheduler aging)
pub fn get_wait_ticks(pid: u64) -> Option<u64> {
    let table = PROCESS_TABLE.lock();
    table.get(&pid).map(|p| p.wait_ticks)
}

/// Set wait_ticks for a process (used by scheduler aging)
pub fn set_wait_ticks(pid: u64, ticks: u64) {
    let mut table = PROCESS_TABLE.lock();
    if let Some(p) = table.get_mut(&pid) {
        p.wait_ticks = ticks;
    }
}

/// Initialize the process manager (creates kernel_idle as PID 1)
pub fn init() -> u64 {
    let idle_pid = spawn_kernel_thread("kernel_idle").expect("Failed to create idle process");
    crate::serial_println!("[PROCESS] Manager initialized, idle PID={}", idle_pid);
    idle_pid
}

/// Execute a closure with a mutable reference to a process.
/// This avoids the need for returning a MutexGuard.
pub fn with_process_mut<F, R>(pid: u64, f: F) -> Option<R>
where
    F: FnOnce(&mut Process) -> R,
{
    let mut table = PROCESS_TABLE.lock();
    table.get_mut(&pid).map(f)
}

/// Execute a closure with an immutable reference to a process.
pub fn with_process<F, R>(pid: u64, f: F) -> Option<R>
where
    F: FnOnce(&Process) -> R,
{
    let table = PROCESS_TABLE.lock();
    table.get(&pid).map(f)
}
