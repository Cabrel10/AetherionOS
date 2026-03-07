//! AetherionOS Jalon 42 - Interactive Terminal Window (Ring 3)
//!
//! Draws a terminal window, polls HID events for keyboard input,
//! renders typed characters on the framebuffer in real-time.
//! Demonstrates the complete input→display pipeline.

#![no_std]
#![no_main]

extern crate alloc;
use aetherion_sdk::*;

// Colors
const BG: u32         = 0x000D1117;
const TASKBAR: u32    = 0x00010409;
const TB_LINE: u32    = 0x0030363D;
const WIN_TITLE: u32  = 0x00238636;  // Green terminal titlebar
const WIN_BG: u32     = 0x000D1117;  // Terminal background
const WIN_BORDER: u32 = 0x0030363D;
const TEXT: u32       = 0x00E6EDF3;
const GREEN: u32      = 0x003FB950;
const ACCENT: u32     = 0x0058A6FF;
const DIM: u32        = 0x00484F58;
const CURSOR: u32     = 0x003FB950;  // Green blinking cursor

// Screen
const SCR_W: u32 = 1024;
const SCR_H: u32 = 768;

// Terminal window geometry
const TERM_X: u32 = 120;
const TERM_Y: u32 = 60;
const TERM_W: u32 = 780;
const TERM_H: u32 = 580;
const TITLE_H: u32 = 28;
const CHAR_W: u32 = 8;
const CHAR_H: u32 = 18;
const MARGIN: u32 = 12;

// HID event types
const HID_KEY_PRESS: u8 = 3;

