//! AetherionOS Jalon 112a - Clock Sensor Agent
//!
//! System sensor that publishes INTENT_TIMER_TICK (0x112A) every second.
//! Uses sys_rdtsc() and assumes ~2 GHz CPU to compute elapsed seconds.
//! Each tick carries session_id=0 (system) and the uptime as payload.
//!
//! This is the first "sensor" agent in the multi-agent architecture.
//! Other agents (e.g., agent_wm) consume INTENT_TIMER_TICK to display
//! real-time uptime counters, schedule periodic tasks, etc.

#![no_std]
#![no_main]

extern crate alloc;
use aetherion_sdk::*;

/// Intent ID for timer tick events (Jalon 112a)
const INTENT_TIMER_TICK: u64 = 0x112A;

/// Assumed CPU frequency for TSC-to-seconds conversion (~2 GHz QEMU Haswell)
const TSC_FREQ_HZ: u64 = 2_000_000_000;

/// Broadcast session_id for system-level messages
const SYSTEM_SESSION: u64 = 0;

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J112a] ====================================");
    println("[J112a] Clock Sensor Agent - Starting");
    println("[J112a] Publishing INTENT_TIMER_TICK (0x112A)");
    println("[J112a] Assumed CPU freq: ~2 GHz (Haswell)");
    println("[J112a] ====================================");

    // Read initial TSC value as baseline
    let tsc_start = sys_rdtsc();
    let mut last_second: u64 = 0;
    let mut tick_count: u64 = 0;

    // Publish initial ready signal
    sys_bus_publish_ext(INTENT_TIMER_TICK, 1, 0, SYSTEM_SESSION, 0);

    print("[J112a] Clock sensor active, baseline TSC=");
    print_hex(tsc_start);
    println("");

    // Main sensor loop: compute elapsed seconds, publish on each new second
    loop {
        let tsc_now = sys_rdtsc();
        let elapsed_cycles = tsc_now.wrapping_sub(tsc_start);
        let elapsed_seconds = elapsed_cycles / TSC_FREQ_HZ;

        if elapsed_seconds > last_second {
            last_second = elapsed_seconds;
            tick_count += 1;

            // Publish INTENT_TIMER_TICK with uptime as payload
            // session_id=0 (system), correlation_id=tick_count
            sys_bus_publish_ext(
                INTENT_TIMER_TICK,
                1,  // Normal priority
                elapsed_seconds,
                SYSTEM_SESSION,
                tick_count,
            );

            // Log first few ticks and then periodic
            if tick_count <= 5 || tick_count % 30 == 0 {
                print("[J112a] TICK #");
                print_u64(tick_count);
                print(" uptime=");
                print_u64(elapsed_seconds);
                println("s");
            }
        }

        // Yield CPU to avoid burning cycles
        sys_yield();
    }
}
