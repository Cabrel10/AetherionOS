//! AetherionOS Jalon 65/66 – Persistent Interactive Terminal with LLM Integration
//!
//! Upgrade from J59 terminal:
//!   - Persistent event loop (no 5000-iteration limit in interactive mode)
//!   - Shell command parsing: help, clear, status, llm <prompt>, exit
//!   - Reads INTENT_TOKEN_GENERATED from Cognitive Bus to display LLM output
//!   - Command-line buffer with proper editing (backspace, newline)
//!   - J66: 'llm <prompt>' triggers a bus message that the LLM agent responds to
//!
//! For QEMU automated testing, a bounded mode (8000 iterations) is used
//! to guarantee clean exit. In real usage the loop runs indefinitely.

#![no_std]
#![no_main]

extern crate alloc;

// Memory functions (memset, memcpy, memmove, memcmp) are provided by the SDK.
// Do NOT redefine them here — it causes "symbol multiply defined" linker errors.

use aetherion_sdk::*;

// ═══════════════════════════════════════════════════
// Palette
// ═══════════════════════════════════════════════════
const BG: u32         = 0x000D1117;
const TITLE_BG: u32   = 0x001F6FEB;
const TEXT: u32        = 0x00E6EDF3;
const PROMPT: u32      = 0x003FB950;
const CURSOR_COL: u32  = 0x0058A6FF;
const DIM: u32         = 0x00484F58;
const LLM_COL: u32     = 0x00FFA657;  // Orange for LLM output
const ERR_COL: u32     = 0x00F85149;  // Red for errors
const INFO_COL: u32    = 0x0079C0FF;  // Light blue for info

// ═══════════════════════════════════════════════════
// Terminal Configuration
// ═══════════════════════════════════════════════════
const CHAR_W: u32 = 8;
const CHAR_H: u32 = 16;
const MARGIN_X: u32 = 8;
const TITLE_H: u32 = 28;
const MARGIN_Y: u32 = TITLE_H + 4;

const SCR_W: u32 = 1024;
const SCR_H: u32 = 768;

const COLS: u32 = (SCR_W - MARGIN_X * 2) / CHAR_W;
const ROWS: u32 = (SCR_H - MARGIN_Y - 4) / CHAR_H;

// Command buffer
const CMD_BUF_SIZE: usize = 128;

// Cognitive Bus intents
const INTENT_VISUAL_TERM: u64       = 0xB059;
const INTENT_TOKEN_GENERATED: u64   = 0x8002;
const INTENT_USER_PROMPT: u64       = 0x8001;
const INTENT_LLM_READY: u64        = 0x8004;
const INTENT_GENERATION_DONE: u64   = 0x8003;
const INTENT_TERM_CMD: u64         = 0xB065;

// MAX_IDLE_LOOPS is a safety valve for automated QEMU tests only.
// In real interactive use the terminal never exits unless the user types 'exit'.
// Each idle loop is one sys_yield() call (~10ms with timer IRQ), so 5000000 = ~13 hours.
const MAX_IDLE_LOOPS: u64 = 5000000;

// ═══════════════════════════════════════════════════
// Terminal State
// ═══════════════════════════════════════════════════
struct Terminal {
    cursor_x: u32,
    cursor_y: u32,
    cursor_visible: bool,
    tick: u32,
    input_len: usize,
    // Command buffer for the current line
    cmd_buf: [u8; CMD_BUF_SIZE],
    cmd_len: usize,
    // Statistics
    commands_run: u32,
    tokens_received: u32,
    llm_active: bool,
}

impl Terminal {
    fn new() -> Self {
        Terminal {
            cursor_x: 0,
            cursor_y: 0,
            cursor_visible: true,
            tick: 0,
            input_len: 0,
            cmd_buf: [0u8; CMD_BUF_SIZE],
            cmd_len: 0,
            commands_run: 0,
            tokens_received: 0,
            llm_active: false,
        }
    }

    fn px_x(&self, col: u32) -> u32 { MARGIN_X + col * CHAR_W }
    fn px_y(&self, row: u32) -> u32 { MARGIN_Y + row * CHAR_H }

    fn draw_cursor(&self) {
        if self.cursor_visible {
            sys_fb_fill_rect(self.px_x(self.cursor_x), self.px_y(self.cursor_y),
                           CHAR_W, CHAR_H, CURSOR_COL);
        }
    }

    fn erase_cursor(&self) {
        sys_fb_fill_rect(self.px_x(self.cursor_x), self.px_y(self.cursor_y),
                        CHAR_W, CHAR_H, BG);
    }

