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

// ===== File Descriptor Table =====

/// Maximum number of file descriptors per process
pub const MAX_FDS: usize = 16;

/// A file descriptor entry
#[derive(Debug, Clone)]
pub struct FileDescriptor {
    /// Path in VFS (or special: "stdin", "stdout", "stderr")
    pub path: String,
    /// Current offset for read/seek
    pub offset: u64,
    /// Flags (O_RDONLY=0, O_WRONLY=1, O_RDWR=2)
    pub flags: u32,
    /// Is this FD active?
    pub active: bool,
}

impl FileDescriptor {
    pub fn new(path: &str, flags: u32) -> Self {
        FileDescriptor {
            path: String::from(path),
            offset: 0,
            flags,
            active: true,
        }
    }

    pub fn empty() -> Self {
        FileDescriptor {
            path: String::new(),
            offset: 0,
            flags: 0,
            active: false,
        }
    }
}

/// File descriptor table for a process
#[derive(Debug, Clone)]
pub struct FdTable {
    pub entries: Vec<FileDescriptor>,
}

impl FdTable {
    /// Create a new FD table with stdin(0), stdout(1), stderr(2)
    pub fn new_with_stdio() -> Self {
        let mut entries = Vec::with_capacity(MAX_FDS);
        entries.push(FileDescriptor::new("stdin", 0));   // FD 0 = stdin
        entries.push(FileDescriptor::new("stdout", 1));  // FD 1 = stdout
        entries.push(FileDescriptor::new("stderr", 1));  // FD 2 = stderr
        FdTable { entries }
    }

    /// Create an empty FD table (for kernel threads)
    pub fn empty() -> Self {
        FdTable { entries: Vec::new() }
    }

    /// Allocate a new FD, returns the FD number or None
    pub fn alloc_fd(&mut self, path: &str, flags: u32) -> Option<usize> {
        // Try to reuse a closed FD slot
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if !entry.active {
                *entry = FileDescriptor::new(path, flags);
                return Some(i);
            }
        }
        // Allocate new slot
        if self.entries.len() < MAX_FDS {
            let fd = self.entries.len();
            self.entries.push(FileDescriptor::new(path, flags));
            Some(fd)
        } else {
            None
        }
    }

    /// Close a file descriptor
    pub fn close_fd(&mut self, fd: usize) -> bool {
        if fd < self.entries.len() && self.entries[fd].active {
            self.entries[fd].active = false;
            self.entries[fd].path.clear();
            true
        } else {
            false
        }
    }

    /// Get a reference to an FD entry
    pub fn get(&self, fd: usize) -> Option<&FileDescriptor> {
        self.entries.get(fd).filter(|e| e.active)
    }

    /// Get a mutable reference to an FD entry
    pub fn get_mut(&mut self, fd: usize) -> Option<&mut FileDescriptor> {
        self.entries.get_mut(fd).filter(|e| e.active)
    }
}

use crate::arch::x86_64::context::TaskContext;

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
    /// CPU register context for context switching
    pub context: TaskContext,
    /// Physical address of the process's PML4 (page map level 4) table
    pub pml4_phys: u64,
    /// Number of scheduler ticks this process has been waiting (for aging)
    pub wait_ticks: u64,
    /// File descriptor table
    pub fd_table: FdTable,
    /// Exit code (set when process terminates)
    pub exit_code: i32,
    /// Entry point address (for Ring 3 processes)
    pub entry_point: u64,
    /// Stack pointer (for Ring 3 processes)
    pub stack_pointer: u64,
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
            context: TaskContext::zero(),
            pml4_phys: 0,
            wait_ticks: 0,
            fd_table: FdTable::new_with_stdio(),
            exit_code: 0,
            entry_point: 0,
            stack_pointer: 0,
        }
    }

    /// Create a new user-space process (Ring 3) with full setup
    pub fn new_userspace(name: &str, ppid: u64, entry: u64, stack: u64, pml4: u64) -> Self {
        let mut proc = Self::new(name, AgentRole::Worker, ppid, 1000, 1000);
        proc.entry_point = entry;
        proc.stack_pointer = stack;
        proc.pml4_phys = pml4;
        proc
    }

    /// Create a kernel thread (uid=0, gid=0, no parent)
    pub fn new_kernel(name: &str) -> Self {
        let mut proc = Self::new(name, AgentRole::KernelThread, 0, 0, 0);
        proc.fd_table = FdTable::empty(); // kernel threads don't need FDs
        proc
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

    /// Set the PML4 physical address for the process.
    pub fn set_pml4_phys(&mut self, pml4: u64) {
        self.pml4_phys = pml4;
    }

    /// Get a mutable reference to the process's context.
    pub fn get_context_mut(&mut self) -> &mut TaskContext {
        &mut self.context
    }

    /// Get a reference to the process's context.
    pub fn get_context(&self) -> &TaskContext {
        &self.context
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
