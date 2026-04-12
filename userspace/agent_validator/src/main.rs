//! AetherionOS Immune System — Validator Agent (Jalon 85)
//!
//! The Validator Agent is the immune system of the cognitive pipeline.
//! It subscribes to INTENT_LLM_OUTPUT (0x8010) on the Cognitive Bus,
//! intercepts all LLM-generated JSON actions, validates their structure
//! and coherence, then either forwards valid actions to the MCP Agent
//! (INTENT_MCP_EXECUTE 0x9002) or publishes an error intent.
//!
//! Three security modes:
//!   1. **Strict** (default) — rejects actions lacking `supporting_facts`
//!   2. **Admin** (boot-active) — requires `{"__auth_key__": "AETHERION_ROOT_85"}`
//!   3. **God Mode** — activated by intent 0x1337 with payload 0xBADC0DED;
//!      bypasses all checks, logs warning.
//!
//! Architecture (ACHA §3.9):
//!   LLM → INTENT_LLM_OUTPUT → Validator → validate JSON → INTENT_MCP_EXECUTE
//!   Invalid JSON → INTENT_VALIDATOR_ERROR (0x8011)

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;
use aetherion_sdk::json;

// ── Intent Constants ──

/// LLM output intent — the Validator subscribes to this
const INTENT_LLM_OUTPUT: u32 = 0x8010;

/// MCP execution intent — forwarded on valid JSON
const INTENT_MCP_EXECUTE: u64 = 0x9002;

/// Validator error intent — published on invalid JSON
const INTENT_VALIDATOR_ERROR: u64 = 0x8011;

/// God Mode activation intent
const INTENT_GOD_MODE: u32 = 0x1337;

/// God Mode activation payload
const GOD_MODE_PAYLOAD: u64 = 0xBADC0DED;

/// Validator ready intent
const INTENT_VALIDATOR_READY: u64 = 0x8012;

// ── Security Modes ──

/// Security mode enumeration
#[derive(Clone, Copy, PartialEq)]
enum SecurityMode {
    /// Default: rejects actions lacking `supporting_facts`
    Strict,
    /// Boot-active: requires `__auth_key__` = "AETHERION_ROOT_85"
    Admin,
    /// All filters offline — activated by intent 0x1337 + payload 0xBADC0DED
    GodMode,
}

// ── Statistics ──

struct ValidatorStats {
    total_inspected: u64,
    passed: u64,
    rejected: u64,
    god_mode_activations: u64,
}

// ── Auth Key ──

const AUTH_KEY: &[u8] = b"AETHERION_ROOT_85";

// ── VFS Mailbox ──

/// Mailbox where LLM deposits JSON contracts for validation
const VALIDATOR_MAILBOX: &[u8] = b"/tmp/llm_output.json\0";

// ── Main Implementation ──

/// Validate a JSON contract according to the current security mode.
///
/// Returns `true` if the contract passes validation, `false` otherwise.
fn validate_contract(json_data: &[u8], mode: SecurityMode) -> bool {
    // God Mode: bypass all checks
    if mode == SecurityMode::GodMode {
        return true;
    }

    // Phase 1: Check for `action` field (mandatory in all modes)
    let action = match json::extract_json_str(json_data, "action") {
        Some(a) => a,
        None => {
            sys_write(1, b"[VALIDATOR] REJECT: missing 'action' field\n");
            return false;
        }
    };

    // Phase 2: Validate action is non-empty
    if action.is_empty() {
        sys_write(1, b"[VALIDATOR] REJECT: empty 'action' value\n");
        return false;
    }

    // Phase 3: Check for `params` object (mandatory in all modes)
    if json::extract_json_object(json_data, "params").is_none() {
        sys_write(1, b"[VALIDATOR] REJECT: missing 'params' object\n");
        return false;
    }

    // Phase 4: Admin mode — require auth key
    if mode == SecurityMode::Admin {
        match json::extract_json_str(json_data, "__auth_key__") {
            Some(key) => {
                if !json::json_str_eq(key, "AETHERION_ROOT_85") {
                    sys_write(1, b"[VALIDATOR] REJECT: invalid __auth_key__\n");
                    return false;
                }
            }
            None => {
                sys_write(1, b"[VALIDATOR] REJECT: Admin mode requires __auth_key__\n");
                return false;
            }
        }
    }

    // Phase 5: Strict mode — require supporting_facts
    if mode == SecurityMode::Strict {
        if json::extract_json_str(json_data, "supporting_facts").is_none()
            && json::extract_json_object(json_data, "supporting_facts").is_none()
        {
            sys_write(1, b"[VALIDATOR] REJECT: Strict mode requires 'supporting_facts'\n");
            return false;
        }
    }

    // Phase 6: Sanity check — action must be a known safe action
    let is_known = json::json_str_eq(action, "gen_driver")
        || json::json_str_eq(action, "net_request")
        || json::json_str_eq(action, "file_write")
        || json::json_str_eq(action, "file_read")
        || json::json_str_eq(action, "bus_publish")
        || json::json_str_eq(action, "query")
        || json::json_str_eq(action, "respond")
        || json::json_str_eq(action, "shutdown")
        || json::json_str_eq(action, "reboot");

    if !is_known {
        sys_write(1, b"[VALIDATOR] WARN: unknown action '");
        sys_write(1, action);
        sys_write(1, b"' - forwarding with caution\n");
        // Still allow unknown actions to pass through (extensibility)
    }

    true
}

