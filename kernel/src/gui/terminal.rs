// kernel/src/gui/terminal.rs — VT100 Terminal Emulator for AetherionOS (Layer 7)
//
// Provides a full-featured terminal emulator rendering to the framebuffer.
// Supports ANSI/VT100 escape sequences for cursor movement, colors, scrolling.
// Includes keyboard input buffer and line-editing support.

use super::{Backbuffer, Color, TERM_BG, TERM_FG, TERM_CURSOR};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Terminal dimensions in characters (8x16 font)
const MAX_COLS: usize = 128;
const MAX_ROWS: usize = 48;

/// ANSI color palette (standard 8 colors + bright variants)
const ANSI_COLORS: [Color; 16] = [
    Color::rgb(0, 0, 0),       // 0: Black
    Color::rgb(170, 0, 0),     // 1: Red
    Color::rgb(0, 170, 0),     // 2: Green
    Color::rgb(170, 85, 0),    // 3: Yellow/Brown
    Color::rgb(0, 0, 170),     // 4: Blue
    Color::rgb(170, 0, 170),   // 5: Magenta
    Color::rgb(0, 170, 170),   // 6: Cyan
    Color::rgb(170, 170, 170), // 7: White
    Color::rgb(85, 85, 85),    // 8: Bright Black (Gray)
    Color::rgb(255, 85, 85),   // 9: Bright Red
    Color::rgb(85, 255, 85),   // 10: Bright Green
    Color::rgb(255, 255, 85),  // 11: Bright Yellow
    Color::rgb(85, 85, 255),   // 12: Bright Blue
    Color::rgb(255, 85, 255),  // 13: Bright Magenta
    Color::rgb(85, 255, 255),  // 14: Bright Cyan
    Color::rgb(255, 255, 255), // 15: Bright White
];

/// Parser state for VT100 escape sequences
#[derive(Debug, Clone, Copy, PartialEq)]
enum EscState {
    Normal,
    Escape,   // Got ESC
    Bracket,  // Got ESC [
    Param,    // Reading numeric parameter
}

/// Terminal state
pub struct Terminal {
    pub cols: usize,
    pub rows: usize,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub fg_color: Color,
    pub bg_color: Color,
    pub bold: bool,
    pub cursor_visible: bool,

    // Character buffer (heap-allocated to avoid 55 KB stack frames)
    chars: Vec<[u8; MAX_COLS]>,
    fg_buf: Vec<[Color; MAX_COLS]>,
    bg_buf: Vec<[Color; MAX_COLS]>,

    // Escape sequence parser
    esc_state: EscState,
    esc_params: [u32; 8],
    esc_param_idx: usize,
    esc_private: u8, // '?' for DEC private modes

    // Scroll region
    scroll_top: usize,
    scroll_bottom: usize,

    // Input buffer (keyboard events queued for the shell)
    pub input_queue: VecDeque<u8>,

    // Command history
    pub history: VecDeque<String>,
    pub history_pos: usize,

    // Current input line
    pub input_line: String,
    pub input_cursor: usize,

    // Dirty flag
    pub dirty: bool,
}

