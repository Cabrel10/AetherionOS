// Aetherion OS - Process Management
// Phase 2.3: User mode processes

use core::sync::atomic::{AtomicUsize, Ordering};

/// Process ID type
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pid(usize);

impl Pid {
    pub fn new(id: usize) -> Self {
        Pid(id)
    }
    
    pub fn as_usize(&self) -> usize {
        self.0
    }
}

/// Process state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Ready,
    Running,
    Blocked,
    Terminated,
}

/// Process privilege level
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivilegeLevel {
    Kernel = 0,  // Ring 0
    User = 3,    // Ring 3
}

/// Process Control Block (PCB)
pub struct Process {
    pid: Pid,
    state: ProcessState,
    privilege: PrivilegeLevel,
    
    // CPU state
    rsp: usize,  // Stack pointer
    rip: usize,  // Instruction pointer
    rflags: u64, // CPU flags
    
    // Memory
    page_table: usize,  // CR3 value
    
    // Scheduling
    priority: u8,
    time_slice: usize,
}

impl Process {
    pub fn new_kernel(pid: Pid, entry: usize, stack: usize) -> Self {
        Process {
            pid,
            state: ProcessState::Ready,
            privilege: PrivilegeLevel::Kernel,
            rsp: stack,
            rip: entry,
            rflags: 0x200,  // Interrupts enabled
            page_table: 0,
            priority: 0,
            time_slice: 100,
        }
    }
    
    pub fn new_user(pid: Pid, entry: usize, stack: usize, page_table: usize) -> Self {
        Process {
            pid,
            state: ProcessState::Ready,
            privilege: PrivilegeLevel::User,
            rsp: stack,
            rip: entry,
            rflags: 0x200,
            page_table,
            priority: 10,
            time_slice: 50,
        }
    }
    
    pub fn pid(&self) -> Pid {
        self.pid
    }
    
    pub fn state(&self) -> ProcessState {
        self.state
    }
    
    pub fn set_state(&mut self, state: ProcessState) {
        self.state = state;
    }
}

/// Process table (simple array for now)
static mut PROCESS_TABLE: [Option<Process>; 256] = [const { None }; 256];
static NEXT_PID: AtomicUsize = AtomicUsize::new(1);

/// Allocate a new process ID
pub fn allocate_pid() -> Pid {
    Pid::new(NEXT_PID.fetch_add(1, Ordering::SeqCst))
}

/// Create a new process
pub fn create_process(entry: usize, stack: usize, is_user: bool) -> Result<Pid, &'static str> {
    let pid = allocate_pid();
    
    let process = if is_user {
        Process::new_user(pid, entry, stack, 0)
    } else {
        Process::new_kernel(pid, entry, stack)
    };
    
    unsafe {
        let index = pid.as_usize() % 256;
        if PROCESS_TABLE[index].is_some() {
            return Err("Process slot occupied");
        }
        PROCESS_TABLE[index] = Some(process);
    }
    
    Ok(pid)
}

/// Simple round-robin scheduler
pub fn schedule() -> Option<Pid> {
    unsafe {
        for proc in PROCESS_TABLE.iter() {
            if let Some(p) = proc {
                if p.state() == ProcessState::Ready {
                    return Some(p.pid());
                }
            }
        }
    }
    None
}

pub fn init() {
    // Process management initialized
}
