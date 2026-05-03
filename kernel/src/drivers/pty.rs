// kernel/src/drivers/pty.rs — Full PTY/TTY Subsystem for AetherionOS
//
// Implements POSIX pseudo-terminal pairs (master/slave) for:
//   - BusyBox sh, bash, and all interactive programs
//   - Line discipline (canonical mode, echo, signal generation)
//   - /dev/ptmx (multiplexer) and /dev/pts/N (slave endpoints)
//   - termios configuration (TCGETS/TCSETS/TIOCGWINSZ/etc.)
//   - Signal generation (Ctrl+C → SIGINT, Ctrl+Z → SIGTSTP)
//
// Architecture:
//   master_write() → line discipline → slave read buffer
//   slave_write()  → output processing → master read buffer
//
// SAFETY: All mutable statics are behind spin::Mutex, single-CPU assumption.

use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;
use lazy_static::lazy_static;

// ═══════════════════════════════════════════════════════════
// Constants
// ═══════════════════════════════════════════════════════════

pub const NCCS: usize = 32;
pub const PTY_BUF_SIZE: usize = 4096;

// c_lflag bits
pub const ISIG:    u32 = 0x0001;
pub const ICANON:  u32 = 0x0002;
pub const ECHO:    u32 = 0x0008;
pub const ECHOE:   u32 = 0x0010;
pub const ECHOK:   u32 = 0x0020;
pub const ECHONL:  u32 = 0x0040;
pub const NOFLSH:  u32 = 0x0080;
pub const TOSTOP:  u32 = 0x0100;
pub const IEXTEN:  u32 = 0x8000;

// c_iflag bits
pub const IGNBRK:  u32 = 0x0001;
pub const BRKINT:  u32 = 0x0002;
pub const IGNPAR:  u32 = 0x0004;
pub const INLCR:   u32 = 0x0040;
pub const IGNCR:   u32 = 0x0080;
pub const ICRNL:   u32 = 0x0100;
pub const IXON:    u32 = 0x0400;
pub const IXANY:   u32 = 0x0800;

// c_oflag bits
pub const OPOST:   u32 = 0x0001;
pub const ONLCR:   u32 = 0x0004;

// Special character indices in c_cc
pub const VINTR:   usize = 0;
pub const VQUIT:   usize = 1;
pub const VERASE:  usize = 2;
pub const VKILL:   usize = 3;
pub const VEOF:    usize = 4;
pub const VTIME:   usize = 5;
pub const VMIN:    usize = 6;
pub const VSTART:  usize = 8;
pub const VSTOP:   usize = 9;
pub const VSUSP:   usize = 10;
pub const VLNEXT:  usize = 15;
pub const VWERASE: usize = 14;

// ioctl commands
pub const TCGETS:      u64 = 0x5401;
pub const TCSETS:      u64 = 0x5402;
pub const TCSETSW:     u64 = 0x5403;
pub const TCSETSF:     u64 = 0x5404;
pub const TIOCGPGRP:   u64 = 0x540F;
pub const TIOCSPGRP:   u64 = 0x5410;
pub const TIOCGWINSZ:  u64 = 0x5413;
pub const TIOCSWINSZ:  u64 = 0x5414;
pub const TIOCSCTTY:   u64 = 0x540E;
pub const TIOCNOTTY:   u64 = 0x5422;
pub const TIOCGPTN:    u64 = 0x80045430;
pub const TIOCSPTLCK:  u64 = 0x40045431;
pub const TIOCGPTLCK:  u64 = 0x80045439;
pub const TIOCGPTPEER: u64 = 0x5441;
pub const TIOCSIG:     u64 = 0x40045436;
pub const FIONREAD:    u64 = 0x541B;

// ═══════════════════════════════════════════════════════════
// Data Structures
// ═══════════════════════════════════════════════════════════

/// POSIX termios structure — matches Linux kernel layout exactly
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Termios {
    pub c_iflag:  u32,
    pub c_oflag:  u32,
    pub c_cflag:  u32,
    pub c_lflag:  u32,
    pub c_line:   u8,
    pub c_cc:     [u8; NCCS],
    _pad:         [u8; 3], // align to 4 bytes after c_line+c_cc
    pub c_ispeed: u32,
    pub c_ospeed: u32,
}

