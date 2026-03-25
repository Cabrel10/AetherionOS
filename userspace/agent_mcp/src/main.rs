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

/// VFS mailbox path for JSON contracts
const MCP_MAILBOX: &[u8] = b"/tmp/mcp_contract.json\0";

#[no_mangle]
pub extern "C" fn main() -> i64 {
    sys_write(1, b"[MCP] Agent MCP starting (Level 8 - Model Context Protocol)\n");
    sys_write(1, b"[MCP] Role: Security firewall between LLM and syscalls\n");
    sys_write(1, b"[MCP] Subscribing to bus intent 0x9002 (INTENT_MCP_EXECUTE)\n");
    sys_write(1, b"[MCP] Using sys_bus_consume_intent (Pub/Sub routing)\n");

    let mut contracts_processed: u32 = 0;
    let mut msg_buf = [0u64; 6];

    // Daemon loop: MCP runs forever as a persistent Ring 3 service.
    // Uses Intent-Based Routing (syscall 204) to consume ONLY 0x9002 messages.
    // Other intents (LLM tokens, terminal events) are left on the bus untouched.
    //
    // Bus buffer layout (as [u64; 6]):
    //   [0] = source(lo32) | destination(hi32)
    //   [1] = intent_id(lo32) | priority(hi32)
    //   [2] = payload (u64)
    //   [3] = timestamp (u64)
    loop {
        // Intent-Based Routing: only receive 0x9002 messages
        let r = sys_bus_consume_intent(&mut msg_buf, INTENT_MCP_EXECUTE);
        if r == 0 {
            // Got a message matching our intent
            let intent = (msg_buf[1] & 0xFFFF_FFFF) as u32;
            sys_write(1, b"[MCP] Received INTENT_MCP_EXECUTE from bus\n");

            if intent == INTENT_MCP_EXECUTE {
                process_contract(&mut contracts_processed);
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

// ── Print helpers (no alloc) ──

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
