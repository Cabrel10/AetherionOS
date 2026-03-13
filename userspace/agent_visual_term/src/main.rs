//! AetherionOS J70 — Production-Grade Terminal with 2D Text Buffer
//!
//! Architecture: Double-buffered terminal
//!   - Layer 1: screen_buf (Vec) — logical grid (source of truth)
//!   - Layer 2: framebuffer — physical display (render target)
//!
//! FIX J70: Heap allocation (Vec & Box) eliminates Ring 3 stack overflow.

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

// Exact geometry
const COLS: usize = (SCR_W - MARGIN_X * 2) / CHAR_W;     // 126 cols
const ROWS: usize = (SCR_H - MARGIN_Y - 34) / CHAR_H;    // 43 rows

const CMD_BUF_SIZE: usize = 128;

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
// Terminal State — Heap Allocated Text Buffer
// ═══════════════════════════════════════════════════
struct Terminal {
    screen_buf: Vec<Cell>, // Flat 1D vector instead of 2D stack array
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
        // Allocate directly on the heap without hitting the stack limits
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

    fn px_x(&self, col: usize) -> u32 {
        (MARGIN_X + col * CHAR_W) as u32
    }
    
    fn px_y(&self, row: usize) -> u32 {
        (MARGIN_Y + row * CHAR_H) as u32
    }

    fn cell(&self, x: usize, y: usize) -> Cell {
        self.screen_buf[y * COLS + x]
    }

    fn cell_mut(&mut self, x: usize, y: usize) -> &mut Cell {
        &mut self.screen_buf[y * COLS + x]
    }

    /// Redraw the entire screen from buffer
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
    
    /// Redraw just one cell (fast, for typing)
    fn render_cell(&self, x: usize, y: usize) {
        let cell = self.cell(x, y);
        let px = self.px_x(x);
        let py = self.px_y(y);
        sys_fb_fill_rect(px, py, CHAR_W as u32, CHAR_H as u32, BG);
        if cell.ch != b' ' && cell.ch != 0 {
            sys_fb_draw_char(px, py, cell.ch, cell.color);
        }
    }
    
    /// Redraw just one line (for newline)
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

    /// Scroll: move buffer content up, clear bottom, redraw all
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

    /// Clear: zero buffer, reset cursor, redraw
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
        for &ch in s {
            self.put_char(ch, color);
        }
    }

    fn newline(&mut self) {
        self.erase_cursor();
        
        self.cursor_x = 0;
        self.cursor_y += 1;
        
        if self.cursor_y >= ROWS {
            self.cursor_y = ROWS - 1;
            self.scroll_up();
        } else {
            // Logically and visually clear the new line
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
            
            let px = self.px_x(cx);
            let py = self.px_y(cy);
            sys_fb_fill_rect(px, py, CHAR_W as u32, CHAR_H as u32, BG);
            
            if self.cmd_len > 0 { self.cmd_len -= 1; }
            self.draw_cursor();
        }
    }

    fn clear_cmd_buf(&mut self) {
        self.cmd_len = 0;
    }
}

// ═══════════════════════════════════════════════════
// UI Chrome and Commands
// ═══════════════════════════════════════════════════

fn draw_chrome() {
    sys_fb_fill_rect(0, 0, SCR_W as u32, SCR_H as u32, BG);
    sys_fb_fill_rect(0, 0, SCR_W as u32, TITLE_H as u32, TITLE_BG);
    sys_fb_draw_string(10, 6, b"AetherionOS Terminal v3.0 [J70 Production]", TEXT);
    sys_fb_draw_string((SCR_W - 280) as u32, 6, b"Ring 3 | Text Buffer | LLM", DIM);
    
    let status_y = SCR_H - CHAR_H - 18;
    sys_fb_fill_rect(0, status_y as u32, SCR_W as u32, (CHAR_H + 18) as u32, 0x00010409);
    sys_fb_draw_string(8, (status_y + 8) as u32,
        b"[help] Commands  [llm <prompt>] AI Chat  [clear] Clear  |  J70", DIM);
}

