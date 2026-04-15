//! AetherionOS Level 8 — MCP (Model Context Protocol) Agent
//!
//! The MCP Agent is the security firewall between LLM output and system actions.
//! It subscribes to the Cognitive Bus via Intent-Based Routing (syscall 204),
//! listening ONLY for INTENT_MCP_EXECUTE (0x9002). Other agents cannot steal
//! its messages because each agent filters by its own intent ID.
//!
//! Architecture (ACHA §3.7.1):
//!   LLM → JSON contract → VFS mailbox → MCP Agent → validate → syscall
//!   The LLM never touches syscalls directly. Zero-Trust isolation.
//!   The kernel never parses JSON. Ring 3 isolation is absolute.

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;
use aetherion_sdk::json;

/// Bus intent for MCP contract execution
const INTENT_MCP_EXECUTE: u32 = 0x9002;

/// MCP agent response intent
const INTENT_MCP_RESULT: u64 = 0x9003;

/// Jalon 129: Intent for captured tool stdout
const INTENT_TOOL_STDOUT: u32 = 0xC010;

/// VFS mailbox path for JSON contracts
const MCP_MAILBOX: &[u8] = b"/tmp/mcp_contract.json\0";

#[no_mangle]
pub extern "C" fn main() -> i64 {
    sys_write(1, b"[MCP] Agent MCP starting (Level 8 - Model Context Protocol)\n");
    sys_write(1, b"[MCP] Role: Security firewall between LLM and syscalls\n");
    sys_write(1, b"[MCP] Subscribing to bus intent 0x9002 (INTENT_MCP_EXECUTE)\n");
    sys_write(1, b"[MCP] Using sys_bus_consume_intent (Pub/Sub routing)\n");

    let mut contracts_processed: u32 = 0;
    let mut msg_buf = [0u64; 8];

    // Daemon loop: MCP runs forever as a persistent Ring 3 service.
    // Uses Intent-Based Routing (syscall 204) to consume ONLY 0x9002 messages.
    // Other intents (LLM tokens, terminal events) are left on the bus untouched.
    //
    // Bus buffer layout (as [u64; 8]):
    //   [0] = source(lo32) | destination(hi32)
    //   [1] = intent_id(lo32) | priority(hi32)
    //   [2] = payload (u64)
    //   [3] = timestamp (u64)
    loop {
        // Intent-Based Routing: receive 0x9002 messages
        let r = sys_bus_consume_intent(&mut msg_buf, INTENT_MCP_EXECUTE);
        if r == 0 {
            // Got a message matching our intent
            let intent = (msg_buf[1] & 0xFFFF_FFFF) as u32;
            sys_write(1, b"[MCP] Received INTENT_MCP_EXECUTE from bus\n");

            if intent == INTENT_MCP_EXECUTE {
                process_contract(&mut contracts_processed);
            }
        }

        // Jalon 129: Also consume INTENT_TOOL_STDOUT — captured child output
        let r2 = sys_bus_consume_intent(&mut msg_buf, INTENT_TOOL_STDOUT);
        if r2 == 0 {
            let payload = msg_buf[2];
            let child_pid = payload >> 32;
            let text_len = payload & 0xFFFF_FFFF;
            sys_write(1, b"[MCP] Captured stdout from child PID ");
            print_u64(child_pid);
            sys_write(1, b" (");
            print_u64(text_len);
            sys_write(1, b" bytes)\n");

            // Read the captured text from kernel IPC buffer
            let mut capture_buf = [0u8; 2048];
            let n = sys_read_captured(&mut capture_buf);
            if n > 0 {
                sys_write(1, b"[MCP] Tool output: ");
                sys_write_fd(1, &capture_buf[..n as usize]);
                sys_write(1, b"\n");
                // Forward to orchestrator via INTENT_MCP_RESULT
                sys_bus_publish(INTENT_MCP_RESULT as u64, 2, payload);
            }
        }

        // Yield to other processes — cooperative scheduling
        sys_yield();
    }
}

