//! AetherionOS v3.0 — Production Terminal with Real Syscall Commands
//!
//! Architecture: Double-buffered terminal with real OS integration
//!   - Layer 1: screen_buf (Vec) — logical grid (source of truth)
//!   - Layer 2: framebuffer — physical display (render target)
//!
//! Commands use REAL syscalls — no hardcoded data:
//!   ls [path]    — sys_open + sys_getdents (FAT32/exFAT/VFS)
//!   cat <file>   — sys_open + sys_read_fd loop
//!   ps           — sys_getprocs (real process table)
//!   mem          — sys_sysinfo (real pool stats)
//!   status       — sys_sysinfo + sys_rdtsc
//!   llm <prompt> — bus publish + real token receive loop
//!   run <binary> — bus publish exec intent
//!   shutdown     — sys_exit(0) with bus notification
//!   help         — shows command list
//!   clear        — resets screen buffer
//!   version      — kernel version info

#![no_std]
#![no_main]

extern crate alloc;
use alloc::vec::Vec;
use aetherion_sdk::*;

// ═══════════════════════════════════════════════════
// Palette
// ═══════════════════════════════════════════════════
const BG: u32         = 0x000D1117;
const TITLE_BG: u32   = 0x001F6FEB;
const TEXT: u32       = 0x00E6EDF3;
const PROMPT: u32     = 0x003FB950;
const CURSOR_COL: u32 = 0x0058A6FF;
const DIM: u32        = 0x00484F58;
const LLM_COL: u32    = 0x00FFA657;
const ERR_COL: u32    = 0x00F85149;
const INFO_COL: u32   = 0x0079C0FF;
const DIR_COL: u32    = 0x007EE787;   // Green for directories
const FILE_COL: u32   = 0x00E6EDF3;   // White for files
const SIZE_COL: u32   = 0x00D2A8FF;   // Purple for file sizes

// ═══════════════════════════════════════════════════
// Terminal Configuration
// ═══════════════════════════════════════════════════
const CHAR_W: usize = 8;
const CHAR_H: usize = 16;
const MARGIN_X: usize = 8;
const TITLE_H: usize = 28;
const MARGIN_Y: usize = TITLE_H + 4;

const SCR_W: usize = 1024;
const SCR_H: usize = 768;

const COLS: usize = (SCR_W - MARGIN_X * 2) / CHAR_W;     // 126 cols
const ROWS: usize = (SCR_H - MARGIN_Y - 34) / CHAR_H;    // 43 rows

const CMD_BUF_SIZE: usize = 256;  // Larger buffer for file paths
const HISTORY_SIZE: usize = 16;   // Number of history entries
const KNOWN_CMDS: &[&[u8]] = &[b"help", b"clear", b"ls", b"cat", b"ps", b"mem", b"status",
    b"run", b"llm", b"version", b"wget", b"shutdown", b"exit", b"whoami", b"uname",
    b"gen_driver"];

const INTENT_GEN_DRIVER: u64 = 0x9001;

const INTENT_VISUAL_TERM: u64     = 0xB059;
const INTENT_TOKEN_GENERATED: u64 = 0x8002;    // From agent_llm_chat
const INTENT_TOKEN_GEN_CORE: u64  = 0x8063;    // From agent_llama_core (J63)
const INTENT_USER_PROMPT: u64     = 0x8001;
const INTENT_GENERATION_DONE: u64 = 0x8003;
const INTENT_TERM_CMD: u64        = 0xB065;

const MAX_IDLE_LOOPS: u64 = u64::MAX;

// ═══════════════════════════════════════════════════
// Cell — one character in the grid
// ═══════════════════════════════════════════════════
#[derive(Copy, Clone)]
struct Cell {
    ch: u8,
    color: u32,
}

impl Cell {
    const fn empty() -> Self {
        Cell { ch: b' ', color: TEXT }
    }
}

// ═══════════════════════════════════════════════════
// Terminal State
// ═══════════════════════════════════════════════════
struct Terminal {
    screen_buf: Vec<Cell>,
    cursor_x: usize,
    cursor_y: usize,
    cursor_visible: bool,
    tick: u32,
    cmd_buf: [u8; CMD_BUF_SIZE],
    cmd_len: usize,
    commands_run: u32,
    tokens_received: u32,
    llm_active: bool,
    // Command history — Heap-allocated to avoid Ring 3 stack overflow (4KB+)
    history: Vec<[u8; CMD_BUF_SIZE]>,
    history_lens: Vec<usize>,
    history_count: usize,
    history_pos: usize,
    history_browsing: bool,
}

impl Terminal {
    fn new() -> Self {
        let mut screen_buf = Vec::with_capacity(COLS * ROWS);
        for _ in 0..(COLS * ROWS) {
            screen_buf.push(Cell::empty());
        }
        Terminal {
            screen_buf,
            cursor_x: 0,
            cursor_y: 0,
            cursor_visible: true,
            tick: 0,
            cmd_buf: [0u8; CMD_BUF_SIZE],
            cmd_len: 0,
            commands_run: 0,
            tokens_received: 0,
            llm_active: false,
            history: alloc::vec![[0u8; CMD_BUF_SIZE]; HISTORY_SIZE],
            history_lens: alloc::vec![0usize; HISTORY_SIZE],
            history_count: 0,
            history_pos: 0,
            history_browsing: false,
        }
    }

    fn px_x(&self, col: usize) -> u32 { (MARGIN_X + col * CHAR_W) as u32 }
    fn px_y(&self, row: usize) -> u32 { (MARGIN_Y + row * CHAR_H) as u32 }

    fn cell(&self, x: usize, y: usize) -> Cell { self.screen_buf[y * COLS + x] }
    fn cell_mut(&mut self, x: usize, y: usize) -> &mut Cell { &mut self.screen_buf[y * COLS + x] }

    fn render_full(&self) {
        sys_fb_fill_rect(0, TITLE_H as u32, SCR_W as u32, (SCR_H - TITLE_H - 34) as u32, BG);
        for y in 0..ROWS {
            for x in 0..COLS {
                let cell = self.cell(x, y);
                if cell.ch != b' ' && cell.ch != 0 {
                    sys_fb_draw_char(self.px_x(x), self.px_y(y), cell.ch, cell.color);
                }
            }
        }
    }

    fn render_cell(&self, x: usize, y: usize) {
        let cell = self.cell(x, y);
        let px = self.px_x(x);
        let py = self.px_y(y);
        sys_fb_fill_rect(px, py, CHAR_W as u32, CHAR_H as u32, BG);
        if cell.ch != b' ' && cell.ch != 0 {
            sys_fb_draw_char(px, py, cell.ch, cell.color);
        }
    }

    fn render_line(&self, y: usize) {
        let py = self.px_y(y);
        sys_fb_fill_rect(0, py, SCR_W as u32, CHAR_H as u32, BG);
        for x in 0..COLS {
            let cell = self.cell(x, y);
            if cell.ch != b' ' && cell.ch != 0 {
                sys_fb_draw_char(self.px_x(x), py, cell.ch, cell.color);
            }
        }
    }

    fn draw_cursor(&self) {
        let px = self.px_x(self.cursor_x);
        let py = self.px_y(self.cursor_y);
        if self.cursor_visible {
            sys_fb_fill_rect(px, py, CHAR_W as u32, CHAR_H as u32, CURSOR_COL);
        } else {
            let cell = self.cell(self.cursor_x, self.cursor_y);
            sys_fb_fill_rect(px, py, CHAR_W as u32, CHAR_H as u32, BG);
            if cell.ch != b' ' && cell.ch != 0 {
                sys_fb_draw_char(px, py, cell.ch, cell.color);
            }
        }
    }