fn print_prompt(term: &mut Terminal) {
    term.put_str(b"aetherion", PROMPT);
    term.put_str(b":~$ ", TEXT);
    term.clear_cmd_buf();
}

fn cmd_help(term: &mut Terminal) {
    term.put_char(b'\n', TEXT);
    term.put_str(b"AetherionOS Production Shell Commands:\n", INFO_COL);
    term.put_str(b"  help          Show this help message\n", TEXT);
    term.put_str(b"  clear         Clear the terminal screen\n", TEXT);
    term.put_str(b"  ls            List directory contents\n", TEXT);
    term.put_str(b"  status        Show system status\n", TEXT);
    term.put_str(b"  llm <prompt>  Send prompt to LLM agent\n", TEXT);
    term.put_str(b"  exit          Shutdown terminal\n", TEXT);
    term.put_str(b"  version       Show AetherionOS version\n", TEXT);
    term.put_char(b'\n', TEXT);
}

fn cmd_clear(term: &mut Terminal) {
    term.clear_screen();
}

fn cmd_ls(term: &mut Terminal) {
    term.put_char(b'\n', TEXT);
    term.put_str(b"total 24\n", TEXT);
    term.put_str(b"drwxr-xr-x  2 root root 4096 Jan  1 00:00 .\n", TEXT);
    term.put_str(b"drwxr-xr-x  2 root root 4096 Jan  1 00:00 ..\n", TEXT);
    term.put_str(b"-rwxr-xr-x  1 root root  22K Jan  1 00:00 agent_visual_term\n", TEXT);
    term.put_str(b"-rwxr-xr-x  1 root root  15K Jan  1 00:00 agent_llm_chat\n", TEXT);
    term.put_str(b"-rwxr-xr-x  1 root root 8.0K Jan  1 00:00 hello\n", TEXT);
    term.put_char(b'\n', TEXT);
}

fn cmd_status(term: &mut Terminal) {
    term.put_char(b'\n', TEXT);
    term.put_str(b"=== System Status ===\n", INFO_COL);
    term.put_str(b"  OS:       AetherionOS v3.0\n", TEXT);
    term.put_str(b"  Arch:     x86_64 Ring 3\n", TEXT);
    term.put_str(b"  Display:  1024x768 32bpp\n", TEXT);
    term.put_str(b"  FS:       FAT32 (VirtIO)\n", TEXT);
    term.put_str(b"  Terminal: Text Buffer v3.0\n", TEXT);
    term.put_str(b"  Bus:      Cognitive Bus active\n", TEXT);
    term.put_str(b"  Commands: ", TEXT);
    let mut buf = [0u8; 12];
    u32_to_str(term.commands_run, &mut buf);
    term.put_str(&buf, PROMPT);
    term.put_char(b'\n', TEXT);
    term.put_str(b"  Tokens:   ", TEXT);
    u32_to_str(term.tokens_received, &mut buf);
    term.put_str(&buf, LLM_COL);
    term.put_char(b'\n', TEXT);
    term.put_str(b"  LLM:      ", TEXT);
    if term.llm_active {
        term.put_str(b"Active (generating)\n", LLM_COL);
    } else {
        term.put_str(b"Idle\n", DIM);
    }
    term.put_char(b'\n', TEXT);
}

fn cmd_version(term: &mut Terminal) {
    term.put_char(b'\n', TEXT);
    term.put_str(b"AetherionOS v3.0 - Production Grade Terminal\n", INFO_COL);
    term.put_str(b"Kernel: x86_64 Ring 3 Preemptive Scheduler\n", TEXT);
    term.put_str(b"Terminal: Text Buffer Architecture (J70)\n", TEXT);
    term.put_str(b"Features: FAT32, TCP/IP, Cognitive Bus, LLM\n", TEXT);
    term.put_char(b'\n', TEXT);
}

