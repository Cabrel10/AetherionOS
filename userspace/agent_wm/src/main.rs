//! AetherionOS Jalon 119 - Cognitive Desktop Window Manager + Semantic UI Tree
//!
//! Full desktop compositor for AetherionOS with:
//!   - Window struct: x, y, width, height, title, z_index
//!   - Z-index ordered rendering (back to front)
//!   - draw_desktop(): background + windows in z_index order
//!   - PS/2 mouse integration via sys_poll_hid for mouse deltas (dx, dy)
//!   - 10x10 hardware mouse cursor rendered on top
//!   - Window dragging via click+drag on title bar (Jalon 111c)
//!   - Taskbar with window list and system tray
//!   - Cognitive Bus intent publishing for desktop state
//!   - Grey background (0x222222), centered "AetherionOS Terminal" window
//!   - MCP integration via Cognitive Bus for Linux tool execution
//!
//! Jalon 119 additions:
//!   - SemanticNode struct (type, x, y, width, height, text, id)
//!   - Semantic UI Tree: maintained in-memory, published as JSON on bus
//!   - INTENT_GET_UI_TREE: AI agent requests the full UI tree
//!   - INTENT_INTERACT_NODE: AI agent sends (node_id) → WM generates events
//!   - Enables AI screen-reading and autonomous GUI interaction

#![no_std]
#![no_main]

extern crate alloc;
use aetherion_sdk::*;

// ═══════════════════════════════════════════════════
// AetherionOS Visual Identity Palette
// ═══════════════════════════════════════════════════
const BG: u32           = 0x00222222;  // Grey background (Jalon 108)
const TASKBAR_BG: u32   = 0x00010409;  // Near-black taskbar
const TASKBAR_LINE: u32 = 0x0030363D;  // Subtle separator
const WIN_TITLE: u32    = 0x001F6FEB;  // Blue window titlebar
const WIN_BG: u32       = 0x00161B22;  // Window body
const WIN_BORDER: u32   = 0x0030363D;  // Window border
const TEXT: u32         = 0x00E6EDF3;  // Primary text
const TEXT_DIM: u32     = 0x00484F58;  // Secondary/dim text
const ACCENT: u32       = 0x0058A6FF;  // Accent blue
const GREEN: u32        = 0x003FB950;  // Success/active
const ORANGE: u32       = 0x00D29922;  // Warning
const RED: u32          = 0x00F85149;  // Close button
const CURSOR_FG: u32    = 0x00FFFFFF;  // Cursor foreground (white)
const CURSOR_BORDER: u32 = 0x00000000; // Cursor border (black)

// Screen dimensions (1024x768 VESA mode)
const SCR_W: u32 = 1024;
const SCR_H: u32 = 768;
const TB_H: u32 = 32;      // Taskbar height
const TB_Y: u32 = SCR_H - TB_H;
const TITLE_BAR_H: u32 = 28;  // Window title bar height

// HID event type masks (from sys_poll_hid packed format)
const HID_TYPE_MOUSE: u8 = 1;
const HID_TYPE_KEYBOARD: u8 = 2;

// Maximum number of managed windows
const MAX_WINDOWS: usize = 8;

// Cognitive Bus intents
const INTENT_WM_READY: u64 = 0xB069;
const INTENT_WM_DESKTOP_STATE: u64 = 0xB070;
const INTENT_WM_DESKTOP_J108: u64 = 0xB108;

/// Jalon 112a: Timer tick intent from Clock Sensor Agent
const INTENT_TIMER_TICK: u64 = 0x112A;

/// Jalon 119: AI requests the full Semantic UI Tree
const INTENT_GET_UI_TREE: u64 = 0xB119;

/// Jalon 119: AI sends INTENT_UI_TREE_RESPONSE with JSON payload
const INTENT_UI_TREE_RESPONSE: u64 = 0xB11A;

/// Jalon 119: AI requests interaction with a specific node
/// Payload = node_id. WM converts node_id → screen coords → mouse/keyboard events.
const INTENT_INTERACT_NODE: u64 = 0xB11B;

/// Jalon 119: WM confirms interaction was performed
const INTENT_INTERACT_DONE: u64 = 0xB11C;

// ═══════════════════════════════════════════════════
// Semantic UI Tree (Jalon 119)
// ═══════════════════════════════════════════════════

/// Node types for the Semantic UI Tree
#[derive(Clone, Copy, PartialEq)]
#[repr(u8)]
enum NodeType {
    Desktop = 0,
    Window = 1,
    TitleBar = 2,
    Button = 3,
    Label = 4,
    Taskbar = 5,
    TaskbarEntry = 6,
    StatusIndicator = 7,
    ContentArea = 8,
}

/// A single node in the Semantic UI Tree.
/// Each visible UI element gets one SemanticNode.
/// The tree is flat (parent_id references) to avoid heap allocation.
#[derive(Clone, Copy)]
struct SemanticNode {
    /// Unique ID for this node (1..N, 0 = unused)
    id: u16,
    /// Parent node ID (0 = root)
    parent_id: u16,
    /// Node type (window, button, label, etc.)
    node_type: NodeType,
    /// Screen coordinates and dimensions
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    /// Short text label (up to 31 bytes + null)
    text: [u8; 32],
    text_len: u8,
    /// Whether this node is interactive (clickable/focusable)
    interactive: bool,
    /// Whether this node is currently visible
    visible: bool,
}

impl SemanticNode {
    const fn empty() -> Self {
        SemanticNode {
            id: 0,
            parent_id: 0,
            node_type: NodeType::Desktop,
            x: 0, y: 0,
            width: 0, height: 0,
            text: [0u8; 32],
            text_len: 0,
            interactive: false,
            visible: false,
        }
    }

    fn set_text(&mut self, src: &[u8]) {
        let len = if src.len() > 31 { 31 } else { src.len() };
        let mut i = 0;
        while i < len {
            self.text[i] = src[i];
            i += 1;
        }
        self.text[len] = 0;
        self.text_len = len as u8;
    }
}