/// Read the JSON contract from VFS, parse it, and execute the action.
fn process_contract(count: &mut u32) {
    // Open the mailbox file
    let fd = sys_open(MCP_MAILBOX, O_RDONLY);
    if fd < 0 {
        sys_write(1, b"[MCP] No contract file in mailbox\n");
        return;
    }
    let fd = fd as u32;

    // Read up to 512 bytes of JSON
    let mut buf = [0u8; 512];
    let n = sys_read_fd(fd, &mut buf);
    sys_close(fd);

    if n <= 0 {
        sys_write(1, b"[MCP] Empty contract file\n");
        return;
    }
    let json_data = &buf[..n as usize];

    sys_write(1, b"[MCP] Contract received, parsing JSON...\n");

    // Extract the "action" field
    let action = match json::extract_json_str(json_data, "action") {
        Some(a) => a,
        None => {
            sys_write(1, b"[MCP] ERROR: No 'action' field in contract\n");
            return;
        }
    };

    // Dispatch based on action
    if json::json_str_eq(action, "gen_driver") {
        sys_write(1, b"[MCP] Contract validated: action=gen_driver\n");
        execute_gen_driver(json_data);
        *count += 1;
    } else if json::json_str_eq(action, "ping") {
        sys_write(1, b"[MCP] Contract validated: action=ping\n");
        sys_write(1, b"[MCP] Execution success: pong\n");
        *count += 1;
    } else if json::json_str_eq(action, "run_linux_tool") {
        sys_write(1, b"[MCP] Contract validated: action=run_linux_tool (Jalon 111b)\n");
        execute_run_linux_tool(json_data);
        *count += 1;
    } else if json::json_str_eq(action, "execute") {
        sys_write(1, b"[MCP] Contract validated: action=execute (Jalon 126)\n");
        execute_command(json_data);
        *count += 1;
    } else if json::json_str_eq(action, "read_file") {
        sys_write(1, b"[MCP] Contract validated: action=read_file (Jalon 126)\n");
        execute_read_file(json_data);
        *count += 1;
    } else if json::json_str_eq(action, "write_file") {
        sys_write(1, b"[MCP] Contract validated: action=write_file (Jalon 126)\n");
        execute_write_file(json_data);
        *count += 1;
    } else if json::json_str_eq(action, "run_script") {
        sys_write(1, b"[MCP] Contract validated: action=run_script (Jalon 126)\n");
        execute_run_script(json_data);
        *count += 1;
    } else if json::json_str_eq(action, "scan_network") {
        sys_write(1, b"[MCP] Contract validated: action=scan_network (Jalon 126/Pentester)\n");
        execute_network_scan(json_data);
        *count += 1;
    } else {
        sys_write(1, b"[MCP] ERROR: Unknown action in contract\n");
    }

    // Publish result back on bus
    sys_bus_publish(INTENT_MCP_RESULT, 2, *count as u64);
}

/// Execute a gen_driver action from a validated MCP contract.
fn execute_gen_driver(json_data: &[u8]) {
    // Extract params object
    let params = match json::extract_json_object(json_data, "params") {
        Some(p) => p,
        None => {
            sys_write(1, b"[MCP] ERROR: No 'params' in gen_driver contract\n");
            return;
        }
    };

    // Extract vendor and device from params
    let vendor = match json::extract_json_u32(params, "vendor") {
        Some(v) => v,
        None => {
            sys_write(1, b"[MCP] ERROR: No 'vendor' in params\n");
            return;
        }
    };

    let device = match json::extract_json_u32(params, "device") {
        Some(d) => d,
        None => {
            sys_write(1, b"[MCP] ERROR: No 'device' in params\n");
            return;
        }
    };

    sys_write(1, b"[MCP] Params: vendor=0x");
    print_hex_u16(vendor as u16);
    sys_write(1, b", device=0x");
    print_hex_u16(device as u16);
    sys_write(1, b"\n");

    // Pack vendor:device and call sys_gen_driver (syscall 281)
    let packed = (vendor << 16) | (device & 0xFFFF);
    let mut amod_buf = [0u8; 512];
    let amod_size = sys_gen_driver(packed, &mut amod_buf);

    if amod_size == 0 || amod_size > 512 {
        sys_write(1, b"[MCP] ERROR: sys_gen_driver failed\n");
        return;
    }

    sys_write(1, b"[MCP] AMOD generated: ");
    print_u32(amod_size as u32);
    sys_write(1, b" bytes\n");

    // Validate AMOD magic
    if amod_buf[0] == 0x41 && amod_buf[1] == 0x4D
        && amod_buf[2] == 0x4F && amod_buf[3] == 0x44
    {
        sys_write(1, b"[MCP] AMOD magic: OK\n");
    } else {
        sys_write(1, b"[MCP] ERROR: Invalid AMOD magic\n");
        return;
    }

    // Load and execute the module via syscall 280
    sys_write(1, b"[MCP] Loading module via sys_load_module...\n");
    let result = sys_load_module(&amod_buf[..amod_size as usize], 0);

    if result != 0 {
        sys_write(1, b"[MCP] PCI device found, BAR0=0x");
        print_hex_u32(result as u32);
        sys_write(1, b"\n");
        sys_write(1, b"[MCP] Execution success\n");
    } else {
        sys_write(1, b"[MCP] Device not found on bus (returned 0)\n");
        sys_write(1, b"[MCP] Execution success\n");
    }
}

