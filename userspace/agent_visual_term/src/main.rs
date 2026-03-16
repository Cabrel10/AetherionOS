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

const INTENT_VISUAL_TERM: u64     = 0xB059;
const INTENT_TOKEN_GENERATED: u64 = 0x8002;
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
        if self.tick % 30 == 0 {
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

    fn clear_cmd_buf(&mut self) { self.cmd_len = 0; }
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
    sys_fb_draw_string(10, 6, b"AetherionOS Terminal v3.0 [Production]", TEXT);
    sys_fb_draw_string((SCR_W - 240) as u32, 6, b"Ring 3 | Real Syscalls | LLM", DIM);

    let status_y = SCR_H - CHAR_H - 18;
    sys_fb_fill_rect(0, status_y as u32, SCR_W as u32, (CHAR_H + 18) as u32, 0x00010409);
    sys_fb_draw_string(8, (status_y + 8) as u32,
        b"[help] Commands | [ls] Files | [ps] Procs | [llm <p>] AI Chat", DIM);
}

fn print_prompt(term: &mut Terminal) {
    term.put_str(b"aetherion", PROMPT);
    term.put_str(b":~$ ", TEXT);
    term.clear_cmd_buf();
}

// ═══════════════════════════════════════════════════
// REAL COMMANDS — all using actual syscalls
// ═══════════════════════════════════════════════════

fn cmd_help(term: &mut Terminal) {
    term.put_char(b'\n', TEXT);
    term.put_str(b"AetherionOS v3.0 Shell Commands:\n", INFO_COL);
    term.put_str(b"  help            Show this help\n", TEXT);
    term.put_str(b"  clear           Clear terminal\n", TEXT);
    term.put_str(b"  ls [path]       List directory (real FAT32/exFAT)\n", TEXT);
    term.put_str(b"  cat <file>      Display file contents\n", TEXT);
    term.put_str(b"  ps              List running processes\n", TEXT);
    term.put_str(b"  mem             Show memory usage\n", TEXT);
    term.put_str(b"  status          System information\n", TEXT);
    term.put_str(b"  llm <prompt>    Send prompt to LLM agent\n", TEXT);
    term.put_str(b"  version         Show OS version\n", TEXT);
    term.put_str(b"  shutdown        Halt the system\n", TEXT);
    term.put_char(b'\n', TEXT);
}

/// ls [path] — uses sys_open + sys_getdents for REAL directory listing
fn cmd_ls(term: &mut Terminal, args: &[u8]) {
    term.put_char(b'\n', TEXT);

    // Default path is /disk/ if no argument
    let mut path_buf = [0u8; 260];
    let path_len;
    if args.is_empty() || bytes_eq(args, b"/") {
        // List root /disk/
        let p = b"/disk/\0";
        for i in 0..p.len() { path_buf[i] = p[i]; }
        path_len = p.len() - 1; // exclude null for display
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

    // Céder le CPU au LLM pour qu'il lise le message avant qu'on entre dans la boucle consume
    sys_yield();
    sys_yield();
    sys_yield();

    // Listen for token stream with timeout (real bus messages)
    let mut token_count: u32 = 0;
    let mut idle_ticks: u32 = 0;
    let max_idle = 500_000u32; // timeout massif — le LLM a besoin de temps pour charger ses poids

    term.put_str(b"[LLM] ", LLM_COL);
    loop {
        let mut bus_msg = [0u64; 6];
        if sys_bus_consume(&mut bus_msg) == 0 {
            let intent = bus_msg[2] as u32;
            let payload = bus_msg[4];

            if intent == INTENT_TOKEN_GENERATED as u32 {
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
    } else if bytes_eq(first_word, b"shutdown") || bytes_eq(first_word, b"halt") {
        cmd_shutdown(term);
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

    term.put_str(b"AetherionOS v3.0 - Production Terminal\n", TEXT);
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
                    term.newline();
                    process_command(&mut term);
                    print_prompt(&mut term);
                }
                0x20..=0x7E => {
                    if term.cmd_len < CMD_BUF_SIZE {
                        term.cmd_buf[term.cmd_len] = ch;
                        term.cmd_len += 1;
                    }
                    term.put_char(ch, TEXT);
                }
                _ => {} // Ignore control codes
            }
        } else {
            idle_count += 1;
        }

        // 2. Listen for bus messages (LLM tokens, etc.)
        if !term.llm_active {
            // Only consume bus messages when not in an active llm session
            // (llm command has its own consume loop)
            let mut bus_msg = [0u64; 6];
            if sys_bus_consume(&mut bus_msg) == 0 {
                let intent = bus_msg[2] as u32;
                let payload = bus_msg[4];

                if intent == INTENT_TOKEN_GENERATED as u32 {
                    let token_char = (payload & 0xFF) as u8;
                    if token_char >= 0x20 && token_char <= 0x7E || token_char == b'\n' {
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