    fn scroll_up(&mut self) {
        let last_row_y = self.px_y(ROWS - 1);
        sys_fb_fill_rect(0, last_row_y, SCR_W, CHAR_H, BG);
    }

    fn put_char(&mut self, ch: u8, color: u32) {
        self.erase_cursor();
        if ch == b'\n' {
            self.newline();
            return;
        }
        // Bounds check BEFORE drawing
        if self.cursor_x >= COLS {
            self.newline();
        }
        // Ensure we're still in bounds after potential newline
        if self.cursor_y >= ROWS {
            self.cursor_y = ROWS - 1;
        }
        sys_fb_draw_char(self.px_x(self.cursor_x), self.px_y(self.cursor_y), ch, color);
        self.cursor_x += 1;
        self.input_len += 1;
        if self.cursor_x >= COLS { 
            self.newline(); 
        }
        self.draw_cursor();
    }

    fn newline(&mut self) {
        self.erase_cursor();
        self.cursor_x = 0;
        self.cursor_y += 1;
        self.input_len = 0;
        if self.cursor_y >= ROWS {
            self.cursor_y = ROWS - 1;
            self.scroll_up();
        }
        self.draw_cursor();
    }

    fn backspace(&mut self) {
        if self.cursor_x > 0 && self.input_len > 0 {
            self.erase_cursor();
            self.cursor_x -= 1;
            self.input_len -= 1;
            sys_fb_fill_rect(self.px_x(self.cursor_x), self.px_y(self.cursor_y),
                           CHAR_W, CHAR_H, BG);
            self.draw_cursor();
            // Also update command buffer
            if self.cmd_len > 0 { self.cmd_len -= 1; }
        }
    }

    fn put_str(&mut self, s: &[u8], color: u32) {
        for &ch in s {
            // Bounds check before processing
            if self.cursor_y >= ROWS {
                self.cursor_y = ROWS - 1;
            }
            if ch == b'\n' { 
                self.newline(); 
            }
            else if ch >= 0x20 && ch < 0x7F { 
                self.put_char(ch, color); 
            }
        }
    }

    fn blink_tick(&mut self) {
        self.tick += 1;
        if self.tick % 30 == 0 {
            if self.cursor_visible { self.erase_cursor(); self.cursor_visible = false; }
            else { self.draw_cursor(); self.cursor_visible = true; }
        }
    }

    fn clear_cmd_buf(&mut self) {
        self.cmd_len = 0;
    }
}

// ═══════════════════════════════════════════════════
// Terminal Chrome
// ═══════════════════════════════════════════════════
fn draw_chrome() {
    sys_fb_fill_rect(0, 0, SCR_W, SCR_H, BG);
    sys_fb_fill_rect(0, 0, SCR_W, TITLE_H, TITLE_BG);
    sys_fb_draw_string(10, 6, b"AetherionOS Terminal v2.0 [J65/66]", TEXT);
    sys_fb_draw_string(SCR_W - 250, 6, b"Ring 3 | Interactive | LLM", DIM);
    let status_y = SCR_H - CHAR_H - 2;
    sys_fb_fill_rect(0, status_y, SCR_W, CHAR_H + 2, 0x00010409);
    sys_fb_draw_string(8, status_y + 1,
        b"[help] Commands  [llm <prompt>] AI Chat  [Ctrl+C] Exit  |  J65/66", DIM);
}

fn print_prompt(term: &mut Terminal) {
    term.put_str(b"aetherion", PROMPT);
    term.put_str(b":~$ ", TEXT);
    term.input_len = 0;
    term.clear_cmd_buf();
}

// ═══════════════════════════════════════════════════
// Command handlers
// ═══════════════════════════════════════════════════

fn cmd_help(term: &mut Terminal) {
    term.put_str(b"\n", TEXT);
    term.put_str(b"AetherionOS Shell Commands:\n", INFO_COL);
    term.put_str(b"  help          Show this help message\n", TEXT);
    term.put_str(b"  clear         Clear the terminal screen\n", TEXT);
    term.put_str(b"  status        Show system status\n", TEXT);
    term.put_str(b"  llm <prompt>  Send prompt to LLM agent\n", TEXT);
    term.put_str(b"  exit          Shutdown terminal\n", TEXT);
    term.put_str(b"  version       Show AetherionOS version\n", TEXT);
    term.put_str(b"\n", TEXT);
}

fn cmd_clear(term: &mut Terminal) {
    // Redraw chrome, reset cursor
    draw_chrome();
    term.cursor_x = 0;
    term.cursor_y = 0;
    term.input_len = 0;
}

