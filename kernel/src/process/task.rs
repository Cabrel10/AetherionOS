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

// ===== File Descriptor Table (Jalon 79: Unified FD with FdType) =====

/// Maximum number of file descriptors per process
pub const MAX_FDS: usize = 256;

/// FD type discriminator — dispatches read/write/close to the correct subsystem.
/// Jalon 79: Unified FD table for POSIX compatibility (musl requirement).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdType {
    /// Regular file backed by VFS or FAT32 (default)
    File,
    /// Network socket (TCP/UDP) — dispatches to net::socket
    Socket,
    /// Kernel pipe (pipe2 / pipe)
    Pipe,
    /// Terminal (stdin/stdout/stderr)
    Tty,
    /// Epoll file descriptor for async I/O (Jalon 135)
    Epoll,
    /// PTY master side — dispatches to drivers::pty (master I/O)
    PtyMaster,
    /// PTY slave side — dispatches to drivers::pty (slave I/O)
    PtySlave,
}

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
    /// Type discriminator for unified dispatch (Jalon 79)
    pub fd_type: FdType,
    /// Socket ID (only valid when fd_type == Socket)
    pub socket_id: u32,
    /// PTY ID (only valid when fd_type == PtyMaster or PtySlave)
    pub pty_id: u32,
}

impl FileDescriptor {
    pub fn new(path: &str, flags: u32) -> Self {
        FileDescriptor {
            path: String::from(path),
            offset: 0,
            flags,
            active: true,
            fd_type: FdType::File,
            socket_id: 0,
            pty_id: 0,
        }
    }

    pub fn new_typed(path: &str, flags: u32, fd_type: FdType) -> Self {
        FileDescriptor {
            path: String::from(path),
            offset: 0,
            flags,
            active: true,
            fd_type,
            socket_id: 0,
            pty_id: 0,
        }
    }

    pub fn new_socket(socket_id: u32) -> Self {
        FileDescriptor {
            path: String::from("socket"),
            offset: 0,
            flags: 2, // O_RDWR
            active: true,
            fd_type: FdType::Socket,
            socket_id,
            pty_id: 0,
        }
    }

    /// Create a PTY master FD
    pub fn new_pty_master(pty_id: u32) -> Self {
        FileDescriptor {
            path: String::from("/dev/ptmx"),
            offset: 0,
            flags: 2, // O_RDWR
            active: true,
            fd_type: FdType::PtyMaster,
            socket_id: 0,
            pty_id,
        }
    }

    /// Create a PTY slave FD
    pub fn new_pty_slave(pty_id: u32) -> Self {
        use alloc::format;
        FileDescriptor {
            path: format!("/dev/pts/{}", pty_id),
            offset: 0,
            flags: 2, // O_RDWR
            active: true,
            fd_type: FdType::PtySlave,
            socket_id: 0,
            pty_id,
        }
    }

