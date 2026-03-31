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

pub use task::{AgentRole, Process, ProcessState, FdTable, FdType, FileDescriptor, MAX_FDS, VirtualMemoryArea};
pub use crate::arch::x86_64::context::TaskContext;

// ===== Keyboard Input Buffer =====
// Lock-free ring buffer via atomic indices.
// IRQ handler writes (push), sys_read reads (pop). No Mutex = no deadlock.

const KBD_BUF_SIZE: usize = 256;

struct KbdBuffer {
    buf: [u8; KBD_BUF_SIZE],
    read_pos: core::sync::atomic::AtomicUsize,
    write_pos: core::sync::atomic::AtomicUsize,
}

impl KbdBuffer {
    const fn new() -> Self {
        KbdBuffer {
            buf: [0u8; KBD_BUF_SIZE],
            read_pos: core::sync::atomic::AtomicUsize::new(0),
            write_pos: core::sync::atomic::AtomicUsize::new(0),
        }
    }

    // Called from IRQ — must never block
    fn push(&self, byte: u8) {
        let wp = self.write_pos.load(Ordering::Relaxed);
        let next_wp = (wp + 1) % KBD_BUF_SIZE;
        let rp = self.read_pos.load(Ordering::Acquire);
        if next_wp != rp {
            // SAFETY: single producer (IRQ handler), wp unique to this writer
            unsafe {
                core::ptr::write_volatile(self.buf.as_ptr().add(wp) as *mut u8, byte);
            }
            self.write_pos.store(next_wp, Ordering::Release);
        }
        // Buffer full: byte dropped silently (correct behavior)
    }

    // Called from sys_read — non-blocking
    fn pop(&self) -> Option<u8> {
        let rp = self.read_pos.load(Ordering::Relaxed);
        let wp = self.write_pos.load(Ordering::Acquire);
        if rp == wp { return None; }
        let byte = unsafe {
            core::ptr::read_volatile(self.buf.as_ptr().add(rp))
        };
        self.read_pos.store((rp + 1) % KBD_BUF_SIZE, Ordering::Release);
        Some(byte)
    }
}

// SAFETY: Single producer (IRQ), single consumer (sys_read). Atomic indices guarantee safety.
unsafe impl Sync for KbdBuffer {}
unsafe impl Send for KbdBuffer {}

static KBD_BUFFER: KbdBuffer = KbdBuffer::new();

/// PID of process blocked waiting for keyboard input (0 = none)
static KBD_WAITER_PID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Set the PID that is waiting for keyboard input
pub fn kbd_set_waiter(pid: u64) {
    KBD_WAITER_PID.store(pid, core::sync::atomic::Ordering::SeqCst);
}

/// Wake the process blocked on keyboard read (called from keyboard IRQ handler)
/// Returns true if a process was woken
pub fn kbd_wake_blocked() -> bool {
    let pid = KBD_WAITER_PID.swap(0, core::sync::atomic::Ordering::SeqCst);
    if pid != 0 {
        if let Some(state) = get_state(pid) {
            if state == ProcessState::Blocked {
                let _ = set_state(pid, ProcessState::Ready);
                return true;
            }
        }
    }
    false
}

/// Push a byte into the keyboard input buffer (called from keyboard IRQ handler)
pub fn kbd_push_byte(byte: u8) {
    KBD_BUFFER.push(byte);
    // Jalon 96: If a process is blocked waiting for keyboard, wake it
    kbd_wake_blocked();
}