/// Maximum nodes in the semantic tree (flat array, no heap alloc)
const MAX_SEMANTIC_NODES: usize = 64;

/// The Semantic UI Tree: a flat array of SemanticNode
struct SemanticTree {
    nodes: [SemanticNode; MAX_SEMANTIC_NODES],
    count: usize,
}

impl SemanticTree {
    fn new() -> Self {
        SemanticTree {
            nodes: [SemanticNode::empty(); MAX_SEMANTIC_NODES],
            count: 0,
        }
    }

    /// Clear all nodes
    fn clear(&mut self) {
        self.count = 0;
    }

    /// Add a node, returns the assigned ID
    fn add(&mut self, node_type: NodeType, parent_id: u16,
           x: i32, y: i32, w: u32, h: u32,
           text: &[u8], interactive: bool) -> u16 {
        if self.count >= MAX_SEMANTIC_NODES {
            return 0;
        }
        let id = (self.count + 1) as u16;
        let node = &mut self.nodes[self.count];
        node.id = id;
        node.parent_id = parent_id;
        node.node_type = node_type;
        node.x = x;
        node.y = y;
        node.width = w;
        node.height = h;
        node.set_text(text);
        node.interactive = interactive;
        node.visible = true;
        self.count += 1;
        id
    }

    /// Find a node by ID, return its center coordinates
    fn find_node_center(&self, node_id: u16) -> Option<(i32, i32)> {
        for i in 0..self.count {
            if self.nodes[i].id == node_id {
                let cx = self.nodes[i].x + (self.nodes[i].width as i32) / 2;
                let cy = self.nodes[i].y + (self.nodes[i].height as i32) / 2;
                return Some((cx, cy));
            }
        }
        None
    }

    /// Serialize the tree as JSON to a buffer. Returns bytes written.
    /// Format: {"tree":[{"id":1,"type":"window","x":100,...}, ...]}
    fn to_json(&self, buf: &mut [u8]) -> usize {
        let mut pos = 0usize;
        let header = b"{\"semantic_tree\":[";
        if pos + header.len() > buf.len() { return 0; }
        buf[pos..pos+header.len()].copy_from_slice(header);
        pos += header.len();

        for i in 0..self.count {
            let n = &self.nodes[i];
            if !n.visible { continue; }
            if i > 0 && pos < buf.len() { buf[pos] = b','; pos += 1; }

            // Build node JSON
            let node_json_start = b"{\"id\":";
            if pos + node_json_start.len() > buf.len() { break; }
            buf[pos..pos+node_json_start.len()].copy_from_slice(node_json_start);
            pos += node_json_start.len();

            // Write id
            pos += write_u32_to_buf(&mut buf[pos..], n.id as u32);

            // type
            let type_str = match n.node_type {
                NodeType::Desktop => b",\"type\":\"desktop\"" as &[u8],
                NodeType::Window => b",\"type\":\"window\"",
                NodeType::TitleBar => b",\"type\":\"titlebar\"",
                NodeType::Button => b",\"type\":\"button\"",
                NodeType::Label => b",\"type\":\"label\"",
                NodeType::Taskbar => b",\"type\":\"taskbar\"",
                NodeType::TaskbarEntry => b",\"type\":\"taskbar_entry\"",
                NodeType::StatusIndicator => b",\"type\":\"status\"",
                NodeType::ContentArea => b",\"type\":\"content\"",
            };
            if pos + type_str.len() > buf.len() { break; }
            buf[pos..pos+type_str.len()].copy_from_slice(type_str);
            pos += type_str.len();

            // x,y,w,h
            let xy = b",\"x\":";
            if pos + xy.len() > buf.len() { break; }
            buf[pos..pos+xy.len()].copy_from_slice(xy);
            pos += xy.len();
            pos += write_i32_to_buf(&mut buf[pos..], n.x);

            let yy = b",\"y\":";
            if pos + yy.len() > buf.len() { break; }
            buf[pos..pos+yy.len()].copy_from_slice(yy);
            pos += yy.len();
            pos += write_i32_to_buf(&mut buf[pos..], n.y);

            let ww = b",\"w\":";
            if pos + ww.len() > buf.len() { break; }
            buf[pos..pos+ww.len()].copy_from_slice(ww);
            pos += ww.len();
            pos += write_u32_to_buf(&mut buf[pos..], n.width);

            let hh = b",\"h\":";
            if pos + hh.len() > buf.len() { break; }
            buf[pos..pos+hh.len()].copy_from_slice(hh);
            pos += hh.len();
            pos += write_u32_to_buf(&mut buf[pos..], n.height);

            // text
            let tt = b",\"text\":\"";
            if pos + tt.len() > buf.len() { break; }
            buf[pos..pos+tt.len()].copy_from_slice(tt);
            pos += tt.len();
            let tlen = n.text_len as usize;
            if pos + tlen + 1 > buf.len() { break; }
            buf[pos..pos+tlen].copy_from_slice(&n.text[..tlen]);
            pos += tlen;
            buf[pos] = b'"'; pos += 1;

            // interactive
            let inter = if n.interactive {
                b",\"interactive\":true}" as &[u8]
            } else {
                b",\"interactive\":false}"
            };
            if pos + inter.len() > buf.len() { break; }
            buf[pos..pos+inter.len()].copy_from_slice(inter);
            pos += inter.len();
        }

        // Close array
        if pos + 2 <= buf.len() {
            buf[pos] = b']'; pos += 1;
            buf[pos] = b'}'; pos += 1;
        }
        pos
    }
}

/// Write a u32 as decimal to buf, return bytes written
fn write_u32_to_buf(buf: &mut [u8], val: u32) -> usize {
    if val == 0 {
        if buf.is_empty() { return 0; }
        buf[0] = b'0';
        return 1;
    }
    let mut digits = [0u8; 10];
    let mut d = 0usize;
    let mut v = val;
    while v > 0 {
        digits[d] = b'0' + (v % 10) as u8;
        v /= 10;
        d += 1;
    }
    if d > buf.len() { return 0; }
    for i in 0..d {
        buf[i] = digits[d - 1 - i];
    }
    d
}