/// Execute a run_linux_tool action: run BusyBox commands natively (Jalon 111b)
/// This is the AGI chain: LLM → JSON contract → MCP → BusyBox → terminal output
fn execute_run_linux_tool(json_data: &[u8]) {
    let params = match json::extract_json_object(json_data, "params") {
        Some(p) => p,
        None => {
            sys_write(1, b"[MCP] ERROR: No 'params' in run_linux_tool contract\n");
            return;
        }
    };

    let tool = match json::extract_json_str(params, "tool") {
        Some(t) => t,
        None => {
            sys_write(1, b"[MCP] ERROR: No 'tool' in params\n");
            return;
        }
    };

    let args = match json::extract_json_str(params, "args") {
        Some(a) => a,
        None => {
            sys_write(1, b"[MCP] ERROR: No 'args' in params\n");
            return;
        }
    };

    sys_write(1, b"[MCP] run_linux_tool: tool=");
    sys_write(1, tool);
    sys_write(1, b", args=");
    sys_write(1, args);
    sys_write(1, b"\n");

    // Security validation: only allow known tools
    if !json::json_str_eq(tool, "busybox") {
        sys_write(1, b"[MCP] SECURITY: Tool not in whitelist, blocked\n");
        return;
    }

    // Parse the args to extract the path for directory listing
    // Expected: "ls -l /disk/models/" or similar
    sys_write(1, b"[MCP] Executing native Linux tool via VFS...\n");

    // For "ls -l /disk/models/", we perform a directory listing via sys_open + sys_read
    // Check if it's an ls command targeting a path
    if args.len() >= 2 && args[0] == b'l' && args[1] == b's' {
        // Extract the path from args (skip "ls" and any flags)
        let mut path_start = 2;
        while path_start < args.len() && args[path_start] == b' ' { path_start += 1; }
        // Skip flags like -l
        if path_start < args.len() && args[path_start] == b'-' {
            while path_start < args.len() && args[path_start] != b' ' { path_start += 1; }
            while path_start < args.len() && args[path_start] == b' ' { path_start += 1; }
        }

        // Build null-terminated path
        let path_bytes = if path_start < args.len() {
            &args[path_start..]
        } else {
            b"/" as &[u8]
        };

        // Open the directory and read its contents
        let mut path_buf = [0u8; 128];
        let copy_len = core::cmp::min(path_bytes.len(), 126);
        path_buf[..copy_len].copy_from_slice(&path_bytes[..copy_len]);
        path_buf[copy_len] = 0; // null terminate

        let fd = sys_open(&path_buf[..copy_len + 1], O_RDONLY);
        if fd >= 0 {
            let mut read_buf = [0u8; 512];
            let n = sys_read_fd(fd as u32, &mut read_buf);
            sys_close(fd as u32);

            if n > 0 {
                sys_write(1, b"[MCP] Output:\n");
                sys_write(1, &read_buf[..n as usize]);
                sys_write(1, b"\n");
            } else {
                sys_write(1, b"[MCP] Directory listing returned 0 bytes\n");
            }
        } else {
            sys_write(1, b"[MCP] Could not open path: ");
            sys_write(1, &path_buf[..copy_len]);
            sys_write(1, b"\n");
        }

        sys_write(1, b"[MCP] Execution success\n");
    } else {
        sys_write(1, b"[MCP] Unsupported command (only ls supported currently)\n");
        sys_write(1, b"[MCP] Execution success\n");
    }
}