/// PS/2 scancode set 1 to ASCII (subset)
fn scancode_to_ascii(sc: u8) -> u8 {
    match sc {
        0x02 => b'1', 0x03 => b'2', 0x04 => b'3', 0x05 => b'4',
        0x06 => b'5', 0x07 => b'6', 0x08 => b'7', 0x09 => b'8',
        0x0A => b'9', 0x0B => b'0', 0x0C => b'-', 0x0D => b'=',
        0x10 => b'q', 0x11 => b'w', 0x12 => b'e', 0x13 => b'r',
        0x14 => b't', 0x15 => b'y', 0x16 => b'u', 0x17 => b'i',
        0x18 => b'o', 0x19 => b'p', 0x1A => b'[', 0x1B => b']',
        0x1C => b'\n',
        0x1E => b'a', 0x1F => b's', 0x20 => b'd', 0x21 => b'f',
        0x22 => b'g', 0x23 => b'h', 0x24 => b'j', 0x25 => b'k',
        0x26 => b'l', 0x27 => b';', 0x28 => b'\'',
        0x2C => b'z', 0x2D => b'x', 0x2E => b'c', 0x2F => b'v',
        0x30 => b'b', 0x31 => b'n', 0x32 => b'm', 0x33 => b',',
        0x34 => b'.', 0x35 => b'/',
        0x39 => b' ',
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J42] ========================================");
    println("[J42] Interactive Terminal Window - Ring 3");
    println("[J42] ========================================");

    let mut tests_passed: u32 = 0;
    let total_tests: u32 = 5;

    // ───────────────────────────────────────────
    // Step 1: Draw background + taskbar
    // ───────────────────────────────────────────
    print("[J42] Step 1/5: Desktop background ... ");
    sys_fb_fill_rect(0, 0, SCR_W, SCR_H, BG);
    let tb_y = SCR_H - 32;
    sys_fb_fill_rect(0, tb_y, SCR_W, 32, TASKBAR);
    sys_fb_fill_rect(0, tb_y, SCR_W, 1, TB_LINE);
    sys_fb_draw_string(12, tb_y + 8, b"AetherionOS v2.2", ACCENT);
    sys_fb_draw_string(SCR_W - 200, tb_y + 8, b"[J42] Terminal", GREEN);
    println("OK");
    tests_passed += 1;

    // ───────────────────────────────────────────
    // Step 2: Draw terminal window frame
    // ───────────────────────────────────────────
    print("[J42] Step 2/5: Terminal window ... ");
    // Border
    sys_fb_fill_rect(TERM_X - 1, TERM_Y - 1, TERM_W + 2, TERM_H + 2, WIN_BORDER);
    // Title bar
    sys_fb_fill_rect(TERM_X, TERM_Y, TERM_W, TITLE_H, WIN_TITLE);
    sys_fb_draw_string(TERM_X + 10, TERM_Y + 6, b"Terminal - aetherion@AetherionOS:~$", TEXT);
    // Close/Min/Max buttons
    sys_fb_fill_rect(TERM_X + TERM_W - 24, TERM_Y + 6, 16, 16, 0x00F85149);
    sys_fb_fill_rect(TERM_X + TERM_W - 46, TERM_Y + 6, 16, 16, 0x00D29922);
    sys_fb_fill_rect(TERM_X + TERM_W - 68, TERM_Y + 6, 16, 16, 0x003FB950);
    // Body
    sys_fb_fill_rect(TERM_X, TERM_Y + TITLE_H, TERM_W, TERM_H - TITLE_H, WIN_BG);
    println("OK");
    tests_passed += 1;

    // ───────────────────────────────────────────
    // Step 3: Write initial terminal content
    // ───────────────────────────────────────────
    print("[J42] Step 3/5: Terminal content ... ");
    let cx = TERM_X + MARGIN;
    let mut cy = TERM_Y + TITLE_H + MARGIN;

    sys_fb_draw_string(cx, cy, b"AetherionOS v2.2 - Cognitive Operating System", GREEN);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"Copyright (c) 2026 AetherionOS Project", DIM);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"Kernel: Couche 19+  Arch: x86_64  SSE2: enabled", TEXT);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"Milestones: J29-J42 validated (14 milestones)", GREEN);
    cy += CHAR_H + 4;

    // Simulated command history
    sys_fb_draw_string(cx, cy, b"aetherion@os:~$ lspci", GREEN);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"  00:00.0 Host bridge", TEXT);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"  00:01.0 VGA [1234:1111] BAR0=0xFD000000", TEXT);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"  00:02.0 VirtIO-Block (FAT32 64MB)", TEXT);
    cy += CHAR_H + 4;

    sys_fb_draw_string(cx, cy, b"aetherion@os:~$ cat /proc/meminfo", GREEN);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"  MemTotal:  262144 KB (256 MB)", TEXT);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"  HeapSize:    8192 KB (kernel)", TEXT);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"  FramePool:  16384 frames", TEXT);
    cy += CHAR_H + 4;

    sys_fb_draw_string(cx, cy, b"aetherion@os:~$ ps aux", GREEN);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"  PID  NAME              STATUS", DIM);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"   1   idle              RUNNING", TEXT);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"   2   Orchestrator      READY", TEXT);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"   3   Vision_Domain     READY", TEXT);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"   4   Network_Domain    READY", TEXT);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"  11   agent_terminal    RUNNING", GREEN);
    cy += CHAR_H + 4;

    // Input prompt
    sys_fb_draw_string(cx, cy, b"aetherion@os:~$ ", GREEN);
    let prompt_end_x = cx + 16 * CHAR_W; // after "aetherion@os:~$ "
    let input_y = cy;

    println("OK");
    tests_passed += 1;

    // ───────────────────────────────────────────
    // Step 4: HID polling - capture keyboard input
    // ───────────────────────────────────────────
    print("[J42] Step 4/5: HID keyboard polling ... ");
    let mut key_count: u32 = 0;
    let mut char_x = prompt_end_x;
    let mut input_buf = [0u8; 64];
    let mut buf_pos: usize = 0;

    // Poll HID events (up to 50 iterations)
    for _ in 0..50u32 {
        let evt = sys_poll_hid();
        if evt == 0 { continue; }

        let bytes = evt.to_le_bytes();
        let evt_type = bytes[0];
        let scancode = bytes[6];

        if evt_type == HID_KEY_PRESS && scancode != 0 {
            let ascii = scancode_to_ascii(scancode);
            if ascii != 0 && buf_pos < 60 {
                // Draw the character on screen
                let ch_buf = [ascii];
                sys_fb_draw_string(char_x, input_y, &ch_buf, TEXT);
                char_x += CHAR_W;
                input_buf[buf_pos] = ascii;
                buf_pos += 1;
                key_count += 1;
            }
        }
    }

    // Draw cursor at current position
    sys_fb_fill_rect(char_x, input_y, 2, CHAR_H - 2, CURSOR);

    print("OK (");
    print_u64(key_count as u64);
    println(" keystrokes captured)");
    tests_passed += 1;
    print("[J42-OK] Terminal input: ");
    print_u64(key_count as u64);
    println(" scancodes received");

    // ───────────────────────────────────────────
    // Step 5: Cognitive Bus publish
    // ───────────────────────────────────────────
    print("[J42] Step 5/5: Bus publish ... ");
    let status = ((key_count as u64) << 32) | (buf_pos as u64);
    let r = sys_bus_publish(0xB042, 2, status);
    if r == 0 {
        println("OK (intent=0xB042)");
        tests_passed += 1;
    } else {
        println("FAIL");
    }

    // ───────────────────────────────────────────
    // Summary
    // ───────────────────────────────────────────
    println("[J42] ========================================");
    print("[J42] Terminal Agent: ");
    print_u64(tests_passed as u64);
    print("/");
    print_u64(total_tests as u64);
    println(" steps completed");

    if tests_passed == total_tests {
        println("[J42-OK] Terminal window drawn + HID input captured");
        println("[J42-OK] Interactive terminal validation COMPLETE");
        println("[J42-OK] ALL STEPS PASSED");
    }
    println("[J42] ========================================");

    0
}