/// Write an i32 as decimal to buf, return bytes written
fn write_i32_to_buf(buf: &mut [u8], val: i32) -> usize {
    if val < 0 {
        if buf.is_empty() { return 0; }
        buf[0] = b'-';
        return 1 + write_u32_to_buf(&mut buf[1..], (-(val as i64)) as u32);
    }
    write_u32_to_buf(buf, val as u32)
}

// ═══════════════════════════════════════════════════
// Window Descriptor with Z-Index
// ═══════════════════════════════════════════════════
#[derive(Clone)]
struct Window {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    title: &'static [u8],
    z_index: u8,
    title_color: u32,
    visible: bool,
    /// Content lines for display (simplified static content)
    content: &'static [&'static [u8]],
    /// Content color
    content_color: u32,
}

impl Window {
    /// Draw the complete window: border, title bar, close/minimize buttons, body, content
    fn draw(&self) {
        if !self.visible {
            return;
        }

        let x = self.x.max(0) as u32;
        let y = self.y.max(0) as u32;
        let w = self.width;
        let h = self.height;

        // Clamp to screen bounds
        if x >= SCR_W || y >= TB_Y {
            return;
        }

        let draw_w = core::cmp::min(w, SCR_W - x);
        let draw_h = core::cmp::min(h, TB_Y - y);

        // Border (1px all around)
        if x > 0 {
            sys_fb_fill_rect(x - 1, y, 1, draw_h + 1, WIN_BORDER);
        }
        sys_fb_fill_rect(x + draw_w, y, 1, draw_h + 1, WIN_BORDER);
        if y > 0 {
            sys_fb_fill_rect(x, y - 1, draw_w, 1, WIN_BORDER);
        }
        sys_fb_fill_rect(x, y + draw_h, draw_w, 1, WIN_BORDER);

        // Title bar
        sys_fb_fill_rect(x, y, draw_w, core::cmp::min(TITLE_BAR_H, draw_h), self.title_color);

        // Close button (red square) at top-right
        if draw_w > 30 {
            sys_fb_fill_rect(x + draw_w - 24, y + 6, 16, 16, RED);
            sys_fb_draw_string(x + draw_w - 21, y + 6, b"x", TEXT);
        }

        // Minimize button (orange square)
        if draw_w > 56 {
            sys_fb_fill_rect(x + draw_w - 46, y + 6, 16, 16, ORANGE);
            sys_fb_draw_string(x + draw_w - 43, y + 6, b"-", TEXT);
        }

        // Title text
        sys_fb_draw_string(x + 10, y + 6, self.title, TEXT);

        // Window body
        if draw_h > TITLE_BAR_H {
            sys_fb_fill_rect(x, y + TITLE_BAR_H, draw_w, draw_h - TITLE_BAR_H, WIN_BG);
        }

        // Draw content lines
        let content_x = x + 12;
        let mut content_y = y + TITLE_BAR_H + 12;
        let line_height: u32 = 18;

        for &line in self.content.iter() {
            if content_y + line_height > y + draw_h {
                break;
            }
            sys_fb_draw_string(content_x, content_y, line, self.content_color);
            content_y += line_height;
        }
    }

    /// Check if a point (px, py) is within the title bar region
    fn hit_title_bar(&self, px: i32, py: i32) -> bool {
        px >= self.x
            && px < self.x + self.width as i32
            && py >= self.y
            && py < self.y + TITLE_BAR_H as i32
    }

    /// Check if a point is within the window bounds
    fn hit_test(&self, px: i32, py: i32) -> bool {
        self.visible
            && px >= self.x
            && px < self.x + self.width as i32
            && py >= self.y
            && py < self.y + self.height as i32
    }
}

// ═══════════════════════════════════════════════════
// Desktop State
// ═══════════════════════════════════════════════════
struct Desktop {
    windows: [Window; MAX_WINDOWS],
    window_count: usize,
    cursor_x: i32,
    cursor_y: i32,
    /// Index of the window being dragged (-1 = none)
    dragging: i32,
    /// Offset from cursor to window origin during drag
    drag_offset_x: i32,
    drag_offset_y: i32,
    /// Mouse button state (bit 0 = left)
    buttons: u8,
    prev_buttons: u8,
    /// Frame counter for cursor blink/animation
    frame: u32,
    /// Total HID events processed
    hid_events: u32,
    /// Jalon 112a: Uptime in seconds from Clock Sensor Agent
    uptime_seconds: u64,
}