/// Terminal window size — returned by TIOCGWINSZ
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct WinSize {
    pub ws_row:    u16,
    pub ws_col:    u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// Simple ring buffer for PTY data transfer
pub struct RingBuf {
    data: [u8; PTY_BUF_SIZE],
    head: usize,
    tail: usize,
    count: usize,
}

impl RingBuf {
    pub const fn new() -> Self {
        RingBuf { data: [0; PTY_BUF_SIZE], head: 0, tail: 0, count: 0 }
    }

    pub fn len(&self) -> usize { self.count }
    pub fn is_empty(&self) -> bool { self.count == 0 }
    pub fn space(&self) -> usize { PTY_BUF_SIZE - self.count }

    pub fn push(&mut self, byte: u8) -> bool {
        if self.count >= PTY_BUF_SIZE { return false; }
        self.data[self.tail] = byte;
        self.tail = (self.tail + 1) % PTY_BUF_SIZE;
        self.count += 1;
        true
    }

    pub fn pop(&mut self) -> Option<u8> {
        if self.count == 0 { return None; }
        let byte = self.data[self.head];
        self.head = (self.head + 1) % PTY_BUF_SIZE;
        self.count -= 1;
        Some(byte)
    }

    pub fn read_into(&mut self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.count);
        for i in 0..n {
            buf[i] = self.data[self.head];
            self.head = (self.head + 1) % PTY_BUF_SIZE;
        }
        self.count -= n;
        n
    }

    pub fn write_from(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(self.space());
        for i in 0..n {
            self.data[self.tail] = data[i];
            self.tail = (self.tail + 1) % PTY_BUF_SIZE;
        }
        self.count += n;
        n
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.count = 0;
    }
}

/// A pseudo-terminal pair (master + slave)
pub struct PtyPair {
    pub id: u32,
    /// Data written to master appears as input for slave's read
    pub master_to_slave: RingBuf,
    /// Data written to slave appears as output for master's read
    pub slave_to_master: RingBuf,
    /// Terminal settings
    pub termios: Termios,
    /// Window dimensions
    pub winsize: WinSize,
    /// PID that controls this terminal
    pub session_leader: u64,
    /// Foreground process group ID
    pub foreground_pgid: u64,
    /// Is the slave side locked (grantpt/unlockpt)
    pub slave_locked: bool,
    /// Line editing buffer for canonical mode
    pub line_buf: VecDeque<u8>,
    /// Is the master side open
    pub master_open: bool,
    /// Is the slave side open
    pub slave_open: bool,
}

// ═══════════════════════════════════════════════════════════
// Global State
// ═══════════════════════════════════════════════════════════

lazy_static! {
    static ref PTY_TABLE: Mutex<BTreeMap<u32, PtyPair>> = Mutex::new(BTreeMap::new());
}
static NEXT_PTY_ID: AtomicU32 = AtomicU32::new(0);

// ═══════════════════════════════════════════════════════════
// Default Termios (matches Linux defaults)
// ═══════════════════════════════════════════════════════════

pub fn default_termios() -> Termios {
    let mut c_cc = [0u8; NCCS];
    c_cc[VINTR]   = 3;    // Ctrl+C
    c_cc[VQUIT]   = 28;   // Ctrl+backslash
    c_cc[VERASE]  = 127;  // DEL
    c_cc[VKILL]   = 21;   // Ctrl+U
    c_cc[VEOF]    = 4;    // Ctrl+D
    c_cc[VTIME]   = 0;
    c_cc[VMIN]    = 1;
    c_cc[VSTART]  = 17;   // Ctrl+Q (XON)
    c_cc[VSTOP]   = 19;   // Ctrl+S (XOFF)
    c_cc[VSUSP]   = 26;   // Ctrl+Z
    c_cc[VLNEXT]  = 22;   // Ctrl+V
    c_cc[VWERASE] = 23;   // Ctrl+W

    Termios {
        c_iflag: ICRNL | IXON,
        c_oflag: OPOST | ONLCR,
        c_cflag: 0x00BF, // CS8 | CREAD | HUPCL
        c_lflag: ISIG | ICANON | ECHO | ECHOE | ECHOK | IEXTEN,
        c_line: 0,
        c_cc,
        _pad: [0; 3],
        c_ispeed: 38400,
        c_ospeed: 38400,
    }
}

// ═══════════════════════════════════════════════════════════
// PTY Creation
// ═══════════════════════════════════════════════════════════

/// Allocate a new PTY pair. Returns the pty_id.
/// The caller is responsible for assigning FDs.
pub fn pty_alloc() -> u32 {
    let id = NEXT_PTY_ID.fetch_add(1, Ordering::Relaxed);
    let pair = PtyPair {
        id,
        master_to_slave: RingBuf::new(),
        slave_to_master: RingBuf::new(),
        termios: default_termios(),
        winsize: WinSize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 },
        session_leader: 0,
        foreground_pgid: 0,
        slave_locked: true,
        line_buf: VecDeque::new(),
        master_open: true,
        slave_open: false,
    };
    PTY_TABLE.lock().insert(id, pair);
    id
}

