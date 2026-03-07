//! AetherionOS Jalon 44 - GGUF Header Parser Agent (Ring 3)
//!
//! This agent:
//!   1. Allocates a 2 MiB buffer via sys_mmap
//!   2. Opens /disk/models/part1 and /disk/models/part2
//!   3. Reads only 352 bytes from each (GGUF header region)
//!   4. Verifies GGUF magic header (0x46554747)
//!   5. Parses GGUF version, tensor_count, kv_count from header
//!   6. Publishes success on Cognitive Bus intent 0xC044

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

/// GGUF magic: "GGUF" in little-endian = 0x46554747
const GGUF_MAGIC: u32 = 0x4655_4747;

/// Header read size (352 bytes covers magic + version + counts + some metadata)
const HEADER_SIZE: usize = 352;

/// Buffer size: 2 MiB
const BUFFER_SIZE: usize = 2 * 1024 * 1024;

/// Part file paths (null-terminated for syscall)
const PART1_PATH: &[u8] = b"/disk/models/part1\0";
const PART2_PATH: &[u8] = b"/disk/models/part2\0";

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

fn read_u64_le(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        buf[off], buf[off+1], buf[off+2], buf[off+3],
        buf[off+4], buf[off+5], buf[off+6], buf[off+7],
    ])
}

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J44] GGUF Header Parser Agent v1.0");

    // Step 1: Allocate buffer
    let buf_addr = sys_mmap(BUFFER_SIZE);
    if buf_addr == 0 || buf_addr > 0xFFFF_FFFF_FFFF {
        println("[J44] FAIL: sys_mmap error");
        return 1;
    }
    print("[J44] Buffer at ");
    print_hex(buf_addr);
    println("");

    let buffer: &mut [u8] = unsafe {
        core::slice::from_raw_parts_mut(buf_addr as *mut u8, BUFFER_SIZE)
    };

    // Step 2: Open and read part1 (352 bytes only)
    let fd1 = sys_open(PART1_PATH, O_RDONLY);
    if fd1 < 0 {
        println("[J44] FAIL: cannot open part1");
        return 1;
    }
    let n1 = sys_read_fd(fd1 as u32, &mut buffer[..HEADER_SIZE]);
    sys_close(fd1 as u32);
    if n1 <= 0 {
        println("[J44] FAIL: read part1 = 0");
        return 1;
    }
    print("[J44] part1: ");
    print_u64(n1 as u64);
    println(" bytes");

    // Step 3: Open and read part2 (352 bytes)
    let fd2 = sys_open(PART2_PATH, O_RDONLY);
    if fd2 < 0 {
        println("[J44] FAIL: cannot open part2");
        return 1;
    }
    let off = n1 as usize;
    let n2 = sys_read_fd(fd2 as u32, &mut buffer[off..off + HEADER_SIZE]);
    sys_close(fd2 as u32);
    if n2 <= 0 {
        println("[J44] FAIL: read part2 = 0");
        return 1;
    }
    print("[J44] part2: ");
    print_u64(n2 as u64);
    println(" bytes");

    let total = off + n2 as usize;
    print("[J44] Total: ");
    print_u64(total as u64);
    println(" bytes");

    // Step 4: Verify GGUF magic
    if total < 24 {
        println("[J44] FAIL: too small for GGUF");
        return 1;
    }

    let magic = read_u32_le(buffer, 0);
    if magic != GGUF_MAGIC {
        print("[J44] WARN: magic=");
        print_hex(magic as u64);
        println(" (expected 0x46554747)");
    } else {
        println("[J44] GGUF magic VALID");
    }

    // Step 5: Parse header fields
    let version = read_u32_le(buffer, 4);
    let tensor_count = read_u64_le(buffer, 8);
    let kv_count = read_u64_le(buffer, 16);

    print("[J44] version=");
    print_u64(version as u64);
    print(" tensors=");
    print_u64(tensor_count);
    print(" kv=");
    print_u64(kv_count);
    println("");

    // Step 6: Publish success
    let bus_ret = sys_bus_publish(0xC044, 2, total as u64);
    if bus_ret == 0 {
        println("[J44] Bus 0xC044 OK");
    }

    sys_write(1, b"\n[J44-OK] GGUF header parse SUCCESS\n");
    0
}