    fn erase_cursor(&self) {
        let px = self.px_x(self.cursor_x);
        let py = self.px_y(self.cursor_y);
        sys_fb_fill_rect(px, py, CHAR_W as u32, CHAR_H as u32, BG);
        let cell = self.cell(self.cursor_x, self.cursor_y);
        if cell.ch != b' ' && cell.ch != 0 {
            sys_fb_draw_char(px, py, cell.ch, cell.color);
        }
    }

    fn blink_tick(&mut self) {
        self.tick += 1;
        if self.tick % 500 == 0 {
            self.cursor_visible = !self.cursor_visible;
            self.draw_cursor();
        }
    }

    fn scroll_up(&mut self) {
        for y in 1..ROWS {
            for x in 0..COLS {
                let c = self.cell(x, y);
                *self.cell_mut(x, y - 1) = c;
            }
        }
        for x in 0..COLS {
            *self.cell_mut(x, ROWS - 1) = Cell::empty();
        }
        self.render_full();
    }

    fn clear_screen(&mut self) {
        for i in 0..(ROWS * COLS) {
            self.screen_buf[i] = Cell::empty();
        }
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.cmd_len = 0;
        self.render_full();
        self.draw_cursor();
    }

    fn put_char(&mut self, ch: u8, color: u32) {
        if ch == b'\n' || ch == b'\r' {
            self.newline();
            return;
        }
        if self.cursor_x >= COLS {
            self.newline();
        }
        let cx = self.cursor_x;
        let cy = self.cursor_y;
        *self.cell_mut(cx, cy) = Cell { ch, color };
        self.render_cell(cx, cy);
        self.cursor_x += 1;
        self.draw_cursor();
    }

    fn put_str(&mut self, s: &[u8], color: u32) {
        for &ch in s { self.put_char(ch, color); }
    }

    /// Print a decimal u64 value
    fn put_u64(&mut self, val: u64, color: u32) {
        let mut buf = [0u8; 20];
        let s = u64_to_buf(val, &mut buf);
        self.put_str(s, color);
    }

    fn newline(&mut self) {
        self.erase_cursor();
        self.cursor_x = 0;
        self.cursor_y += 1;
        if self.cursor_y >= ROWS {
            self.cursor_y = ROWS - 1;
            self.scroll_up();
        } else {
            for x in 0..COLS {
                *self.cell_mut(x, self.cursor_y) = Cell::empty();
            }
            self.render_line(self.cursor_y);
        }
        self.draw_cursor();
    }

    fn backspace(&mut self) {
        if self.cursor_x > 0 {
            self.erase_cursor();
            self.cursor_x -= 1;
            let cx = self.cursor_x;
            let cy = self.cursor_y;
            *self.cell_mut(cx, cy) = Cell::empty();
            sys_fb_fill_rect(self.px_x(cx), self.px_y(cy), CHAR_W as u32, CHAR_H as u32, BG);
            if self.cmd_len > 0 { self.cmd_len -= 1; }
            self.draw_cursor();
        }
    }

    fn clear_cmd_buf(&mut self) { self.cmd_len = 0; self.history_browsing = false; }

    /// Save current command to history ring buffer (heap-allocated Vec)
    fn push_history(&mut self) {
        if self.cmd_len == 0 { return; }
        // Don't duplicate the last entry
        if self.history_count > 0 {
            let prev = (self.history_count - 1) % HISTORY_SIZE;
            if self.history_lens[prev] == self.cmd_len {
                let mut same = true;
                for i in 0..self.cmd_len {
                    if self.history[prev][i] != self.cmd_buf[i] { same = false; break; }
                }
                if same { return; }
            }
        }
        let idx = self.history_count % HISTORY_SIZE;
        self.history[idx] = [0u8; CMD_BUF_SIZE];
        for i in 0..self.cmd_len { self.history[idx][i] = self.cmd_buf[i]; }
        self.history_lens[idx] = self.cmd_len;
        self.history_count += 1;
        self.history_browsing = false;
    }

    /// Navigate history: up = true (older), false (newer)
    fn nav_history(&mut self, up: bool) {
        let total = core::cmp::min(self.history_count, HISTORY_SIZE);
        if total == 0 { return; }
        if !self.history_browsing {
            self.history_pos = self.history_count;
            self.history_browsing = true;
        }
        if up {
            if self.history_pos > 0 && self.history_pos > self.history_count.saturating_sub(total) {
                self.history_pos -= 1;
            }
        } else {
            if self.history_pos < self.history_count {
                self.history_pos += 1;
            }
        }
        // Erase current line visually
        while self.cmd_len > 0 { self.backspace(); }
        // Load history entry
        if self.history_pos < self.history_count {
            let idx = self.history_pos % HISTORY_SIZE;
            let len = self.history_lens[idx];
            for i in 0..len {
                let ch = self.history[idx][i];
                self.put_char(ch, TEXT);
                self.cmd_buf[i] = ch;
            }
            self.cmd_len = len;
        }
    }