    pub fn empty() -> Self {
        FileDescriptor {
            path: String::new(),
            offset: 0,
            flags: 0,
            active: false,
            fd_type: FdType::File,
            socket_id: 0,
            pty_id: 0,
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
    /// Jalon 79: These are now typed as Tty for unified dispatch.
    pub fn new_with_stdio() -> Self {
        let mut entries = Vec::with_capacity(MAX_FDS);
        entries.push(FileDescriptor::new_typed("stdin", 0, FdType::Tty));   // FD 0 = stdin
        entries.push(FileDescriptor::new_typed("stdout", 1, FdType::Tty));  // FD 1 = stdout
        entries.push(FileDescriptor::new_typed("stderr", 1, FdType::Tty));  // FD 2 = stderr
        FdTable { entries }
    }

    /// Create an empty FD table (for kernel threads)
    pub fn empty() -> Self {
        FdTable { entries: Vec::new() }
    }

    /// Allocate a new FD, returns the FD number or None
    pub fn alloc_fd(&mut self, path: &str, flags: u32) -> Option<usize> {
        self.alloc_fd_typed(path, flags, FdType::File)
    }

    /// Allocate a new FD with explicit type (Jalon 79: unified FD)
    pub fn alloc_fd_typed(&mut self, path: &str, flags: u32, fd_type: FdType) -> Option<usize> {
        // Try to reuse a closed FD slot
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if !entry.active {
                *entry = FileDescriptor::new_typed(path, flags, fd_type);
                return Some(i);
            }
        }
        // Allocate new slot
        if self.entries.len() < MAX_FDS {
            let fd = self.entries.len();
            self.entries.push(FileDescriptor::new_typed(path, flags, fd_type));
            Some(fd)
        } else {
            None
        }
    }

    /// Allocate a socket FD (Jalon 79)
    pub fn alloc_socket_fd(&mut self, socket_id: u32) -> Option<usize> {
        for (i, entry) in self.entries.iter_mut().enumerate() {
            if !entry.active {
                *entry = FileDescriptor::new_socket(socket_id);
                return Some(i);
            }
        }
        if self.entries.len() < MAX_FDS {
            let fd = self.entries.len();
            self.entries.push(FileDescriptor::new_socket(socket_id));
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

use crate::arch::x86_64::context::{TaskContext, FpuState};

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

// ===== Syscall Register Context =====

/// Complete register context saved during SYSCALL entry.
///
/// Field order matches the push sequence in `syscall_entry` assembly
/// (first pushed = lowest address = first field):
///
/// ```text
///   [RSP+0]   r9    — 6th syscall arg
///   [RSP+8]   r10   — 4th syscall arg (Linux ABI)
///   [RSP+16]  r8    — 5th syscall arg
///   [RSP+24]  rdx   — 3rd syscall arg
///   [RSP+32]  rsi   — 2nd syscall arg
///   [RSP+40]  rdi   — 1st syscall arg
///   [RSP+48]  r15   — callee-saved
///   [RSP+56]  r14   — callee-saved
///   [RSP+64]  r13   — callee-saved
///   [RSP+72]  r12   — callee-saved
///   [RSP+80]  rbx   — callee-saved
///   [RSP+88]  rbp   — callee-saved
///   [RSP+96]  r11   — RFLAGS (set by SYSCALL instruction)
///   [RSP+104] rcx   — RIP   (set by SYSCALL instruction)
/// ```
///
/// The `sysretq` instruction reloads RIP from RCX and RFLAGS from R11.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SyscallContext {
    // --- caller-saved / syscall arguments ---
    pub r9:  u64,
    pub r10: u64,
    pub r8:  u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    // --- callee-saved ---
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbx: u64,
    pub rbp: u64,
    // --- special (set by SYSCALL HW) ---
    pub rflags: u64,  // r11 on the stack
    pub rip:    u64,   // rcx on the stack
}

impl SyscallContext {
    /// All-zero context (no registers saved yet).
    pub const ZERO: Self = Self {
        r9: 0, r10: 0, r8: 0, rdx: 0, rsi: 0, rdi: 0,
        r15: 0, r14: 0, r13: 0, r12: 0, rbx: 0, rbp: 0,
        rflags: 0, rip: 0,
    };

    /// Returns true if a valid context was saved (RIP != 0).
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.rip != 0
    }

    /// Read a full context from the kernel stack at the given base address.
    /// # Safety
    /// `base` must point to the first pushed register (r9) in a valid
    /// `syscall_entry` frame.
    pub unsafe fn from_kernel_stack(base: u64) -> Self {
        let p = base as *const u64;
        Self {
            r9:     core::ptr::read_unaligned(p),
            r10:    core::ptr::read_unaligned(p.add(1)),
            r8:     core::ptr::read_unaligned(p.add(2)),
            rdx:    core::ptr::read_unaligned(p.add(3)),
            rsi:    core::ptr::read_unaligned(p.add(4)),
            rdi:    core::ptr::read_unaligned(p.add(5)),
            r15:    core::ptr::read_unaligned(p.add(6)),
            r14:    core::ptr::read_unaligned(p.add(7)),
            r13:    core::ptr::read_unaligned(p.add(8)),
            r12:    core::ptr::read_unaligned(p.add(9)),
            rbx:    core::ptr::read_unaligned(p.add(10)),
            rbp:    core::ptr::read_unaligned(p.add(11)),
            rflags: core::ptr::read_unaligned(p.add(12)),
            rip:    core::ptr::read_unaligned(p.add(13)),
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
    /// True if this is a thread (shares parent's address space)
    pub is_thread: bool,
    /// Saved user RIP (for resuming parent after child threads finish)
    pub saved_user_rip: u64,
    /// Saved user RSP (for resuming parent after child threads finish)
    pub saved_user_rsp: u64,
    /// Saved kernel RSP pointing to syscall_entry register frame (for sysretq resume)
    pub saved_kernel_rsp: u64,
    /// Saved user-mode register context from syscall_entry.
    /// Needed because the shared kernel syscall stack gets overwritten by child threads.
    pub saved_ctx: SyscallContext,
    /// FPU/SSE state (512 bytes, 16-byte aligned) for fxsave/fxrstor
    /// Preserves XMM0-XMM15, MXCSR, x87 FPU registers across context switches
    pub fpu_state: FpuState,
    /// True if this process was created by fork() and should resume
    /// at the parent's saved RIP with RAX=0 (fork return value for child).
    pub is_forked: bool,
    /// Per-process heap break: current end of the heap region.
    /// Userspace calls sys_brk to grow/shrink this.
    /// Base address: 0x0000_3000_0000_0000 (PML4[96])
    pub heap_break: u64,
    /// Virtual Memory Areas for file-backed mmap (Jalon 68)
    /// Each VMA describes a region of virtual memory backed by a file.
    pub vmas: Vec<VirtualMemoryArea>,
    /// Jalon 96: CPU core affinity (0 = BSP/any, 1+ = pinned to AP core)
    /// 0xFF = no affinity (run on any core)
    pub cpu_affinity: u8,
    /// Jalon 105: ABI compatibility mode (AetherionOS native vs Linux)
    /// Linux ABI processes get Linux-specific uname, arch_prctl, etc.
    pub abi: crate::compat::linux_abi::Abi,
    /// Jalon 155: User-space pointer where wait4 should write the child exit status.
    /// Set by linux_wait4/sys_wait before blocking; used by sys_exit when resuming parent.
    pub wait_wstatus_ptr: u64,
    /// Jalon 129: PID of the process capturing this process's stdout.
    /// When set, sys_write(fd=1/2) stores output in IPC buffer and publishes
    /// INTENT_TOOL_STDOUT instead of printing to serial.
    pub captured_by_pid: Option<u64>,
    /// Jalon 128: Signal handler table — signum → handler address (up to 32 signals).
    /// 0 = SIG_DFL, 1 = SIG_IGN, other = userspace handler address.
    pub signal_handlers: [u64; 32],
    /// Jalon 128: Signal mask — bitfield of blocked signals.
    pub signal_mask: u64,
    /// Pending signals — bitfield of signals waiting to be delivered.
    pub pending_signals: u64,
    /// Jalon 127: Saved argv strings for this process (e.g., for /proc/self/cmdline).
    pub argv: Vec<String>,
    /// Jalon 131: FS segment base address (MSR 0xC0000100) for TLS support.
    /// Must be saved/restored on context switch for musl/glibc thread-local storage.
    pub fs_base: u64,
    /// Jalon 131: GS segment base address (MSR 0xC0000101).
    pub gs_base: u64,
    /// Jalon 131: Robust futex list head pointer (for musl thread cleanup).
    pub robust_list_head: u64,
    /// Per-process epoll interest list: tracks which FDs are monitored by epoll instances.
    pub epoll_interests: Vec<EpollInterest>,
    /// Active timerfd descriptors for PIT-based timer expiry.
    pub timer_fds: Vec<TimerFdState>,
}

/// Virtual Memory Area — describes a file-backed memory mapping
/// Used for zero-copy model loading via demand paging
#[derive(Debug, Clone)]
pub struct VirtualMemoryArea {
    /// Start virtual address (page-aligned)
    pub vaddr_start: u64,
    /// End virtual address (exclusive, page-aligned)
    pub vaddr_end: u64,
    /// File path in VFS (e.g., "/disk/models/mistral-7b.gguf")
    pub file_path: String,
    /// Offset into the file where this mapping starts
    pub file_offset: u64,
    /// Size of the mapping in bytes
    pub size: u64,
    /// Is this mapping writable? (false = read-only for model files)
    pub writable: bool,
    /// Is this mapping executable? (PROT_EXEC was set)
    pub executable: bool,
}

// ===== Epoll Infrastructure (Phase 1: Real Epoll) =====

/// EPOLLIN/EPOLLOUT/EPOLLERR constants (Linux ABI)
pub const EPOLLIN: u32  = 0x001;
pub const EPOLLOUT: u32 = 0x004;
pub const EPOLLERR: u32 = 0x008;
pub const EPOLLHUP: u32 = 0x010;
pub const EPOLLET: u32  = 1 << 31; // Edge-triggered

/// An epoll interest entry: tracks one FD being monitored by an epoll instance.
#[derive(Debug, Clone)]
pub struct EpollInterest {
    /// The epoll FD that owns this interest
    pub epfd: u32,
    /// The target FD being monitored
    pub fd: u32,
    /// Event mask (EPOLLIN, EPOLLOUT, etc.)
    pub events: u32,
    /// User data (returned in epoll_event.data)
    pub data: u64,
}

/// TimerFd state: tracks an active timerfd descriptor.
#[derive(Debug, Clone)]
pub struct TimerFdState {
    /// The FD number for this timer
    pub fd: u32,
    /// Interval in nanoseconds (0 = one-shot)
    pub interval_ns: u64,
    /// Next expiry time (TSC value)
    pub next_expiry_tsc: u64,
    /// Number of expirations since last read
    pub expirations: u64,
    /// Is this timer armed?
    pub armed: bool,
}

impl Process {
    /// Create a new process with full parameters.
    /// FPU state is initialized with proper MXCSR defaults for safe Ring 3 SSE/AVX usage.
    pub fn new(name: &str, role: AgentRole, ppid: u64, uid: u32, gid: u32) -> Self {
        let priority = match role {
            AgentRole::Matriarch => 20,
            AgentRole::SubMatriarch => 15,
            AgentRole::Worker => 5,
            AgentRole::KernelThread => 25,
        };
        let fpu = FpuState::zero();
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
            is_thread: false,
            saved_user_rip: 0,
            saved_user_rsp: 0,
            saved_kernel_rsp: 0,
            saved_ctx: SyscallContext::ZERO,
            fpu_state: fpu,
            is_forked: false,
            heap_break: 0x0000_3000_0000_0000, // Initial heap base (PML4[96])
            vmas: Vec::new(),
            cpu_affinity: 0xFF, // Default: no affinity (run on any core)
            abi: crate::compat::linux_abi::Abi::AetherionOS, // Default: native ABI
            wait_wstatus_ptr: 0,
            captured_by_pid: None,
            signal_handlers: [0u64; 32],
            signal_mask: 0,
            pending_signals: 0,
            argv: Vec::new(),
            fs_base: 0,
            gs_base: 0,
            robust_list_head: 0,
            epoll_interests: Vec::new(),
            timer_fds: Vec::new(),
        }
    }

    /// Create a new user-space process (Ring 3) with full setup
    pub fn new_userspace(name: &str, ppid: u64, entry: u64, stack: u64, pml4: u64) -> Self {
        let mut proc = Self::new(name, AgentRole::Worker, ppid, 1000, 1000);
        proc.entry_point = entry;
        proc.stack_pointer = stack;
        proc.pml4_phys = pml4;
        proc.saved_user_rip = entry;  // CRITICAL: Initialize so find_next_ready_userspace finds it
        proc.saved_user_rsp = stack;
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
            (Ready, Blocked) => true,   // Jalon 132: scheduler doesn't track Running; Ready processes can block in sys_wait
            (Running, Ready) => true,
            (Running, Blocked) => true,
            (Running, Terminated) => true,
            (Blocked, Ready) => true,
            (Blocked, Running) => true, // Jalon 132: wake from Blocked directly to Running
            (_, Terminated) => true,    // anything can be killed
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