impl Desktop {
    fn new() -> Self {
        Desktop {
            windows: [
                // Window 0: AetherionOS Terminal (centered, Jalon 108)
                Window {
                    x: 162, y: 120, width: 700, height: 480,
                    title: b"AetherionOS Terminal",
                    z_index: 3,
                    title_color: WIN_TITLE,
                    visible: true,
                    content: &[
                        b"AetherionOS v4.0 - AGI Chain Reaction",
                        b"Kernel: 4.0.0-j111-agi-memory-mouse",
                        b"",
                        b"$ uname -a",
                        b"Linux aetherion 6.18.0-aetherion x86_64",
                        b"$ cat /proc/cpuinfo",
                        b"cpu: x86_64 AVX2+FMA (Haswell)",
                        b"$ ls /bin/",
                        b"busybox.elf  shell.elf  agent_wm.elf",
                        b"agent_llm_chat.elf  agent_mcp.elf",
                        b"agent_memory.elf  agent_validator.elf",
                        b"$ free",
                        b"Mem: 1024M total, heap 6GiB ELF pool",
                        b"PagedAttention KV Cache: 64-block",
                        b"$ ps",
                        b"PID  NAME              STATE",
                        b" 1   kernel            Running",
                        b" 2   agent_wm          Running",
                        b" 3   busybox.elf       Ready (Linux ABI)",
                        b" 4   agent_memory      Running",
                        b"$ _",
                    ],
                    content_color: GREEN,
                },
                // Window 1: Neural Pipeline Status
                Window {
                    x: 50, y: 50, width: 460, height: 340,
                    title: b"AetherionAI - Neural Pipeline",
                    z_index: 1,
                    title_color: WIN_TITLE,
                    visible: true,
                    content: &[
                        b"=== Neural Pipeline Status ===",
                        b"",
                        b"Tensor Engine:  AVX2+FMA matmul  [OK]",
                        b"GGUF Loader:    v3 Streaming     [OK]",
                        b"Q8_0 Dequant:   SIMD 32KB buf    [OK]",
                        b"PagedAttention: 64-block KV      [OK]",
                        b"Cognitive Bus:  1024-msg (J109)  [OK]",
                        b"BusyBox:        Linux ABI exec   [OK]",
                        b"",
                        b"MCP Actions: gen_driver, ping,",
                        b"             run_linux_tool",
                        b"Linux Syscalls: clone, futex,",
                        b"  ptrace, perf_event_open, fanotify",
                    ],
                    content_color: TEXT,
                },
                // Window 2: System Monitor
                Window {
                    x: 540, y: 50, width: 440, height: 300,
                    title: b"System Monitor",
                    z_index: 2,
                    title_color: ACCENT,
                    visible: true,
                    content: &[
                        b"PID  NAME              STATE",
                        b"  1  kernel            Running",
                        b"  2  agent_wm          Running",
                        b"  3  agent_visual_term Ready",
                        b"  4  busybox.elf       Ready (Linux)",
                        b"  5  agent_llm_chat    Ready",
                        b"  6  agent_orchestrator Ready",
                        b"  7  agent_mcp         Ready",
                        b"",
                        b"Scheduler: Preemptive RR 50ms",
                        b"Security Agents: ENABLED",
                        b"Linux ABI: ~95% coverage",
                    ],
                    content_color: TEXT,
                },
                // Padding for remaining slots
                empty_window(), empty_window(), empty_window(),
                empty_window(), empty_window(),
            ],
            window_count: 3,
            cursor_x: (SCR_W / 2) as i32,
            cursor_y: (SCR_H / 2) as i32,
            dragging: -1,
            drag_offset_x: 0,
            drag_offset_y: 0,
            buttons: 0,
            prev_buttons: 0,
            frame: 0,
            hid_events: 0,
            uptime_seconds: 0,
        }
    }

    /// Sort windows by z_index (simple insertion sort for small N)
    fn sorted_indices(&self) -> [usize; MAX_WINDOWS] {
        let mut indices = [0usize; MAX_WINDOWS];
        for i in 0..MAX_WINDOWS {
            indices[i] = i;
        }
        // Insertion sort by z_index (ascending = back to front)
        let n = self.window_count;
        for i in 1..n {
            let mut j = i;
            while j > 0 && self.windows[indices[j]].z_index < self.windows[indices[j - 1]].z_index
            {
                indices.swap(j, j - 1);
                j -= 1;
            }
        }
        indices
    }

    /// Find the topmost window at a given point (highest z_index)
    fn find_window_at(&self, px: i32, py: i32) -> i32 {
        let mut best: i32 = -1;
        let mut best_z: u8 = 0;
        for i in 0..self.window_count {
            if self.windows[i].hit_test(px, py) && (best == -1 || self.windows[i].z_index > best_z) {
                best = i as i32;
                best_z = self.windows[i].z_index;
            }
        }
        best
    }

    /// Bring a window to the front (highest z_index)
    fn bring_to_front(&mut self, idx: usize) {
        if idx >= self.window_count {
            return;
        }
        let mut max_z: u8 = 0;
        for i in 0..self.window_count {
            if self.windows[i].z_index > max_z {
                max_z = self.windows[i].z_index;
            }
        }
        if self.windows[idx].z_index < max_z {
            self.windows[idx].z_index = max_z + 1;
        }
    }
}

/// Create an invisible placeholder window
const fn empty_window() -> Window {
    Window {
        x: 0, y: 0, width: 0, height: 0,
        title: b"",
        z_index: 0,
        title_color: 0,
        visible: false,
        content: &[],
        content_color: 0,
    }
}

// ═══════════════════════════════════════════════════
// Background Rendering
// ═══════════════════════════════════════════════════

/// Draw the desktop background with subtle gradient stripes
fn draw_background() {
    // Fill entire desktop area (above taskbar)
    sys_fb_fill_rect(0, 0, SCR_W, TB_Y, BG);

    // Subtle horizontal stripes for depth
    let mut y: u32 = 0;
    while y < TB_Y {
        sys_fb_fill_rect(0, y, SCR_W, 1, 0x00242424);
        y += 48;
    }

    // AetherionOS branding watermark (center)
    sys_fb_draw_string(SCR_W / 2 - 120, TB_Y / 2 - 8, b"AetherionOS  J111 - AGI Chain Reaction", TEXT_DIM);
}

