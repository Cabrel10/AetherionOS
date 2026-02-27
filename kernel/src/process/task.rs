// process/task.rs - Couche 6: Process Descriptor with Matriarchal Hierarchy
//
// Defines:
//   - AgentRole: Matriarch, SubMatriarch, Worker
//   - ProcessState: Ready, Running, Blocked, Terminated
//   - Process struct with ppid, role, children, uid, gid

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use core::fmt;

// ===== Global PID Counter =====

static NEXT_PID: AtomicU64 = AtomicU64::new(1);

/// Allocate the next unique PID (monotonically increasing)
pub fn alloc_pid() -> u64 {
    NEXT_PID.fetch_add(1, Ordering::SeqCst)
}

/// Peek at the next PID without allocating
pub fn peek_next_pid() -> u64 {
    NEXT_PID.load(Ordering::SeqCst)
}

// ===== Agent Role (Matriarchal Hierarchy) =====

/// Role of a process in the matriarchal swarm hierarchy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    /// Root orchestrator — unique, PID must be low, highest priority
    Matriarch,
    /// Domain leader — manages a subset of workers, medium priority
    SubMatriarch,
    /// Leaf worker — executes tasks, lowest priority
    Worker,
    /// Kernel-internal thread (idle, IRQ, etc.)
    KernelThread,
}

impl fmt::Display for AgentRole {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Matriarch => write!(f, "Matriarch"),
            Self::SubMatriarch => write!(f, "SubMatriarch"),
            Self::Worker => write!(f, "Worker"),
            Self::KernelThread => write!(f, "KernelThread"),
        }
    }
}

// ===== Process State =====

/// Current execution state of a process
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Ready,
    Running,
    Blocked,
    Terminated,
}

impl fmt::Display for ProcessState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Ready => write!(f, "READY"),
            Self::Running => write!(f, "RUNNING"),
            Self::Blocked => write!(f, "BLOCKED"),
            Self::Terminated => write!(f, "TERMINATED"),
        }
    }
}

// ===== Process Descriptor =====

/// A process in the AetherionOS kernel
#[derive(Debug, Clone)]
pub struct Process {
    /// Unique process identifier
    pub pid: u64,
    /// Parent process identifier (0 = no parent)
    pub ppid: u64,
    /// Process name
    pub name: String,
    /// Role in the matriarchal hierarchy
    pub role: AgentRole,
    /// Current execution state
    pub state: ProcessState,
    /// User ID (0 = root/kernel)
    pub uid: u32,
    /// Group ID
    pub gid: u32,
    /// Scheduling priority (higher = more important; Matriarch=20, Sub=15, Worker=5)
    pub priority: u8,
    /// List of child PIDs
    pub children: Vec<u64>,
}

impl Process {
    /// Create a new process with full parameters
    pub fn new(name: &str, role: AgentRole, ppid: u64, uid: u32, gid: u32) -> Self {
        let priority = match role {
            AgentRole::Matriarch => 20,
            AgentRole::SubMatriarch => 15,
            AgentRole::Worker => 5,
            AgentRole::KernelThread => 25,
        };
        Process {
            pid: alloc_pid(),
            ppid,
            name: String::from(name),
            role,
            state: ProcessState::Ready,
            uid,
            gid,
            priority,
            children: Vec::new(),
        }
    }

    /// Create a kernel thread (uid=0, gid=0, no parent)
    pub fn new_kernel(name: &str) -> Self {
        Self::new(name, AgentRole::KernelThread, 0, 0, 0)
    }

    /// Add a child PID to this process
    pub fn add_child(&mut self, child_pid: u64) {
        self.children.push(child_pid);
    }

    /// Check if a state transition is valid
    pub fn can_transition_to(&self, new_state: ProcessState) -> bool {
        use ProcessState::*;
        match (self.state, new_state) {
            (Ready, Running) => true,
            (Running, Ready) => true,
            (Running, Blocked) => true,
            (Running, Terminated) => true,
            (Blocked, Ready) => true,
            (_, Terminated) => true,   // anything can be killed
            _ => false,
        }
    }

    /// Attempt to set a new state, returns false if the transition is invalid
    pub fn set_state(&mut self, new_state: ProcessState) -> bool {
        if self.can_transition_to(new_state) {
            self.state = new_state;
            true
        } else {
            false
        }
    }

    /// Is this process alive (not terminated)?
    pub fn is_alive(&self) -> bool {
        self.state != ProcessState::Terminated
    }
}

impl fmt::Display for Process {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "[PID {} | {} | {} | {} | ppid={}]",
            self.pid, self.role, self.name, self.state, self.ppid)
    }
}