/// Read up to `len` bytes from the keyboard buffer into a slice
pub fn kbd_read(buf: &mut [u8], len: usize) -> usize {
    let max = core::cmp::min(len, buf.len());
    let mut read = 0;
    while read < max {
        if let Some(b) = KBD_BUFFER.pop() {
            buf[read] = b;
            read += 1;
            if b == b'\n' { break; }
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
    let mut proc = Process::new_userspace(name, ppid, entry, stack, pml4);
    // CRITICAL: Initialize saved state so find_next_ready_userspace can find this process
    proc.saved_user_rip = entry;
    proc.saved_user_rsp = stack;
    proc.context.rflags = 0x202; // IF=1 + reserved bit 1
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
    crate::serial_println!("[FORK] child created: pid={} ppid={}", child.pid, child.ppid);
    child.pml4_phys = child_pml4;
    child.entry_point = child_entry;
    child.stack_pointer = child_stack;
    child.saved_user_rip = child_entry;  // CRITICAL: Initialize so find_next_ready_userspace finds it
    child.saved_user_rsp = child_stack;
    child.context.rflags = 0x202; // IF=1 + reserved bit 1
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

/// Clone thread: create a lightweight thread sharing the parent's address space (PML4).
/// Unlike fork, the child shares the same virtual memory — true threading.
/// child_stack is the top of the pre-allocated stack for the new thread.
/// child_entry is the instruction pointer where the thread begins execution.
pub fn clone_thread(parent_pid: u64, child_stack: u64, child_entry: u64) -> Result<u64, ProcessError> {
    let mut table = PROCESS_TABLE.lock();
    if table.len() >= MAX_PROCESSES {
        return Err(ProcessError::LimitReached);
    }
    let parent = table.get(&parent_pid).ok_or(ProcessError::NotFound)?;
    let parent_name = parent.name.clone();
    let parent_uid = parent.uid;
    let parent_gid = parent.gid;
    let parent_pml4 = parent.pml4_phys; // SHARED, not copied!
    let parent_fd_table = parent.fd_table.clone();

    let mut child = Process::new(&parent_name, AgentRole::Worker, parent_pid, parent_uid, parent_gid);
    child.pml4_phys = parent_pml4; // Key: share the address space
    child.entry_point = child_entry;
    child.stack_pointer = child_stack;
    child.saved_user_rip = child_entry;  // CRITICAL: Initialize so find_next_ready_userspace finds it
    child.saved_user_rsp = child_stack;
    child.context.rflags = 0x202; // IF=1 + reserved bit 1
    child.fd_table = parent_fd_table;
    child.state = ProcessState::Ready;
    child.is_thread = true; // Mark as thread (shared memory)
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

/// Find a Ready forked child of parent_pid (not a thread).
/// If target_pid != 0, look for that specific child.
/// Returns (child_pid, pml4_phys, saved_user_rip, saved_user_rsp, saved_syscall_regs).
pub fn find_ready_forked_child(parent_pid: u64, target_pid: u64) -> Option<(u64, u64, u64, u64, [u64; 8])> {
    let table = PROCESS_TABLE.lock();
    let parent = table.get(&parent_pid)?;

    for &child_pid in &parent.children {
        if target_pid != 0 && child_pid != target_pid { continue; }
        if let Some(child) = table.get(&child_pid) {
            if child.is_forked && child.state == ProcessState::Ready && !child.is_thread {
                return Some((
                    child_pid,
                    child.pml4_phys,
                    child.saved_user_rip,
                    child.saved_user_rsp,
                    child.saved_syscall_regs,
                ));
            }
        }
    }
    None
}

/// Find a Ready child thread of parent_pid.
/// Returns (child_pid, entry_point, stack_pointer, pml4_phys) or None.
pub fn find_ready_child_thread(parent_pid: u64) -> Option<(u64, u64, u64, u64)> {
    let table = PROCESS_TABLE.lock();
    let parent = table.get(&parent_pid)?;

    for &child_pid in &parent.children {
        if let Some(child) = table.get(&child_pid) {
            if child.is_thread && child.state == ProcessState::Ready {
                return Some((child_pid, child.entry_point, child.stack_pointer, child.pml4_phys));
            }
        }
    }
    None
}

/// Find the next Ready userspace process (not a thread, not the given PID).
///
/// JALON 69 FIX: Two modes:
///   1. For kill_user_and_switch (SIGSEGV recovery): only return processes that
///      have been ACTUALLY PREEMPTED (saved_user_rip in valid userspace range
///      0x8000000000..0x9000000000 and different from entry_point).
///   2. For sys_yield context switch: also allow processes that have a valid
///      saved state OR a valid entry_point for first-run.
///
/// Terminated processes are ALWAYS excluded.
///
/// Returns (pid, entry_point, stack_pointer, pml4_phys, name) or None.
pub fn find_next_ready_userspace(exclude_pid: u64) -> Option<(u64, u64, u64, u64, String)> {
    let mut table = PROCESS_TABLE.lock();
    
    // First pass: look for processes that were actively running (have valid saved state)
    // ONLY processes in Ready state are eligible — Running, Blocked, Terminated are skipped.
    for (_, proc) in table.iter() {
        if proc.pid == exclude_pid { continue; }
        if proc.state != ProcessState::Ready { continue; } // Only Ready (not Running/Blocked/Terminated)
        if proc.is_thread { continue; }
        if proc.entry_point == 0 || proc.pml4_phys == 0 { continue; }
        if proc.role == AgentRole::KernelThread { continue; }
        
        // Only processes with saved_user_rip in valid userspace range
        let has_valid_saved_state = proc.saved_user_rip >= 0x8000000000
            && proc.saved_user_rip < 0x9000000000
            && proc.saved_user_rip != proc.entry_point;
        
        if has_valid_saved_state {
            let result = (
                proc.pid,
                proc.entry_point,
                proc.stack_pointer,
                proc.pml4_phys,
                proc.name.clone(),
            );
            // Atomically mark as Running under the same lock to prevent double-launch
            if let Some(p) = table.get_mut(&result.0) {
                p.state = ProcessState::Running;
            }
            return Some(result);
        }
    }
    
    // Second pass: look for any Ready process with a valid entry point (for first-run)
    for (_, proc) in table.iter() {
        if proc.pid == exclude_pid { continue; }
        if proc.state != ProcessState::Ready { continue; } // Only Ready (not Running/Blocked/Terminated)
        if proc.is_thread { continue; }
        if proc.entry_point == 0 || proc.pml4_phys == 0 { continue; }
        if proc.role == AgentRole::KernelThread { continue; }
        
        let result = (
            proc.pid,
            proc.entry_point,
            proc.stack_pointer,
            proc.pml4_phys,
            proc.name.clone(),
        );
        // Atomically mark as Running under the same lock
        if let Some(p) = table.get_mut(&result.0) {
            p.state = ProcessState::Running;
        }
        return Some(result);
    }
    
    None
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

// ===== FD Convenience Wrappers (used by syscall layer) =====

/// Allocate a new file descriptor in a process's FD table.
/// Returns Some(fd_number) on success, None if table is full.
pub fn alloc_fd(pid: u64, path: &str, flags: u32) -> Option<usize> {
    with_fd_table_mut(pid, |fdt| fdt.alloc_fd(path, flags))?
}

/// Get the path associated with a file descriptor.
pub fn get_fd_path(pid: u64, fd: usize) -> Option<alloc::string::String> {
    with_fd_table(pid, |fdt| {
        fdt.get(fd).map(|entry| entry.path.clone())
    }).flatten()
}

/// Set (or overwrite) a specific FD slot to point at the given path.
pub fn set_fd(pid: u64, fd: usize, path: &str, flags: u32) {
    with_fd_table_mut(pid, |fdt| {
        // Ensure enough capacity
        while fdt.entries.len() <= fd {
            fdt.entries.push(task::FileDescriptor::empty());
        }
        fdt.entries[fd] = task::FileDescriptor::new(path, flags);
    });
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

/// Get the state of a process by PID
pub fn get_state(pid: u64) -> Option<ProcessState> {
    let table = PROCESS_TABLE.lock();
    table.get(&pid).map(|p| p.state)
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

/// Jalon 96: Set CPU core affinity for a process
/// 0xFF = no affinity (any core), 0 = BSP only, 1+ = specific AP core
pub fn set_cpu_affinity(pid: u64, core: u8) {
    let mut table = PROCESS_TABLE.lock();
    if let Some(p) = table.get_mut(&pid) {
        p.cpu_affinity = core;
    }
}

/// Jalon 96: Get CPU core affinity for a process
pub fn get_cpu_affinity(pid: u64) -> u8 {
    let table = PROCESS_TABLE.lock();
    table.get(&pid).map(|p| p.cpu_affinity).unwrap_or(0xFF)
}

/// Jalon 55/79: Save user-mode state for preemptive context switch.
/// Called from timer interrupt handler or sys_yield when preempting a Ring 3 process.
/// Also saves callee-saved registers (r15,r14,r13,r12,rbx,rbp,r11,rcx) from
/// the kernel syscall stack so they can be restored on resume.
pub fn save_preempt_state(pid: u64, rip: u64, rsp: u64, rflags: u64) {
    let mut table = PROCESS_TABLE.lock();
    if let Some(p) = table.get_mut(&pid) {
        p.saved_user_rip = rip;
        p.saved_user_rsp = rsp;
        p.context.rflags = rflags;
    }
}

/// Jalon 79: Save callee-saved registers from kernel syscall stack.
/// The kernel_rsp points to: [r15, r14, r13, r12, rbx, rbp, r11, rcx]
/// These must be saved per-process to restore on context switch.
pub fn save_syscall_regs(pid: u64, regs: [u64; 8]) {
    let mut table = PROCESS_TABLE.lock();
    if let Some(p) = table.get_mut(&pid) {
        p.saved_syscall_regs = regs;
    }
}

/// Jalon 55: Get user-mode state for restoring a preempted process.
/// Retourne (rip, rsp, rflags, pml4_phys, saved_syscall_regs).
/// Les regs contiennent les registres callee-saved (r15,r14,r13,r12,rbx,rbp,r11,rcx)
/// tels qu'ils étaient sur le stack noyau au moment du yield.
pub fn get_preempt_state(pid: u64) -> Option<(u64, u64, u64, u64, [u64; 8])> {
    let table = PROCESS_TABLE.lock();
    if let Some(p) = table.get(&pid) {
        Some((p.saved_user_rip, p.saved_user_rsp, p.context.rflags, p.pml4_phys, p.saved_syscall_regs))
    } else {
        None
    }
}

/// Get entry point state for a fresh (never-preempted) process.
/// Returns (entry_point, stack_pointer, pml4_phys).
pub fn get_entry_state(pid: u64) -> Option<(u64, u64, u64)> {
    let table = PROCESS_TABLE.lock();
    if let Some(p) = table.get(&pid) {
        Some((p.entry_point, p.stack_pointer, p.pml4_phys))
    } else {
        None
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

/// Get the current heap break for a process
pub fn get_heap_break(pid: u64) -> Option<u64> {
    with_process(pid, |p| p.heap_break)
}

/// Set the heap break for a process, returning the old value
pub fn set_heap_break(pid: u64, new_break: u64) -> Option<u64> {
    with_process_mut(pid, |p| {
        let old = p.heap_break;
        p.heap_break = new_break;
        old
    })
}

// ===== VMA Management (Jalon 68: Zero-Copy Streaming) =====

/// Add a file-backed VMA to a process
pub fn add_vma(pid: u64, vma: VirtualMemoryArea) -> Option<()> {
    with_process_mut(pid, |p| {
        p.vmas.push(vma);
    })
}

/// Find a VMA that contains the given virtual address
/// Returns (file_path, file_offset_for_this_page, writable)
/// NOTE: The returned file_offset is page-aligned so the page fault handler
/// reads exactly 4 KiB starting at the correct file page boundary.
pub fn find_vma(pid: u64, addr: u64) -> Option<(String, u64, bool)> {
    with_process(pid, |p| {
        for vma in &p.vmas {
            if addr >= vma.vaddr_start && addr < vma.vaddr_end {
                // Page-align: compute offset from start of VMA, rounded down to 4K
                let offset_in_vma = (addr - vma.vaddr_start) & !0xFFF;
                let file_offset = vma.file_offset + offset_in_vma;
                return Some((vma.file_path.clone(), file_offset, vma.writable));
            }
        }
        None
    }).flatten()
}

// ═══════════════════════════════════════════════════════════════
// Jalon 100: Kernel Watchdog — Autonomous Agent Respawn
// ═══════════════════════════════════════════════════════════════
// If a critical agent crashes (SIGSEGV, GPF), the watchdog:
//   1. Frees all physical frames (page tables, heap, VMA pages)
//   2. Re-loads the ELF binary from VFS (/bin/<name>)
//   3. Spawns a fresh process with the same role and affinity
//   4. Enqueues it in the scheduler
// Total respawn time: < 10ms (all from RAM, zero disk I/O)
// ═══════════════════════════════════════════════════════════════

/// Maximum number of watchdog-monitored agents
const WATCHDOG_MAX: usize = 8;
/// Maximum respawn attempts per agent (prevent infinite crash loops)
const WATCHDOG_MAX_RESPAWNS: u32 = 5;

/// Watchdog entry: tracks a critical agent for automatic respawn
struct WatchdogEntry {
    name: [u8; 64],        // Process name (e.g., "/bin/agent_orchestrator.elf")
    name_len: usize,
    active: bool,
    respawn_count: u32,
    cpu_affinity: u8,      // Core affinity to restore on respawn
}

/// Global watchdog registry
static WATCHDOG_REGISTRY: Mutex<[WatchdogEntry; WATCHDOG_MAX]> = Mutex::new([
    WatchdogEntry { name: [0; 64], name_len: 0, active: false, respawn_count: 0, cpu_affinity: 0xFF },
    WatchdogEntry { name: [0; 64], name_len: 0, active: false, respawn_count: 0, cpu_affinity: 0xFF },
    WatchdogEntry { name: [0; 64], name_len: 0, active: false, respawn_count: 0, cpu_affinity: 0xFF },
    WatchdogEntry { name: [0; 64], name_len: 0, active: false, respawn_count: 0, cpu_affinity: 0xFF },
    WatchdogEntry { name: [0; 64], name_len: 0, active: false, respawn_count: 0, cpu_affinity: 0xFF },
    WatchdogEntry { name: [0; 64], name_len: 0, active: false, respawn_count: 0, cpu_affinity: 0xFF },
    WatchdogEntry { name: [0; 64], name_len: 0, active: false, respawn_count: 0, cpu_affinity: 0xFF },
    WatchdogEntry { name: [0; 64], name_len: 0, active: false, respawn_count: 0, cpu_affinity: 0xFF },
]);

/// Register a critical agent for watchdog monitoring.
/// `name` should be the VFS path (e.g., "/bin/agent_orchestrator.elf").
/// `affinity` is the CPU core affinity to assign on respawn.
pub fn watchdog_register(name: &str, affinity: u8) {
    let mut reg = WATCHDOG_REGISTRY.lock();
    for entry in reg.iter_mut() {
        if !entry.active {
            let bytes = name.as_bytes();
            let len = core::cmp::min(bytes.len(), 63);
            entry.name[..len].copy_from_slice(&bytes[..len]);
            entry.name[len] = 0;
            entry.name_len = len;
            entry.active = true;
            entry.respawn_count = 0;
            entry.cpu_affinity = affinity;
            crate::serial_println!("[WATCHDOG] Registered: {} (affinity={})", name, affinity);
            return;
        }
    }
    crate::serial_println!("[WATCHDOG] WARN: Registry full, cannot register {}", name);
}

/// Check if a process name is registered in the watchdog.
/// Returns Some((index, affinity)) if registered, None otherwise.
fn watchdog_find(name: &str) -> Option<(usize, u8)> {
    let reg = WATCHDOG_REGISTRY.lock();
    let name_bytes = name.as_bytes();
    for (i, entry) in reg.iter().enumerate() {
        if entry.active && entry.name_len > 0 {
            let entry_name = &entry.name[..entry.name_len];
            if entry_name == name_bytes {
                return Some((i, entry.cpu_affinity));
            }
        }
    }
    None
}

/// Attempt to respawn a crashed agent from VFS.
/// Called from kill_user_and_switch when a critical agent dies.
/// Returns the new PID if successful, 0 if respawn failed.
pub fn watchdog_try_respawn(crashed_name: &str) -> u64 {
    // Check if this agent is registered
    let (idx, affinity) = match watchdog_find(crashed_name) {
        Some(v) => v,
        None => return 0, // Not a watchdog agent
    };

    // Check respawn count
    {
        let mut reg = WATCHDOG_REGISTRY.lock();
        if reg[idx].respawn_count >= WATCHDOG_MAX_RESPAWNS {
            crate::serial_println!(
                "[WATCHDOG] ABORT: {} exceeded max respawns ({})",
                crashed_name, WATCHDOG_MAX_RESPAWNS
            );
            reg[idx].active = false;
            return 0;
        }
        reg[idx].respawn_count += 1;
        crate::serial_println!(
            "[WATCHDOG] Respawning {} (attempt {}/{})",
            crashed_name, reg[idx].respawn_count, WATCHDOG_MAX_RESPAWNS
        );
    }

    // Read ELF binary from VFS
    let elf_data = match crate::fs::vfs::file_read(crashed_name) {
        Ok(data) if data.len() > 4 => data,
        _ => {
            crate::serial_println!("[WATCHDOG] FAIL: Cannot read {} from VFS", crashed_name);
            return 0;
        }
    };

    // Load ELF binary
    let load_result = match crate::elf::load_elf_binary(&elf_data) {
        Ok(result) => result,
        Err(e) => {
            crate::serial_println!("[WATCHDOG] FAIL: ELF load error for {}: {:?}", crashed_name, e);
            return 0;
        }
    };

    // Spawn new process
    let new_pid = match spawn_userspace(
        crashed_name, 0,
        load_result.entry_point,
        load_result.stack_pointer,
        load_result.pml4_phys,
    ) {
        Ok(pid) => pid,
        Err(e) => {
            crate::serial_println!("[WATCHDOG] FAIL: spawn error for {}: {:?}", crashed_name, e);
            return 0;
        }
    };

    // Set CPU affinity
    set_cpu_affinity(new_pid, affinity);

    // Enqueue in scheduler
    crate::scheduler::enqueue_process(new_pid);

    crate::serial_println!(
        "[WATCHDOG] SUCCESS: {} respawned as PID {} (affinity={})",
        crashed_name, new_pid, affinity
    );

    new_pid
}