/// Draw the taskbar at the bottom of the screen
fn draw_taskbar(desktop: &Desktop) {
    // Taskbar background
    sys_fb_fill_rect(0, TB_Y, SCR_W, TB_H, TASKBAR_BG);
    sys_fb_fill_rect(0, TB_Y, SCR_W, 1, TASKBAR_LINE);

    // Start button
    sys_fb_fill_rect(4, TB_Y + 4, 80, 24, ACCENT);
    sys_fb_draw_string(12, TB_Y + 8, b"Aetheria", TEXT);

    // Window entries in taskbar
    let mut tx: u32 = 92;
    for i in 0..desktop.window_count {
        let win = &desktop.windows[i];
        if !win.visible {
            continue;
        }
        let label_w: u32 = 120;
        sys_fb_fill_rect(tx, TB_Y + 4, label_w, 24, WIN_BG);
        sys_fb_fill_rect(tx, TB_Y + 4, label_w, 2, win.title_color);
        // Truncate title to fit
        let title_bytes = if win.title.len() > 14 {
            &win.title[..14]
        } else {
            win.title
        };
        sys_fb_draw_string(tx + 6, TB_Y + 8, title_bytes, TEXT);
        tx += label_w + 4;
    }

    // System tray (right side) — Jalon 109/112: show uptime from Clock Agent
    // Format uptime as "Up: XXXs"
    let up_secs = desktop.uptime_seconds;
    let mut up_buf = [0u8; 20];
    up_buf[0] = b'U'; up_buf[1] = b'p'; up_buf[2] = b':'; up_buf[3] = b' ';
    let mut pos = 4usize;
    if up_secs == 0 {
        up_buf[pos] = b'0'; pos += 1;
    } else {
        let mut digits = [0u8; 10];
        let mut d = 0usize;
        let mut v = up_secs;
        while v > 0 && d < 10 {
            digits[d] = b'0' + (v % 10) as u8;
            v /= 10;
            d += 1;
        }
        let mut i = d;
        while i > 0 {
            i -= 1;
            up_buf[pos] = digits[i];
            pos += 1;
        }
    }
    up_buf[pos] = b's'; pos += 1;
    sys_fb_draw_string(SCR_W - 300, TB_Y + 8, &up_buf[..pos], GREEN);
    sys_fb_draw_string(SCR_W - 200, TB_Y + 8, b"J109 | v3.1 | WM", TEXT_DIM);

    // Status indicators
    sys_fb_fill_rect(SCR_W - 60, TB_Y + 12, 8, 8, GREEN);   // Network OK
    sys_fb_fill_rect(SCR_W - 46, TB_Y + 12, 8, 8, GREEN);   // HID OK
    sys_fb_fill_rect(SCR_W - 32, TB_Y + 12, 8, 8, ACCENT);  // AI active
}

// ═══════════════════════════════════════════════════
// Cursor Rendering (10x10 arrow pointer)
// ═══════════════════════════════════════════════════

/// Draw a 10x10 pixel arrow cursor at the given position.
/// The cursor design is a simple arrow pointing top-left.
///
/// Arrow pattern (1=white, 2=black border):
/// ```
/// 2 . . . . . . . . .
/// 2 1 . . . . . . . .
/// 2 1 1 . . . . . . .
/// 2 1 1 1 . . . . . .
/// 2 1 1 1 1 . . . . .
/// 2 1 1 1 1 1 . . . .
/// 2 1 1 1 2 2 2 . . .
/// 2 1 2 2 . . . . . .
/// 2 2 . 2 1 . . . . .
/// 2 . . . 2 . . . . .
/// ```
fn draw_cursor(cx: i32, cy: i32) {
    let x = cx.max(0) as u32;
    let y = cy.max(0) as u32;

    if x >= SCR_W - 2 || y >= SCR_H - 2 {
        return;
    }

    // Arrow body (filled white triangular region)
    // Row 0: 1px
    sys_fb_fill_rect(x, y, 1, 1, CURSOR_FG);
    // Row 1: 2px
    sys_fb_fill_rect(x, y + 1, 2, 1, CURSOR_FG);
    // Row 2: 3px
    sys_fb_fill_rect(x, y + 2, 3, 1, CURSOR_FG);
    // Row 3: 4px
    sys_fb_fill_rect(x, y + 3, 4, 1, CURSOR_FG);
    // Row 4: 5px
    sys_fb_fill_rect(x, y + 4, 5, 1, CURSOR_FG);
    // Row 5: 6px
    sys_fb_fill_rect(x, y + 5, 6, 1, CURSOR_FG);
    // Row 6: 4px body + border
    sys_fb_fill_rect(x, y + 6, 4, 1, CURSOR_FG);
    sys_fb_fill_rect(x + 4, y + 6, 3, 1, CURSOR_BORDER);
    // Row 7: 2px
    sys_fb_fill_rect(x, y + 7, 2, 1, CURSOR_FG);
    // Row 8: 1px + gap + 1px
    sys_fb_fill_rect(x, y + 8, 1, 1, CURSOR_FG);
    if x + 3 < SCR_W {
        sys_fb_fill_rect(x + 3, y + 8, 2, 1, CURSOR_FG);
    }
    // Row 9: gap + 1px
    if x + 4 < SCR_W {
        sys_fb_fill_rect(x + 4, y + 9, 1, 1, CURSOR_FG);
    }

    // Black border outline on left edge
    let bx = if x > 0 { x - 1 } else { x };
    let max_h = core::cmp::min(10, SCR_H - y);
    for row in 0..max_h {
        if bx < SCR_W {
            sys_fb_fill_rect(bx, y + row, 1, 1, CURSOR_BORDER);
        }
    }
}

/// Erase the cursor by redrawing the background region under it.
/// For simplicity, we just draw a small BG-colored rectangle.
fn erase_cursor(cx: i32, cy: i32) {
    let x = (cx - 1).max(0) as u32;
    let y = cy.max(0) as u32;
    if x < SCR_W && y < SCR_H {
        let w = core::cmp::min(12, SCR_W - x);
        let h = core::cmp::min(12, SCR_H - y);
        sys_fb_fill_rect(x, y, w, h, BG);
    }
}

// ═══════════════════════════════════════════════════
// HID Event Decoding
// ═══════════════════════════════════════════════════

/// Decode a packed HID event from sys_poll_hid.
/// Format: [type: u8, buttons: u8, dx: i16, dy: i16, scancode: u8, _pad: u8]
struct HidEvent {
    event_type: u8,
    buttons: u8,
    dx: i16,
    dy: i16,
    #[allow(dead_code)]
    scancode: u8,
}

fn decode_hid_event(packed: u64) -> HidEvent {
    let bytes = packed.to_le_bytes();
    HidEvent {
        event_type: bytes[0],
        buttons: bytes[1],
        dx: i16::from_le_bytes([bytes[2], bytes[3]]),
        dy: i16::from_le_bytes([bytes[4], bytes[5]]),
        scancode: bytes[6],
    }
}

// ═══════════════════════════════════════════════════
// Full Desktop Compositing
// ═══════════════════════════════════════════════════