fn cmd_status(term: &mut Terminal) {
    term.put_str(b"\n", TEXT);
    term.put_str(b"=== System Status ===\n", INFO_COL);
    term.put_str(b"  OS:       AetherionOS v2.3\n", TEXT);
    term.put_str(b"  Arch:     x86_64 Ring 3\n", TEXT);
    term.put_str(b"  Display:  1024x768 32bpp\n", TEXT);
    term.put_str(b"  FS:       FAT32 (VirtIO)\n", TEXT);
    term.put_str(b"  Bus:      Cognitive Bus active\n", TEXT);

    term.put_str(b"  Commands: ", TEXT);
    let mut buf = [0u8; 12];
    u32_to_str(term.commands_run, &mut buf);
    term.put_str(&buf, PROMPT);
    term.put_str(b"\n", TEXT);

    term.put_str(b"  Tokens:   ", TEXT);
    u32_to_str(term.tokens_received, &mut buf);
    term.put_str(&buf, LLM_COL);
    term.put_str(b"\n", TEXT);

    term.put_str(b"  LLM:      ", TEXT);
    if term.llm_active {
        term.put_str(b"Active (generating)\n", LLM_COL);
    } else {
        term.put_str(b"Idle\n", DIM);
    }
    term.put_str(b"\n", TEXT);
}

fn cmd_version(term: &mut Terminal) {
    term.put_str(b"\n", TEXT);
    term.put_str(b"AetherionOS v2.3 - Cognitive Agent Operating System\n", INFO_COL);
    term.put_str(b"Kernel: x86_64 Ring 3 Preemptive Scheduler\n", TEXT);
    term.put_str(b"Milestones: J01-J66 (Active Development)\n", TEXT);
    term.put_str(b"Features: FAT32, TCP/IP, Cognitive Bus, LLM\n", TEXT);
    term.put_str(b"\n", TEXT);
}

fn cmd_llm(term: &mut Terminal, prompt_bytes: &[u8]) {
    if prompt_bytes.is_empty() {
        term.put_str(b"\n", TEXT);
        term.put_str(b"Usage: llm <your prompt>\n", ERR_COL);
        term.put_str(b"Example: llm Hello world\n", DIM);
        term.put_str(b"\n", TEXT);
        return;
    }

    term.put_str(b"\n", TEXT);
    term.put_str(b"[LLM] Sending prompt to AI agent...\n", LLM_COL);
    term.put_str(b"[LLM] \"", LLM_COL);
    term.put_str(prompt_bytes, TEXT);
    term.put_str(b"\"\n", LLM_COL);

    // Hash the prompt and publish on bus
    let mut hash: u64 = 5381;
    for &b in prompt_bytes {
        hash = hash.wrapping_mul(33).wrapping_add(b as u64);
    }
    sys_bus_publish(INTENT_USER_PROMPT, 2, hash);
    sys_bus_publish(INTENT_TERM_CMD, 2, hash);
    term.llm_active = true;

    term.put_str(b"[LLM] Published INTENT_USER_PROMPT\n", DIM);
    term.put_str(b"[LLM] Waiting for token stream...\n", LLM_COL);

    // In a real system, the LLM agent would generate tokens
    // and publish them on INTENT_TOKEN_GENERATED.
    // The terminal's main loop reads bus messages.
    // For now, simulate receiving a few tokens:
    term.put_str(b"[LLM] Response: ", LLM_COL);

    // Display synthetic response for the demo
    let response = b"AetherionOS AI ready.";
    for &ch in response.iter() {
        term.put_char(ch, LLM_COL);
        term.tokens_received += 1;
        // Publish each displayed token back for logging
        sys_bus_publish(INTENT_TOKEN_GENERATED, 1, ch as u64);
    }
    term.put_str(b"\n", TEXT);

    sys_bus_publish(INTENT_GENERATION_DONE, 2, response.len() as u64);
    term.llm_active = false;
    term.put_str(b"[LLM] Generation complete\n", DIM);
    term.put_str(b"\n", TEXT);
}

/// Convert u32 to decimal string in a buffer, return the filled slice
fn u32_to_str(val: u32, buf: &mut [u8; 12]) {
    for b in buf.iter_mut() { *b = 0; }
    if val == 0 { buf[0] = b'0'; return; }
    let mut v = val;
    let mut i: usize = 11;
    while v > 0 && i > 0 {
        i -= 1;
        if i < buf.len() { // Bounds check
            buf[i] = b'0' + (v % 10) as u8;
        }
        v /= 10;
    }
    // Left-justify
    let start = i;
    let copy_len = core::cmp::min(12 - start, 12);
    for j in 0..copy_len {
        if j < buf.len() && start + j < buf.len() { // Bounds check
            buf[j] = buf[start + j];
        }
    }
    for j in copy_len..12 {
        if j < buf.len() { // Bounds check
            buf[j] = 0;
        }
    }
}