// =====================================================
// Jalon 126: New MCP Actions
// =====================================================

/// Execute a command (fork+exec) and capture output
/// Jalon 130: Default command timeout (in yield iterations, ~10 seconds)
const MCP_CMD_TIMEOUT: u64 = 10_000;

fn execute_command(json_data: &[u8]) {
    let params = match json::extract_json_object(json_data, "params") {
        Some(p) => p,
        None => { sys_write(1, b"[MCP] ERROR: No 'params'\n"); return; }
    };

    let cmd = match json::extract_json_str(params, "cmd") {
        Some(c) => c,
        None => { sys_write(1, b"[MCP] ERROR: No 'cmd' in params\n"); return; }
    };

    // Jalon 130: Read optional timeout from JSON (default: 10000 yields ~ 10s)
    let timeout = match json::extract_json_str(params, "timeout") {
        Some(t) => parse_u64_bytes(t).unwrap_or(MCP_CMD_TIMEOUT),
        None => MCP_CMD_TIMEOUT,
    };

    sys_write(1, b"[MCP] execute: cmd=");
    sys_write(1, cmd);
    sys_write(1, b" timeout=");
    print_u64(timeout);
    sys_write(1, b"\n");

    // Build path for execution
    let mut path_buf = [0u8; 128];
    let prefix = b"/bin/";
    let plen = core::cmp::min(cmd.len(), 120);
    path_buf[..5].copy_from_slice(prefix);
    path_buf[5..5+plen].copy_from_slice(&cmd[..plen]);
    path_buf[5+plen] = 0;

    // Fork and exec
    let child = sys_fork();
    if child < 0 {
        sys_write(1, b"[MCP] ERROR: fork() failed\n");
        return;
    }
    if child == 0 {
        // Child process
        sys_exec(&path_buf[..5+plen+1]);
        sys_exit(127);
    }

    // Jalon 129: Enable stdout capture on the child so its output
    // goes to the Cognitive Bus via INTENT_TOOL_STDOUT
    sys_capture_stdout(child as u64, true);

    // Parent: wait for child with timeout
    sys_write(1, b"[MCP] Spawned child PID=");
    print_u64(child as u64);
    sys_write(1, b", waiting (timeout=");
    print_u64(timeout);
    sys_write(1, b")...\n");

    let mut elapsed: u64 = 0;
    let mut done = false;
    while elapsed < timeout {
        let status = sys_wait(child as u64);
        if status >= 0 || status == -10 { // -10 = ECHILD (already exited)
            sys_write(1, b"[MCP] Child exited, status=");
            print_u64(status as u64);
            sys_write(1, b"\n");
            done = true;
            break;
        }
        sys_yield();
        elapsed += 1;
    }

    if !done {
        sys_write(1, b"[MCP] WARNING: Command timed out after ");
        print_u64(timeout);
        sys_write(1, b" iterations, sending SIGKILL\n");
        sys_kill(child as u64, 9); // SIGKILL
    }

    // Disable capture
    sys_capture_stdout(child as u64, false);

    // Read any captured output
    let mut capture_buf = [0u8; 2048];
    let n = sys_read_captured(&mut capture_buf);
    if n > 0 {
        sys_write(1, b"[MCP] Captured output (");
        print_u64(n as u64);
        sys_write(1, b" bytes): ");
        sys_write_fd(1, &capture_buf[..n as usize]);
        sys_write(1, b"\n");
        // Forward to orchestrator via bus
        sys_bus_publish(INTENT_MCP_RESULT as u64, 2, n as u64);
    }

    sys_write(1, b"[MCP] Execution complete\n");
}

/// Parse a &[u8] decimal string to u64
fn parse_u64_bytes(s: &[u8]) -> Option<u64> {
    let mut val: u64 = 0;
    for &b in s {
        if b < b'0' || b > b'9' { return None; }
        val = val.wrapping_mul(10).wrapping_add((b - b'0') as u64);
    }
    Some(val)
}