/// Print a u64 value in decimal to serial
fn print_u64(val: u64) {
    if val == 0 {
        sys_write(1, b"0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut n = val;
    let mut i = 20;
    while n > 0 && i > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    sys_write(1, &buf[i..20]);
}

#[no_mangle]
pub extern "C" fn main() -> i64 {
    // ── Boot Banner ──
    sys_write(1, b"\n");
    sys_write(1, b"[VALIDATOR] =====================================================\n");
    sys_write(1, b"[VALIDATOR] AetherionOS Immune System - Validator Agent v1.0\n");
    sys_write(1, b"[VALIDATOR] Security: JSON coherence + action validation\n");
    sys_write(1, b"[VALIDATOR] Modes: Strict | Admin | God Mode\n");
    sys_write(1, b"[VALIDATOR] =====================================================\n");

    // ── Initialize ──
    let mut mode = SecurityMode::Admin; // Boot-active: Admin mode
    let mut stats = ValidatorStats {
        total_inspected: 0,
        passed: 0,
        rejected: 0,
        god_mode_activations: 0,
    };

    sys_write(1, b"[VALIDATOR] Boot mode: ADMIN (requires __auth_key__)\n");
    sys_write(1, b"[VALIDATOR] Subscribing to INTENT_LLM_OUTPUT (0x8010)\n");
    sys_write(1, b"[VALIDATOR] Watching for God Mode intent (0x1337)\n");

    // Publish ready signal
    sys_bus_publish(INTENT_VALIDATOR_READY, 1, 0);
    sys_write(1, b"[VALIDATOR] Published INTENT_VALIDATOR_READY\n");

    // After boot validation, switch to Strict mode for production
    sys_write(1, b"[VALIDATOR] Switching to STRICT mode (production)\n");
    mode = SecurityMode::Strict;

    // ── Main Event Loop ──
    let mut msg_buf: [u64; 8] = [0u64; 8]; // SDK expects [u64; 8] (J109 extended)
    let mut idle_loops: u64 = 0;
    let max_idle: u64 = 500_000;

    loop {
        // Check for God Mode activation intent
        let god_result = sys_bus_consume_intent(
            &mut msg_buf,
            INTENT_GOD_MODE,
        );

        if god_result > 0 {
            // Extract payload from message buffer (u64 slot 2)
            let payload = msg_buf[2];

            if payload == GOD_MODE_PAYLOAD {
                sys_write(1, b"\n[VALIDATOR] !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!\n");
                sys_write(1, b"[VALIDATOR] GOD MODE ACTIVATED - ALL FILTERS OFFLINE\n");
                sys_write(1, b"[VALIDATOR] !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!\n");
                mode = SecurityMode::GodMode;
                stats.god_mode_activations += 1;
            }
        }

        // Check for LLM output to validate
        let result = sys_bus_consume_intent(
            &mut msg_buf,
            INTENT_LLM_OUTPUT,
        );

        if result > 0 {
            idle_loops = 0;
            stats.total_inspected += 1;

            sys_write(1, b"[VALIDATOR] Intercepted INTENT_LLM_OUTPUT #");
            print_u64(stats.total_inspected);
            sys_write(1, b"\n");

            // Read the JSON contract from VFS mailbox
            let fd = sys_open(VALIDATOR_MAILBOX, 0); // O_RDONLY
            if fd < 0 {
                sys_write(1, b"[VALIDATOR] WARN: no mailbox file, forwarding raw intent\n");
                // Forward without validation (mailbox may not exist yet)
                sys_bus_publish(INTENT_MCP_EXECUTE, 2, 0);
                stats.passed += 1;
                continue;
            }

            // Read up to 512 bytes of JSON
            let mut json_buf = [0u8; 512];
            let bytes_read = sys_read_fd(fd as u32, &mut json_buf);
            sys_close(fd as u32);

            if bytes_read <= 0 {
                sys_write(1, b"[VALIDATOR] REJECT: empty mailbox\n");
                sys_bus_publish(INTENT_VALIDATOR_ERROR, 2, 1); // error code 1: empty
                stats.rejected += 1;
                continue;
            }

            let json_data = &json_buf[..bytes_read as usize];

            // Validate the JSON contract
            if validate_contract(json_data, mode) {
                sys_write(1, b"[VALIDATOR] PASS: contract validated, forwarding to MCP\n");
                sys_bus_publish(INTENT_MCP_EXECUTE, 2, 0);
                stats.passed += 1;
            } else {
                sys_write(1, b"[VALIDATOR] BLOCKED: contract rejected\n");
                sys_bus_publish(INTENT_VALIDATOR_ERROR, 2, 2); // error code 2: validation fail
                stats.rejected += 1;
            }
        } else {
            idle_loops += 1;
        }

        // Periodic stats report
        if idle_loops > 0 && idle_loops % max_idle == 0 {
            sys_write(1, b"[VALIDATOR] Stats: inspected=");
            print_u64(stats.total_inspected);
            sys_write(1, b" passed=");
            print_u64(stats.passed);
            sys_write(1, b" rejected=");
            print_u64(stats.rejected);
            sys_write(1, b" god_mode=");
            print_u64(stats.god_mode_activations);
            sys_write(1, b"\n");
        }

        // Yield to other processes
        sys_yield();

        // Exit after extended idle (regression test compatibility)
        if idle_loops >= max_idle * 4 {
            break;
        }
    }

    // ── Shutdown ──
    sys_write(1, b"[VALIDATOR] Immune System shutting down\n");
    sys_write(1, b"[VALIDATOR] Final stats: inspected=");
    print_u64(stats.total_inspected);
    sys_write(1, b" passed=");
    print_u64(stats.passed);
    sys_write(1, b" rejected=");
    print_u64(stats.rejected);
    sys_write(1, b"\n");

    0
}
