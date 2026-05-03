// kernel/src/gui/mod.rs — Framebuffer GUI System for AetherionOS (Layer 7)
//
// Architecture:
//   - Double-buffered rendering (backbuffer → frontbuffer via vsync)
//   - PSF2 font rendering for terminal and UI text
//   - VT100-compatible terminal emulator
//   - Minimal compositing window manager
//   - PS/2 mouse cursor support
//
// Prerequisites:
//   - Limine framebuffer (1024×768×32bpp)
//   - PS/2 keyboard (already in IDT)
//   - PS/2 mouse driver (kernel/src/drivers/mouse.rs)

pub mod terminal;
pub mod compositor;

use alloc::vec::Vec;

/// RGBA color (32-bit, native framebuffer format)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub b: u8,
    pub g: u8,
    pub r: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { b, g, r, a: 0xFF }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { b, g, r, a }
    }

    pub fn to_u32(self) -> u32 {
        (self.a as u32) << 24 | (self.r as u32) << 16 | (self.g as u32) << 8 | self.b as u32
    }

    pub fn from_u32(val: u32) -> Self {
        Self {
            b: val as u8,
            g: (val >> 8) as u8,
            r: (val >> 16) as u8,
            a: (val >> 24) as u8,
        }
    }
}

// Common colors
pub const BLACK: Color = Color::rgb(0, 0, 0);
pub const WHITE: Color = Color::rgb(255, 255, 255);
pub const RED: Color = Color::rgb(255, 0, 0);
pub const GREEN: Color = Color::rgb(0, 255, 0);
pub const BLUE: Color = Color::rgb(0, 0, 255);
pub const CYAN: Color = Color::rgb(0, 255, 255);
pub const YELLOW: Color = Color::rgb(255, 255, 0);
pub const MAGENTA: Color = Color::rgb(255, 0, 255);
pub const GRAY: Color = Color::rgb(128, 128, 128);
pub const DARK_GRAY: Color = Color::rgb(64, 64, 64);
pub const LIGHT_GRAY: Color = Color::rgb(192, 192, 192);

// Terminal theme colors (Solarized Dark inspired)
pub const TERM_BG: Color = Color::rgb(0, 43, 54);
pub const TERM_FG: Color = Color::rgb(131, 148, 150);
pub const TERM_CURSOR: Color = Color::rgb(38, 139, 210);
pub const TERM_HIGHLIGHT: Color = Color::rgb(7, 54, 66);
pub const TITLE_BAR_BG: Color = Color::rgb(0, 80, 120);
pub const TITLE_BAR_FG: Color = Color::rgb(200, 220, 240);
pub const DESKTOP_BG: Color = Color::rgb(30, 30, 50);

/// Backbuffer for double-buffered rendering
pub struct Backbuffer {
    pub data: Vec<u32>,
    pub width: u32,
    pub height: u32,
    pub pitch: u32, // in bytes
}

impl Backbuffer {
    pub fn new(width: u32, height: u32) -> Self {
        let pitch = width * 4;
        let size = (width * height) as usize;
        let mut data = Vec::with_capacity(size);
        data.resize(size, DESKTOP_BG.to_u32());
        Self { data, width, height, pitch }
    }

