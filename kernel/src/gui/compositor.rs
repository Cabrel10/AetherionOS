// kernel/src/gui/compositor.rs — Minimal Window Compositor for AetherionOS (Layer 7)
//
// Manages a list of windows and composites them onto the backbuffer.
// Supports:
//   - Window creation/destruction
//   - Title bars with close buttons
//   - Window dragging (via PS/2 mouse)
//   - Focus management (click to focus, raise to top)
//   - Desktop background with system info
//   - Taskbar with window list and clock

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use super::*;
use super::terminal::Terminal;

/// A window in the compositor
pub struct Window {
    pub id: u32,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
    pub focused: bool,
    pub minimized: bool,
    pub terminal: Option<Box<Terminal>>,
}

/// Title bar height
const TITLE_HEIGHT: u32 = 24;
/// Window border width
const BORDER_WIDTH: u32 = 1;

impl Window {
    pub fn new(id: u32, title: &str, x: i32, y: i32, width: u32, height: u32) -> Self {
        let term_cols = ((width - 2 * BORDER_WIDTH) / 8) as usize;
        let term_rows = ((height - TITLE_HEIGHT - BORDER_WIDTH) / 16) as usize;

        Self {
            id,
            title: String::from(title),
            x,
            y,
            width,
            height,
            visible: true,
            focused: false,
            minimized: false,
            terminal: Some(Box::new(Terminal::new(term_cols, term_rows))),
        }
    }

    /// Total height including title bar
    pub fn total_height(&self) -> u32 {
        self.height + TITLE_HEIGHT
    }

    /// Check if a point is inside this window (including title bar)
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x
            && px < self.x + self.width as i32
            && py >= self.y
            && py < self.y + self.total_height() as i32
    }

    /// Check if a point is on the title bar
    pub fn title_bar_contains(&self, px: i32, py: i32) -> bool {
        px >= self.x
            && px < self.x + self.width as i32
            && py >= self.y
            && py < self.y + TITLE_HEIGHT as i32
    }

    /// Check if a point is on the close button
    pub fn close_button_contains(&self, px: i32, py: i32) -> bool {
        let close_x = self.x + self.width as i32 - 20;
        let close_y = self.y + 4;
        px >= close_x && px < close_x + 16 && py >= close_y && py < close_y + 16
    }

    /// Check if a point is on the minimize button
    pub fn minimize_button_contains(&self, px: i32, py: i32) -> bool {
        let min_x = self.x + self.width as i32 - 40;
        let min_y = self.y + 4;
        px >= min_x && px < min_x + 16 && py >= min_y && py < min_y + 16
    }

    /// Render this window to the backbuffer
    pub fn render(&self, buf: &mut Backbuffer) {
        if !self.visible || self.minimized { return; }

        let wx = self.x.max(0) as u32;
        let wy = self.y.max(0) as u32;

        // Drop shadow
        let shadow_off = 3u32;
        buf.fill_rect(wx + shadow_off, wy + shadow_off, self.width, self.total_height(),
            Color::rgba(0, 0, 0, 80));

        // Title bar background
        let title_bg = if self.focused { TITLE_BAR_BG } else { GRAY };
        buf.fill_rect(wx, wy, self.width, TITLE_HEIGHT, title_bg);

        // Title text
        let text_x = wx + 8;
        let text_y = wy + 4;
        buf.draw_string(text_x, text_y, &self.title, TITLE_BAR_FG, title_bg);

        // Close button [X] at top-right
        let close_x = wx + self.width - 20;
        let close_y = wy + 4;
        buf.fill_rect(close_x, close_y, 16, 16, RED);
        buf.draw_char(close_x + 4, close_y, b'X', WHITE, RED);

        // Minimize button [-] next to close
        let min_x = wx + self.width - 40;
        let min_y = wy + 4;
        buf.fill_rect(min_x, min_y, 16, 16, DARK_GRAY);
        buf.draw_char(min_x + 4, min_y, b'-', WHITE, DARK_GRAY);

        // Window border
        let content_y = wy + TITLE_HEIGHT;
        let content_h = self.height;
        let border_color = if self.focused { TITLE_BAR_BG } else { DARK_GRAY };
        buf.fill_rect(wx, content_y, self.width, BORDER_WIDTH, border_color);
        buf.fill_rect(wx, content_y + content_h - BORDER_WIDTH, self.width, BORDER_WIDTH, border_color);
        buf.vline(wx, content_y, content_h, border_color);
        buf.vline(wx + self.width - 1, content_y, content_h, border_color);

        // Content area background
        buf.fill_rect(
            wx + BORDER_WIDTH,
            content_y + BORDER_WIDTH,
            self.width - 2 * BORDER_WIDTH,
            content_h - 2 * BORDER_WIDTH,
            TERM_BG,
        );

        // Render terminal if present
        if let Some(ref term) = self.terminal {
            term.render(buf, wx + BORDER_WIDTH, content_y + BORDER_WIDTH);
        }
    }

    /// Write text to this window's terminal
    pub fn write(&mut self, text: &str) {
        if let Some(ref mut term) = self.terminal {
            term.write_str(text);
        }
    }
}

