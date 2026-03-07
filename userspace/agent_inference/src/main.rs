//! AetherionOS Jalon 47 - GGUF Tensor Metadata Inspector (Ring 3)
//!
//! Reads the GGUF header from /disk/models/part1, parses:
//!   - Magic, version, tensor count, KV count
//!   - First 5 tensor names + dimensions + type
//!   - Estimates total parameters
//! Publishes intent 0xE047 with parameter count.

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

const FILE_PATH: &[u8] = b"/disk/models/part1\0";
const HEADER_BUF_SIZE: usize = 4096;

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

/// GGUF value types
const GGUF_TYPE_UINT8: u32 = 0;
const GGUF_TYPE_INT8: u32 = 1;
const GGUF_TYPE_UINT16: u32 = 2;
const GGUF_TYPE_INT16: u32 = 3;
const GGUF_TYPE_UINT32: u32 = 4;
const GGUF_TYPE_INT32: u32 = 5;
const GGUF_TYPE_FLOAT32: u32 = 6;
const GGUF_TYPE_BOOL: u32 = 7;
const GGUF_TYPE_STRING: u32 = 8;
const GGUF_TYPE_ARRAY: u32 = 9;
const GGUF_TYPE_UINT64: u32 = 10;
const GGUF_TYPE_INT64: u32 = 11;
const GGUF_TYPE_FLOAT64: u32 = 12;

/// Skip a GGUF KV value and return new offset
fn skip_gguf_value(buf: &[u8], off: usize, vtype: u32) -> usize {
    match vtype {
        GGUF_TYPE_UINT8 | GGUF_TYPE_INT8 | GGUF_TYPE_BOOL => off + 1,
        GGUF_TYPE_UINT16 | GGUF_TYPE_INT16 => off + 2,
        GGUF_TYPE_UINT32 | GGUF_TYPE_INT32 | GGUF_TYPE_FLOAT32 => off + 4,
        GGUF_TYPE_UINT64 | GGUF_TYPE_INT64 | GGUF_TYPE_FLOAT64 => off + 8,
        GGUF_TYPE_STRING => {
            let slen = read_u64_le(buf, off) as usize;
            off + 8 + slen
        },
        GGUF_TYPE_ARRAY => {
            let arr_type = read_u32_le(buf, off);
            let arr_len = read_u64_le(buf, off + 4) as usize;
            let mut p = off + 12;
            for _ in 0..arr_len {
                if p >= buf.len() { break; }
                p = skip_gguf_value(buf, p, arr_type);
            }
            p
        },
        _ => off + 4, // unknown, skip 4 bytes
    }
}

/// GGUF tensor type names
fn tensor_type_name(t: u32) -> &'static [u8] {
    match t {
        0 => b"F32",
        1 => b"F16",
        2 => b"Q4_0",
        3 => b"Q4_1",
        6 => b"Q5_0",
        7 => b"Q5_1",
        8 => b"Q8_0",
        9 => b"Q8_1",
        10 => b"Q2_K",
        11 => b"Q3_K",
        12 => b"Q4_K",
        13 => b"Q5_K",
        14 => b"Q6_K",
        _ => b"UNK",
    }
}

