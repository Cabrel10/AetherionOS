//! AetherionOS Jalon 39 - Framebuffer GUI Test Agent (Ring 3)
//!
//! Draws a minimal desktop UI using framebuffer syscalls:
//! - Background gradient
//! - Taskbar at bottom
//! - OS name title
//! - Clock placeholder
//! Publishes status on Cognitive Bus (intent 0x9039).

#![no_std]
#![no_main]

extern crate alloc;
use aetherion_sdk::*;

// Colors (ARGB format for 32-bit framebuffer)
const COLOR_BG: u32 = 0x00203040;         // Dark blue-gray background
const COLOR_TASKBAR: u32 = 0x00101820;    // Darker taskbar
const COLOR_ACCENT: u32 = 0x004488CC;     // Blue accent
const COLOR_TEXT: u32 = 0x00E0E0E0;       // Light gray text
const COLOR_GREEN: u32 = 0x0040C040;      // Green for status
const COLOR_TITLE: u32 = 0x0060A0E0;      // Light blue for title

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J39] ========================================");
    println("[J39] Framebuffer GUI Test - Ring 3 Validation");
    println("[J39] ========================================");

    let mut tests_passed: u32 = 0;
    let total_tests: u32 = 5;

    // -----------------------------------------------------------
    // Test 1: Get framebuffer info
    // -----------------------------------------------------------
    print("[J39] Test 1/5: sys_fb_get_info() ... ");
    let mut info = [0u64; 4];
    let result = sys_fb_get_info(&mut info);
    if result != 0 {
        let width = info[0] as u32;
        let height = info[1] as u32;
        let stride = info[2] as u32;
        let bpp = info[3] as u32;
        print("OK (");
        print_u64(width as u64);
        print("x");
        print_u64(height as u64);
        print(" stride=");
        print_u64(stride as u64);
        print(" bpp=");
        print_u64(bpp as u64);
        println(")");
        tests_passed += 1;
    } else {
        println("FAIL (no framebuffer)");
        // Can't continue without FB
        println("[J39-FAIL] No framebuffer available");
        sys_bus_publish(0x9039, 2, 0);
        return 1;
    }

    let width = info[0] as u32;
    let height = info[1] as u32;

    // -----------------------------------------------------------
    // Test 2: Fill background
    // -----------------------------------------------------------
    print("[J39] Test 2/5: Fill background ... ");
    let r = sys_fb_fill_rect(0, 0, width, height, COLOR_BG);
    if r == 0 {
        println("OK");
        tests_passed += 1;
    } else {
        println("FAIL");
    }

    // -----------------------------------------------------------
    // Test 3: Draw taskbar (bottom 40px)
    // -----------------------------------------------------------
    print("[J39] Test 3/5: Draw taskbar ... ");
    let taskbar_y = height - 40;
    let r1 = sys_fb_fill_rect(0, taskbar_y, width, 40, COLOR_TASKBAR);
    // Accent line at top of taskbar
    let r2 = sys_fb_fill_rect(0, taskbar_y, width, 2, COLOR_ACCENT);
    if r1 == 0 && r2 == 0 {
        println("OK");
        tests_passed += 1;
    } else {
        println("FAIL");
    }

    // -----------------------------------------------------------
    // Test 4: Draw OS name and title text
    // -----------------------------------------------------------
    print("[J39] Test 4/5: Draw text ... ");
    // Title in center-ish
    let title = b"AetherionOS v2.2";
    let title_x = (width / 2) - ((title.len() as u32) * 8 / 2);
    let r1 = sys_fb_draw_string(title_x, 20, title, COLOR_TITLE);

    // Subtitle
    let subtitle = b"Cognitive Desktop - Jalon 39";
    let sub_x = (width / 2) - ((subtitle.len() as u32) * 8 / 2);
    let r2 = sys_fb_draw_string(sub_x, 44, subtitle, COLOR_TEXT);

    // Taskbar text
    let tb_text = b"[Start]  AetherionOS  [J39-OK]";
    let r3 = sys_fb_draw_string(8, taskbar_y + 12, tb_text, COLOR_TEXT);

    // Status indicator
    let status_text = b"Ready";
    let status_x = width - 60;
    let r4 = sys_fb_draw_string(status_x, taskbar_y + 12, status_text, COLOR_GREEN);

    if r1 == 0 && r2 == 0 && r3 == 0 && r4 == 0 {
        println("OK");
        tests_passed += 1;
    } else {
        println("FAIL");
    }

    // -----------------------------------------------------------
    // Test 5: Draw decorative elements
    // -----------------------------------------------------------
    print("[J39] Test 5/5: Draw UI elements ... ");
    // Window-like box
    let win_x: u32 = 100;
    let win_y: u32 = 80;
    let win_w: u32 = 400;
    let win_h: u32 = 300;

    // Window title bar
    sys_fb_fill_rect(win_x, win_y, win_w, 24, COLOR_ACCENT);
    sys_fb_draw_string(win_x + 8, win_y + 4, b"System Info", COLOR_TEXT);

    // Window body
    sys_fb_fill_rect(win_x, win_y + 24, win_w, win_h - 24, 0x00182838);

    // Window content text
    sys_fb_draw_string(win_x + 16, win_y + 40, b"OS: AetherionOS v2.2", COLOR_TEXT);
    sys_fb_draw_string(win_x + 16, win_y + 64, b"Kernel: Couche 19+", COLOR_TEXT);
    sys_fb_draw_string(win_x + 16, win_y + 88, b"Milestones: J29-J39", COLOR_TEXT);
    sys_fb_draw_string(win_x + 16, win_y + 112, b"GGUF: OK  SSE2: OK", COLOR_GREEN);
    sys_fb_draw_string(win_x + 16, win_y + 136, b"FAT32: OK  VirtIO: OK", COLOR_GREEN);
    sys_fb_draw_string(win_x + 16, win_y + 160, b"Mouse: IRQ12  KB: IRQ1", COLOR_TEXT);
    sys_fb_draw_string(win_x + 16, win_y + 184, b"Bus: Cognitive IPC", COLOR_TEXT);

    // Status squares
    sys_fb_fill_rect(win_x + 16, win_y + 220, 20, 20, COLOR_GREEN);   // GGUF OK
    sys_fb_fill_rect(win_x + 48, win_y + 220, 20, 20, COLOR_GREEN);   // SSE2 OK
    sys_fb_fill_rect(win_x + 80, win_y + 220, 20, 20, COLOR_GREEN);   // FAT32 OK
    sys_fb_fill_rect(win_x + 112, win_y + 220, 20, 20, COLOR_ACCENT); // Network

    let r = sys_fb_draw_string(win_x + 16, win_y + 250, b"[All systems nominal]", COLOR_GREEN);
    if r == 0 {
        println("OK");
        tests_passed += 1;
    } else {
        println("FAIL");
    }

    // -----------------------------------------------------------
    // Cognitive Bus publish
    // -----------------------------------------------------------
    sys_bus_publish(0x9039, 2, tests_passed as u64);

    // -----------------------------------------------------------
    // Summary
    // -----------------------------------------------------------
    println("[J39] ========================================");
    print("[J39] GUI Test Agent: ");
    print_u64(tests_passed as u64);
    print("/");
    print_u64(total_tests as u64);
    println(" tests passed");

    if tests_passed == total_tests {
        println("[J39-OK] Framebuffer GUI Test validation COMPLETE");
    } else {
        println("[J39-FAIL] Some tests failed");
    }

    println("[J39] ========================================");

    0
}
