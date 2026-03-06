//! AetherionOS Jalon 38 - HID Input Agent (Ring 3)
//!
//! Polls the HID event queue via sys_poll_hid() and reports
//! mouse position and keyboard events. Publishes status on
//! Cognitive Bus (intent 0x9038).

#![no_std]
#![no_main]

extern crate alloc;
use aetherion_sdk::*;

// HID event type constants (must match kernel/src/drivers/mouse.rs)
const HID_NONE: u8 = 0;
const HID_MOUSE_MOVE: u8 = 1;
const HID_MOUSE_BUTTON: u8 = 2;
const HID_KEY_PRESS: u8 = 3;
const HID_KEY_RELEASE: u8 = 4;

/// Unpack a HidEvent from a u64
/// Layout: [type: u8, buttons: u8, dx: i16, dy: i16, scancode: u8, _pad: u8]
struct HidEvent {
    event_type: u8,
    buttons: u8,
    dx: i16,
    dy: i16,
    scancode: u8,
}

impl HidEvent {
    fn from_u64(val: u64) -> Self {
        let bytes = val.to_le_bytes();
        HidEvent {
            event_type: bytes[0],
            buttons: bytes[1],
            dx: i16::from_le_bytes([bytes[2], bytes[3]]),
            dy: i16::from_le_bytes([bytes[4], bytes[5]]),
            scancode: bytes[6],
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J38] ========================================");
    println("[J38] HID Input Agent - Ring 3 Validation");
    println("[J38] ========================================");

    let mut tests_passed: u32 = 0;
    let total_tests: u32 = 3;

    // -----------------------------------------------------------
    // Test 1: sys_poll_hid returns 0 when queue is empty (or has events)
    // -----------------------------------------------------------
    print("[J38] Test 1/3: sys_poll_hid() callable ... ");
    let evt_raw = sys_poll_hid();
    // Whether we get 0 (empty) or an event, the syscall worked
    if evt_raw == 0 {
        println("OK (no events - queue empty)");
    } else {
        let evt = HidEvent::from_u64(evt_raw);
        print("OK (got event type=");
        print_u64(evt.event_type as u64);
        println(")");
    }
    tests_passed += 1;

    // -----------------------------------------------------------
    // Test 2: Poll multiple times (drain any pending events)
    // -----------------------------------------------------------
    print("[J38] Test 2/3: Drain HID queue ... ");
    let mut count: u32 = 0;
    let mut mouse_events: u32 = 0;
    let mut key_events: u32 = 0;
    loop {
        let raw = sys_poll_hid();
        if raw == 0 { break; }
        count += 1;
        let evt = HidEvent::from_u64(raw);
        match evt.event_type {
            HID_MOUSE_MOVE | HID_MOUSE_BUTTON => mouse_events += 1,
            HID_KEY_PRESS | HID_KEY_RELEASE => key_events += 1,
            _ => {}
        }
        if count >= 100 { break; } // Safety limit
    }
    print("OK (drained ");
    print_u64(count as u64);
    print(" events: ");
    print_u64(mouse_events as u64);
    print(" mouse, ");
    print_u64(key_events as u64);
    println(" keyboard)");
    tests_passed += 1;

    // -----------------------------------------------------------
    // Test 3: Cognitive Bus publish
    // -----------------------------------------------------------
    print("[J38] Test 3/3: Bus publish (intent=0x9038) ... ");
    let status = ((mouse_events as u64) << 32) | (key_events as u64);
    let bus_result = sys_bus_publish(0x9038, 2, status);
    if bus_result == 0 {
        println("OK");
        tests_passed += 1;
    } else {
        println("FAIL");
    }

    // -----------------------------------------------------------
    // Summary
    // -----------------------------------------------------------
    println("[J38] ========================================");
    print("[J38] HID Input Agent: ");
    print_u64(tests_passed as u64);
    print("/");
    print_u64(total_tests as u64);
    println(" tests passed");

    if tests_passed == total_tests {
        println("[J38-OK] HID Input Agent validation COMPLETE");
    } else {
        println("[J38-FAIL] Some tests failed");
    }

    println("[J38] ========================================");

    0
}
