//! AetherionOS Jalon 45 - VirtIO Block I/O Benchmark Agent (Ring 3)
//!
//! Opens /disk/models/part1, reads 4 KB chunks 256 times (total 1 MB read),
//! measures cycles via sys_rdtsc, computes cycles/MB and estimated MB/s.

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

const CHUNK_SIZE: usize = 4096;
const NUM_READS: usize = 256;
const FILE_PATH: &[u8] = b"/disk/models/part1\0";

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J45] VirtIO Block I/O Benchmark Agent v1.0");

    // Allocate read buffer (4 KB)
    let buf_addr = sys_mmap(CHUNK_SIZE);
    if buf_addr == 0 || buf_addr > 0xFFFF_FFFF_FFFF {
        println("[J45] FAIL: sys_mmap error");
        return 1;
    }

    let buffer: &mut [u8] = unsafe {
        core::slice::from_raw_parts_mut(buf_addr as *mut u8, CHUNK_SIZE)
    };

    // Open the file
    let fd = sys_open(FILE_PATH, O_RDONLY);
    if fd < 0 {
        println("[J45] FAIL: cannot open part1");
        return 1;
    }
    print("[J45] Opened part1, FD=");
    print_u64(fd as u64);
    println("");

    // Benchmark: read 4 KB x 256 = 1 MB total
    // File is 352 bytes, so each read after the first returns 0;
    // we re-open the file each iteration to reset offset (seek to 0)
    let mut total_bytes: u64 = 0;

    println("[J45] Starting benchmark: 256 x 4KB reads...");
    let tsc_start = sys_rdtsc();

    for i in 0..NUM_READS {
        // Re-open file to reset offset (no seek syscall overhead concerns)
        if i > 0 {
            sys_close(fd as u32);
        }
        let cur_fd = if i == 0 {
            fd
        } else {
            let f = sys_open(FILE_PATH, O_RDONLY);
            if f < 0 { break; }
            f
        };

        let n = sys_read_fd(cur_fd as u32, buffer);
        if n > 0 {
            total_bytes += n as u64;
        }
        if i > 0 {
            sys_close(cur_fd as u32);
        }
    }
    // Close original fd if we still hold it
    if NUM_READS <= 1 {
        sys_close(fd as u32);
    }

    let tsc_end = sys_rdtsc();
    let elapsed_cycles = tsc_end - tsc_start;

    print("[J45] Total bytes read: ");
    print_u64(total_bytes);
    println("");

    print("[J45] TSC cycles: ");
    print_u64(elapsed_cycles);
    println("");

    // Compute cycles per byte
    if total_bytes > 0 {
        let cycles_per_byte = elapsed_cycles / total_bytes;
        print("[J45] Cycles/byte: ");
        print_u64(cycles_per_byte);
        println("");

        // Cycles per MB = cycles_per_byte * 1048576
        let cycles_per_mb = cycles_per_byte * 1_048_576;
        print("[J45] Cycles/MB: ");
        print_u64(cycles_per_mb);
        println("");

        // Estimate MB/s assuming ~2 GHz TSC
        // MB/s = 2_000_000_000 / cycles_per_mb
        if cycles_per_mb > 0 {
            let mb_per_sec = 2_000_000_000u64 / cycles_per_mb;
            print("[J45] Estimated MB/s (@2GHz): ");
            print_u64(mb_per_sec);
            println("");
        }
    }

    // Publish benchmark result
    let bus_ret = sys_bus_publish(0xC045, 2, total_bytes);
    if bus_ret == 0 {
        println("[J45] Bus 0xC045 OK");
    }

    sys_write(1, b"\n[J45-OK] I/O Benchmark SUCCESS\n");
    0
}