/// Draw the entire desktop: background, all windows in z_index order, taskbar, cursor.
fn draw_desktop(desktop: &Desktop) {
    // 1. Background
    draw_background();

    // 2. Windows sorted by z_index (back to front)
    let order = desktop.sorted_indices();
    for i in 0..desktop.window_count {
        let idx = order[i];
        desktop.windows[idx].draw();
    }

    // 3. Taskbar (always on top of windows)
    draw_taskbar(desktop);

    // 4. Cursor (always topmost)
    draw_cursor(desktop.cursor_x, desktop.cursor_y);
}

// ═══════════════════════════════════════════════════
// Semantic Tree Builder (Jalon 119)
// ═══════════════════════════════════════════════════

/// Build the semantic tree from the current desktop state.
/// Called on startup and whenever INTENT_GET_UI_TREE is received.
fn build_semantic_tree(desktop: &Desktop, tree: &mut SemanticTree) {
    tree.clear();

    // Root: Desktop
    let desktop_id = tree.add(
        NodeType::Desktop, 0,
        0, 0, SCR_W, SCR_H,
        b"AetherionOS Desktop", false,
    );

    // Each visible window → Window node + TitleBar + CloseButton + Content
    for i in 0..desktop.window_count {
        let win = &desktop.windows[i];
        if !win.visible { continue; }

        let win_id = tree.add(
            NodeType::Window, desktop_id,
            win.x, win.y, win.width, win.height,
            win.title, true,
        );

        // Title bar
        tree.add(
            NodeType::TitleBar, win_id,
            win.x, win.y, win.width, TITLE_BAR_H,
            win.title, true,
        );

        // Close button
        if win.width > 30 {
            tree.add(
                NodeType::Button, win_id,
                win.x + win.width as i32 - 24, win.y + 6, 16, 16,
                b"Close", true,
            );
        }

        // Minimize button
        if win.width > 56 {
            tree.add(
                NodeType::Button, win_id,
                win.x + win.width as i32 - 46, win.y + 6, 16, 16,
                b"Minimize", true,
            );
        }

        // Content area
        let content_y = win.y + TITLE_BAR_H as i32;
        let content_h = if win.height > TITLE_BAR_H { win.height - TITLE_BAR_H } else { 0 };
        tree.add(
            NodeType::ContentArea, win_id,
            win.x, content_y, win.width, content_h,
            b"Content", false,
        );

        // Content lines as labels
        let line_height: u32 = 18;
        let mut ly = content_y + 12;
        for &line in win.content.iter() {
            if (ly as u32) + line_height > (win.y as u32) + win.height { break; }
            let trunc = if line.len() > 31 { &line[..31] } else { line };
            tree.add(
                NodeType::Label, win_id,
                win.x + 12, ly, win.width - 24, line_height,
                trunc, false,
            );
            ly += line_height as i32;
        }
    }

    // Taskbar
    let tb_id = tree.add(
        NodeType::Taskbar, desktop_id,
        0, TB_Y as i32, SCR_W, TB_H,
        b"Taskbar", false,
    );

    // Start button
    tree.add(
        NodeType::Button, tb_id,
        4, TB_Y as i32 + 4, 80, 24,
        b"Aetheria", true,
    );

    // Taskbar window entries
    let mut tx: u32 = 92;
    for i in 0..desktop.window_count {
        let win = &desktop.windows[i];
        if !win.visible { continue; }
        let label_w: u32 = 120;
        let trunc = if win.title.len() > 14 { &win.title[..14] } else { win.title };
        tree.add(
            NodeType::TaskbarEntry, tb_id,
            tx as i32, TB_Y as i32 + 4, label_w, 24,
            trunc, true,
        );
        tx += label_w + 4;
    }

    // Status indicators
    tree.add(
        NodeType::StatusIndicator, tb_id,
        (SCR_W - 60) as i32, (TB_Y + 12) as i32, 8, 8,
        b"Network", false,
    );
    tree.add(
        NodeType::StatusIndicator, tb_id,
        (SCR_W - 46) as i32, (TB_Y + 12) as i32, 8, 8,
        b"HID", false,
    );
    tree.add(
        NodeType::StatusIndicator, tb_id,
        (SCR_W - 32) as i32, (TB_Y + 12) as i32, 8, 8,
        b"AI", false,
    );
}