/// Bytes per element for tensor type
fn tensor_type_bpe(t: u32) -> u64 {
    match t {
        0 => 4,  // F32
        1 => 2,  // F16
        _ => 2,  // quantized ~2 bytes avg
    }
}

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J47] GGUF Tensor Metadata Inspector v1.0");

    // Allocate buffer
    let buf_addr = sys_mmap(HEADER_BUF_SIZE);
    if buf_addr == 0 || buf_addr > 0xFFFF_FFFF_FFFF {
        println("[J47] FAIL: mmap error");
        return 1;
    }

    let buffer: &mut [u8] = unsafe {
        core::slice::from_raw_parts_mut(buf_addr as *mut u8, HEADER_BUF_SIZE)
    };

    // Open and read file
    let fd = sys_open(FILE_PATH, O_RDONLY);
    if fd < 0 {
        println("[J47] FAIL: cannot open part1");
        return 1;
    }
    let n = sys_read_fd(fd as u32, buffer);
    sys_close(fd as u32);
    if n <= 0 {
        println("[J47] FAIL: read = 0");
        return 1;
    }
    let file_len = n as usize;
    print("[J47] Read ");
    print_u64(file_len as u64);
    println(" bytes from part1");

    // Parse GGUF header
    let magic = read_u32_le(buffer, 0);
    if magic != 0x4655_4747 {
        println("[J47] FAIL: not GGUF");
        return 1;
    }
    println("[J47] GGUF magic OK");

    let version = read_u32_le(buffer, 4);
    let tensor_count = read_u64_le(buffer, 8);
    let kv_count = read_u64_le(buffer, 16);

    print("[J47] v");
    print_u64(version as u64);
    print(" tensors=");
    print_u64(tensor_count);
    print(" kv=");
    print_u64(kv_count);
    println("");

    // Skip KV pairs to reach tensor info
    let mut offset: usize = 24; // after header

    for _kv in 0..kv_count {
        if offset + 12 >= file_len { break; }
        // key: len(u64) + string
        let key_len = read_u64_le(buffer, offset) as usize;
        offset += 8 + key_len;
        if offset + 4 >= file_len { break; }
        // value type
        let vtype = read_u32_le(buffer, offset);
        offset += 4;
        // skip value
        offset = skip_gguf_value(buffer, offset, vtype);
    }

    print("[J47] Tensor info starts at offset ");
    print_u64(offset as u64);
    println("");

    // Parse up to 5 tensor info entries
    let mut total_params: u64 = 0;
    let show_count = if tensor_count < 5 { tensor_count as usize } else { 5 };

    for i in 0..tensor_count as usize {
        if offset + 8 >= file_len { break; }

        // Tensor name: len(u64) + string
        let name_len = read_u64_le(buffer, offset) as usize;
        offset += 8;
        let name_end = offset + name_len;
        if name_end > file_len { break; }

        // Print first 5 tensor names
        if i < show_count {
            print("[J47] T");
            print_u64(i as u64);
            print(": ");
            // Print tensor name (limited to 40 chars)
            let print_len = if name_len > 40 { 40 } else { name_len };
            sys_write(1, &buffer[offset..offset + print_len]);
        }
        offset = name_end;

        if offset + 4 >= file_len { break; }
        // n_dims
        let n_dims = read_u32_le(buffer, offset) as usize;
        offset += 4;

        // dimensions
        let mut params: u64 = 1;
        for d in 0..n_dims {
            if offset + 8 > file_len { break; }
            let dim = read_u64_le(buffer, offset);
            offset += 8;
            params *= dim;
            if i < show_count {
                if d == 0 { print(" ["); }
                print_u64(dim);
                if d + 1 < n_dims { print("x"); }
                else { print("]"); }
            }
        }

        if offset + 4 > file_len { break; }
        // tensor type
        let ttype = read_u32_le(buffer, offset);
        offset += 4;

        if i < show_count {
            print(" ");
            sys_write(1, tensor_type_name(ttype));
            print(" params=");
            print_u64(params);
            println("");
        }

        // offset into data
        if offset + 8 > file_len { break; }
        let _data_offset = read_u64_le(buffer, offset);
        offset += 8;

        total_params += params;
    }

    print("[J47] Total parameters: ");
    print_u64(total_params);
    println("");

    // Publish result
    let bus_ret = sys_bus_publish(0xE047, 2, total_params);
    if bus_ret == 0 {
        println("[J47] Bus 0xE047 OK");
    }

    sys_write(1, b"\n[J47-OK] GGUF tensor metadata inspect SUCCESS\n");
    0
}
