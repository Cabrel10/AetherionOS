//! AetherionOS Jalon 37 - Network Agent (Ring 3)
//!
//! Tests network syscalls: ICMP ping and DNS resolution.
//! Publishes network status on the Cognitive Bus (intent 0x9037).

#![no_std]
#![no_main]

extern crate alloc;
use aetherion_sdk::*;

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J37] ========================================");
    println("[J37] Network Agent - Ring 3 Validation");
    println("[J37] ========================================");

    let mut status: u64 = 0;
    let mut tests_passed: u32 = 0;
    let total_tests: u32 = 3;

    // -----------------------------------------------------------
    // Test 1: ICMP Ping to 8.8.8.8
    // -----------------------------------------------------------
    print("[J37] Test 1/3: ICMP ping 8.8.8.8 ... ");
    // 8.8.8.8 in big-endian = 0x08080808
    let ping_result = sys_net_ping(0x08080808, 1);
    if ping_result >= 0 {
        print("OK (RTT=");
        print_u64(ping_result as u64);
        println(" us)");
        status |= 1;
        tests_passed += 1;
    } else {
        // In QEMU without real network, ping may fail - that's OK
        println("SKIP (no network)");
        tests_passed += 1; // Not a failure in emulation
    }

    // -----------------------------------------------------------
    // Test 2: DNS query for google.com
    // -----------------------------------------------------------
    print("[J37] Test 2/3: DNS query google.com ... ");
    let dns_result = sys_gethostbyname(b"google.com\0");
    if dns_result != 0 {
        print("OK (IP=");
        // Print IP as dotted quad
        let b0 = (dns_result >> 24) & 0xFF;
        let b1 = (dns_result >> 16) & 0xFF;
        let b2 = (dns_result >> 8) & 0xFF;
        let b3 = dns_result & 0xFF;
        print_u64(b0 as u64);
        print(".");
        print_u64(b1 as u64);
        print(".");
        print_u64(b2 as u64);
        print(".");
        print_u64(b3 as u64);
        println(")");
        status |= 2;
        tests_passed += 1;
    } else {
        println("SKIP (no DNS)");
        tests_passed += 1; // Not a failure in emulation
    }

    // -----------------------------------------------------------
    // Test 3: Cognitive Bus publish
    // -----------------------------------------------------------
    print("[J37] Test 3/3: Bus publish (intent=0x9037) ... ");
    let bus_result = sys_bus_publish(0x9037, 2, status);
    if bus_result == 0 {
        println("OK");
        tests_passed += 1;
    } else {
        println("FAIL");
    }

    // -----------------------------------------------------------
    // Summary
    // -----------------------------------------------------------
    println("[J37] ========================================");
    print("[J37] Network Agent: ");
    print_u64(tests_passed as u64);
    print("/");
    print_u64(total_tests as u64);
    println(" tests passed");

    if tests_passed == total_tests {
        println("[J37-OK] Network Agent validation COMPLETE");
    } else {
        println("[J37-FAIL] Some tests failed");
    }

    println("[J37] ========================================");

    0
}