    /// Put a pixel at (x, y)
    #[inline]
    pub fn put_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x < self.width && y < self.height {
            self.data[(y * self.width + x) as usize] = color.to_u32();
        }
    }

    /// Fill a rectangle
    pub fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        let c = color.to_u32();
        for row in y..core::cmp::min(y + h, self.height) {
            for col in x..core::cmp::min(x + w, self.width) {
                self.data[(row * self.width + col) as usize] = c;
            }
        }
    }

    /// Draw a horizontal line
    pub fn hline(&mut self, x: u32, y: u32, w: u32, color: Color) {
        let c = color.to_u32();
        if y < self.height {
            for col in x..core::cmp::min(x + w, self.width) {
                self.data[(y * self.width + col) as usize] = c;
            }
        }
    }

    /// Draw a vertical line
    pub fn vline(&mut self, x: u32, y: u32, h: u32, color: Color) {
        let c = color.to_u32();
        if x < self.width {
            for row in y..core::cmp::min(y + h, self.height) {
                self.data[(row * self.width + x) as usize] = c;
            }
        }
    }

    /// Draw an 8×16 glyph from the built-in font
    pub fn draw_char(&mut self, x: u32, y: u32, ch: u8, fg: Color, bg: Color) {
        let glyph = crate::font::get_font_glyph(ch);
        for row in 0..16u32 {
            let bits = glyph[row as usize];
            for col in 0..8u32 {
                let color = if bits & (0x80 >> col) != 0 { fg } else { bg };
                self.put_pixel(x + col, y + row, color);
            }
        }
    }

    /// Draw a string
    pub fn draw_string(&mut self, x: u32, y: u32, s: &str, fg: Color, bg: Color) {
        let mut cx = x;
        for &b in s.as_bytes() {
            if cx + 8 > self.width { break; }
            self.draw_char(cx, y, b, fg, bg);
            cx += 8;
        }
    }

    /// Copy backbuffer to the framebuffer hardware
    pub fn flip_to_fb(&self) {
        // Get the framebuffer info and blit
        if let Some(fb) = crate::framebuffer::get_info() {
            // Convert physical address to virtual using HHDM offset
            let phys_offset = crate::elf::phys_offset();
            let fb_ptr = (fb.phys_addr + phys_offset) as *mut u32;
            let fb_pitch_pixels = fb.stride / 4;

            for y in 0..core::cmp::min(self.height, fb.height) {
                let src_start = (y * self.width) as usize;
                let src_end = src_start + core::cmp::min(self.width, fb.width) as usize;
                let dst_start = (y * fb_pitch_pixels) as usize;

                unsafe {
                    let src = &self.data[src_start..src_end];
                    let dst = core::slice::from_raw_parts_mut(
                        fb_ptr.add(dst_start),
                        src.len(),
                    );
                    dst.copy_from_slice(src);
                }
            }
        }
    }

    /// Clear the entire buffer
    pub fn clear(&mut self, color: Color) {
        let c = color.to_u32();
        for pixel in self.data.iter_mut() {
            *pixel = c;
        }
    }
}

/// Draw a simple mouse cursor (arrow, 12×16 pixels)
pub fn draw_cursor(buf: &mut Backbuffer, mx: u32, my: u32) {
    #[rustfmt::skip]
    const CURSOR: [[u8; 12]; 16] = [
        [1,0,0,0,0,0,0,0,0,0,0,0],
        [1,1,0,0,0,0,0,0,0,0,0,0],
        [1,2,1,0,0,0,0,0,0,0,0,0],
        [1,2,2,1,0,0,0,0,0,0,0,0],
        [1,2,2,2,1,0,0,0,0,0,0,0],
        [1,2,2,2,2,1,0,0,0,0,0,0],
        [1,2,2,2,2,2,1,0,0,0,0,0],
        [1,2,2,2,2,2,2,1,0,0,0,0],
        [1,2,2,2,2,2,2,2,1,0,0,0],
        [1,2,2,2,2,2,2,2,2,1,0,0],
        [1,2,2,2,2,2,1,1,1,1,1,0],
        [1,2,2,1,2,2,1,0,0,0,0,0],
        [1,2,1,0,1,2,2,1,0,0,0,0],
        [1,1,0,0,1,2,2,1,0,0,0,0],
        [0,0,0,0,0,1,2,1,0,0,0,0],
        [0,0,0,0,0,1,1,0,0,0,0,0],
    ];

    for row in 0..16u32 {
        for col in 0..12u32 {
            let val = CURSOR[row as usize][col as usize];
            if val == 1 {
                buf.put_pixel(mx + col, my + row, BLACK);
            } else if val == 2 {
                buf.put_pixel(mx + col, my + row, WHITE);
            }
        }
    }
}

/// Initialize the GUI subsystem
pub fn init() -> bool {
    if let Some(fb) = crate::framebuffer::get_info() {
        crate::serial_println!("[GUI] Framebuffer: {}x{} @ {}bpp, stride={}",
            fb.width, fb.height, fb.bpp, fb.stride);
        true
    } else {
        crate::serial_println!("[GUI] No framebuffer available");
        false
    }
}