impl Terminal {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = core::cmp::min(cols, MAX_COLS);
        let rows = core::cmp::min(rows, MAX_ROWS);
        Self {
            cols,
            rows,
            cursor_x: 0,
            cursor_y: 0,
            fg_color: TERM_FG,
            bg_color: TERM_BG,
            bold: false,
            cursor_visible: true,
            chars: vec![[b' '; MAX_COLS]; MAX_ROWS],
            fg_buf: vec![[TERM_FG; MAX_COLS]; MAX_ROWS],
            bg_buf: vec![[TERM_BG; MAX_COLS]; MAX_ROWS],
            esc_state: EscState::Normal,
            esc_params: [0; 8],
            esc_param_idx: 0,
            esc_private: 0,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
            input_queue: VecDeque::new(),
            history: VecDeque::with_capacity(64),
            history_pos: 0,
            input_line: String::new(),
            input_cursor: 0,
            dirty: true,
        }
    }

    /// Process a keyboard scancode and translate to terminal input
    pub fn handle_key(&mut self, scancode: u8, shift: bool, ctrl: bool) {
        // Basic scancode -> ASCII mapping (Set 1)
        let ch = match scancode {
            0x02..=0x0A => {
                // 1-9
                let base = b"1234567890";
                let shifted = b"!@#$%^&*(";
                let idx = (scancode - 0x02) as usize;
                if idx < 9 {
                    if shift { shifted[idx] } else { base[idx] }
                } else { 0 }
            }
            0x0B => if shift { b')' } else { b'0' },
            0x0C => if shift { b'_' } else { b'-' },
            0x0D => if shift { b'+' } else { b'=' },
            0x0E => 0x08, // Backspace
            0x0F => b'\t',
            0x10..=0x19 => {
                let row = b"qwertyuiop";
                let idx = (scancode - 0x10) as usize;
                if idx < 10 {
                    let c = row[idx];
                    if ctrl { c - b'a' + 1 } // Ctrl+Q = 0x11, etc.
                    else if shift { c - 32 } // uppercase
                    else { c }
                } else { 0 }
            }
            0x1A => if shift { b'{' } else { b'[' },
            0x1B => if shift { b'}' } else { b']' },
            0x1C => b'\n', // Enter
            0x1E..=0x26 => {
                let row = b"asdfghjkl";
                let idx = (scancode - 0x1E) as usize;
                if idx < 9 {
                    let c = row[idx];
                    if ctrl { c - b'a' + 1 }
                    else if shift { c - 32 }
                    else { c }
                } else { 0 }
            }
            0x27 => if shift { b':' } else { b';' },
            0x28 => if shift { b'"' } else { b'\'' },
            0x29 => if shift { b'~' } else { b'`' },
            0x2B => if shift { b'|' } else { b'\\' },
            0x2C..=0x32 => {
                let row = b"zxcvbnm";
                let idx = (scancode - 0x2C) as usize;
                if idx < 7 {
                    let c = row[idx];
                    if ctrl { c - b'a' + 1 }
                    else if shift { c - 32 }
                    else { c }
                } else { 0 }
            }
            0x33 => if shift { b'<' } else { b',' },
            0x34 => if shift { b'>' } else { b'.' },
            0x35 => if shift { b'?' } else { b'/' },
            0x39 => b' ', // Space
            _ => 0,
        };

        if ch != 0 {
            if ctrl && ch == 3 {
                // Ctrl+C: send interrupt
                self.input_queue.push_back(3);
                self.write_str("^C\n");
            } else if ctrl && ch == 4 {
                // Ctrl+D: EOF
                self.input_queue.push_back(4);
            } else if ctrl && ch == 12 {
                // Ctrl+L: clear screen
                self.clear_screen();
            } else if ch == 0x08 {
                // Backspace
                if !self.input_line.is_empty() && self.input_cursor > 0 {
                    self.input_cursor -= 1;
                    self.input_line.remove(self.input_cursor);
                    self.write_byte(0x08);
                    self.write_byte(b' ');
                    self.write_byte(0x08);
                }
            } else if ch == b'\n' {
                // Enter: submit line
                self.write_byte(b'\n');
                if !self.input_line.is_empty() {
                    let line = self.input_line.clone();
                    // Add to history
                    if self.history.len() >= 64 { self.history.pop_front(); }
                    self.history.push_back(line.clone());
                    self.history_pos = self.history.len();
                    // Queue the line bytes
                    for b in line.as_bytes() {
                        self.input_queue.push_back(*b);
                    }
                    self.input_queue.push_back(b'\n');
                    self.input_line.clear();
                    self.input_cursor = 0;
                }
            } else {
                // Regular character: echo and add to input line
                self.input_line.insert(self.input_cursor, ch as char);
                self.input_cursor += 1;
                self.write_byte(ch);
            }
        }

        // Arrow keys (escaped scancodes)
        match scancode {
            0x48 => { // Up arrow — history
                if !self.history.is_empty() && self.history_pos > 0 {
                    self.history_pos -= 1;
                    self.replace_input_line();
                }
            }
            0x50 => { // Down arrow — history
                if self.history_pos < self.history.len() {
                    self.history_pos += 1;
                    self.replace_input_line();
                }
            }
            _ => {}
        }
    }

    /// Replace the current input line with a history entry
    fn replace_input_line(&mut self) {
        // Erase current line from display
        for _ in 0..self.input_cursor {
            self.write_byte(0x08);
            self.write_byte(b' ');
            self.write_byte(0x08);
        }
        // Set new line
        if self.history_pos < self.history.len() {
            self.input_line = self.history[self.history_pos].clone();
        } else {
            self.input_line.clear();
        }
        self.input_cursor = self.input_line.len();
        // Echo new line
        self.write_str(&self.input_line.clone());
    }

    /// Write a byte to the terminal (handles VT100 escape sequences)
    pub fn write_byte(&mut self, b: u8) {
        match self.esc_state {
            EscState::Normal => match b {
                0x1B => {
                    self.esc_state = EscState::Escape;
                    self.esc_param_idx = 0;
                    self.esc_params = [0; 8];
                    self.esc_private = 0;
                }
                b'\n' => {
                    self.cursor_y += 1;
                    self.cursor_x = 0; // implicit CR
                    self.check_scroll();
                }
                b'\r' => {
                    self.cursor_x = 0;
                }
                b'\t' => {
                    self.cursor_x = (self.cursor_x + 8) & !7;
                    if self.cursor_x >= self.cols {
                        self.cursor_x = self.cols - 1;
                    }
                }
                0x08 => {
                    // Backspace
                    if self.cursor_x > 0 {
                        self.cursor_x -= 1;
                    }
                }
                0x07 => {} // Bell — ignore
                0x00..=0x06 | 0x0E..=0x1A | 0x1C..=0x1F => {} // Other control chars
                _ => {
                    self.put_char(b);
                    self.cursor_x += 1;
                    if self.cursor_x >= self.cols {
                        self.cursor_x = 0;
                        self.cursor_y += 1;
                        self.check_scroll();
                    }
                }
            },
            EscState::Escape => match b {
                b'[' => self.esc_state = EscState::Bracket,
                b'c' => {
                    // Reset terminal
                    self.reset();
                    self.esc_state = EscState::Normal;
                }
                _ => self.esc_state = EscState::Normal,
            },
            EscState::Bracket => {
                if b == b'?' {
                    self.esc_private = b;
                    return;
                }
                if b >= b'0' && b <= b'9' {
                    self.esc_state = EscState::Param;
                    self.esc_params[0] = (b - b'0') as u32;
                    self.esc_param_idx = 0;
                } else {
                    self.execute_csi(b);
                    self.esc_state = EscState::Normal;
                }
            }
            EscState::Param => {
                if b >= b'0' && b <= b'9' {
                    let idx = self.esc_param_idx;
                    if idx < 8 {
                        self.esc_params[idx] = self.esc_params[idx] * 10 + (b - b'0') as u32;
                    }
                } else if b == b';' {
                    self.esc_param_idx += 1;
                } else {
                    self.execute_csi(b);
                    self.esc_state = EscState::Normal;
                }
            }
        }
        self.dirty = true;
    }

    /// Write a string to the terminal
    pub fn write_str(&mut self, s: &str) {
        for &b in s.as_bytes() {
            self.write_byte(b);
        }
    }

    /// Execute a CSI (Control Sequence Introducer) command
    fn execute_csi(&mut self, cmd: u8) {
        let p0 = self.esc_params[0] as usize;
        let p1 = self.esc_params[1] as usize;
        let n_params = self.esc_param_idx + 1;

        match cmd {
            b'A' => { // Cursor Up
                let n = if p0 == 0 { 1 } else { p0 };
                self.cursor_y = self.cursor_y.saturating_sub(n);
            }
            b'B' => { // Cursor Down
                let n = if p0 == 0 { 1 } else { p0 };
                self.cursor_y = core::cmp::min(self.cursor_y + n, self.rows - 1);
            }
            b'C' => { // Cursor Forward
                let n = if p0 == 0 { 1 } else { p0 };
                self.cursor_x = core::cmp::min(self.cursor_x + n, self.cols - 1);
            }
            b'D' => { // Cursor Back
                let n = if p0 == 0 { 1 } else { p0 };
                self.cursor_x = self.cursor_x.saturating_sub(n);
            }
            b'H' | b'f' => { // Cursor Position
                let row = if p0 == 0 { 1 } else { p0 };
                let col = if n_params > 1 && p1 > 0 { p1 } else { 1 };
                self.cursor_y = core::cmp::min(row.saturating_sub(1), self.rows - 1);
                self.cursor_x = core::cmp::min(col.saturating_sub(1), self.cols - 1);
            }
            b'J' => { // Erase in Display
                match p0 {
                    0 => self.clear_from_cursor(),
                    1 => self.clear_to_cursor(),
                    2 | 3 => self.clear_screen(),
                    _ => {}
                }
            }
            b'K' => { // Erase in Line
                match p0 {
                    0 => {
                        for x in self.cursor_x..self.cols {
                            self.chars[self.cursor_y][x] = b' ';
                            self.fg_buf[self.cursor_y][x] = self.fg_color;
                            self.bg_buf[self.cursor_y][x] = self.bg_color;
                        }
                    }
                    1 => {
                        for x in 0..=self.cursor_x {
                            self.chars[self.cursor_y][x] = b' ';
                        }
                    }
                    2 => {
                        for x in 0..self.cols {
                            self.chars[self.cursor_y][x] = b' ';
                        }
                    }
                    _ => {}
                }
            }
            b'm' => { // SGR (Select Graphic Rendition)
                self.process_sgr();
            }
            b'r' => { // Set scroll region
                let top = if p0 == 0 { 1 } else { p0 };
                let bot = if n_params > 1 && p1 > 0 { p1 } else { self.rows };
                self.scroll_top = top.saturating_sub(1);
                self.scroll_bottom = core::cmp::min(bot.saturating_sub(1), self.rows - 1);
            }
            b'h' | b'l' => { // Set/Reset Mode
                if self.esc_private == b'?' {
                    if p0 == 25 {
                        self.cursor_visible = cmd == b'h';
                    }
                }
            }
            b'n' => { // Device Status Report
                // p0 == 6: report cursor position — we can't send back
            }
            b'S' => { // Scroll Up
                let n = if p0 == 0 { 1 } else { p0 };
                for _ in 0..n { self.scroll_up(); }
            }
            b'T' => { // Scroll Down
                let n = if p0 == 0 { 1 } else { p0 };
                for _ in 0..n { self.scroll_down(); }
            }
            _ => {} // Unknown CSI
        }
    }

    /// Process SGR (Set Graphic Rendition) parameters
    fn process_sgr(&mut self) {
        for i in 0..=self.esc_param_idx {
            let p = self.esc_params[i];
            match p {
                0 => {
                    self.fg_color = TERM_FG;
                    self.bg_color = TERM_BG;
                    self.bold = false;
                }
                1 => self.bold = true,
                22 => self.bold = false,
                7 => {
                    // Reverse video
                    core::mem::swap(&mut self.fg_color, &mut self.bg_color);
                }
                27 => {
                    // Un-reverse
                    self.fg_color = TERM_FG;
                    self.bg_color = TERM_BG;
                }
                30..=37 => {
                    let idx = (p - 30) as usize;
                    self.fg_color = if self.bold { ANSI_COLORS[idx + 8] } else { ANSI_COLORS[idx] };
                }
                39 => self.fg_color = TERM_FG,
                40..=47 => self.bg_color = ANSI_COLORS[(p - 40) as usize],
                49 => self.bg_color = TERM_BG,
                90..=97 => self.fg_color = ANSI_COLORS[(p - 90 + 8) as usize],
                100..=107 => self.bg_color = ANSI_COLORS[(p - 100 + 8) as usize],
                _ => {}
            }
        }
    }

    /// Put a character at the current cursor position
    fn put_char(&mut self, ch: u8) {
        if self.cursor_y < self.rows && self.cursor_x < self.cols {
            self.chars[self.cursor_y][self.cursor_x] = ch;
            self.fg_buf[self.cursor_y][self.cursor_x] = self.fg_color;
            self.bg_buf[self.cursor_y][self.cursor_x] = self.bg_color;
        }
    }

    /// Check if scrolling is needed
    fn check_scroll(&mut self) {
        while self.cursor_y > self.scroll_bottom {
            self.scroll_up();
            self.cursor_y -= 1;
        }
    }

    /// Scroll the screen up by one line
    fn scroll_up(&mut self) {
        for y in self.scroll_top..self.scroll_bottom {
            self.chars[y] = self.chars[y + 1];
            self.fg_buf[y] = self.fg_buf[y + 1];
            self.bg_buf[y] = self.bg_buf[y + 1];
        }
        // Clear last line
        for x in 0..self.cols {
            self.chars[self.scroll_bottom][x] = b' ';
            self.fg_buf[self.scroll_bottom][x] = self.fg_color;
            self.bg_buf[self.scroll_bottom][x] = self.bg_color;
        }
    }

    /// Scroll the screen down by one line
    fn scroll_down(&mut self) {
        for y in (self.scroll_top + 1..=self.scroll_bottom).rev() {
            self.chars[y] = self.chars[y - 1];
            self.fg_buf[y] = self.fg_buf[y - 1];
            self.bg_buf[y] = self.bg_buf[y - 1];
        }
        for x in 0..self.cols {
            self.chars[self.scroll_top][x] = b' ';
            self.fg_buf[self.scroll_top][x] = self.fg_color;
            self.bg_buf[self.scroll_top][x] = self.bg_color;
        }
    }

    /// Clear screen
    fn clear_screen(&mut self) {
        for y in 0..self.rows {
            for x in 0..self.cols {
                self.chars[y][x] = b' ';
                self.fg_buf[y][x] = self.fg_color;
                self.bg_buf[y][x] = self.bg_color;
            }
        }
        self.cursor_x = 0;
        self.cursor_y = 0;
    }

    fn clear_from_cursor(&mut self) {
        for x in self.cursor_x..self.cols {
            self.chars[self.cursor_y][x] = b' ';
        }
        for y in (self.cursor_y + 1)..self.rows {
            for x in 0..self.cols {
                self.chars[y][x] = b' ';
            }
        }
    }

    fn clear_to_cursor(&mut self) {
        for y in 0..self.cursor_y {
            for x in 0..self.cols {
                self.chars[y][x] = b' ';
            }
        }
        for x in 0..=self.cursor_x {
            self.chars[self.cursor_y][x] = b' ';
        }
    }

    /// Reset terminal to defaults
    fn reset(&mut self) {
        self.fg_color = TERM_FG;
        self.bg_color = TERM_BG;
        self.bold = false;
        self.cursor_visible = true;
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.clear_screen();
    }

    /// Get the current screen content as a string (for testing)
    pub fn screen_text(&self) -> String {
        let mut s = String::new();
        for y in 0..self.rows {
            for x in 0..self.cols {
                s.push(self.chars[y][x] as char);
            }
            s.push('\n');
        }
        s
    }

    /// Render the terminal content to a backbuffer
    pub fn render(&self, buf: &mut Backbuffer, offset_x: u32, offset_y: u32) {
        for y in 0..self.rows {
            for x in 0..self.cols {
                let ch = self.chars[y][x];
                let fg = self.fg_buf[y][x];
                let bg = self.bg_buf[y][x];
                let px = offset_x + (x as u32) * 8;
                let py = offset_y + (y as u32) * 16;
                buf.draw_char(px, py, ch, fg, bg);
            }
        }

        // Draw cursor
        if self.cursor_visible {
            let cx = offset_x + (self.cursor_x as u32) * 8;
            let cy = offset_y + (self.cursor_y as u32) * 16 + 14;
            buf.hline(cx, cy, 8, TERM_CURSOR);
            buf.hline(cx, cy + 1, 8, TERM_CURSOR);
        }
    }
}
