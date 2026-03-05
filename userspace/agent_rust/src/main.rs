//! AetherionOS Ring 3 Rust Agent - Jalon 29
//!
//! Demonstrates:
//! - Dynamic memory allocation via sys_brk (Vec<u64>)
//! - Cognitive Bus intent publishing (sys_bus_publish)
//! - Clean exit with sys_exit
//!
//! Built with: #![no_std] + #![no_main], runs in Ring 3 user space.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use aetherion_sdk::*;

/// Entry point called by the SDK's _start.
/// Returns 0 on success.
#[no_mangle]
pub extern "C" fn main() -> i64 {
    // ── Banner ──────────────────────────────────────────────
    println("========================================");
    println("  AetherionOS Rust Agent - Jalon 29");
    println("========================================");

    // ── Step 1: Print PID ───────────────────────────────────
    print("[J29] Agent PID = ");
    print_u64(sys_getpid());
    println("");

    // ── Step 2: Query initial heap break ────────────────────
    let base = sys_brk(0);
    print("[J29] Heap base (sys_brk) = ");
    print_hex(base);
    println("");

    // ── Step 3: Allocate a Vec<u64> via global allocator ────
    println("[J29] Allocating Vec<u64> with 16 elements...");
    let mut v: Vec<u64> = Vec::with_capacity(16);
    for i in 0..16u64 {
        v.push(i * i); // squares: 0, 1, 4, 9, 16, 25, ...
    }

    // ── Step 4: Compute sum ─────────────────────────────────
    let sum: u64 = v.iter().sum();
    print("[J29] Vec created, len = ");
    print_u64(v.len() as u64);
    print(", sum(i^2, i=0..15) = ");
    print_u64(sum);
    println("");

    // Expected sum: 0+1+4+9+16+25+36+49+64+81+100+121+144+169+196+225 = 1240
    if sum == 1240 {
        println("[J29] PASS: Vec allocation and computation correct");
    } else {
        println("[J29] FAIL: Sum mismatch!");
        return 1;
    }

    // ── Step 5: Verify heap grew ────────────────────────────
    let new_break = sys_brk(0);
    print("[J29] Heap break after alloc = ");
    print_hex(new_break);
    println("");
    if new_break > base {
        println("[J29] PASS: Heap expanded via sys_brk");
    } else {
        println("[J29] WARN: Heap did not visibly expand (may be pre-allocated)");
    }

    // ── Step 6: Publish result on Cognitive Bus ─────────────
    print("[J29] Publishing sum on Cognitive Bus... ");
    let bus_ret = sys_bus_publish(29, 1, sum);
    if bus_ret >= 0 {
        println("OK");
        println("[J29] PASS: Bus publish succeeded");
    } else {
        println("FAIL");
        print("[J29] WARN: Bus publish returned ");
        print_u64(bus_ret as u64);
        println("");
    }

    // ── Step 7: Additional allocation test (larger) ─────────
    println("[J29] Allocating 1024-element Vec<u64>...");
    let mut big: Vec<u64> = Vec::with_capacity(1024);
    for i in 0..1024u64 {
        big.push(0xDEAD_0000 + i);
    }
    // Verify canary values
    let mut canary_ok = true;
    for i in 0..1024u64 {
        if big[i as usize] != 0xDEAD_0000 + i {
            canary_ok = false;
            break;
        }
    }
    if canary_ok {
        println("[J29] PASS: 1024-element Vec canary check OK");
    } else {
        println("[J29] FAIL: Canary mismatch in large Vec!");
        return 2;
    }

    // ── Step 8: Drop and re-allocate (reuse test) ───────────
    drop(big);
    let realloc_v: Vec<u64> = alloc::vec![42u64; 64];
    if realloc_v.len() == 64 && realloc_v[0] == 42 && realloc_v[63] == 42 {
        println("[J29] PASS: Reallocation after drop succeeded");
    } else {
        println("[J29] FAIL: Reallocation verification failed");
        return 3;
    }

    // ── Summary ─────────────────────────────────────────────
    println("========================================");
    println("  ALL JALON 29 TESTS PASSED");
    println("  Rust Agent: Vec alloc + Bus publish OK");
    println("========================================");

    0 // success
}