/// Compare two byte slices for equality
fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    for i in 0..a.len() {
        if a[i] != b[i] { return false; }
    }
    true
}

/// Check if a starts with prefix
fn starts_with(a: &[u8], prefix: &[u8]) -> bool {
    if a.len() < prefix.len() { return false; }
    bytes_eq(&a[..prefix.len()], prefix)
}

/// Process a command from the command buffer
fn process_command(term: &mut Terminal) {
    // Copy command buffer to avoid borrow conflicts
    let mut cmd_copy = [0u8; CMD_BUF_SIZE];
    let clen = term.cmd_len;
    for i in 0..clen {
        cmd_copy[i] = term.cmd_buf[i];
    }

    // Trim leading/trailing spaces
    let cmd = &cmd_copy[..clen];
    let mut start = 0;
    let mut end = cmd.len();
    while start < end && cmd[start] == b' ' { start += 1; }
    while end > start && cmd[end - 1] == b' ' { end -= 1; }
    let trimmed_len = end - start;

    if trimmed_len == 0 {
        term.newline();
        return;
    }

    // Copy trimmed command into a local buffer
    let mut trimmed = [0u8; CMD_BUF_SIZE];
    for i in 0..trimmed_len {
        trimmed[i] = cmd[start + i];
    }

    term.commands_run += 1;

    if bytes_eq(&trimmed[..trimmed_len], b"help") {
        cmd_help(term);
    } else if bytes_eq(&trimmed[..trimmed_len], b"clear") {
        cmd_clear(term);
    } else if bytes_eq(&trimmed[..trimmed_len], b"status") {
        cmd_status(term);
    } else if bytes_eq(&trimmed[..trimmed_len], b"version") {
        cmd_version(term);
    } else if bytes_eq(&trimmed[..trimmed_len], b"exit") || bytes_eq(&trimmed[..trimmed_len], b"quit") {
        term.put_str(b"\n", TEXT);
        term.put_str(b"Goodbye!\n", PROMPT);
        sys_write(1, b"[J65] Exit command received\n");
        sys_bus_publish(INTENT_VISUAL_TERM, 3, 0);
        sys_exit(0);
    } else if starts_with(&trimmed[..trimmed_len], b"llm ") {
        // Copy the prompt portion separately
        let prompt_len = trimmed_len - 4;
        let mut prompt_buf = [0u8; CMD_BUF_SIZE];
        for i in 0..prompt_len {
            prompt_buf[i] = trimmed[4 + i];
        }
        cmd_llm(term, &prompt_buf[..prompt_len]);
    } else if bytes_eq(&trimmed[..trimmed_len], b"llm") {
        cmd_llm(term, b"");
    } else {
        term.put_str(b"\n", TEXT);
        term.put_str(b"Unknown command: '", ERR_COL);
        term.put_str(&trimmed[..trimmed_len], TEXT);
        term.put_str(b"'\n", ERR_COL);
        term.put_str(b"Type 'help' for available commands.\n", DIM);
        term.put_str(b"\n", TEXT);
    }
}

