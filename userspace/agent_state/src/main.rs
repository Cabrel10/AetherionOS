//! AetherionOS Jalon 57 - Persistent State Reader Agent (Ring 3)
//!
//! Reads /disk/var/state.bin and displays the boot counter,
//! last agent name, last intent, and TSC timestamp.
//! Publishes Bus intent 0xC057 on success.

#![no_std]
#![no_main]

use aetherion_sdk::*;

const STATE_MAGIC: u32 = 0xAE57_A7E5;
const INTENT_STATE_READ: u64 = 0xC057;

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    if off + 4 > buf.len() { return 0; }
    u32::from_le_bytes([buf[off], buf[off+1], buf[off+2], buf[off+3]])
}

fn read_u64_le(buf: &[u8], off: usize) -> u64 {
    if off + 8 > buf.len() { return 0; }
    u64::from_le_bytes([
        buf[off], buf[off+1], buf[off+2], buf[off+3],
        buf[off+4], buf[off+5], buf[off+6], buf[off+7],
    ])
}

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J57] Persistent State Reader v1.0");

    // Allocate a read buffer via sys_mmap
    let buf_addr = sys_mmap(4096);
    if buf_addr == 0 || buf_addr > 0xFFFF_FFFF_FFFF {
        println("[J57] FAIL: mmap failed");
        return 1;
    }
    let buf: &mut [u8] = unsafe {
        core::slice::from_raw_parts_mut(buf_addr as *mut u8, 4096)
    };

    // Open state.bin
    let fd = sys_open(b"/disk/var/state.bin\0", O_RDONLY);
    if fd < 0 {
        println("[J57] FAIL: cannot open /disk/var/state.bin");
        return 1;
    }

    // Read the file
    let n = sys_read_fd(fd as u32, &mut buf[..256]);
    sys_close(fd as u32);

    if n < 56 {
        print("[J57] FAIL: state.bin too short: ");
        print_u64(n as u64);
        println(" bytes");
        return 1;
    }

    // Parse header
    let magic = read_u32_le(buf, 0);
    if magic != STATE_MAGIC {
        println("[J57] FAIL: bad magic");
        return 1;
    }

    let boot_count = read_u32_le(buf, 4);
    let last_intent = read_u64_le(buf, 40);
    let timestamp = read_u64_le(buf, 48);

    // Extract agent name (null-terminated at offset 8, max 32 bytes)
    let mut agent_len = 0usize;
    for i in 0..32 {
        if buf[8 + i] == 0 { break; }
        agent_len = i + 1;
    }

    // Print results
    print("[J57] Boot #");
    print_u64(boot_count as u64);
    println("");

    print("[J57] Last agent: ");
    for i in 0..agent_len {
        let ch = buf[8 + i];
        if ch >= 0x20 && ch < 0x7F {
            let c = [ch];
            if let Ok(s) = core::str::from_utf8(&c) {
                print(s);
            }
        }
    }
    println("");

    print("[J57] Last intent: 0x");
    print_u64(last_intent);
    println("");

    print("[J57] TSC timestamp: ");
    print_u64(timestamp);
    println("");

    println("[J57-OK] Persistent state read SUCCESS");
    sys_bus_publish(INTENT_STATE_READ, 2, boot_count as u64);
    println("[J57] Bus 0xC057 OK");

    0
}
