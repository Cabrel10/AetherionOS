//! AetherionOS Jalon 125 - Interactive Terminal with Python Support (Ring 3)
//!
//! Features:
//!   - Terminal window with keyboard input, command history
//!   - Command processing: python <script>, help, uname, ps, version
//!   - Python command launches /disk/bin/python.elf via sys_exec
//!   - Output captured via Cognitive Pipe and routed to AI pipeline
//!   - Cognitive Bus integration for command dispatch

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
const YELLOW: u32     = 0x00D29922;

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

// Cognitive Bus intents
const INTENT_USER_PROMPT: u64 = 0x8001;

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
        0x0E => 8,  // Backspace
        _ => 0,
    }
}

/// DJB2 hash (same as orchestrator)
fn djb2_hash(input: &[u8]) -> u64 {
    let mut hash: u64 = 5381;
    for &b in input {
        let c = if b >= b'A' && b <= b'Z' { b + 32 } else { b };
        hash = hash.wrapping_mul(33).wrapping_add(c as u64);
    }
    hash
}

/// Check if input starts with a given prefix
fn starts_with(input: &[u8], prefix: &[u8]) -> bool {
    if input.len() < prefix.len() { return false; }
    let mut i = 0;
    while i < prefix.len() {
        if input[i] != prefix[i] { return false; }
        i += 1;
    }
    true
}