/// Run GUI self-tests
pub fn run_tests() {
    crate::serial_println!("\n========================================");
    crate::serial_println!("[GUI TESTS] Framebuffer + Window Manager (Layer 7)");
    crate::serial_println!("========================================\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // Test 1: Backbuffer creation
    crate::serial_write("  [TEST 1/8] Backbuffer alloc... ");
    let buf = Backbuffer::new(320, 200);
    if buf.data.len() == 64000 && buf.width == 320 && buf.height == 200 {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_write("FAIL\n");
        failed += 1;
    }

    // Test 2: Pixel operations
    crate::serial_write("  [TEST 2/8] Pixel put/get... ");
    let mut buf = Backbuffer::new(100, 100);
    buf.put_pixel(50, 50, RED);
    if buf.data[50 * 100 + 50] == RED.to_u32() {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_write("FAIL\n");
        failed += 1;
    }

    // Test 3: Fill rect
    crate::serial_write("  [TEST 3/8] Fill rect... ");
    buf.fill_rect(10, 10, 20, 20, GREEN);
    let ok = buf.data[15 * 100 + 15] == GREEN.to_u32()
        && buf.data[0] != GREEN.to_u32();
    if ok {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_write("FAIL\n");
        failed += 1;
    }

    // Test 4: Character rendering
    crate::serial_write("  [TEST 4/8] Char render... ");
    let mut buf = Backbuffer::new(100, 100);
    buf.draw_char(0, 0, b'A', WHITE, BLACK);
    let mut has_fg = false;
    for y in 0..16 {
        for x in 0..8 {
            if buf.data[y * 100 + x] == WHITE.to_u32() {
                has_fg = true;
            }
        }
    }
    if has_fg {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_write("FAIL\n");
        failed += 1;
    }

    // Test 5: Terminal VT100 basic
    crate::serial_write("  [TEST 5/8] VT100 terminal... ");
    {
        let mut term = terminal::Terminal::new(80, 25);
        term.write_str("Hello World");
        let ok = term.cursor_x == 11 && term.cursor_y == 0;
        if ok {
            crate::serial_write("OK\n");
            passed += 1;
        } else {
            crate::serial_write("FAIL\n");
            failed += 1;
        }
    }

    // Test 6: Terminal escape sequences
    crate::serial_write("  [TEST 6/8] VT100 escape seq... ");
    {
        let mut term = terminal::Terminal::new(80, 25);
        term.write_str("\x1b[2J"); // clear screen
        term.write_str("\x1b[5;10H"); // move to row 5 col 10
        let ok = term.cursor_y == 4 && term.cursor_x == 9;
        if ok {
            crate::serial_write("OK\n");
            passed += 1;
        } else {
            crate::serial_println!("FAIL (cursor at {},{})", term.cursor_x, term.cursor_y);
            failed += 1;
        }
    }

    // Test 7: Window manager creation
    crate::serial_write("  [TEST 7/8] Window manager... ");
    {
        let mut wm = compositor::WindowManager::new(1024, 768);
        let id1 = wm.create_window("Test", 100, 100, 400, 300);
        let id2 = wm.create_window("Test2", 200, 200, 400, 300);
        let ok = wm.windows.len() == 2 && id1 == 1 && id2 == 2;
        if ok {
            crate::serial_write("OK\n");
            passed += 1;
        } else {
            crate::serial_write("FAIL\n");
            failed += 1;
        }
    }

    // Test 8: Framebuffer availability
    crate::serial_write("  [TEST 8/8] Framebuffer present... ");
    if crate::framebuffer::get_info().is_some() {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_write("SKIP (no framebuffer in -nographic mode)\n");
    }

    crate::serial_println!("\n========================================");
    crate::serial_println!("[GUI TESTS] {}/{} passed", passed, passed + failed);
    if failed == 0 && passed > 0 {
        crate::serial_write("[GUI TESTS] ALL PASSED!\n");
    }
    crate::serial_println!("========================================");
}