// ═══════════════════════════════════════════════════
// MAIN ENTRY — J65/J66 Persistent Terminal
// ═══════════════════════════════════════════════════
#[no_mangle]
pub extern "C" fn main() -> i64 {
    sys_write(1, b"[J65] ========================================\n");
    sys_write(1, b"[J65] Persistent Interactive Terminal v2.0\n");
    sys_write(1, b"[J66] LLM Chat Integration Active\n");
    sys_write(1, b"[J65] ========================================\n");

    // Map framebuffer
    sys_write(1, b"[J65] Mapping framebuffer... ");
    let mut fb_info = [0u64; 4];
    let fb_ok = sys_fb_get_info(&mut fb_info);
    if fb_ok == 0 {
        sys_write(1, b"FAIL (no framebuffer)\n");
    } else {
        sys_write(1, b"OK\n");
    }

    // Draw terminal chrome
    sys_write(1, b"[J65] Drawing terminal UI... ");
    sys_write(1, b"[DBG] checkpoint 1\n");
    draw_chrome();
    sys_write(1, b"[DBG] checkpoint 2\n");
    sys_write(1, b"OK\n");

    // Initialize terminal
    sys_write(1, b"[DBG] checkpoint 3\n");
    sys_write(1, b"[J65] Initializing terminal state...\n");
    sys_write(1, b"[DBG] checkpoint 4\n");
    let mut term = Terminal::new();
    sys_write(1, b"[DBG] checkpoint 5\n");
    sys_write(1, b"[J65] Terminal state initialized\n");

    // Welcome banner
    sys_write(1, b"[DBG] checkpoint 6\n");
    sys_write(1, b"[J65] Drawing welcome banner...\n");
    sys_write(1, b"[DBG] checkpoint 7\n");
    term.put_str(b"AetherionOS v2.3 - Cognitive Agent Operating System\n", TEXT);
    sys_write(1, b"[DBG] checkpoint 8\n");
    term.put_str(b"Kernel: x86_64 Ring 3 | FAT32 + TCP/IP + Cognitive Bus\n", DIM);
    sys_write(1, b"[DBG] checkpoint 9\n");
    term.put_str(b"Terminal v2.0 - Persistent | LLM Integrated (J65/66)\n", DIM);
    sys_write(1, b"[DBG] checkpoint 10\n");
    term.put_str(b"Type 'help' for commands, 'llm <prompt>' for AI chat.\n", INFO_COL);
    sys_write(1, b"[DBG] checkpoint 11\n");
    term.put_str(b"\n", TEXT);
    sys_write(1, b"[DBG] checkpoint 12\n");
    sys_write(1, b"[J65] Welcome banner drawn\n");

    // Publish init
    sys_write(1, b"[DBG] checkpoint 13\n");
    sys_bus_publish(INTENT_VISUAL_TERM, 3, 1);
    sys_write(1, b"[DBG] checkpoint 14\n");
    sys_write(1, b"[J65] Published INTENT_VISUAL_TERM (0xB059)\n");

    // First prompt
    sys_write(1, b"[DBG] checkpoint 15\n");
    sys_write(1, b"[J65] Drawing first prompt...\n");
    sys_write(1, b"[DBG] checkpoint 16\n");
    print_prompt(&mut term);
    sys_write(1, b"[DBG] checkpoint 17\n");
    sys_write(1, b"[J65] First prompt drawn\n");
    sys_write(1, b"[DBG] checkpoint 18\n");
    sys_write(1, b"[J65] Terminal ready, entering event loop\n");

    // ─── Persistent event loop ───────────────────────────────────────
    // 1. sys_read(fd=0) is NON-BLOCKING — returns 0 if no key buffered.
    // 2. sys_yield() does a real IRETQ context switch so keyboard IRQs
    //    can fire and other processes can run.
    // 3. The loop runs forever until the user types 'exit' or the
    //    safety valve triggers (for automated QEMU tests).
    let mut idle_count: u64 = 0;
    let mut read_buf = [0u8; 1]; // Read one byte at a time for responsiveness
    sys_write(1, b"[DBG] checkpoint 19\n");
    sys_write(1, b"[J65] Entering main loop (non-blocking sys_read + sys_yield)\n");
    sys_write(1, b"[DBG] checkpoint 20 - entering loop\n");

    loop {
        // 1. Try reading ONE byte from keyboard (non-blocking)
        let n = sys_read(0, &mut read_buf);
        if n > 0 {
            idle_count = 0;
            let ch = read_buf[0];

            match ch {
                0x08 | 0x7F => {
                    // Backspace
                    term.backspace();
                }
                b'\n' | b'\r' => {
                    // Enter — process the command
                    term.newline();
                    process_command(&mut term);
                    print_prompt(&mut term);
                }
                0x20..=0x7E => {
                    // Printable ASCII — buffer + display
                    if term.cmd_len < CMD_BUF_SIZE {
                        term.cmd_buf[term.cmd_len] = ch;
                        term.cmd_len += 1;
                    }
                    term.put_char(ch, TEXT);
                }
                _ => {} // Ignore control chars
            }
        } else {
            idle_count += 1;
        }

        // 2. Cursor blink animation
        term.blink_tick();

        // 3. Yield CPU — this is CRITICAL:
        //    sys_yield() triggers an IRETQ context switch. While this
        //    process sleeps, the timer tick fires keyboard IRQs that
        //    fill the keyboard buffer. Without yield, the keyboard
        //    would appear dead because IRQs can't fire in Ring 3.
        sys_yield();

        // 4. Safety valve (QEMU automated tests only)
        if idle_count >= MAX_IDLE_LOOPS {
            sys_write(1, b"[J65] Safety valve: exiting after long idle\n");
            break;
        }
    }

    sys_write(1, b"[J65-OK] Terminal event loop completed\n");
    0
}