/// Window Manager state
pub struct WindowManager {
    pub windows: Vec<Window>,
    pub next_id: u32,
    pub mouse_x: i32,
    pub mouse_y: i32,
    pub dragging: Option<(u32, i32, i32)>, // (window_id, offset_x, offset_y)
    pub screen_width: u32,
    pub screen_height: u32,
    pub ticks: u64, // for status bar animations
}

impl WindowManager {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            windows: Vec::new(),
            next_id: 1,
            mouse_x: (width / 2) as i32,
            mouse_y: (height / 2) as i32,
            dragging: None,
            screen_width: width,
            screen_height: height,
            ticks: 0,
        }
    }

    /// Create a new terminal window
    pub fn create_window(&mut self, title: &str, x: i32, y: i32, width: u32, height: u32) -> u32 {
        let id = self.next_id;
        self.next_id += 1;

        let win = Window::new(id, title, x, y, width, height);
        self.windows.push(win);

        // Focus new window
        self.focus_window(id);

        crate::serial_println!("[WM] Created window {} '{}' at ({},{}) {}x{}",
            id, title, x, y, width, height);
        id
    }

    /// Close a window
    pub fn close_window(&mut self, id: u32) {
        self.windows.retain(|w| w.id != id);
        crate::serial_println!("[WM] Closed window {}", id);
    }

    /// Toggle minimize for a window
    pub fn toggle_minimize(&mut self, id: u32) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.minimized = !w.minimized;
            if !w.minimized {
                self.focus_window(id);
            }
        }
    }

    /// Focus a window (bring to top)
    pub fn focus_window(&mut self, id: u32) {
        for w in &mut self.windows {
            w.focused = w.id == id;
        }
        // Move focused window to end (rendered last = on top)
        if let Some(idx) = self.windows.iter().position(|w| w.id == id) {
            let win = self.windows.remove(idx);
            self.windows.push(win);
        }
    }

    /// Handle mouse click at (x, y)
    pub fn mouse_click(&mut self, x: i32, y: i32) {
        // Check taskbar first
        let taskbar_y = self.screen_height as i32 - 32;
        if y >= taskbar_y {
            self.handle_taskbar_click(x, y);
            return;
        }

        // Check windows in reverse order (top to bottom)
        let mut clicked_id = None;
        let mut on_title = false;
        let mut on_close = false;
        let mut on_minimize = false;

        for w in self.windows.iter().rev() {
            if w.minimized { continue; }
            if w.contains(x, y) {
                clicked_id = Some(w.id);
                on_title = w.title_bar_contains(x, y);
                on_close = w.close_button_contains(x, y);
                on_minimize = w.minimize_button_contains(x, y);
                break;
            }
        }

        if let Some(id) = clicked_id {
            if on_close {
                self.close_window(id);
            } else if on_minimize {
                self.toggle_minimize(id);
            } else {
                self.focus_window(id);
                if on_title {
                    if let Some(w) = self.windows.iter().find(|w| w.id == id) {
                        self.dragging = Some((id, x - w.x, y - w.y));
                    }
                }
            }
        }
    }

    /// Handle taskbar click
    fn handle_taskbar_click(&mut self, x: i32, _y: i32) {
        // Click on window entries in taskbar to toggle minimize
        let mut tx = 200i32;
        let ids: Vec<u32> = self.windows.iter().map(|w| w.id).collect();
        for &id in &ids {
            let title_len = self.windows.iter().find(|w| w.id == id)
                .map(|w| w.title.len().min(12))
                .unwrap_or(0);
            let width = ((title_len + 2) * 8) as i32;
            if x >= tx && x < tx + width {
                self.toggle_minimize(id);
                return;
            }
            tx += ((title_len + 3) * 8) as i32;
        }
    }

    /// Handle mouse release
    pub fn mouse_release(&mut self) {
        self.dragging = None;
    }

    /// Handle mouse move
    pub fn mouse_move(&mut self, x: i32, y: i32) {
        self.mouse_x = x.max(0).min(self.screen_width as i32 - 1);
        self.mouse_y = y.max(0).min(self.screen_height as i32 - 1);

        if let Some((id, ox, oy)) = self.dragging {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
                w.x = self.mouse_x - ox;
                w.y = self.mouse_y - oy;
            }
        }
    }

    /// Route a keyboard scancode to the focused window's terminal
    pub fn handle_keyboard(&mut self, scancode: u8, shift: bool, ctrl: bool) {
        if let Some(w) = self.windows.iter_mut().rev().find(|w| w.focused && !w.minimized) {
            if let Some(ref mut term) = w.terminal {
                term.handle_key(scancode, shift, ctrl);
            }
        }
    }

    /// Write to a specific window's terminal
    pub fn write_to_window(&mut self, id: u32, text: &str) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == id) {
            w.write(text);
        }
    }

    /// Tick (called from timer ISR for animations)
    pub fn tick(&mut self) {
        self.ticks += 1;
    }

    /// Render all windows to a backbuffer
    pub fn render(&self, buf: &mut Backbuffer) {
        // Desktop background gradient
        for y in 0..buf.height.saturating_sub(32) {
            let r = (30.0 + (y as f32 / buf.height as f32) * 30.0) as u8;
            let g = (30.0 + (y as f32 / buf.height as f32) * 20.0) as u8;
            let b = (50.0 + (y as f32 / buf.height as f32) * 40.0) as u8;
            let c = Color::rgb(r, g, b).to_u32();
            let start = (y * buf.width) as usize;
            for x in 0..buf.width as usize {
                buf.data[start + x] = c;
            }
        }

        // Desktop info text
        buf.draw_string(20, 20, "AetherionOS v4.3.0-phase8", WHITE, Color::rgba(0, 0, 0, 0));
        buf.draw_string(20, 40, "Layers: Network | ext2 | APK | GUI | LLM", LIGHT_GRAY, Color::rgba(0, 0, 0, 0));

        // Taskbar at bottom
        let taskbar_y = buf.height - 32;
        buf.fill_rect(0, taskbar_y, buf.width, 32, Color::rgb(20, 20, 40));
        buf.hline(0, taskbar_y, buf.width, Color::rgb(60, 60, 100));

        // Start button
        buf.fill_rect(4, taskbar_y + 4, 60, 24, TITLE_BAR_BG);
        buf.draw_string(12, taskbar_y + 8, "Start", WHITE, TITLE_BAR_BG);

        // AetherionOS branding
        buf.draw_string(80, taskbar_y + 8, "|", GRAY, Color::rgb(20, 20, 40));
        buf.draw_string(96, taskbar_y + 8, "AetherionOS", CYAN, Color::rgb(20, 20, 40));

        // Window list in taskbar
        let mut tx = 200u32;
        for w in &self.windows {
            let bg = if w.focused { TITLE_BAR_BG }
                     else if w.minimized { Color::rgb(40, 40, 60) }
                     else { DARK_GRAY };
            let title_short: String = w.title.chars().take(12).collect();
            let btn_width = (title_short.len() as u32 + 2) * 8;
            buf.fill_rect(tx, taskbar_y + 4, btn_width, 24, bg);
            buf.draw_string(tx + 8, taskbar_y + 8, &title_short, WHITE, bg);
            tx += btn_width + 4;
        }

        // System tray (right side of taskbar)
        let tray_x = buf.width - 100;
        let uptime = self.ticks / 100; // approximate seconds
        let time_str = format!("{}:{:02}", uptime / 60, uptime % 60);
        buf.draw_string(tray_x, taskbar_y + 8, &time_str, WHITE, Color::rgb(20, 20, 40));

        // Render windows (bottom to top)
        for w in &self.windows {
            w.render(buf);
        }

        // Mouse cursor on top
        draw_cursor(buf, self.mouse_x as u32, self.mouse_y as u32);
    }
}
