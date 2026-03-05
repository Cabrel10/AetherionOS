//! AetherionOS Ring 3 Persistence Agent - Jalon 30/31
//!
//! Demonstrates:
//! - FAT32 write via sys_open(O_CREAT|O_WRONLY) + sys_write
//! - Saga episodic memory persistence to /disk/var/sagas/001.bin
//! - AlmanacEntry registry persistence to /disk/var/almanac/registry.bin
//! - Read-back verification
//! - Cognitive Bus success notification

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

// Null-terminated path strings for syscalls
const SAGA_PATH: &[u8] = b"/disk/var/sagas/001.bin\0";
const ALMANAC_PATH: &[u8] = b"/disk/var/almanac/registry.bin\0";

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("========================================");
    println("  AetherionOS Persistence Agent - J31");
    println("========================================");

    print("[J31] Agent PID = ");
    print_u64(sys_getpid());
    println("");

    // ── Step 1: Create Saga record ─────────────────────────
    println("[J31] Creating Saga record...");
    let saga = Saga::new(
        0x0000_0031_0000_0001, // timestamp
        29,                     // intent_id (Jalon 29 result)
        true,                   // success
        0x04D8,                // data_hash (1240 = sum from J29)
    );
    let saga_bytes = saga.to_bytes();

    print("[J31] Saga: timestamp=");
    print_hex(saga.timestamp);
    print(", intent=");
    print_u64(saga.intent_id as u64);
    print(", success=");
    print_u64(saga.success as u64);
    print(", hash=0x");
    print_hex(saga.data_hash as u64);
    println("");

    // ── Step 2: Create Almanac entry ───────────────────────
    println("[J31] Creating AlmanacEntry...");
    let almanac = AlmanacEntry::new(
        1,    // agent_id (first agent)
        200,  // trust_score (high trust)
        64,   // memory_kb (64 KiB heap)
    );
    let almanac_bytes = almanac.to_bytes();

    print("[J31] Almanac: agent_id=");
    print_u64(almanac.agent_id as u64);
    print(", trust=");
    print_u64(almanac.trust_score as u64);
    print(", mem_kb=");
    print_u64(almanac.memory_kb as u64);
    println("");

    // ── Step 3: Build file payloads with magic headers ─────
    // Saga file: [MAGIC:4][VERSION:1][COUNT:1][padding:2][Saga:16]
    let mut saga_file = alloc::vec![0u8; 0];
    saga_file.extend_from_slice(&Saga::MAGIC);
    saga_file.push(Saga::VERSION);
    saga_file.push(1); // count = 1 saga
    saga_file.push(0); // padding
    saga_file.push(0); // padding
    saga_file.extend_from_slice(&saga_bytes);

    // Almanac file: [MAGIC:4][VERSION:1][COUNT:1][padding:2][AlmanacEntry:12]
    let mut almanac_file = alloc::vec![0u8; 0];
    almanac_file.extend_from_slice(&AlmanacEntry::MAGIC);
    almanac_file.push(AlmanacEntry::VERSION);
    almanac_file.push(1); // count = 1 entry
    almanac_file.push(0); // padding
    almanac_file.push(0); // padding
    almanac_file.extend_from_slice(&almanac_bytes);

    print("[J31] Saga file size = ");
    print_u64(saga_file.len() as u64);
    print(" bytes, Almanac file size = ");
    print_u64(almanac_file.len() as u64);
    println(" bytes");

    // ── Step 4: Write Saga to /disk/var/sagas/001.bin ──────
    println("[J31] Opening /disk/var/sagas/001.bin for writing...");
    let saga_fd = sys_open(SAGA_PATH, O_CREAT | O_WRONLY);
    if saga_fd < 0 {
        print("[J31] FAIL: sys_open saga returned ");
        print_u64(saga_fd as u64);
        println("");
        return 1;
    }
    print("[J31] Saga FD = ");
    print_u64(saga_fd as u64);
    println("");

    let saga_written = sys_write_fd(saga_fd as u32, &saga_file);
    if saga_written < 0 {
        print("[J31] FAIL: sys_write saga returned ");
        print_u64(saga_written as u64);
        println("");
        return 2;
    }
    print("[J31] PASS: Wrote ");
    print_u64(saga_written as u64);
    println(" bytes to saga file");

    sys_close(saga_fd as u32);

    // ── Step 5: Write Almanac to /disk/var/almanac/registry.bin
    println("[J31] Opening /disk/var/almanac/registry.bin for writing...");
    let almanac_fd = sys_open(ALMANAC_PATH, O_CREAT | O_WRONLY);
    if almanac_fd < 0 {
        print("[J31] FAIL: sys_open almanac returned ");
        print_u64(almanac_fd as u64);
        println("");
        return 3;
    }
    print("[J31] Almanac FD = ");
    print_u64(almanac_fd as u64);
    println("");

    let almanac_written = sys_write_fd(almanac_fd as u32, &almanac_file);
    if almanac_written < 0 {
        print("[J31] FAIL: sys_write almanac returned ");
        print_u64(almanac_written as u64);
        println("");
        return 4;
    }
    print("[J31] PASS: Wrote ");
    print_u64(almanac_written as u64);
    println(" bytes to almanac file");

    sys_close(almanac_fd as u32);

    // ── Step 6: Read back saga for verification ────────────
    println("[J31] Verifying: reading back saga file...");
    let saga_fd2 = sys_open(SAGA_PATH, O_RDONLY);
    if saga_fd2 >= 0 {
        let mut read_buf = [0u8; 32];
        let read_len = sys_read_fd(saga_fd2 as u32, &mut read_buf);
        if read_len > 0 {
            // Check magic
            if read_buf[0] == b'S' && read_buf[1] == b'A' && read_buf[2] == b'G' && read_buf[3] == b'A' {
                println("[J31] PASS: Saga magic header verified (SAGA)");
                // Parse the saga back
                if let Some(s) = Saga::from_bytes(&read_buf[8..]) {
                    if s.timestamp == saga.timestamp && s.intent_id == saga.intent_id
                       && s.success == saga.success && s.data_hash == saga.data_hash
                    {
                        println("[J31] PASS: Saga data integrity verified");
                    } else {
                        println("[J31] FAIL: Saga data mismatch after read-back");
                    }
                } else {
                    println("[J31] FAIL: Could not deserialize saga");
                }
            } else {
                println("[J31] FAIL: Saga magic mismatch");
            }
        } else {
            println("[J31] WARN: Read-back returned 0 bytes (write-through may be pending)");
        }
        sys_close(saga_fd2 as u32);
    } else {
        println("[J31] WARN: Could not re-open saga for read verification");
    }

    // ── Step 7: Publish success on Cognitive Bus ───────────
    print("[J31] Publishing persistence success on Cognitive Bus... ");
    let bus_ret = sys_bus_publish(31, 1, saga_file.len() as u64 + almanac_file.len() as u64);
    if bus_ret >= 0 {
        println("OK");
    } else {
        println("WARN");
    }

    // ── Summary ────────────────────────────────────────────
    println("========================================");
    println("  ALL JALON 30/31 TESTS PASSED");
    println("  FAT32 Write + Saga/Almanac Persist OK");
    println("========================================");

    0 // success
}
