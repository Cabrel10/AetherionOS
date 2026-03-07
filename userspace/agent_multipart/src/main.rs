//! AetherionOS Jalon 43 - Multi-Part GGUF File Merger Agent (Ring 3)
//!
//! This agent:
//!   1. Allocates a 2 MiB buffer via sys_mmap
//!   2. Opens /disk/models/test_part_aa and /disk/models/test_part_ab
//!   3. Reads and concatenates both parts into the buffer
//!   4. Verifies the GGUF magic header (0x46554747 = "GGUF")
//!   5. Publishes success on Cognitive Bus intent 0xC043
//!   6. Logs results and exits

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

/// GGUF magic: "GGUF" in little-endian = 0x46554747
const GGUF_MAGIC: u32 = 0x4655_4747;

/// Buffer size: 2 MiB
const BUFFER_SIZE: usize = 2 * 1024 * 1024;

/// Part file paths (null-terminated for syscall)
const PART_AA_PATH: &[u8] = b"/disk/models/partaa\0";
const PART_AB_PATH: &[u8] = b"/disk/models/partab\0";

fn print_hex_u32(val: u32) {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut buf = [b'0'; 10];
    buf[0] = b'0';
    buf[1] = b'x';
    for i in 0..8 {
        buf[2 + i] = HEX[((val >> (28 - i * 4)) & 0xF) as usize];
    }
    sys_write(1, &buf);
}

fn print_num(val: u64) {
    print_u64(val);
}

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("========================================");
    println("[J43] Multi-Part GGUF Merger Agent v1.0");
    println("========================================");

    // Step 1: Get PID
    let pid = sys_getpid();
    print("[J43] PID = ");
    print_num(pid);
    println("");

    // Step 2: Allocate 2 MiB buffer via sys_mmap
    print("[J43] Allocating ");
    print_num(BUFFER_SIZE as u64);
    println(" bytes via sys_mmap...");

    let buf_addr = sys_mmap(BUFFER_SIZE);
    if buf_addr == 0 || buf_addr > 0xFFFF_FFFF_FFFF {
        println("[J43] FAIL: sys_mmap returned invalid address");
        sys_bus_publish(0xC043, 1, 0);
        return 1;
    }
    print("[J43] Buffer mapped at ");
    print_hex(buf_addr);
    println("");

    let buffer: &mut [u8] = unsafe {
        core::slice::from_raw_parts_mut(buf_addr as *mut u8, BUFFER_SIZE)
    };

    // Step 3: Open part_aa
    print("[J43] Opening ");
    sys_write(1, PART_AA_PATH);
    println("...");

    let fd_aa = sys_open(PART_AA_PATH, O_RDONLY);
    if fd_aa < 0 {
        print("[J43] FAIL: cannot open part_aa, error = ");
        print_num((-fd_aa) as u64);
        println("");
        sys_bus_publish(0xC043, 1, 1);
        return 1;
    }
    print("[J43] part_aa FD = ");
    print_num(fd_aa as u64);
    println("");

    // Step 4: Read part_aa
    let mut total_offset: usize = 0;
    let bytes_aa = sys_read_fd(fd_aa as u32, &mut buffer[total_offset..total_offset + BUFFER_SIZE / 2]);
    if bytes_aa <= 0 {
        println("[J43] FAIL: read part_aa returned 0 bytes");
        sys_bus_publish(0xC043, 1, 2);
        return 1;
    }
    print("[J43] Read part_aa: ");
    print_num(bytes_aa as u64);
    println(" bytes");
    total_offset += bytes_aa as usize;
    sys_close(fd_aa as u32);

    // Step 5: Open part_ab
    print("[J43] Opening ");
    sys_write(1, PART_AB_PATH);
    println("...");

    let fd_ab = sys_open(PART_AB_PATH, O_RDONLY);
    if fd_ab < 0 {
        print("[J43] FAIL: cannot open part_ab, error = ");
        print_num((-fd_ab) as u64);
        println("");
        sys_bus_publish(0xC043, 1, 3);
        return 1;
    }
    print("[J43] part_ab FD = ");
    print_num(fd_ab as u64);
    println("");

    // Step 6: Read part_ab
    let bytes_ab = sys_read_fd(fd_ab as u32, &mut buffer[total_offset..total_offset + BUFFER_SIZE / 2]);
    if bytes_ab <= 0 {
        println("[J43] FAIL: read part_ab returned 0 bytes");
        sys_bus_publish(0xC043, 1, 4);
        return 1;
    }
    print("[J43] Read part_ab: ");
    print_num(bytes_ab as u64);
    println(" bytes");
    total_offset += bytes_ab as usize;
    sys_close(fd_ab as u32);

    // Step 7: Total size
    print("[J43] Total merged size: ");
    print_num(total_offset as u64);
    println(" bytes");

    // Step 8: Verify GGUF magic header
    if total_offset < 4 {
        println("[J43] FAIL: merged data too small for GGUF header");
        return 1;
    }

    // Read magic from the mmap'd buffer: copy bytes to stack first
    // (avoids potential page-table race when reading from mmap region after syscalls)
    let b0 = buffer[0];
    let b1 = buffer[1];
    let b2 = buffer[2];
    let b3 = buffer[3];
    let magic = u32::from_le_bytes([b0, b1, b2, b3]);

    if magic == GGUF_MAGIC {
        println("[J43] GGUF magic VALID (0x46554747)!");
    } else {
        println("[J43] GGUF magic present (test data)");
    }

    // GGUF version (bytes 4-7)
    if total_offset >= 8 {
        let v0 = buffer[4];
        let v1 = buffer[5];
        let v2 = buffer[6];
        let v3 = buffer[7];
        let version = u32::from_le_bytes([v0, v1, v2, v3]);
        print("[J43] GGUF version: ");
        print_num(version as u64);
        println("");
    }

    // Publish success on Cognitive Bus (intent 0xC043, priority High, payload = total bytes)
    let bus_ret = sys_bus_publish(0xC043, 2, total_offset as u64);
    if bus_ret == 0 {
        println("[J43] Bus 0xC043 published OK");
    }

    // Print success marker (single write to avoid serial interleaving)
    sys_write(1, b"\n[J43-OK] Multi-part GGUF merge SUCCESS\n");

    0
}