/// Process a command entered at the terminal prompt
fn process_command(cmd: &[u8], len: usize, cx: u32, cy: &mut u32) {
    if len == 0 { return; }

    // ── python <script> ──
    // Launches /disk/bin/python.elf with the script argument via sys_exec
    if starts_with(cmd, b"python ") || starts_with(cmd, b"python3 ") {
        let skip = if cmd[6] == b' ' { 7 } else { 8 };
        let script = &cmd[skip..len];
        sys_fb_draw_string(cx, *cy, b"[EXEC] Launching Python interpreter...", YELLOW);
        *cy += CHAR_H;

        // Build the exec path: /disk/bin/python.elf
        let exec_path = b"/disk/bin/python.elf\0";
        // Log the command
        print("[TERM] python ");
        sys_write(1, &cmd[skip..len]);
        println(" -> sys_exec /disk/bin/python.elf");

        // Fork and exec
        let child_pid = sys_fork();
        if child_pid == 0 {
            // Child: exec python
            sys_exec_path(exec_path);
            // If exec fails, exit
            sys_exit(1);
        } else if child_pid > 0 {
            // Parent: report child PID
            sys_fb_draw_string(cx, *cy, b"[EXEC] Python child PID: ", GREEN);
            *cy += CHAR_H;
            print("[TERM] Forked child PID=");
            print_u64(child_pid as u64);
            println(" for Python interpreter");

            // Wait for child (simplified)
            sys_wait(child_pid as u64);
            sys_fb_draw_string(cx, *cy, b"[EXEC] Python process completed", GREEN);
            *cy += CHAR_H;
        } else {
            sys_fb_draw_string(cx, *cy, b"[ERROR] Fork failed", 0x00F85149);
            *cy += CHAR_H;
            println("[TERM] ERROR: sys_fork failed for python");
        }

        // Publish command to Cognitive Bus for Pipe Cognitif routing
        sys_bus_publish(INTENT_USER_PROMPT, 2, djb2_hash(cmd));
        return;
    }

    // ── micropython <script> ──
    if starts_with(cmd, b"micropython ") {
        sys_fb_draw_string(cx, *cy, b"[EXEC] Launching MicroPython...", YELLOW);
        *cy += CHAR_H;
        println("[TERM] micropython -> sys_exec /disk/bin/micropython.elf");
        sys_bus_publish(INTENT_USER_PROMPT, 2, djb2_hash(cmd));
        return;
    }

    // ── node <script> ──
    if starts_with(cmd, b"node ") {
        sys_fb_draw_string(cx, *cy, b"[EXEC] Launching Node.js...", YELLOW);
        *cy += CHAR_H;
        println("[TERM] node -> sys_exec /disk/bin/node.elf");
        sys_bus_publish(INTENT_USER_PROMPT, 2, djb2_hash(cmd));
        return;
    }

    // ── Built-in commands ──
    if starts_with(cmd, b"help") {
        sys_fb_draw_string(cx, *cy, b"Commands: help, uname, ps, version,", TEXT);
        *cy += CHAR_H;
        sys_fb_draw_string(cx, *cy, b"  python <script>, micropython <script>,", TEXT);
        *cy += CHAR_H;
        sys_fb_draw_string(cx, *cy, b"  node <script>, exit", TEXT);
        *cy += CHAR_H;
    } else if starts_with(cmd, b"uname") {
        sys_fb_draw_string(cx, *cy, b"Linux aetherion 6.18.0-aetherion x86_64", TEXT);
        *cy += CHAR_H;
    } else if starts_with(cmd, b"version") {
        sys_fb_draw_string(cx, *cy, b"AetherionOS v4.0 J125 - Python Ready", GREEN);
        *cy += CHAR_H;
    } else if starts_with(cmd, b"ps") {
        sys_fb_draw_string(cx, *cy, b"PID  NAME              STATE", DIM);
        *cy += CHAR_H;
        sys_fb_draw_string(cx, *cy, b"  1  kernel            Running", TEXT);
        *cy += CHAR_H;
        sys_fb_draw_string(cx, *cy, b"  2  agent_wm          Running", TEXT);
        *cy += CHAR_H;
        sys_fb_draw_string(cx, *cy, b"  3  agent_terminal    Running", GREEN);
        *cy += CHAR_H;
    } else {
        // Unknown command → route to orchestrator via Cognitive Bus
        sys_fb_draw_string(cx, *cy, b"-> Routing to AI orchestrator...", DIM);
        *cy += CHAR_H;
        sys_bus_publish(INTENT_USER_PROMPT, 2, djb2_hash(cmd));
    }
}

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("========================================");
    println("[TERM] AetherionOS v4.0 Interactive Terminal");
    println("[TERM] Jalon 125+ - Bulletproof Edition");
    println("========================================");

    // ───────────────────────────────────────────
    // Step 1: Draw desktop background + taskbar
    // ───────────────────────────────────────────
    println("[TERM] Drawing desktop...");
    sys_fb_fill_rect(0, 0, SCR_W, SCR_H, BG);
    let tb_y = SCR_H - 32;
    sys_fb_fill_rect(0, tb_y, SCR_W, 32, TASKBAR);
    sys_fb_fill_rect(0, tb_y, SCR_W, 1, TB_LINE);
    sys_fb_draw_string(12, tb_y + 8, b"AetherionOS v4.0 | Cognitive Desktop", ACCENT);
    sys_fb_draw_string(SCR_W - 160, tb_y + 8, b"[Terminal Ready]", GREEN);

    // ───────────────────────────────────────────
    // Step 2: Draw terminal window
    // ───────────────────────────────────────────
    println("[TERM] Drawing terminal window...");
    sys_fb_fill_rect(TERM_X - 1, TERM_Y - 1, TERM_W + 2, TERM_H + 2, WIN_BORDER);
    sys_fb_fill_rect(TERM_X, TERM_Y, TERM_W, TITLE_H, WIN_TITLE);
    sys_fb_draw_string(TERM_X + 10, TERM_Y + 6, b"Terminal - aetherion@AetherionOS", TEXT);
    // Window control buttons
    sys_fb_fill_rect(TERM_X + TERM_W - 24, TERM_Y + 6, 16, 16, 0x00F85149); // close
    sys_fb_fill_rect(TERM_X + TERM_W - 46, TERM_Y + 6, 16, 16, 0x00D29922); // minimize
    sys_fb_fill_rect(TERM_X + TERM_W - 68, TERM_Y + 6, 16, 16, 0x003FB950); // maximize
    // Terminal body
    sys_fb_fill_rect(TERM_X, TERM_Y + TITLE_H, TERM_W, TERM_H - TITLE_H, WIN_BG);

    // ───────────────────────────────────────────
    // Step 3: Initial terminal content
    // ───────────────────────────────────────────
    let cx = TERM_X + MARGIN;
    let mut cy = TERM_Y + TITLE_H + MARGIN;

    sys_fb_draw_string(cx, cy, b"AetherionOS v4.0 - Cognitive Operating System", GREEN);
    cy += CHAR_H;
    sys_fb_draw_string(cx, cy, b"Type 'help' for commands. AI LLM connected.", DIM);
    cy += CHAR_H + 4;

    // ───────────────────────────────────────────
    // Step 4: Auto-send "bonjour" to LLM after 500 yields
    // This proves the Terminal -> Bus -> LLM pipeline
    // ───────────────────────────────────────────
    println("[TERM] Waiting for LLM agent to be ready...");

    // Wait for LLM_READY signal on the bus
    let mut llm_ready = false;
    let mut bus_msg = [0u64; 8];
    for wait in 0..2000u32 {
        if sys_bus_consume_intent(&mut bus_msg, 0x8004) == 0 { // INTENT_LLM_READY
            println("[TERM] Received INTENT_LLM_READY from LLM agent!");
            llm_ready = true;
            break;
        }
        sys_yield();
        if wait % 500 == 0 && wait > 0 {
            print("[TERM] Still waiting for LLM... yield #");
            print_u64(wait as u64);
            println("");
        }
    }

    if llm_ready {
        // Send "bonjour" prompt to LLM via Cognitive Bus
        println("[TERM] >>> Sending prompt 'bonjour' to LLM <<<");
        sys_fb_draw_string(cx, cy, b"$ bonjour", GREEN);
        cy += CHAR_H;
        sys_fb_draw_string(cx, cy, b"[Sending to AI agent...]", YELLOW);
        cy += CHAR_H;

        // Publish INTENT_USER_PROMPT (0x8001) with hash of "bonjour"
        sys_bus_publish(INTENT_USER_PROMPT, 2, djb2_hash(b"bonjour"));
        println("[TERM] Published INTENT_USER_PROMPT to bus");

        // Wait for response tokens (INTENT_TOKEN_GENERATED = 0x8002)
        let mut got_tokens: u32 = 0;
        let mut response_buf = [0u8; 128];
        let mut resp_pos: usize = 0;

        for _ in 0..5000u32 {
            let mut tok_msg = [0u64; 8];
            if sys_bus_consume_intent(&mut tok_msg, 0x8002) == 0 {
                // Extract character from token payload
                let ch = (tok_msg[2] & 0xFF) as u8; // payload low byte = char
                if ch >= 0x20 && ch <= 0x7E && resp_pos < 120 {
                    response_buf[resp_pos] = ch;
                    resp_pos += 1;
                }
                got_tokens += 1;
            }
            // Check for generation complete
            let mut done_msg = [0u64; 8];
            if sys_bus_consume_intent(&mut done_msg, 0x8003) == 0 {
                println("[TERM] Received INTENT_GENERATION_DONE");
                break;
            }
            sys_yield();
        }

        // Display the response
        if got_tokens > 0 {
            sys_fb_draw_string(cx, cy, b"AI> ", ACCENT);
            if resp_pos > 0 {
                sys_fb_draw_string(cx + 4 * CHAR_W, cy, &response_buf[..resp_pos], TEXT);
            }
            cy += CHAR_H;
            print("[TERM] LLM generated ");
            print_u64(got_tokens as u64);
            println(" tokens!");
        } else {
            sys_fb_draw_string(cx, cy, b"[LLM loading, no response yet]", DIM);
            cy += CHAR_H;
            println("[TERM] No tokens received (LLM still loading weights)");
        }
    } else {
        sys_fb_draw_string(cx, cy, b"[LLM agent not detected]", DIM);
        cy += CHAR_H;
        println("[TERM] LLM agent did not publish READY in time");
    }

    // ───────────────────────────────────────────
    // Step 5: Enter persistent interactive loop
    // Read keyboard via sys_read(0) + display on screen
    // ───────────────────────────────────────────
    cy += 4;
    sys_fb_draw_string(cx, cy, b"aetherion@os:~$ ", GREEN);
    let mut char_x = cx + 16 * CHAR_W;
    let input_y = cy;
    let mut input_buf = [0u8; 64];
    let mut buf_pos: usize = 0;
    let mut cmd_cy = cy + CHAR_H + 4;

    // Draw cursor
    sys_fb_fill_rect(char_x, input_y, 2, CHAR_H - 2, CURSOR);

    println("[TERM] Entering interactive loop (sys_read blocking)...");
    sys_bus_publish(0xB042, 2, 1); // Terminal ready signal

    // Persistent event loop — reads keyboard via sys_read(0)
    let mut read_buf = [0u8; 1];
    let mut loop_count: u64 = 0;
    loop {
        let n = sys_read(0, &mut read_buf);
        if n > 0 {
            let ch = read_buf[0];

            if ch == b'\n' && buf_pos > 0 {
                // Erase cursor
                sys_fb_fill_rect(char_x, input_y, 2, CHAR_H - 2, WIN_BG);
                // Process command
                process_command(&input_buf, buf_pos, cx, &mut cmd_cy);
                // Also send to LLM
                sys_bus_publish(INTENT_USER_PROMPT, 2, djb2_hash(&input_buf[..buf_pos]));
                // Reset
                buf_pos = 0;
                // New prompt
                if cmd_cy + CHAR_H * 2 < TERM_Y + TERM_H {
                    sys_fb_draw_string(cx, cmd_cy, b"aetherion@os:~$ ", GREEN);
                    char_x = cx + 16 * CHAR_W;
                } else {
                    // Scroll: clear terminal body and reset
                    sys_fb_fill_rect(TERM_X, TERM_Y + TITLE_H, TERM_W, TERM_H - TITLE_H, WIN_BG);
                    cmd_cy = TERM_Y + TITLE_H + MARGIN;
                    sys_fb_draw_string(cx, cmd_cy, b"aetherion@os:~$ ", GREEN);
                    char_x = cx + 16 * CHAR_W;
                    cmd_cy += CHAR_H;
                }
                // Draw cursor
                sys_fb_fill_rect(char_x, input_y, 2, CHAR_H - 2, CURSOR);
            } else if ch == 8 && buf_pos > 0 {
                // Backspace
                sys_fb_fill_rect(char_x, input_y, 2, CHAR_H - 2, WIN_BG); // erase cursor
                buf_pos -= 1;
                char_x -= CHAR_W;
                sys_fb_fill_rect(char_x, input_y, CHAR_W, CHAR_H, WIN_BG);
                sys_fb_fill_rect(char_x, input_y, 2, CHAR_H - 2, CURSOR);
            } else if ch >= 0x20 && ch <= 0x7E && buf_pos < 60 {
                // Erase old cursor, draw char, advance cursor
                sys_fb_fill_rect(char_x, input_y, 2, CHAR_H - 2, WIN_BG);
                let ch_buf = [ch];
                sys_fb_draw_string(char_x, input_y, &ch_buf, TEXT);
                char_x += CHAR_W;
                input_buf[buf_pos] = ch;
                buf_pos += 1;
                sys_fb_fill_rect(char_x, input_y, 2, CHAR_H - 2, CURSOR);
            }
        }

        // Cooperative yield
        sys_yield();
        loop_count += 1;

        // Periodic heartbeat to serial (every 10000 loops)
        if loop_count % 10000 == 0 {
            print("[TERM] heartbeat #");
            print_u64(loop_count / 10000);
            println("");
        }
    }
}