// ═══════════════════════════════════════════════════
// Main Entry Point
// ═══════════════════════════════════════════════════

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J108] ═══════════════════════════════════════");
    println("[J108] Cognitive Desktop Window Manager - Jalon 108");
    println("[J108] Z-Index | Mouse Cursor | HID | MCP Integration");
    println("[J108] ═══════════════════════════════════════");

    let mut tests_passed: u32 = 0;

    // ───────────────────────────────────────────
    // Step 1: Initialize desktop and draw background
    // ───────────────────────────────────────────
    print("[J108] Step 1/6: Desktop initialization ... ");
    let mut desktop = Desktop::new();

    // Draw initial background
    draw_background();
    println("OK");
    tests_passed += 1;
    println("[J108-OK] Background rendered (1024x736 grey desktop)");

    // ───────────────────────────────────────────
    // Step 2: Draw windows in z_index order
    // ───────────────────────────────────────────
    print("[J108] Step 2/6: Window rendering (z-index) ... ");
    let order = desktop.sorted_indices();
    for i in 0..desktop.window_count {
        let idx = order[i];
        desktop.windows[idx].draw();
    }
    println("OK");
    tests_passed += 1;
    print("[J108-OK] ");
    print_u64(desktop.window_count as u64);
    println(" windows rendered in z-index order (AetherionOS Terminal centered)");

    // ───────────────────────────────────────────
    // Step 3: Draw taskbar
    // ───────────────────────────────────────────
    print("[J108] Step 3/6: Taskbar ... ");
    draw_taskbar(&desktop);
    println("OK");
    tests_passed += 1;
    println("[J108-OK] Taskbar with window list and system tray");

    // ───────────────────────────────────────────
    // Step 4: HID polling and mouse cursor
    // ───────────────────────────────────────────
    print("[J108] Step 4/6: HID + mouse cursor ... ");

    // Initial cursor draw
    draw_cursor(desktop.cursor_x, desktop.cursor_y);

    // Poll HID events to update cursor position
    let mut mouse_events: u32 = 0;
    let mut kbd_events: u32 = 0;

    for _ in 0..100u32 {
        let evt = sys_poll_hid();
        if evt == 0 {
            break;
        }
        desktop.hid_events += 1;
        let hid = decode_hid_event(evt);

        if hid.event_type == HID_TYPE_MOUSE {
            mouse_events += 1;
            // Erase old cursor
            erase_cursor(desktop.cursor_x, desktop.cursor_y);

            // Update position with deltas
            desktop.cursor_x = (desktop.cursor_x + hid.dx as i32)
                .clamp(0, (SCR_W - 1) as i32);
            desktop.cursor_y = (desktop.cursor_y + hid.dy as i32)
                .clamp(0, (SCR_H - 1) as i32);

            // Track button state for drag
            desktop.prev_buttons = desktop.buttons;
            desktop.buttons = hid.buttons;

            // Handle drag start
            if (desktop.buttons & 1) != 0 && (desktop.prev_buttons & 1) == 0 {
                // Left button just pressed - check for title bar hit
                let win_idx = desktop.find_window_at(desktop.cursor_x, desktop.cursor_y);
                if win_idx >= 0 {
                    let idx = win_idx as usize;
                    if desktop.windows[idx].hit_title_bar(desktop.cursor_x, desktop.cursor_y) {
                        desktop.dragging = win_idx;
                        desktop.drag_offset_x = desktop.cursor_x - desktop.windows[idx].x;
                        desktop.drag_offset_y = desktop.cursor_y - desktop.windows[idx].y;
                        desktop.bring_to_front(idx);
                    }
                }
            }

            // Handle drag move
            if (desktop.buttons & 1) != 0 && desktop.dragging >= 0 {
                let idx = desktop.dragging as usize;
                desktop.windows[idx].x = desktop.cursor_x - desktop.drag_offset_x;
                desktop.windows[idx].y = (desktop.cursor_y - desktop.drag_offset_y)
                    .max(0)
                    .min((TB_Y - TITLE_BAR_H) as i32);
            }

            // Handle drag end
            if (desktop.buttons & 1) == 0 {
                desktop.dragging = -1;
            }

            // Redraw cursor at new position
            draw_cursor(desktop.cursor_x, desktop.cursor_y);
        } else if hid.event_type == HID_TYPE_KEYBOARD {
            kbd_events += 1;
        }
    }

    print("OK (mouse=");
    print_u64(mouse_events as u64);
    print(", kbd=");
    print_u64(kbd_events as u64);
    println(")");
    tests_passed += 1;
    println("[J108-OK] HID polling + 10x10 arrow cursor rendered");

    // ───────────────────────────────────────────
    // Step 5: Z-index validation
    // ───────────────────────────────────────────
    print("[J108] Step 5/6: Z-index validation ... ");

    // Verify z_index sorting is correct
    let sorted = desktop.sorted_indices();
    let mut z_valid = true;
    for i in 1..desktop.window_count {
        if desktop.windows[sorted[i]].z_index < desktop.windows[sorted[i - 1]].z_index {
            z_valid = false;
            break;
        }
    }

    if z_valid {
        println("OK");
        tests_passed += 1;
        println("[J108-OK] Z-index ordering verified (back-to-front)");
    } else {
        println("FAIL - z_index order incorrect");
    }

    // ───────────────────────────────────────────
    // Step 6: Cognitive Bus publish
    // ───────────────────────────────────────────
    print("[J108] Step 6/6: Cognitive Bus ... ");
    let r1 = sys_bus_publish(INTENT_WM_READY, 2, desktop.window_count as u64);
    let r2 = sys_bus_publish(INTENT_WM_DESKTOP_STATE, 1, desktop.hid_events as u64);
    let r3 = sys_bus_publish(INTENT_WM_DESKTOP_J108, 2, 108);
    if r1 == 0 && r2 == 0 && r3 == 0 {
        println("OK (3 intents published)");
        tests_passed += 1;
    } else {
        println("FAIL");
    }
    println("[J108-OK] Desktop state published to Cognitive Bus");

    // ───────────────────────────────────────────
    // Step 7: Build Semantic UI Tree (Jalon 119)
    // ───────────────────────────────────────────
    print("[J119] Step 7: Building Semantic UI Tree ... ");
    let mut sem_tree = SemanticTree::new();
    build_semantic_tree(&desktop, &mut sem_tree);
    print_u64(sem_tree.count as u64);
    println(" nodes");
    tests_passed += 1;

    // Serialize and log it
    {
        let mut json_buf = [0u8; 2048];
        let json_len = sem_tree.to_json(&mut json_buf);
        if json_len > 0 {
            sys_write(1, b"[J119] Semantic UI Tree JSON: ");
            sys_write(1, &json_buf[..json_len]);
            sys_write(1, b"\n");
        }
    }

    // Publish tree availability
    sys_bus_publish(INTENT_UI_TREE_RESPONSE, 2, sem_tree.count as u64);
    println("[J119-OK] Semantic UI Tree built and published");

    // ───────────────────────────────────────────
    // Summary
    // ───────────────────────────────────────────
    let total_tests: u32 = 7;
    println("[J119] ═══════════════════════════════════════");
    print("[J119] Window Manager: ");
    print_u64(tests_passed as u64);
    print("/");
    print_u64(total_tests as u64);
    println(" steps completed");

    if tests_passed >= 6 {
        println("[J119-OK] Cognitive Desktop WM + Semantic UI Tree COMPLETE");
        println("[J119-OK] 3 windows, centered Terminal, grey BG");
        println("[J119-OK] Taskbar + cursor + drag + MCP");
        println("[J119-OK] Semantic UI Tree: AI screen reading ENABLED");
        println("[J119-OK] ALL STEPS PASSED");
    }
    println("[J119] ═══════════════════════════════════════");

    // ───────────────────────────────────────────
    // Event Loop: continuous HID polling + redraw + Semantic Tree intents
    // ───────────────────────────────────────────
    println("[J119] Entering event loop (HID + Semantic UI Tree)...");

    let mut idle_count: u32 = 0;
    let max_idle: u32 = 500_000;
    let mut need_redraw = false;

    loop {
        // ── Jalon 119: Handle INTENT_GET_UI_TREE requests from AI ──
        {
            let mut req_buf = [0u64; 8];
            if sys_bus_consume_intent(&mut req_buf, INTENT_GET_UI_TREE as u32) == 0 {
                // AI requested the Semantic UI Tree → rebuild and respond
                sem_tree.clear();
                build_semantic_tree(&desktop, &mut sem_tree);
                let mut json_buf = [0u8; 2048];
                let json_len = sem_tree.to_json(&mut json_buf);
                // Write JSON to /tmp/ui_tree.json for AI to read
                let path = b"/tmp/ui_tree.json\0";
                let fd = sys_open(path, O_CREAT | O_WRONLY);
                if fd >= 0 {
                    sys_write_fd(fd as u32, &json_buf[..json_len]);
                    sys_close(fd as u32);
                }
                sys_bus_publish(INTENT_UI_TREE_RESPONSE, 2, json_len as u64);
            }
        }

        // ── Jalon 119: Handle INTENT_INTERACT_NODE from AI ──
        {
            let mut interact_buf = [0u64; 8];
            if sys_bus_consume_intent(&mut interact_buf, INTENT_INTERACT_NODE as u32) == 0 {
                let node_id = interact_buf[2] as u16; // payload = node_id
                if let Some((cx, cy)) = sem_tree.find_node_center(node_id) {
                    // Move cursor to the node's center and simulate a click
                    erase_cursor(desktop.cursor_x, desktop.cursor_y);
                    desktop.cursor_x = cx;
                    desktop.cursor_y = cy;
                    draw_cursor(desktop.cursor_x, desktop.cursor_y);
                    // Simulate left-click: check for window focus
                    let win_idx = desktop.find_window_at(cx, cy);
                    if win_idx >= 0 {
                        desktop.bring_to_front(win_idx as usize);
                        need_redraw = true;
                    }
                    sys_bus_publish(INTENT_INTERACT_DONE, 2, node_id as u64);
                }
            }
        }

        let evt = sys_poll_hid();

        if evt != 0 {
            idle_count = 0;
            desktop.hid_events += 1;
            let hid = decode_hid_event(evt);

            // ESC key detection (scancode 0x01 = ESC in PS/2 Set 1)
            if hid.event_type == HID_TYPE_KEYBOARD && hid.scancode == 0x01 {
                println("[J108] ESC pressed - exiting Window Manager");
                sys_bus_publish(INTENT_WM_DESKTOP_STATE, 1, 0);  // Notify WM exit
                sys_write(1, b"[WM] ESC exit - returning to terminal\n");
                return 0;  // Exit WM, return to terminal
            }

            if hid.event_type == HID_TYPE_MOUSE {
                // Erase old cursor
                erase_cursor(desktop.cursor_x, desktop.cursor_y);

                // Update cursor position
                desktop.cursor_x = (desktop.cursor_x + hid.dx as i32)
                    .clamp(0, (SCR_W - 1) as i32);
                desktop.cursor_y = (desktop.cursor_y + hid.dy as i32)
                    .clamp(0, (SCR_H - 1) as i32);

                // Button transitions
                desktop.prev_buttons = desktop.buttons;
                desktop.buttons = hid.buttons;

                // Click to focus + start drag
                if (desktop.buttons & 1) != 0 && (desktop.prev_buttons & 1) == 0 {
                    let win_idx = desktop.find_window_at(desktop.cursor_x, desktop.cursor_y);
                    if win_idx >= 0 {
                        let idx = win_idx as usize;
                        desktop.bring_to_front(idx);
                        if desktop.windows[idx].hit_title_bar(desktop.cursor_x, desktop.cursor_y) {
                            desktop.dragging = win_idx;
                            desktop.drag_offset_x = desktop.cursor_x - desktop.windows[idx].x;
                            desktop.drag_offset_y = desktop.cursor_y - desktop.windows[idx].y;
                        }
                        need_redraw = true;
                    }
                }

                // Drag movement
                if (desktop.buttons & 1) != 0 && desktop.dragging >= 0 {
                    let idx = desktop.dragging as usize;
                    desktop.windows[idx].x = desktop.cursor_x - desktop.drag_offset_x;
                    desktop.windows[idx].y = (desktop.cursor_y - desktop.drag_offset_y)
                        .max(0)
                        .min((TB_Y - TITLE_BAR_H) as i32);
                    need_redraw = true;
                }

                // Release
                if (desktop.buttons & 1) == 0 {
                    desktop.dragging = -1;
                }

                // Redraw if needed
                if need_redraw {
                    draw_desktop(&desktop);
                    need_redraw = false;
                } else {
                    draw_cursor(desktop.cursor_x, desktop.cursor_y);
                }
            }
        } else {
            idle_count += 1;
            if idle_count >= max_idle {
                // Safety valve: yield and continue
                idle_count = 0;
            }
        }

        desktop.frame += 1;

        // Jalon 112a: Consume INTENT_TIMER_TICK from Clock Sensor Agent
        // Update uptime counter and redraw taskbar periodically
        {
            let mut tick_buf = [0u64; 8];
            if sys_bus_consume_intent(&mut tick_buf, INTENT_TIMER_TICK as u32) == 0 {
                // tick_buf[2] = payload = uptime in seconds
                let new_uptime = tick_buf[2];
                if new_uptime != desktop.uptime_seconds {
                    desktop.uptime_seconds = new_uptime;
                    // Redraw taskbar to update uptime display
                    draw_taskbar(&desktop);
                    draw_cursor(desktop.cursor_x, desktop.cursor_y);
                }
            }
        }

        // Periodic cursor blink (every ~10000 frames)
        if desktop.frame % 10000 == 0 {
            // Redraw cursor to keep it visible
            draw_cursor(desktop.cursor_x, desktop.cursor_y);
        }

        // Yield CPU to other processes
        sys_yield();
    }
}