fn cmd_llm(term: &mut Terminal, prompt_bytes: &[u8]) {
    if prompt_bytes.is_empty() {
        term.put_char(b'\n', TEXT);
        term.put_str(b"Usage: llm <your prompt>\n", ERR_COL);
        term.put_str(b"Example: llm Hello world\n", DIM);
        term.put_char(b'\n', TEXT);
        return;
    }
    term.put_char(b'\n', TEXT);
    term.put_str(b"[LLM] Sending prompt to AI agent...\n", LLM_COL);
    term.put_str(b"[LLM] \"", LLM_COL);
    term.put_str(prompt_bytes, TEXT);
    term.put_str(b"\"\n", LLM_COL);
    let mut hash: u64 = 5381;
    for &b in prompt_bytes {
        hash = hash.wrapping_mul(33).wrapping_add(b as u64);
    }
    sys_bus_publish(INTENT_USER_PROMPT, 2, hash);
    sys_bus_publish(INTENT_TERM_CMD, 2, hash);
    term.llm_active = true;
    term.put_str(b"[LLM] Published INTENT_USER_PROMPT\n", DIM);
    term.put_str(b"[LLM] Waiting for token stream...\n", LLM_COL);
    
    // Simulate generation locally until Multi-Processing LLM works
    let response = b"AetherionOS AI ready.";
    term.put_str(b"[LLM] Response: ", LLM_COL);
    for &ch in response.iter() {
        term.put_char(ch, LLM_COL);
        term.tokens_received += 1;
    }
    term.put_char(b'\n', TEXT);
    
    sys_bus_publish(INTENT_GENERATION_DONE, 2, response.len() as u64);
    term.llm_active = false;
    term.put_str(b"[LLM] Generation complete\n", DIM);
    term.put_char(b'\n', TEXT);
}

// ═══════════════════════════════════════════════════
// Command Parser and String Utils
// ═══════════════════════════════════════════════════

fn u32_to_str(val: u32, buf: &mut [u8; 12]) {
    for b in buf.iter_mut() { *b = 0; }
    if val == 0 { buf[0] = b'0'; return; }
    let mut v = val;
    let mut i: usize = 11;
    while v > 0 && i > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    let start = i;
    let copy_len = 12 - start;
    for j in 0..copy_len {
        buf[j] = buf[start + j];
    }
    for j in copy_len..12 {
        buf[j] = 0;
    }
}

fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    for i in 0..a.len() {
        if a[i] != b[i] { return false; }
    }
    true
}

fn starts_with(a: &[u8], prefix: &[u8]) -> bool {
    if a.len() < prefix.len() { return false; }
    bytes_eq(&a[..prefix.len()], prefix)
}

fn process_command(term: &mut Terminal) {
    let mut cmd_copy = [0u8; CMD_BUF_SIZE];
    let clen = term.cmd_len;
    for i in 0..clen {
        cmd_copy[i] = term.cmd_buf[i];
    }
    let cmd = &cmd_copy[..clen];
    
    let mut start = 0;
    let mut end = cmd.len();
    while start < end && cmd[start] == b' ' { start += 1; }
    while end > start && cmd[end - 1] == b' ' { end -= 1; }
    
    let trimmed_len = end - start;
    if trimmed_len == 0 {
        return;
    }
    
    let mut trimmed = [0u8; CMD_BUF_SIZE];
    for i in 0..trimmed_len {
        trimmed[i] = cmd[start + i];
    }
    let command = &trimmed[..trimmed_len];
    term.commands_run += 1;
    
    if bytes_eq(command, b"help") {
        cmd_help(term);
    } else if bytes_eq(command, b"clear") {
        cmd_clear(term);
        return;
    } else if bytes_eq(command, b"ls") {
        cmd_ls(term);
    } else if bytes_eq(command, b"status") {
        cmd_status(term);
    } else if bytes_eq(command, b"version") {
        cmd_version(term);
    } else if bytes_eq(command, b"exit") || bytes_eq(command, b"quit") {
        term.put_char(b'\n', TEXT);
        term.put_str(b"Goodbye!\n", PROMPT);
        sys_write(1, b"[J70] Exit command received\n");
        sys_bus_publish(INTENT_VISUAL_TERM, 3, 0);
        sys_exit(0);
    } else if starts_with(command, b"llm ") {
        let prompt_len = trimmed_len - 4;
        let mut prompt_buf = [0u8; CMD_BUF_SIZE];
        for i in 0..prompt_len {
            prompt_buf[i] = trimmed[4 + i];
        }
        cmd_llm(term, &prompt_buf[..prompt_len]);
    } else if bytes_eq(command, b"llm") {
        cmd_llm(term, b"");
    } else {
        term.put_char(b'\n', TEXT);
        term.put_str(b"Unknown command: '", ERR_COL);
        term.put_str(command, TEXT);
        term.put_str(b"'\n", ERR_COL);
        term.put_str(b"Type 'help' for available commands.\n", DIM);
        term.put_char(b'\n', TEXT);
    }
}