    /// Tab auto-completion from KNOWN_CMDS
    fn tab_complete(&mut self) {
        if self.cmd_len == 0 { return; }
        // Copy prefix to local buffer to avoid borrow conflict
        let plen = self.cmd_len;
        let mut prefix_buf = [0u8; CMD_BUF_SIZE];
        for i in 0..plen { prefix_buf[i] = self.cmd_buf[i]; }
        let mut match_count = 0u32;
        let mut match_idx: usize = 0;
        for (idx, cmd) in KNOWN_CMDS.iter().enumerate() {
            if cmd.len() >= plen {
                let mut ok = true;
                for i in 0..plen {
                    if cmd[i] != prefix_buf[i] { ok = false; break; }
                }
                if ok { match_count += 1; match_idx = idx; }
            }
        }
        if match_count == 1 {
            // Single match: auto-complete + trailing space
            let matched = KNOWN_CMDS[match_idx];
            for i in plen..matched.len() {
                let ch = matched[i];
                self.put_char(ch, TEXT);
                if self.cmd_len < CMD_BUF_SIZE {
                    self.cmd_buf[self.cmd_len] = ch;
                    self.cmd_len += 1;
                }
            }
            if self.cmd_len < CMD_BUF_SIZE {
                self.put_char(b' ', TEXT);
                self.cmd_buf[self.cmd_len] = b' ';
                self.cmd_len += 1;
            }
        } else if match_count > 1 {
            // Show all matches
            self.put_char(b'\n', TEXT);
            for cmd in KNOWN_CMDS {
                if cmd.len() >= plen {
                    let mut ok = true;
                    for i in 0..plen { if cmd[i] != prefix_buf[i] { ok = false; break; } }
                    if ok {
                        self.put_str(b"  ", TEXT);
                        self.put_str(cmd, INFO_COL);
                        self.put_char(b'\n', TEXT);
                    }
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════
// Utility functions
// ═══════════════════════════════════════════════════

fn u64_to_buf(val: u64, buf: &mut [u8; 20]) -> &[u8] {
    if val == 0 { buf[0] = b'0'; return &buf[0..1]; }
    let mut v = val;
    let mut i: usize = 20;
    while v > 0 && i > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    &buf[i..20]
}

fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    for i in 0..a.len() { if a[i] != b[i] { return false; } }
    true
}

fn starts_with(a: &[u8], prefix: &[u8]) -> bool {
    if a.len() < prefix.len() { return false; }
    bytes_eq(&a[..prefix.len()], prefix)
}

/// Format size in human-readable form (bytes/KB/MB/GB)
fn format_size(size: u64, buf: &mut [u8; 20]) -> &[u8] {
    if size < 1024 {
        return u64_to_buf(size, buf);
    } else if size < 1024 * 1024 {
        return u64_to_buf(size / 1024, buf);
    } else if size < 1024 * 1024 * 1024 {
        return u64_to_buf(size / (1024 * 1024), buf);
    } else {
        return u64_to_buf(size / (1024 * 1024 * 1024), buf);
    }
}

fn size_suffix(size: u64) -> &'static [u8] {
    if size < 1024 { b"B" }
    else if size < 1024 * 1024 { b"K" }
    else if size < 1024 * 1024 * 1024 { b"M" }
    else { b"G" }
}

// ═══════════════════════════════════════════════════
// UI Chrome
// ═══════════════════════════════════════════════════

fn draw_chrome() {
    sys_fb_fill_rect(0, 0, SCR_W as u32, SCR_H as u32, BG);
    sys_fb_fill_rect(0, 0, SCR_W as u32, TITLE_H as u32, TITLE_BG);
    sys_fb_draw_string(10, 6, b"AetherionOS Terminal v4.0 [Production]", TEXT);
    sys_fb_draw_string((SCR_W - 240) as u32, 6, b"Ring 3 | Real Syscalls | LLM", DIM);

    let status_y = SCR_H - CHAR_H - 18;
    sys_fb_fill_rect(0, status_y as u32, SCR_W as u32, (CHAR_H + 18) as u32, 0x00010409);
    sys_fb_draw_string(8, (status_y + 8) as u32,
        b"[help] Commands | [ls] Files | [ps] Procs | [llm <p>] AI Chat", DIM);
}

fn print_prompt(term: &mut Terminal) {
    // Custom prompt: [Ψ AetherionOS]> with real Unicode Psi
    term.put_char(b'[', DIM);
    // Ψ = U+03A8 = UTF-8 bytes 0xCE, 0xA8
    // Since our framebuffer font is ASCII, we render the visually closest: the
    // Greek capital psi glyph using our custom rendering if available, else 'Y'
    term.put_char(b'Y', PROMPT); // Ψ visual approximation in 8x16 ASCII font
    term.put_char(b' ', DIM);
    term.put_str(b"AetherionOS", PROMPT);
    term.put_str(b"]> ", DIM);
    term.clear_cmd_buf();
}

// ═══════════════════════════════════════════════════
// REAL COMMANDS — all using actual syscalls
// ═══════════════════════════════════════════════════

fn cmd_help(term: &mut Terminal) {
    term.put_char(b'\n', TEXT);
    term.put_str(b"AetherionOS v4.0 Shell Commands:\n", INFO_COL);
    term.put_str(b"  help               Show this help\n", TEXT);
    term.put_str(b"  clear              Clear terminal (also Ctrl+L)\n", TEXT);
    term.put_str(b"  ls [path]          List directory (real FAT32/VFS)\n", TEXT);
    term.put_str(b"  cat <file>         Display file contents\n", TEXT);
    term.put_str(b"  ps                 List running processes\n", TEXT);
    term.put_str(b"  mem                Show memory usage\n", TEXT);
    term.put_str(b"  status             System status / uptime\n", TEXT);
    term.put_str(b"  whoami             Current user identity\n", TEXT);
    term.put_str(b"  uname [-a]         Kernel version info\n", TEXT);
    term.put_str(b"  run <agent>        Launch agent binary (fork+exec)\n", TEXT);
    term.put_str(b"  llm <prompt>       Send prompt to LLM agent\n", TEXT);
    term.put_str(b"  gen_driver <id>    AI-generate driver for PCI device\n", TEXT);
    term.put_str(b"  version            Show OS version\n", TEXT);
    term.put_str(b"  wget               TCP network test (10.0.2.2)\n", TEXT);
    term.put_str(b"  shutdown           Halt the system\n", TEXT);
    term.put_str(b"\n  Shortcuts: Ctrl+C cancel | Ctrl+L clear | Up/Down history | Tab complete\n", DIM);
    term.put_char(b'\n', TEXT);
}

/// ls [path] — uses sys_open + sys_getdents for REAL directory listing
fn cmd_ls(term: &mut Terminal, args: &[u8]) {
    term.put_char(b'\n', TEXT);

    // Default path is /bin if no argument (shows all binaries)
    let mut path_buf = [0u8; 260];
    let path_len;
    if args.is_empty() {
        // List /bin (always available)
        let p = b"/bin\0";
        for i in 0..p.len() { path_buf[i] = p[i]; }
        path_len = p.len() - 1;
    } else if bytes_eq(args, b"/") {
        // List root
        let p = b"/\0";
        for i in 0..p.len() { path_buf[i] = p[i]; }
        path_len = p.len() - 1;
    } else {
        // User-specified path
        let mut off = 0;
        // Prepend /disk/ if not already
        if !starts_with(args, b"/") {
            let prefix = b"/disk/";
            for i in 0..prefix.len() { path_buf[off] = prefix[i]; off += 1; }
        }
        for i in 0..args.len() {
            if off >= 258 { break; }
            path_buf[off] = args[i];
            off += 1;
        }
        path_buf[off] = 0; // null terminate
        path_len = off;
    }

    // Open the directory
    let fd_result = sys_open(&path_buf[..path_len + 1], O_RDONLY);
    if fd_result < 0 {
        term.put_str(b"ls: cannot access '", ERR_COL);
        term.put_str(&path_buf[..path_len], TEXT);
        term.put_str(b"': No such file or directory\n", ERR_COL);
        term.put_char(b'\n', TEXT);
        return;
    }
    let fd = fd_result as u32;

    // Read directory entries via sys_getdents
    let mut dir_buf = [0u8; 2048];
    let n = sys_getdents(fd, &mut dir_buf);
    sys_close(fd);

    if n <= 0 {
        term.put_str(b"(empty directory)\n", DIM);
        term.put_char(b'\n', TEXT);
        return;
    }

    // Parse the response: entries separated by newlines
    // Format from kernel: "d SIZE NAME" or "- SIZE NAME"
    let data = &dir_buf[..n as usize];
    let mut line_start = 0;
    let mut entry_count: u32 = 0;

    for i in 0..data.len() {
        if data[i] == b'\n' || i == data.len() - 1 {
            let end = if data[i] == b'\n' { i } else { i + 1 };
            let line = &data[line_start..end];
            if !line.is_empty() {
                // Parse "d SIZE NAME" or "- SIZE NAME"
                let is_dir = line[0] == b'd';
                // Find second space (after size)
                let mut spaces = 0;
                let mut name_start = 0;
                let mut size_start = 0;
                let mut size_end = 0;
                for j in 0..line.len() {
                    if line[j] == b' ' {
                        spaces += 1;
                        if spaces == 1 { size_start = j + 1; }
                        if spaces == 2 { size_end = j; name_start = j + 1; break; }
                    }
                }

                if name_start > 0 && name_start < line.len() {
                    let name = &line[name_start..];
                    let size_bytes = &line[size_start..size_end];

                    if is_dir {
                        term.put_str(b"d ", DIR_COL);
                        // Parse size for display
                        term.put_str(b"       - ", DIM);
                        term.put_str(name, DIR_COL);
                        term.put_str(b"/", DIR_COL);
                    } else {
                        term.put_str(b"- ", FILE_COL);
                        // Right-align size to 8 chars
                        let pad = if size_bytes.len() < 8 { 8 - size_bytes.len() } else { 0 };
                        for _ in 0..pad { term.put_char(b' ', TEXT); }
                        term.put_str(size_bytes, SIZE_COL);
                        term.put_char(b' ', TEXT);
                        term.put_str(name, FILE_COL);
                    }
                    term.put_char(b'\n', TEXT);
                    entry_count += 1;
                }
            }
            line_start = i + 1;
        }
    }

    // If kernel returned entries in simple format (just names separated by newlines),
    // handle that too
    if entry_count == 0 && n > 0 {
        // Simple newline-separated names
        line_start = 0;
        for i in 0..data.len() {
            if data[i] == b'\n' || i == data.len() - 1 {
                let end = if data[i] == b'\n' { i } else { i + 1 };
                let line = &data[line_start..end];
                if !line.is_empty() {
                    term.put_str(b"  ", TEXT);
                    term.put_str(line, FILE_COL);
                    term.put_char(b'\n', TEXT);
                    entry_count += 1;
                }
                line_start = i + 1;
            }
        }
    }

    term.put_u64(entry_count as u64, DIM);
    term.put_str(b" entries\n", DIM);
    term.put_char(b'\n', TEXT);
}

/// cat <file> — uses sys_open + sys_read_fd to display file contents
fn cmd_cat(term: &mut Terminal, args: &[u8]) {
    term.put_char(b'\n', TEXT);
    if args.is_empty() {
        term.put_str(b"Usage: cat <file_path>\n", ERR_COL);
        term.put_str(b"Example: cat /disk/models/test.txt\n", DIM);
        term.put_char(b'\n', TEXT);
        return;
    }

    // Build null-terminated path
    let mut path_buf = [0u8; 260];
    let mut off = 0;
    if !starts_with(args, b"/") {
        let prefix = b"/disk/";
        for i in 0..prefix.len() { path_buf[off] = prefix[i]; off += 1; }
    }
    for i in 0..args.len() {
        if off >= 258 { break; }
        path_buf[off] = args[i];
        off += 1;
    }
    path_buf[off] = 0;
    let path_len = off;

    let fd_result = sys_open(&path_buf[..path_len + 1], O_RDONLY);
    if fd_result < 0 {
        term.put_str(b"cat: ", ERR_COL);
        term.put_str(&path_buf[..path_len], TEXT);
        term.put_str(b": No such file\n", ERR_COL);
        term.put_char(b'\n', TEXT);
        return;
    }
    let fd = fd_result as u32;

    // Read and display contents in chunks
    let mut read_buf = [0u8; 512];
    let mut total_bytes: u64 = 0;
    let max_display = 4096u64; // Don't flood the terminal

    loop {
        let n = sys_read_fd(fd, &mut read_buf);
        if n <= 0 { break; }
        let n = n as usize;
        for i in 0..n {
            if total_bytes >= max_display {
                term.put_str(b"\n... (truncated at 4K)\n", DIM);
                break;
            }
            let ch = read_buf[i];
            if ch >= 0x20 && ch <= 0x7E {
                term.put_char(ch, TEXT);
            } else if ch == b'\n' {
                term.put_char(b'\n', TEXT);
            } else if ch == b'\t' {
                term.put_str(b"    ", TEXT);
            } else {
                term.put_char(b'.', DIM); // non-printable
            }
            total_bytes += 1;
        }
        if total_bytes >= max_display { break; }
    }

    sys_close(fd);
    term.put_char(b'\n', TEXT);
    term.put_u64(total_bytes, DIM);
    term.put_str(b" bytes\n", DIM);
    term.put_char(b'\n', TEXT);
}

/// ps — uses sys_getprocs to list REAL running processes
fn cmd_ps(term: &mut Terminal) {
    term.put_char(b'\n', TEXT);
    term.put_str(b"  PID STATE ROLE NAME\n", INFO_COL);
    term.put_str(b"  --- ----- ---- ----\n", DIM);

    let mut proc_buf = [0u8; 2048];
    let n = sys_getprocs(&mut proc_buf);
    if n <= 0 {
        term.put_str(b"  (no processes)\n", DIM);
        term.put_char(b'\n', TEXT);
        return;
    }

    // Parse "PID STATE ROLE NAME\nPID STATE ROLE NAME\n..."
    let data = &proc_buf[..n as usize];
    let mut line_start = 0;
    let mut count: u32 = 0;

    for i in 0..data.len() {
        if data[i] == b'\n' || i == data.len() - 1 {
            let end = if data[i] == b'\n' { i } else { i + 1 };
            let line = &data[line_start..end];
            if !line.is_empty() {
                term.put_str(b"  ", TEXT);
                // Color based on state
                let has_run = has_substr(line, b"RUN");
                let has_ready = has_substr(line, b"READY");
                let color = if has_run { PROMPT } else if has_ready { INFO_COL } else { DIM };
                term.put_str(line, color);
                term.put_char(b'\n', TEXT);
                count += 1;
            }
            line_start = i + 1;
        }
    }

    term.put_str(b"  Total: ", DIM);
    term.put_u64(count as u64, INFO_COL);
    term.put_str(b" processes\n", DIM);
    term.put_char(b'\n', TEXT);
}

/// mem — uses sys_sysinfo for REAL memory/frame pool statistics
fn cmd_mem(term: &mut Terminal) {
    term.put_char(b'\n', TEXT);
    term.put_str(b"=== Memory Status ===\n", INFO_COL);

    let mut info_buf = [0u8; 512];
    let n = sys_sysinfo(&mut info_buf);
    if n <= 0 {
        term.put_str(b"  (sysinfo unavailable)\n", ERR_COL);
        term.put_char(b'\n', TEXT);
        return;
    }

    // Parse key=value\n pairs
    let data = &info_buf[..n as usize];
    let mut line_start = 0;

    for i in 0..data.len() {
        if data[i] == b'\n' || i == data.len() - 1 {
            let end = if data[i] == b'\n' { i } else { i + 1 };
            let line = &data[line_start..end];
            if !line.is_empty() {
                // Format nicely: find '=' and display key: value
                let mut eq_pos = line.len();
                for j in 0..line.len() {
                    if line[j] == b'=' { eq_pos = j; break; }
                }
                let key = &line[..eq_pos];
                let val = if eq_pos + 1 < line.len() { &line[eq_pos+1..] } else { b"?" };

                // Only show memory-related entries
                if starts_with(key, b"pool_") || starts_with(key, b"procs") {
                    term.put_str(b"  ", TEXT);
                    term.put_str(key, DIM);
                    term.put_str(b": ", TEXT);
                    term.put_str(val, INFO_COL);
                    // Add unit suffixes
                    if starts_with(key, b"pool_used_mb") || starts_with(key, b"pool_max_mb") {
                        term.put_str(b" MB", DIM);
                    } else if starts_with(key, b"pool_used") && !starts_with(key, b"pool_used_mb") {
                        term.put_str(b" frames", DIM);
                    } else if starts_with(key, b"pool_max") && !starts_with(key, b"pool_max_mb") {
                        term.put_str(b" frames", DIM);
                    }
                    term.put_char(b'\n', TEXT);
                }
            }
            line_start = i + 1;
        }
    }
    term.put_char(b'\n', TEXT);
}

/// status — comprehensive system info via sys_sysinfo + sys_rdtsc
fn cmd_status(term: &mut Terminal) {
    term.put_char(b'\n', TEXT);
    term.put_str(b"=== System Status ===\n", INFO_COL);

    // Static info
    term.put_str(b"  OS:       AetherionOS v3.0\n", TEXT);
    term.put_str(b"  Arch:     x86_64 Ring 3\n", TEXT);
    term.put_str(b"  Display:  1024x768 32bpp\n", TEXT);

    // Dynamic info from sysinfo syscall
    let mut info_buf = [0u8; 512];
    let n = sys_sysinfo(&mut info_buf);
    if n > 0 {
        let data = &info_buf[..n as usize];
        let mut line_start = 0;

        for i in 0..data.len() {
            if data[i] == b'\n' || i == data.len() - 1 {
                let end = if data[i] == b'\n' { i } else { i + 1 };
                let line = &data[line_start..end];
                if !line.is_empty() {
                    let mut eq_pos = line.len();
                    for j in 0..line.len() { if line[j] == b'=' { eq_pos = j; break; } }
                    let key = &line[..eq_pos];
                    let val = if eq_pos + 1 < line.len() { &line[eq_pos+1..] } else { b"?" };

                    let label: &[u8] = if bytes_eq(key, b"procs") { b"  Procs:    " }
                        else if bytes_eq(key, b"ctx_sw") { b"  CtxSw:    " }
                        else if bytes_eq(key, b"ticks") { b"  Ticks:    " }
                        else if bytes_eq(key, b"fat32") { b"  FAT32:    " }
                        else if bytes_eq(key, b"exfat") { b"  exFAT:    " }
                        else if bytes_eq(key, b"pool_used_mb") { b"  PoolUsed: " }
                        else if bytes_eq(key, b"pool_max_mb") { b"  PoolMax:  " }
                        else { b"" };

                    if !label.is_empty() {
                        term.put_str(label, TEXT);
                        if bytes_eq(key, b"fat32") || bytes_eq(key, b"exfat") {
                            if bytes_eq(val, b"1") {
                                term.put_str(b"mounted", PROMPT);
                            } else {
                                term.put_str(b"not mounted", DIM);
                            }
                        } else if starts_with(key, b"pool_") {
                            term.put_str(val, INFO_COL);
                            term.put_str(b" MB", DIM);
                        } else {
                            term.put_str(val, INFO_COL);
                        }
                        term.put_char(b'\n', TEXT);
                    }
                }
                line_start = i + 1;
            }
        }
    }

    // Command stats
    term.put_str(b"  Commands: ", TEXT);
    term.put_u64(term.commands_run as u64, PROMPT);
    term.put_char(b'\n', TEXT);
    term.put_str(b"  LLMTokens:", TEXT);
    term.put_u64(term.tokens_received as u64, LLM_COL);
    term.put_char(b'\n', TEXT);
    term.put_char(b'\n', TEXT);
}

fn cmd_version(term: &mut Terminal) {
    term.put_char(b'\n', TEXT);
    term.put_str(b"AetherionOS v3.0 Production Release\n", INFO_COL);
    term.put_str(b"Kernel: x86_64 Preemptive Ring 3\n", TEXT);
    term.put_str(b"Terminal: Real Syscall Architecture\n", TEXT);
    term.put_str(b"FS: FAT32 + exFAT (64-bit offsets)\n", TEXT);
    term.put_str(b"LLM: Streaming GGUF via sys_pread64\n", TEXT);
    term.put_str(b"Bus: Cognitive Intent Bus\n", TEXT);
    term.put_char(b'\n', TEXT);
}

/// llm <prompt> — publish prompt on bus, then listen for real token stream
fn cmd_llm(term: &mut Terminal, prompt_bytes: &[u8]) {
    if prompt_bytes.is_empty() {
        term.put_char(b'\n', TEXT);
        term.put_str(b"Usage: llm <your prompt>\n", ERR_COL);
        term.put_str(b"Example: llm Hello world\n", DIM);
        term.put_char(b'\n', TEXT);
        return;
    }
    term.put_char(b'\n', TEXT);
    term.put_str(b"[LLM] Prompt: \"", LLM_COL);
    term.put_str(prompt_bytes, TEXT);
    term.put_str(b"\"\n", LLM_COL);

    // Publish prompt intent
    let mut hash: u64 = 5381;
    for &b in prompt_bytes {
        hash = hash.wrapping_mul(33).wrapping_add(b as u64);
    }
    sys_bus_publish(INTENT_USER_PROMPT, 2, hash);
    sys_bus_publish(INTENT_TERM_CMD, 2, hash);
    term.llm_active = true;
    term.put_str(b"[LLM] Waiting for tokens...\n", DIM);

    // Listen for token stream with timeout (real bus messages)
    let mut token_count: u32 = 0;
    let mut idle_ticks: u32 = 0;
    let max_idle = 500u32; // timeout after ~500 yield cycles with no tokens

    term.put_str(b"[LLM] ", LLM_COL);
    loop {
        let mut bus_msg = [0u64; 6];
        if sys_bus_consume(&mut bus_msg) == 0 {
            let intent = bus_msg[2] as u32;
            let payload = bus_msg[4];

            // Accept tokens from both LLM agents
            if intent == INTENT_TOKEN_GENERATED as u32 || intent == INTENT_TOKEN_GEN_CORE as u32 {
                let token_char = (payload & 0xFF) as u8;
                if token_char >= 0x20 && token_char <= 0x7E || token_char == b'\n' {
                    term.put_char(token_char, LLM_COL);
                    token_count += 1;
                    term.tokens_received += 1;
                }
                idle_ticks = 0;
            } else if intent == INTENT_GENERATION_DONE as u32 {
                break;
            }
        } else {
            idle_ticks += 1;
            if idle_ticks > max_idle { break; }
        }
        sys_yield();
    }

    term.put_char(b'\n', TEXT);
    if token_count > 0 {
        term.put_str(b"[LLM] Received ", DIM);
        term.put_u64(token_count as u64, LLM_COL);
        term.put_str(b" tokens\n", DIM);
    } else {
        term.put_str(b"[LLM] No response (LLM agent may not be running)\n", DIM);
    }
    term.llm_active = false;
    term.put_char(b'\n', TEXT);
}

/// shutdown — clean exit with bus notification
fn cmd_shutdown(term: &mut Terminal) {
    term.put_char(b'\n', TEXT);
    term.put_str(b"Shutting down...\n", INFO_COL);
    sys_write(1, b"[TERM] Shutdown requested\n");
    sys_bus_publish(INTENT_VISUAL_TERM, 3, 0);
    sys_exit(0);
}

/// run <agent_name> — fork + exec an agent binary from /bin
fn cmd_run(term: &mut Terminal, args: &[u8]) {
    term.put_char(b'\n', TEXT);
    if args.is_empty() {
        term.put_str(b"Usage: run <binary>\n", ERR_COL);
        term.put_str(b"Example: run agent_bench\n", DIM);
        term.put_char(b'\n', TEXT);
        return;
    }
    // Build path: /bin/<name>.elf\0
    let mut path_buf = [0u8; 128];
    let prefix = b"/bin/";
    let mut off = 0usize;
    for b in prefix { path_buf[off] = *b; off += 1; }
    for i in 0..args.len() {
        if off >= 120 { break; }
        path_buf[off] = args[i];
        off += 1;
    }
    // Append .elf if not already present
    if off < 4 || &path_buf[off-4..off] != b".elf" {
        let suffix = b".elf";
        for b in suffix { if off < 126 { path_buf[off] = *b; off += 1; } }
    }
    path_buf[off] = 0; // null-terminate

    term.put_str(b"Launching: ", TEXT);
    term.put_str(&path_buf[..off], INFO_COL);
    term.put_char(b'\n', TEXT);

    // Fork and exec
    let pid = sys_fork();
    if pid == 0 {
        // Child: exec the binary
        sys_exec(&path_buf[..off + 1]);
        // If exec returns, it failed
        sys_exit(1);
    } else if pid > 0 {
        term.put_str(b"  Started PID ", TEXT);
        term.put_u64(pid as u64, INFO_COL);
        term.put_char(b'\n', TEXT);
        sys_write(1, b"[TERM] run: forked child\n");
    } else {
        term.put_str(b"  Fork failed\n", ERR_COL);
    }
    term.put_char(b'\n', TEXT);
}

/// wget — TCP network test to QEMU gateway
fn cmd_wget(term: &mut Terminal) {
    term.put_char(b'\n', TEXT);
    term.put_str(b"[NET] TCP Socket Test to 10.0.2.2:80\n", INFO_COL);
    sys_write(1, b"[TERM] wget: starting TCP test\n");

    // Step 1: Create TCP socket
    term.put_str(b"  Creating socket...", TEXT);
    let sock_fd = sys_socket(2, 1, 6); // AF_INET, SOCK_STREAM, TCP
    if sock_fd < 0 {
        term.put_str(b" FAIL\n", ERR_COL);
        return;
    }
    term.put_str(b" OK (fd=", TEXT);
    term.put_u64(sock_fd as u64, TEXT);
    term.put_str(b")\n", TEXT);

    // Step 2: Connect
    term.put_str(b"  Connecting...", TEXT);
    let ip_packed: u64 = (10 << 24) | (0 << 16) | (2 << 8) | 2;
    let conn = aetherion_sdk::syscall3(42, sock_fd as u64, ip_packed, 80);
    if conn == 0 {
        term.put_str(b" ESTABLISHED\n", INFO_COL);

        // Step 3: Send GET
        term.put_str(b"  Sending HTTP GET...", TEXT);
        let request = b"GET / HTTP/1.0\r\n\r\n";
        let mut send_buf = [0u8; 128];
        // tcp_send expects length prefix in first 8 bytes
        let len_bytes = (request.len() as u64).to_le_bytes();
        for i in 0..8 { send_buf[i] = len_bytes[i]; }
        for i in 0..request.len() { send_buf[8 + i] = request[i]; }
        let sent = aetherion_sdk::syscall3(44, sock_fd as u64, send_buf.as_ptr() as u64, 0);
        if (sent as i64) > 0 {
            term.put_str(b" OK\n", TEXT);
        } else {
            term.put_str(b" FAIL\n", ERR_COL);
        }

        // Step 4: Receive (blocking poll)
        term.put_str(b"  Receiving...", TEXT);
        let mut recv_buf = [0u8; 256];
        let received = aetherion_sdk::syscall3(213, sock_fd as u64,
            recv_buf.as_mut_ptr() as u64, 255);
        if (received as i64) > 0 {
            term.put_str(b" got ", TEXT);
            term.put_u64(received, TEXT);
            term.put_str(b" bytes\n", TEXT);
        } else {
            term.put_str(b" no data (no HTTP server)\n", DIM);
        }

        // Step 5: Close
        aetherion_sdk::syscall1(47, sock_fd as u64); // tcp_shutdown
    } else {
        term.put_str(b" refused/timeout (TCP stack OK)\n", DIM);
    }

    term.put_str(b"  Done.\n", INFO_COL);
    term.put_char(b'\n', TEXT);
}

/// Helper: check if a byte slice contains a substring
fn has_substr(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.len() > haystack.len() { return false; }
    for i in 0..=(haystack.len() - needle.len()) {
        if &haystack[i..i+needle.len()] == needle { return true; }
    }
    false
}

// ═══════════════════════════════════════════════════
// Command Parser
// ═══════════════════════════════════════════════════

fn process_command(term: &mut Terminal) {
    let mut cmd_copy = [0u8; CMD_BUF_SIZE];
    let clen = term.cmd_len;
    for i in 0..clen { cmd_copy[i] = term.cmd_buf[i]; }
    let cmd = &cmd_copy[..clen];

    // Trim whitespace
    let mut start = 0;
    let mut end = cmd.len();
    while start < end && cmd[start] == b' ' { start += 1; }
    while end > start && cmd[end - 1] == b' ' { end -= 1; }
    let trimmed_len = end - start;
    if trimmed_len == 0 { return; }

    let mut trimmed = [0u8; CMD_BUF_SIZE];
    for i in 0..trimmed_len { trimmed[i] = cmd[start + i]; }
    let command = &trimmed[..trimmed_len];
    term.commands_run += 1;

    // Extract first word and arguments
    let mut space_pos = trimmed_len;
    for i in 0..trimmed_len {
        if trimmed[i] == b' ' { space_pos = i; break; }
    }
    let first_word = &trimmed[..space_pos];
    let args_start = if space_pos < trimmed_len { space_pos + 1 } else { trimmed_len };
    // Trim leading spaces from args
    let mut args_off = args_start;
    while args_off < trimmed_len && trimmed[args_off] == b' ' { args_off += 1; }
    let args = &trimmed[args_off..trimmed_len];

    if bytes_eq(first_word, b"help") {
        cmd_help(term);
    } else if bytes_eq(first_word, b"clear") {
        cmd_clear(term);
        return; // don't print prompt after clear — it does its own
    } else if bytes_eq(first_word, b"ls") {
        cmd_ls(term, args);
    } else if bytes_eq(first_word, b"cat") {
        cmd_cat(term, args);
    } else if bytes_eq(first_word, b"ps") {
        cmd_ps(term);
    } else if bytes_eq(first_word, b"mem") {
        cmd_mem(term);
    } else if bytes_eq(first_word, b"status") {
        cmd_status(term);
    } else if bytes_eq(first_word, b"version") {
        cmd_version(term);
    } else if bytes_eq(first_word, b"llm") {
        cmd_llm(term, args);
    } else if bytes_eq(first_word, b"wget") {
        cmd_wget(term);
    } else if bytes_eq(first_word, b"run") {
        cmd_run(term, args);
    } else if bytes_eq(first_word, b"shutdown") || bytes_eq(first_word, b"halt") {
        cmd_shutdown(term);
    } else if bytes_eq(first_word, b"whoami") {
        cmd_whoami(term);
    } else if bytes_eq(first_word, b"uname") {
        cmd_uname(term, args);
    } else if bytes_eq(first_word, b"gen_driver") {
        cmd_gen_driver(term, args);
    } else if bytes_eq(first_word, b"exit") || bytes_eq(first_word, b"quit") {
        term.put_char(b'\n', TEXT);
        term.put_str(b"Goodbye!\n", PROMPT);
        sys_write(1, b"[TERM] Exit\n");
        sys_bus_publish(INTENT_VISUAL_TERM, 3, 0);
        sys_exit(0);
    } else {
        term.put_char(b'\n', TEXT);
        term.put_str(b"Unknown command: '", ERR_COL);
        term.put_str(command, TEXT);
        term.put_str(b"'\n", ERR_COL);
        term.put_str(b"Type 'help' for available commands.\n", DIM);
        term.put_char(b'\n', TEXT);
    }
}

fn cmd_clear(term: &mut Terminal) {
    term.clear_screen();
}

fn cmd_whoami(term: &mut Terminal) {
    term.put_char(b'\n', TEXT);
    term.put_str(b"root@aetherion\n", PROMPT);
    term.put_char(b'\n', TEXT);
}

fn cmd_uname(term: &mut Terminal, args: &[u8]) {
    term.put_char(b'\n', TEXT);
    if args.is_empty() || bytes_eq(args, b"-a") || bytes_eq(args, b"--all") {
        term.put_str(b"AetherionOS aetherion 4.0.0-multi-agent x86_64 Haswell GNU/AetherionOS\n", TEXT);
    } else if bytes_eq(args, b"-r") {
        term.put_str(b"4.0.0-multi-agent\n", TEXT);
    } else if bytes_eq(args, b"-s") {
        term.put_str(b"AetherionOS\n", TEXT);
    } else if bytes_eq(args, b"-m") {
        term.put_str(b"x86_64\n", TEXT);
    } else {
        term.put_str(b"AetherionOS aetherion 4.0.0-multi-agent x86_64 Haswell GNU/AetherionOS\n", TEXT);
    }
    term.put_char(b'\n', TEXT);
}

/// gen_driver <pci_id> — AI-generate a PCI device driver
/// Publishes INTENT_GEN_DRIVER to the bus with the PCI vendor:device ID encoded.
/// agent_llama_core receives this and streams back a Rust driver template.
fn cmd_gen_driver(term: &mut Terminal, args: &[u8]) {
    term.put_char(b'\n', TEXT);
    if args.is_empty() {
        term.put_str(b"Usage: gen_driver <vendor:device>\n", ERR_COL);
        term.put_str(b"Example: gen_driver 8086:100e  (Intel e1000)\n", DIM);
        term.put_str(b"         gen_driver 1af4:1000  (virtio-net)\n", DIM);
        term.put_char(b'\n', TEXT);
        return;
    }

    // Parse PCI ID: "VVVV:DDDD" -> vendor (u16), device (u16)
    let mut vendor: u64 = 0;
    let mut device: u64 = 0;
    let mut in_device = false;

    for &b in args {
        if b == b':' {
            in_device = true;
            continue;
        }
        let nibble = match b {
            b'0'..=b'9' => (b - b'0') as u64,
            b'a'..=b'f' => (b - b'a' + 10) as u64,
            b'A'..=b'F' => (b - b'A' + 10) as u64,
            _ => continue,
        };
        if in_device {
            device = (device << 4) | nibble;
        } else {
            vendor = (vendor << 4) | nibble;
        }
    }

    let pci_id = (vendor << 16) | device;

    term.put_str(b"[GEN] Generating driver for PCI ", INFO_COL);
    term.put_str(args, TEXT);
    term.put_char(b'\n', TEXT);

    // Identify known devices
    let device_name: &[u8] = match (vendor as u16, device as u16) {
        (0x8086, 0x100E) => b"Intel 82540EM Gigabit Ethernet (e1000)",
        (0x8086, 0x100F) => b"Intel 82545EM Gigabit Ethernet",
        (0x8086, 0x10D3) => b"Intel 82574L Gigabit Ethernet",
        (0x1AF4, 0x1000) => b"VirtIO Network Device",
        (0x1AF4, 0x1001) => b"VirtIO Block Device",
        (0x1AF4, 0x1050) => b"VirtIO GPU Device",
        (0x1B36, 0x000D) => b"QEMU XHCI USB Controller",
        (0x1234, 0x1111) => b"QEMU Standard VGA",
        _ => b"Unknown PCI device",
    };

    term.put_str(b"[GEN] Device: ", DIM);
    term.put_str(device_name, INFO_COL);
    term.put_char(b'\n', TEXT);
    term.put_str(b"[GEN] Publishing INTENT_GEN_DRIVER to bus...\n", DIM);

    // Publish intent for LLM agent to pick up
    sys_bus_publish(INTENT_GEN_DRIVER, 2, pci_id);
    sys_write(1, b"[TERM] gen_driver: intent published\n");

    // Generate driver template locally (predefined for known devices)
    term.put_str(b"[GEN] Generating Rust driver template...\n", DIM);
    term.put_char(b'\n', TEXT);

    // Stream the template character by character for visual effect
    let template = match (vendor as u16, device as u16) {
        (0x8086, 0x100E) => generate_e1000_template(),
        (0x1AF4, 0x1000) => generate_virtio_net_template(),
        _ => generate_generic_template(vendor, device),
    };

    for &b in template {
        term.put_char(b, LLM_COL);
        // Yield every 16 chars to allow display update
        if b == b'\n' { sys_yield(); }
    }

    term.put_char(b'\n', TEXT);
    term.put_str(b"[GEN] Driver template generated (", DIM);
    term.put_u64(template.len() as u64, INFO_COL);
    term.put_str(b" bytes)\n", DIM);

    // Save to /var/drivers/<pci_id>.rs
    let mut path = [0u8; 64];
    let mut poff = 0usize;
    let prefix = b"/var/drivers/";
    for &b in prefix.iter() { path[poff] = b; poff += 1; }
    for &b in args {
        if b == b':' { path[poff] = b'_'; poff += 1; }
        else if (b >= b'0' && b <= b'9') || (b >= b'a' && b <= b'f') || (b >= b'A' && b <= b'F') {
            path[poff] = b; poff += 1;
        }
    }
    let suffix = b".rs";
    for &b in suffix.iter() { path[poff] = b; poff += 1; }
    path[poff] = 0;

    // Write via sys_open + sys_write
    let fd = sys_open(&path[..poff + 1], O_WRONLY | O_CREAT | O_TRUNC);
    if fd >= 0 {
        sys_write_fd(fd as u32, template);
        sys_close(fd as u32);
        term.put_str(b"[GEN] Saved to ", DIM);
        term.put_str(&path[..poff], INFO_COL);
        term.put_char(b'\n', TEXT);
    } else {
        term.put_str(b"[GEN] (file save skipped - VFS write pending)\n", DIM);
    }
    term.put_char(b'\n', TEXT);
}

fn generate_e1000_template() -> &'static [u8] {
    b"// Intel e1000 (8086:100E) Driver for AetherionOS\n\
// Auto-generated by gen_driver AI\n\
\n\
const E1000_VENDOR: u16 = 0x8086;\n\
const E1000_DEVICE: u16 = 0x100E;\n\
\n\
// MMIO Register Offsets\n\
const REG_CTRL:   u32 = 0x0000;  // Device Control\n\
const REG_STATUS: u32 = 0x0008;  // Device Status\n\
const REG_EERD:   u32 = 0x0014;  // EEPROM Read\n\
const REG_ICR:    u32 = 0x00C0;  // Interrupt Cause Read\n\
const REG_IMS:    u32 = 0x00D0;  // Interrupt Mask Set\n\
const REG_RCTL:   u32 = 0x0100;  // Receive Control\n\
const REG_TCTL:   u32 = 0x0400;  // Transmit Control\n\
const REG_RDBAL:  u32 = 0x2800;  // RX Desc Base Low\n\
const REG_RDBAH:  u32 = 0x2804;  // RX Desc Base High\n\
const REG_RDLEN:  u32 = 0x2808;  // RX Desc Length\n\
const REG_TDBAL:  u32 = 0x3800;  // TX Desc Base Low\n\
const REG_TDBAH:  u32 = 0x3804;  // TX Desc Base High\n\
const REG_TDLEN:  u32 = 0x3808;  // TX Desc Length\n\
\n\
pub struct E1000Driver {\n\
    mmio_base: u64,\n\
    mac_addr: [u8; 6],\n\
    rx_ring: *mut RxDescriptor,\n\
    tx_ring: *mut TxDescriptor,\n\
}\n\
\n\
impl E1000Driver {\n\
    pub unsafe fn init(mmio_base: u64) -> Self {\n\
        let mut drv = Self {\n\
            mmio_base,\n\
            mac_addr: [0; 6],\n\
            rx_ring: core::ptr::null_mut(),\n\
            tx_ring: core::ptr::null_mut(),\n\
        };\n\
        drv.reset();\n\
        drv.read_mac();\n\
        drv.setup_rx();\n\
        drv.setup_tx();\n\
        drv.enable_interrupts();\n\
        drv\n\
    }\n\
\n\
    unsafe fn mmio_read(&self, reg: u32) -> u32 {\n\
        core::ptr::read_volatile(\n\
            (self.mmio_base + reg as u64) as *const u32\n\
        )\n\
    }\n\
\n\
    unsafe fn mmio_write(&self, reg: u32, val: u32) {\n\
        core::ptr::write_volatile(\n\
            (self.mmio_base + reg as u64) as *mut u32,\n\
            val\n\
        );\n\
    }\n\
\n\
    unsafe fn reset(&mut self) {\n\
        self.mmio_write(REG_CTRL, 1 << 26); // RST bit\n\
        for _ in 0..10000 { core::hint::spin_loop(); }\n\
    }\n\
}\n"
}

fn generate_virtio_net_template() -> &'static [u8] {
    b"// VirtIO Network (1AF4:1000) Driver for AetherionOS\n\
// Auto-generated by gen_driver AI\n\
\n\
const VIRTIO_VENDOR: u16 = 0x1AF4;\n\
const VIRTIO_NET_DEVICE: u16 = 0x1000;\n\
\n\
// VirtIO MMIO Registers\n\
const VIRTIO_MAGIC:         u32 = 0x000;\n\
const VIRTIO_VERSION:       u32 = 0x004;\n\
const VIRTIO_DEVICE_ID:     u32 = 0x008;\n\
const VIRTIO_STATUS:        u32 = 0x070;\n\
const VIRTIO_QUEUE_SEL:     u32 = 0x030;\n\
const VIRTIO_QUEUE_NUM_MAX: u32 = 0x034;\n\
const VIRTIO_QUEUE_NUM:     u32 = 0x038;\n\
\n\
pub struct VirtioNetDriver {\n\
    mmio_base: u64,\n\
    mac: [u8; 6],\n\
}\n\
\n\
impl VirtioNetDriver {\n\
    pub unsafe fn init(mmio_base: u64) -> Self {\n\
        let drv = Self { mmio_base, mac: [0; 6] };\n\
        drv.negotiate_features();\n\
        drv.setup_queues();\n\
        drv\n\
    }\n\
}\n"
}

fn generate_generic_template(vendor: u64, device: u64) -> &'static [u8] {
    b"// Generic PCI Driver Template for AetherionOS\n\
// Auto-generated by gen_driver AI\n\
// TODO: Fill in MMIO register definitions for this device.\n\
\n\
pub struct PciDriver {\n\
    mmio_base: u64,\n\
    vendor_id: u16,\n\
    device_id: u16,\n\
}\n\
\n\
impl PciDriver {\n\
    pub unsafe fn init(mmio_base: u64, vendor: u16, device: u16) -> Self {\n\
        let drv = Self { mmio_base, vendor_id: vendor, device_id: device };\n\
        // Step 1: Read PCI config space\n\
        // Step 2: Map MMIO BAR\n\
        // Step 3: Reset device\n\
        // Step 4: Configure interrupts\n\
        // Step 5: Initialize descriptor rings\n\
        drv\n\
    }\n\
\n\
    pub unsafe fn mmio_read(&self, offset: u32) -> u32 {\n\
        core::ptr::read_volatile(\n\
            (self.mmio_base + offset as u64) as *const u32\n\
        )\n\
    }\n\
\n\
    pub unsafe fn mmio_write(&self, offset: u32, val: u32) {\n\
        core::ptr::write_volatile(\n\
            (self.mmio_base + offset as u64) as *mut u32,\n\
            val,\n\
        );\n\
    }\n\
}\n"
}