/// Unlock the slave side (equivalent to unlockpt)
pub fn pty_unlock(id: u32) -> bool {
    if let Some(pty) = PTY_TABLE.lock().get_mut(&id) {
        pty.slave_locked = false;
        true
    } else {
        false
    }
}

/// Open the slave side
pub fn pty_open_slave(id: u32) -> bool {
    if let Some(pty) = PTY_TABLE.lock().get_mut(&id) {
        if pty.slave_locked { return false; }
        pty.slave_open = true;
        true
    } else {
        false
    }
}

// ═══════════════════════════════════════════════════════════
// Master side I/O
// ═══════════════════════════════════════════════════════════

/// Write data to the master side (user input → goes to slave's stdin).
/// Processes line discipline if canonical mode is active.
pub fn pty_master_write(id: u32, data: &[u8]) -> usize {
    let mut table = PTY_TABLE.lock();
    let pty = match table.get_mut(&id) {
        Some(p) => p,
        None => return 0,
    };

    let mut written = 0usize;

    for &byte in data {
        let mut ch = byte;

        // Input processing (c_iflag)
        if pty.termios.c_iflag & ICRNL != 0 && ch == b'\r' {
            ch = b'\n';
        }
        if pty.termios.c_iflag & IGNCR != 0 && ch == b'\r' {
            continue;
        }
        if pty.termios.c_iflag & INLCR != 0 && ch == b'\n' {
            ch = b'\r';
        }

        // Signal generation (c_lflag & ISIG)
        if pty.termios.c_lflag & ISIG != 0 {
            if ch == pty.termios.c_cc[VINTR] {
                // Ctrl+C → SIGINT to foreground pgrp
                if pty.foreground_pgid != 0 {
                    // Signal delivery will be done by the caller
                    // We just flag it by writing a special marker
                    signal_foreground(pty.foreground_pgid, 2); // SIGINT
                }
                if pty.termios.c_lflag & NOFLSH == 0 {
                    pty.line_buf.clear();
                    pty.master_to_slave.clear();
                }
                // Echo ^C
                if pty.termios.c_lflag & ECHO != 0 {
                    pty.slave_to_master.push(b'^');
                    pty.slave_to_master.push(b'C');
                    pty.slave_to_master.push(b'\n');
                }
                written += 1;
                continue;
            }
            if ch == pty.termios.c_cc[VQUIT] {
                // Ctrl+\ → SIGQUIT
                if pty.foreground_pgid != 0 {
                    signal_foreground(pty.foreground_pgid, 3); // SIGQUIT
                }
                written += 1;
                continue;
            }
            if ch == pty.termios.c_cc[VSUSP] {
                // Ctrl+Z → SIGTSTP
                if pty.foreground_pgid != 0 {
                    signal_foreground(pty.foreground_pgid, 20); // SIGTSTP
                }
                written += 1;
                continue;
            }
        }

        if pty.termios.c_lflag & ICANON != 0 {
            // Canonical mode: buffer until newline or EOF
            if ch == pty.termios.c_cc[VERASE] {
                // Backspace: remove last char from line buffer
                if let Some(_) = pty.line_buf.pop_back() {
                    if pty.termios.c_lflag & (ECHO | ECHOE) != 0 {
                        // Echo: backspace + space + backspace
                        pty.slave_to_master.push(8);   // BS
                        pty.slave_to_master.push(b' ');
                        pty.slave_to_master.push(8);   // BS
                    }
                }
            } else if ch == pty.termios.c_cc[VKILL] {
                // Kill line: clear line buffer
                let line_len = pty.line_buf.len();
                pty.line_buf.clear();
                if pty.termios.c_lflag & ECHO != 0 {
                    for _ in 0..line_len {
                        pty.slave_to_master.push(8);
                        pty.slave_to_master.push(b' ');
                        pty.slave_to_master.push(8);
                    }
                }
            } else if ch == pty.termios.c_cc[VEOF] {
                // EOF (Ctrl+D): flush line buffer (may be empty → read returns 0)
                while let Some(b) = pty.line_buf.pop_front() {
                    pty.master_to_slave.push(b);
                }
                // Don't push EOF itself
            } else if ch == b'\n' || ch == b'\r' {
                // Newline: push line buffer + newline to slave
                while let Some(b) = pty.line_buf.pop_front() {
                    pty.master_to_slave.push(b);
                }
                pty.master_to_slave.push(b'\n');
                // Echo newline
                if pty.termios.c_lflag & (ECHO | ECHONL) != 0 {
                    pty.slave_to_master.push(b'\r');
                    pty.slave_to_master.push(b'\n');
                }
            } else {
                // Regular character: add to line buffer
                if pty.line_buf.len() < PTY_BUF_SIZE {
                    pty.line_buf.push_back(ch);
                }
                if pty.termios.c_lflag & ECHO != 0 {
                    // Echo the character back to master
                    if ch < 32 && ch != b'\t' {
                        // Control character: echo as ^X
                        pty.slave_to_master.push(b'^');
                        pty.slave_to_master.push(ch + 64);
                    } else {
                        pty.slave_to_master.push(ch);
                    }
                }
            }
        } else {
            // Raw mode: push directly to slave
            pty.master_to_slave.push(ch);
            if pty.termios.c_lflag & ECHO != 0 {
                pty.slave_to_master.push(ch);
            }
        }

        written += 1;
    }

    written
}

