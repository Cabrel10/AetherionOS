// Aetherion OS - System Call Interface
// Phase 2.2: Syscall implementation

#[derive(Debug, Clone, Copy)]
#[repr(usize)]
pub enum Syscall {
    Write = 1,
    Read = 2,
    Exit = 3,
    Open = 4,
    Close = 5,
}

pub fn syscall_handler(syscall_num: usize, arg1: usize, arg2: usize, arg3: usize) -> isize {
    match syscall_num {
        1 => sys_write(arg1, arg2, arg3),
        2 => sys_read(arg1, arg2, arg3),
        3 => sys_exit(arg1),
        _ => -1, // ENOSYS
    }
}

fn sys_write(fd: usize, buf: usize, count: usize) -> isize {
    if fd == 1 { // stdout
        // Write to VGA/serial (simplified)
        count as isize
    } else {
        -1
    }
}

fn sys_read(_fd: usize, _buf: usize, _count: usize) -> isize {
    -1 // Not implemented yet
}

fn sys_exit(code: usize) -> isize {
    panic!("Process exited with code: {}", code);
}

pub fn init() {
    // Setup syscall MSR (Model Specific Register)
}
