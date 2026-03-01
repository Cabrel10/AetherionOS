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

pub use task::{AgentRole, Process, ProcessState, FdTable, FileDescriptor, MAX_FDS};
pub use crate::arch::x86_64::context::TaskContext;

// ===== Keyboard Input Buffer =====
// Ring buffer for keyboard input, shared between IRQ handler and sys_read(0)

const KBD_BUF_SIZE: usize = 256;

struct KbdBuffer {
    buf: [u8; KBD_BUF_SIZE],
    read_pos: usize,
    write_pos: usize,
    count: usize,
}

impl KbdBuffer {
    const fn new() -> Self {
        KbdBuffer {
            buf: [0; KBD_BUF_SIZE],
            read_pos: 0,
            write_pos: 0,
            count: 0,
        }
    }

    fn push(&mut self, byte: u8) {
        if self.count < KBD_BUF_SIZE {
            self.buf[self.write_pos] = byte;
            self.write_pos = (self.write_pos + 1) % KBD_BUF_SIZE;
            self.count += 1;
        }
    }

    fn pop(&mut self) -> Option<u8> {
        if self.count > 0 {
            let byte = self.buf[self.read_pos];
            self.read_pos = (self.read_pos + 1) % KBD_BUF_SIZE;
            self.count -= 1;
            Some(byte)
        } else {
            None
        }
    }

    fn available(&self) -> usize {
        self.count
    }
}

lazy_static! {
    static ref KBD_BUFFER: Mutex<KbdBuffer> = Mutex::new(KbdBuffer::new());
}

/// Push a byte into the keyboard input buffer (called from keyboard IRQ handler)
pub fn kbd_push_byte(byte: u8) {
    KBD_BUFFER.lock().push(byte);
}

/// Read up to `len` bytes from the keyboard buffer into a slice
pub fn kbd_read(buf: &mut [u8], len: usize) -> usize {
    let mut kbd = KBD_BUFFER.lock();
    let mut read = 0;
    let max = core::cmp::min(len, buf.len());
    while read < max {
        if let Some(b) = kbd.pop() {
            buf[read] = b;
            read += 1;
            // Stop at newline for line-buffered input
            if b == b'\n' {
                break;
            }
        } else {
            break;
        }
    }
    read
}

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
    /// Process has no FD table entry
    FdError,
    /// Process is waiting for child
    WaitingForChild,
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
            Self::FdError => write!(f, "FD table error"),
            Self::WaitingForChild => write!(f, "Waiting for child"),
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

/// Spawn a user-space process from an ELF load result
pub fn spawn_userspace(name: &str, ppid: u64, entry: u64, stack: u64, pml4: u64) -> Result<u64, ProcessError> {
    let mut table = PROCESS_TABLE.lock();
    if table.len() >= MAX_PROCESSES {
        return Err(ProcessError::LimitReached);
    }
    let proc = Process::new_userspace(name, ppid, entry, stack, pml4);
    let pid = proc.pid;
    table.insert(pid, proc);
    // Add as child of parent if parent exists
    if ppid != 0 {
        if let Some(parent) = table.get_mut(&ppid) {
            parent.add_child(pid);
        }
    }
    PROCESSES_CREATED.fetch_add(1, Ordering::Relaxed);
    Ok(pid)
}

/// Fork: clone a process. Returns (child_pid).
/// The child gets a copy of the parent's FD table, name, etc.
/// PML4 cloning is handled by the caller (syscall handler).
pub fn fork_process(parent_pid: u64, child_pml4: u64, child_entry: u64, child_stack: u64) -> Result<u64, ProcessError> {
    let mut table = PROCESS_TABLE.lock();
    if table.len() >= MAX_PROCESSES {
        return Err(ProcessError::LimitReached);
    }
    let parent = table.get(&parent_pid).ok_or(ProcessError::NotFound)?;
    let parent_name = parent.name.clone();
    let parent_uid = parent.uid;
    let parent_gid = parent.gid;
    let parent_fd_table = parent.fd_table.clone();

    let mut child = Process::new(&parent_name, AgentRole::Worker, parent_pid, parent_uid, parent_gid);
    child.pml4_phys = child_pml4;
    child.entry_point = child_entry;
    child.stack_pointer = child_stack;
    child.fd_table = parent_fd_table;
    child.state = ProcessState::Ready;
    let child_pid = child.pid;
    table.insert(child_pid, child);

    // Add child to parent's children list
    if let Some(parent) = table.get_mut(&parent_pid) {
        parent.add_child(child_pid);
    }

    PROCESSES_CREATED.fetch_add(1, Ordering::Relaxed);
    Ok(child_pid)
}

/// Wait for any child of parent_pid to terminate.
/// Returns (child_pid, exit_code) or error.
pub fn wait_for_child(parent_pid: u64) -> Result<(u64, i32), ProcessError> {
    let table = PROCESS_TABLE.lock();
    let parent = table.get(&parent_pid).ok_or(ProcessError::NotFound)?;
    
    // Look for any terminated child
    for &child_pid in &parent.children {
        if let Some(child) = table.get(&child_pid) {
            if child.state == ProcessState::Terminated {
                let exit_code = child.exit_code;
                return Ok((child_pid, exit_code));
            }
        }
    }
    
    // No terminated child found
    Err(ProcessError::WaitingForChild)
}

/// Set exit code for a process
pub fn set_exit_code(pid: u64, code: i32) {
    let mut table = PROCESS_TABLE.lock();
    if let Some(proc) = table.get_mut(&pid) {
        proc.exit_code = code;
    }
}

/// Get the FD table entry for a process (for syscall use)
pub fn with_fd_table<F, R>(pid: u64, f: F) -> Option<R>
where
    F: FnOnce(&FdTable) -> R,
{
    let table = PROCESS_TABLE.lock();
    table.get(&pid).map(|p| f(&p.fd_table))
}

/// Get mutable FD table for a process
pub fn with_fd_table_mut<F, R>(pid: u64, f: F) -> Option<R>
where
    F: FnOnce(&mut FdTable) -> R,
{
    let mut table = PROCESS_TABLE.lock();
    table.get_mut(&pid).map(|p| f(&mut p.fd_table))
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