// ═══════════════════════════════════════════════════
// MAIN EVENT LOOP
// ═══════════════════════════════════════════════════

#[no_mangle]
pub extern "C" fn main() -> i64 {
    sys_write(1, b"[J70] ========================================\n");
    sys_write(1, b"[J70] Production Terminal v3.0 - Text Buffer\n");
    sys_write(1, b"[J70] ========================================\n");
    
    draw_chrome();
    
    // FIX J70: Allocating Terminal Struct & Grid on Heap via Box::new()
    let mut term = alloc::boxed::Box::new(Terminal::new());
    term.clear_screen();
    
    term.put_str(b"AetherionOS v3.0 - Production Grade Terminal\n", TEXT);
    term.put_str(b"Kernel: x86_64 Ring 3 | Text Buffer Architecture\n", DIM);
    term.put_str(b"Features: FAT32, TCP/IP, Cognitive Bus, LLM\n", DIM);
    term.put_str(b"Type 'help' for commands, 'llm <prompt>' for AI chat.\n", INFO_COL);
    term.put_char(b'\n', TEXT);
    
    sys_bus_publish(INTENT_VISUAL_TERM, 3, 1);
    sys_write(1, b"[J70] Terminal ready\n");
    
    print_prompt(&mut term);
    
    let mut idle_count: u64 = 0;
    let mut read_buf = [0u8; 1];
    
    loop {
        // 1. Read character from ring-buffer keyboard handler
        let n = sys_read(0, &mut read_buf);
        if n > 0 {
            idle_count = 0;
            let ch = read_buf[0];
            match ch {
                0x08 | 0x7F => { term.backspace(); } // Backspace
                b'\n' | b'\r' => {
                    term.newline();
                    process_command(&mut term);
                    print_prompt(&mut term);
                }
                0x20..=0x7E => {
                    // Alphanumeric chars - ONLY printable ASCII (avoids weird cursor blocks)
                    if term.cmd_len < CMD_BUF_SIZE {
                        term.cmd_buf[term.cmd_len] = ch;
                        term.cmd_len += 1;
                    }
                    term.put_char(ch, TEXT);
                }
                _ => {} // Ignore all other characters (control codes, etc.)
            }
        } else {
            idle_count += 1;
        }
        
        // 2. Listen to Cognitive Bus for LLM responses (Jalon 71)
        let mut bus_msg = [0u64; 6];
        if sys_bus_consume(&mut bus_msg) == 0 {
            let intent = bus_msg[2] as u32; // intent_id at offset 8 (index 2 in u32 view)
            let payload = bus_msg[4]; // payload at offset 16 (index 4 in u64 view)
            
            if intent == INTENT_TOKEN_GENERATED as u32 {
                // LLM generated a token - display it in real-time
                let token_char = (payload & 0xFF) as u8;
                if token_char >= 0x20 && token_char <= 0x7E || token_char == b'\n' {
                    term.put_char(token_char, LLM_COL);
                }
            } else if intent == INTENT_GENERATION_DONE as u32 {
                // LLM finished generation
                term.put_char(b'\n', TEXT);
                term.put_str(b"[LLM] Generation complete\n", DIM);
                term.llm_active = false;
                print_prompt(&mut term);
            }
        }
        
        term.blink_tick();
        sys_yield();
        
        if idle_count >= MAX_IDLE_LOOPS {
            sys_write(1, b"[J70] Safety valve: exiting\n");
            break;
        }
    }
    
    sys_write(1, b"[J70] Terminal event loop completed\n");
    0
}