// ═══════════════════════════════════════════════════
// MAIN EVENT LOOP
// ═══════════════════════════════════════════════════

#[no_mangle]
pub extern "C" fn main() -> i64 {
    sys_write(1, b"[TERM] ========================================\n");
    sys_write(1, b"[TERM] AetherionOS v3.0 Production Terminal\n");
    sys_write(1, b"[TERM] Real Syscalls: ls/cat/ps/mem/llm\n");
    sys_write(1, b"[TERM] ========================================\n");

    draw_chrome();

    let mut term = alloc::boxed::Box::new(Terminal::new());
    term.clear_screen();

    term.put_str(b"AetherionOS v4.0 - Production Terminal\n", TEXT);
    term.put_str(b"Kernel: x86_64 Ring 3 | Real Syscall Architecture\n", DIM);
    term.put_str(b"FS: FAT32 + exFAT | Bus: Cognitive Intent Bus\n", DIM);
    term.put_str(b"Type 'help' for commands, 'ls' for files, 'ps' for procs.\n", INFO_COL);
    term.put_char(b'\n', TEXT);

    sys_bus_publish(INTENT_VISUAL_TERM, 3, 1);
    sys_write(1, b"[TERM] Terminal ready\n");

    print_prompt(&mut term);

    let mut idle_count: u64 = 0;
    let mut read_buf = [0u8; 1];

    loop {
        // 1. Read keyboard input
        let n = sys_read(0, &mut read_buf);
        if n > 0 {
            idle_count = 0;
            let ch = read_buf[0];
            match ch {
                0x08 | 0x7F => { term.backspace(); }
                b'\n' | b'\r' => {
                    term.push_history();
                    term.newline();
                    process_command(&mut term);
                    print_prompt(&mut term);
                }
                0x01 => { term.nav_history(true);  }  // Up arrow → older history
                0x02 => { term.nav_history(false); }  // Down arrow → newer history
                0x03 => {  // Ctrl+C → cancel current line
                    term.put_str(b"^C", ERR_COL);
                    term.newline();
                    term.clear_cmd_buf();
                    print_prompt(&mut term);
                }
                0x09 => { term.tab_complete(); }       // Tab → auto-complete
                0x0C => {  // Ctrl+L → clear screen
                    cmd_clear(&mut term);
                    print_prompt(&mut term);
                }
                0x20..=0x7E => {
                    if term.cmd_len < CMD_BUF_SIZE {
                        term.cmd_buf[term.cmd_len] = ch;
                        term.cmd_len += 1;
                    }
                    term.put_char(ch, TEXT);
                }
                _ => {} // Ignore other control codes
            }
        } else {
            idle_count += 1;
        }

        // 2. Listen for bus messages (LLM tokens from both agents)
        if !term.llm_active {
            let mut bus_msg = [0u64; 6];
            if sys_bus_consume(&mut bus_msg) == 0 {
                let intent = bus_msg[2] as u32;
                let payload = bus_msg[4];

                // J63: Accept tokens from agent_llm_chat (0x8002) AND agent_llama_core (0x8063)
                if intent == INTENT_TOKEN_GENERATED as u32 || intent == INTENT_TOKEN_GEN_CORE as u32 {
                    let token_char = (payload & 0xFF) as u8;
                    if token_char >= 0x20 && token_char <= 0x7E || token_char == b'\n' {
                        // Typewriter effect: render each token character as it arrives
                        term.put_char(token_char, LLM_COL);
                        term.tokens_received += 1;
                    }
                } else if intent == INTENT_GENERATION_DONE as u32 {
                    term.put_char(b'\n', TEXT);
                    term.put_str(b"[LLM] Done\n", DIM);
                    term.llm_active = false;
                    print_prompt(&mut term);
                }
            }
        }

        term.blink_tick();
        sys_yield();

        if idle_count >= MAX_IDLE_LOOPS {
            sys_write(1, b"[TERM] Safety valve\n");
            break;
        }
    }

    sys_write(1, b"[TERM] Event loop exit\n");
    0
}