/// Read data from the master side (slave's stdout → user display)
pub fn pty_master_read(id: u32, buf: &mut [u8]) -> usize {
    let mut table = PTY_TABLE.lock();
    let pty = match table.get_mut(&id) {
        Some(p) => p,
        None => return 0,
    };
    pty.slave_to_master.read_into(buf)
}

/// Check how many bytes are available to read on master side
pub fn pty_master_readable(id: u32) -> usize {
    let table = PTY_TABLE.lock();
    match table.get(&id) {
        Some(p) => p.slave_to_master.len(),
        None => 0,
    }
}

// ═══════════════════════════════════════════════════════════
// Slave side I/O
// ═══════════════════════════════════════════════════════════

/// Read data from the slave side (process reads its stdin)
pub fn pty_slave_read(id: u32, buf: &mut [u8]) -> usize {
    let mut table = PTY_TABLE.lock();
    let pty = match table.get_mut(&id) {
        Some(p) => p,
        None => return 0,
    };
    pty.master_to_slave.read_into(buf)
}

/// Write data to the slave side (process writes to stdout)
/// Applies output processing (c_oflag)
pub fn pty_slave_write(id: u32, data: &[u8]) -> usize {
    let mut table = PTY_TABLE.lock();
    let pty = match table.get_mut(&id) {
        Some(p) => p,
        None => return 0,
    };

    let mut written = 0;
    for &byte in data {
        if pty.termios.c_oflag & OPOST != 0 {
            if byte == b'\n' && pty.termios.c_oflag & ONLCR != 0 {
                // Translate NL to CR+NL
                pty.slave_to_master.push(b'\r');
                pty.slave_to_master.push(b'\n');
                written += 1;
                continue;
            }
        }
        if pty.slave_to_master.push(byte) {
            written += 1;
        } else {
            break; // buffer full
        }
    }
    written
}

/// Check how many bytes are available to read on slave side
pub fn pty_slave_readable(id: u32) -> usize {
    let table = PTY_TABLE.lock();
    match table.get(&id) {
        Some(p) => p.master_to_slave.len(),
        None => 0,
    }
}

// ═══════════════════════════════════════════════════════════
// ioctl handling
// ═══════════════════════════════════════════════════════════