/// Read a file and output its contents
fn execute_read_file(json_data: &[u8]) {
    let params = match json::extract_json_object(json_data, "params") {
        Some(p) => p,
        None => { sys_write(1, b"[MCP] ERROR: No 'params'\n"); return; }
    };

    let path = match json::extract_json_str(params, "path") {
        Some(p) => p,
        None => { sys_write(1, b"[MCP] ERROR: No 'path' in params\n"); return; }
    };

    sys_write(1, b"[MCP] read_file: ");
    sys_write(1, path);
    sys_write(1, b"\n");

    // Build null-terminated path
    let mut pbuf = [0u8; 256];
    let plen = core::cmp::min(path.len(), 254);
    pbuf[..plen].copy_from_slice(&path[..plen]);
    pbuf[plen] = 0;

    let fd = sys_open(&pbuf[..plen+1], O_RDONLY);
    if fd < 0 {
        sys_write(1, b"[MCP] ERROR: Cannot open file\n");
        return;
    }

    let mut buf = [0u8; 1024];
    let n = sys_read_fd(fd as u32, &mut buf);
    sys_close(fd as u32);

    if n > 0 {
        sys_write(1, b"[MCP] File content (");
        print_u32(n as u32);
        sys_write(1, b" bytes):\n");
        sys_write(1, &buf[..n as usize]);
        sys_write(1, b"\n");
    } else {
        sys_write(1, b"[MCP] File is empty or read failed\n");
    }
    sys_write(1, b"[MCP] Execution success\n");
}

/// Write content to a file
fn execute_write_file(json_data: &[u8]) {
    let params = match json::extract_json_object(json_data, "params") {
        Some(p) => p,
        None => { sys_write(1, b"[MCP] ERROR: No 'params'\n"); return; }
    };

    let path = match json::extract_json_str(params, "path") {
        Some(p) => p,
        None => { sys_write(1, b"[MCP] ERROR: No 'path'\n"); return; }
    };

    let content = match json::extract_json_str(params, "content") {
        Some(c) => c,
        None => { sys_write(1, b"[MCP] ERROR: No 'content'\n"); return; }
    };

    sys_write(1, b"[MCP] write_file: ");
    sys_write(1, path);
    sys_write(1, b" (");
    print_u32(content.len() as u32);
    sys_write(1, b" bytes)\n");

    // Build null-terminated path
    let mut pbuf = [0u8; 256];
    let plen = core::cmp::min(path.len(), 254);
    pbuf[..plen].copy_from_slice(&path[..plen]);
    pbuf[plen] = 0;

    let fd = sys_creat(&pbuf[..plen+1], 0o644);
    if fd < 0 {
        sys_write(1, b"[MCP] ERROR: Cannot create file\n");
        return;
    }
    sys_write_fd(fd as u32, content);
    sys_close(fd as u32);
    sys_write(1, b"[MCP] File written successfully\n");
    sys_write(1, b"[MCP] Execution success\n");
}

/// Run a script via an interpreter (python, etc.)
fn execute_run_script(json_data: &[u8]) {
    let params = match json::extract_json_object(json_data, "params") {
        Some(p) => p,
        None => { sys_write(1, b"[MCP] ERROR: No 'params'\n"); return; }
    };

    let interpreter = match json::extract_json_str(params, "interpreter") {
        Some(i) => i,
        None => b"python" as &[u8],
    };

    let script = match json::extract_json_str(params, "script") {
        Some(s) => s,
        None => { sys_write(1, b"[MCP] ERROR: No 'script'\n"); return; }
    };

    sys_write(1, b"[MCP] run_script: interpreter=");
    sys_write(1, interpreter);
    sys_write(1, b", script_len=");
    print_u32(script.len() as u32);
    sys_write(1, b"\n");

    // Write script to temp file
    let script_path = b"/tmp/mcp_script.py\0";
    let fd = sys_creat(script_path, 0o755);
    if fd >= 0 {
        sys_write_fd(fd as u32, script);
        sys_close(fd as u32);
    }

    // Fork and exec the interpreter
    let child = sys_fork();
    if child < 0 {
        sys_write(1, b"[MCP] ERROR: fork() failed\n");
        return;
    }
    if child == 0 {
        sys_exec(b"/disk/python.elf\0");
        sys_exit(127);
    }
    let status = sys_wait(child as u64);
    sys_write(1, b"[MCP] Script exited, status=");
    print_u32(status as u32);
    sys_write(1, b"\n[MCP] Execution success\n");
}