/// Handle ioctl on a PTY fd.
/// Returns 0 on success, negative errno on error.
pub fn pty_ioctl(id: u32, cmd: u64, arg: u64) -> i64 {
    let mut table = PTY_TABLE.lock();
    let pty = match table.get_mut(&id) {
        Some(p) => p,
        None => return -25, // ENOTTY
    };

    match cmd {
        TCGETS => {
            // Copy termios to userspace
            let bytes: &[u8] = unsafe {
                core::slice::from_raw_parts(
                    &pty.termios as *const Termios as *const u8,
                    core::mem::size_of::<Termios>(),
                )
            };
            if arg != 0 {
                // Caller must handle copy_to_user
                unsafe {
                    let dst = arg as *mut u8;
                    let n = core::mem::size_of::<Termios>().min(60); // safety
                    core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, n);
                }
            }
            0
        }
        TCSETS | TCSETSW | TCSETSF => {
            if arg != 0 {
                let src = arg as *const u8;
                let dst = &mut pty.termios as *mut Termios as *mut u8;
                unsafe {
                    let n = core::mem::size_of::<Termios>().min(60);
                    core::ptr::copy_nonoverlapping(src, dst, n);
                }
                if cmd == TCSETSF {
                    pty.master_to_slave.clear();
                    pty.line_buf.clear();
                }
            }
            0
        }
        TIOCGWINSZ => {
            if arg != 0 {
                let bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        &pty.winsize as *const WinSize as *const u8,
                        core::mem::size_of::<WinSize>(),
                    )
                };
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        arg as *mut u8,
                        core::mem::size_of::<WinSize>(),
                    );
                }
            }
            0
        }
        TIOCSWINSZ => {
            if arg != 0 {
                let src = arg as *const u8;
                let dst = &mut pty.winsize as *mut WinSize as *mut u8;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src, dst,
                        core::mem::size_of::<WinSize>(),
                    );
                }
                // TODO: Send SIGWINCH to foreground pgrp
            }
            0
        }
        TIOCGPGRP => {
            if arg != 0 {
                let val = pty.foreground_pgid as u32;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        &val as *const u32 as *const u8,
                        arg as *mut u8,
                        4,
                    );
                }
            }
            0
        }
        TIOCSPGRP => {
            if arg != 0 {
                let mut pgid_bytes = [0u8; 4];
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        arg as *const u8,
                        pgid_bytes.as_mut_ptr(),
                        4,
                    );
                }
                pty.foreground_pgid = u32::from_ne_bytes(pgid_bytes) as u64;
            }
            0
        }
        TIOCSCTTY => {
            // Set controlling terminal
            pty.session_leader = current_pid_pty();
            0
        }
        TIOCNOTTY => {
            pty.session_leader = 0;
            0
        }
        TIOCGPTN => {
            // Get PTY number
            if arg != 0 {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        &pty.id as *const u32 as *const u8,
                        arg as *mut u8,
                        4,
                    );
                }
            }
            0
        }
        TIOCSPTLCK => {
            if arg != 0 {
                let mut val = [0u8; 4];
                unsafe {
                    core::ptr::copy_nonoverlapping(arg as *const u8, val.as_mut_ptr(), 4);
                }
                pty.slave_locked = u32::from_ne_bytes(val) != 0;
            }
            0
        }
        TIOCGPTLCK => {
            if arg != 0 {
                let val = if pty.slave_locked { 1u32 } else { 0u32 };
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        &val as *const u32 as *const u8,
                        arg as *mut u8,
                        4,
                    );
                }
            }
            0
        }
        FIONREAD => {
            if arg != 0 {
                let avail = pty.master_to_slave.len() as u32;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        &avail as *const u32 as *const u8,
                        arg as *mut u8,
                        4,
                    );
                }
            }
            0
        }
        _ => {
            // Unknown ioctl: return 0 instead of ENOTTY for compatibility
            0
        }
    }
}

/// Get termios for a PTY (used by syscall TCGETS)
pub fn pty_get_termios(id: u32) -> Option<Termios> {
    PTY_TABLE.lock().get(&id).map(|p| p.termios)
}

/// Set the foreground process group
pub fn pty_set_fg_pgid(id: u32, pgid: u64) {
    if let Some(pty) = PTY_TABLE.lock().get_mut(&id) {
        pty.foreground_pgid = pgid;
    }
}

/// Close one side of the PTY
pub fn pty_close(id: u32, is_master: bool) {
    let mut table = PTY_TABLE.lock();
    if let Some(pty) = table.get_mut(&id) {
        if is_master {
            pty.master_open = false;
        } else {
            pty.slave_open = false;
        }
        // If both sides are closed, remove the PTY
        if !pty.master_open && !pty.slave_open {
            table.remove(&id);
        }
    }
}

/// Check if a PTY exists
pub fn pty_exists(id: u32) -> bool {
    PTY_TABLE.lock().contains_key(&id)
}

/// Get the number of allocated PTYs
pub fn pty_count() -> usize {
    PTY_TABLE.lock().len()
}

// ═══════════════════════════════════════════════════════════
// Signal helpers (callbacks to process module)
// ═══════════════════════════════════════════════════════════

/// Send a signal to the foreground process group.
/// Ctrl+C → SIGINT (2), Ctrl+\ → SIGQUIT (3), Ctrl+Z → SIGTSTP (20)
fn signal_foreground(pgid: u64, signum: u64) {
    crate::serial_println!("[PTY] Signal {} → pgrp {}", signum, pgid);
    crate::process::send_signal_to_pgrp(pgid, signum);
}

/// Get current PID (stub — real impl in process module)
fn current_pid_pty() -> u64 {
    // Avoid circular dependency; caller should provide PID
    0
}