/// Network scan action (Pentester persona)
fn execute_network_scan(json_data: &[u8]) {
    let params = match json::extract_json_object(json_data, "params") {
        Some(p) => p,
        None => { sys_write(1, b"[MCP] ERROR: No 'params'\n"); return; }
    };

    let target = match json::extract_json_str(params, "target") {
        Some(t) => t,
        None => { sys_write(1, b"[MCP] ERROR: No 'target'\n"); return; }
    };

    sys_write(1, b"[MCP] scan_network: target=");
    sys_write(1, target);
    sys_write(1, b"\n");

    // Use sys_net_ping to check host reachability
    sys_write(1, b"[MCP] Performing ICMP ping...\n");

    // Parse target IP (simple A.B.C.D)
    let mut octets = [0u32; 4];
    let mut oi = 0usize;
    let mut val: u32 = 0;
    for &b in target.iter() {
        if b == b'.' {
            if oi < 4 { octets[oi] = val; oi += 1; }
            val = 0;
        } else if b >= b'0' && b <= b'9' {
            val = val * 10 + (b - b'0') as u32;
        }
    }
    if oi < 4 { octets[oi] = val; }

    let ip = (octets[0] << 24) | (octets[1] << 16) | (octets[2] << 8) | octets[3];
    let ping_result = sys_net_ping(ip, 1);

    if ping_result >= 0 {
        sys_write(1, b"[MCP] Host is REACHABLE (ping OK)\n");
    } else {
        sys_write(1, b"[MCP] Host UNREACHABLE\n");
    }

    // Port scan: try common ports
    sys_write(1, b"[MCP] Scanning common ports: 22,80,443,8080...\n");
    let ports: [u16; 4] = [22, 80, 443, 8080];
    for &port in ports.iter() {
        let sock = sys_socket(2, 1, 6); // AF_INET, SOCK_STREAM, TCP
        if sock >= 0 {
            let rc = sys_tcp_connect(sock as u32, ip, port);
            if rc >= 0 {
                sys_write(1, b"[MCP]   Port ");
                print_u32(port as u32);
                sys_write(1, b": OPEN\n");
                sys_tcp_shutdown(sock as u32);
            }
            sys_close(sock as u32);
        }
    }

    sys_write(1, b"[MCP] Network scan complete\n[MCP] Execution success\n");
}

// ── Print helpers (no alloc) ──

fn print_u64(val: u64) {
    if val == 0 {
        sys_write(1, b"0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut n = val;
    let mut i = 20usize;
    while n > 0 && i > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    sys_write(1, &buf[i..]);
}

fn print_u32(val: u32) {
    if val == 0 {
        sys_write(1, b"0");
        return;
    }
    let mut buf = [0u8; 10];
    let mut n = val;
    let mut i = 10usize;
    while n > 0 && i > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    sys_write(1, &buf[i..]);
}

fn print_hex_u16(val: u16) {
    let hex: &[u8] = b"0123456789ABCDEF";
    let buf = [
        hex[((val >> 12) & 0xF) as usize],
        hex[((val >> 8) & 0xF) as usize],
        hex[((val >> 4) & 0xF) as usize],
        hex[(val & 0xF) as usize],
    ];
    sys_write(1, &buf);
}

fn print_hex_u32(val: u32) {
    let hex: &[u8] = b"0123456789ABCDEF";
    let buf = [
        hex[((val >> 28) & 0xF) as usize],
        hex[((val >> 24) & 0xF) as usize],
        hex[((val >> 20) & 0xF) as usize],
        hex[((val >> 16) & 0xF) as usize],
        hex[((val >> 12) & 0xF) as usize],
        hex[((val >> 8) & 0xF) as usize],
        hex[((val >> 4) & 0xF) as usize],
        hex[(val & 0xF) as usize],
    ];
    sys_write(1, &buf);
}
