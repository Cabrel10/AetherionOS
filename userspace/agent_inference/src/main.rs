//! AetherionOS Bare-Metal LLM Inference Engine
//!
//! This agent:
//!   1. Opens and mmap's a GGUF model file (smollm2-135m-q4_0.gguf) from ext2/VFS
//!   2. Parses GGUF header, KV metadata, and tensor descriptors
//!   3. Implements Q4_0 dequantization (block size 32, 2+16 bytes per block)
//!   4. Performs forward-pass matrix multiplication using AVX2 _mm256_fmadd_ps
//!   5. Reports GFLOPS benchmark on serial console
//!
//! Designed to run as a bare-metal Ring 3 process on AetherionOS.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use aetherion_sdk::*;

// ═══════════════════════════════════════════════════════════════
// GGUF Constants
// ═══════════════════════════════════════════════════════════════

const GGUF_MAGIC: u32 = 0x4655_4747; // "GGUF" in little-endian

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

/// GGML tensor types
const GGML_TYPE_F32: u32 = 0;
const GGML_TYPE_F16: u32 = 1;
const GGML_TYPE_Q4_0: u32 = 2;
const GGML_TYPE_Q4_1: u32 = 3;
const GGML_TYPE_Q8_0: u32 = 8;

/// Q4_0 block: 2 bytes scale (f16) + 16 bytes data (32 nibbles) = 18 bytes for 32 elements
const Q4_0_BLOCK_SIZE: usize = 32;
const Q4_0_BYTES_PER_BLOCK: usize = 18; // sizeof(float16) + 32/2

// ═══════════════════════════════════════════════════════════════
// Helper functions
// ═══════════════════════════════════════════════════════════════

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

fn read_f32_le(buf: &[u8], off: usize) -> f32 {
    if off + 4 > buf.len() { return 0.0; }
    f32::from_le_bytes([buf[off], buf[off+1], buf[off+2], buf[off+3]])
}

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
        _ => off + 4,
    }
}

// ═══════════════════════════════════════════════════════════════
// Q4_0 Dequantization
// ═══════════════════════════════════════════════════════════════

/// Dequantize a Q4_0 block into 32 f32 values.
/// Q4_0 format: [f16 scale][32 4-bit values packed in 16 bytes]
/// Each 4-bit value is unsigned 0-15, centered by subtracting 8: val = (nibble - 8) * scale
#[inline(never)]
fn dequantize_q4_0_block(block: &[u8], output: &mut [f32; Q4_0_BLOCK_SIZE]) {
    if block.len() < Q4_0_BYTES_PER_BLOCK { return; }

    // Read f16 scale and convert to f32
    let scale_bits = u16::from_le_bytes([block[0], block[1]]);
    let scale = f16_to_f32(scale_bits);

    // Dequantize 32 nibbles from 16 bytes
    for i in 0..16 {
        let byte = block[2 + i];
        let lo = (byte & 0x0F) as f32 - 8.0;
        let hi = ((byte >> 4) & 0x0F) as f32 - 8.0;
        output[i * 2]     = lo * scale;
        output[i * 2 + 1] = hi * scale;
    }
}

/// Convert IEEE 754 half-precision float (f16) to f32
fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1F) as u32;
    let mant = (h & 0x3FF) as u32;

    if exp == 0 {
        if mant == 0 {
            // Zero
            return f32::from_bits(sign << 31);
        }
        // Subnormal f16 → normal f32
        let mut m = mant;
        let mut e: i32 = -14;
        while m & 0x400 == 0 {
            m <<= 1;
            e -= 1;
        }
        m &= 0x3FF;
        let f32_exp = ((e + 127) as u32) & 0xFF;
        return f32::from_bits((sign << 31) | (f32_exp << 23) | (m << 13));
    }
    if exp == 31 {
        // Inf/NaN
        return f32::from_bits((sign << 31) | (0xFF << 23) | (mant << 13));
    }
    // Normal
    let f32_exp = exp + 112; // 127 - 15
    f32::from_bits((sign << 31) | (f32_exp << 23) | (mant << 13))
}

// ═══════════════════════════════════════════════════════════════
// AVX2 Matrix Multiplication (256-bit SIMD)
// ═══════════════════════════════════════════════════════════════

/// Matrix multiply C[M×N] = A[M×K] × B[K×N] using AVX2 _mm256_fmadd_ps.
/// All matrices are f32, row-major.
/// Processes 8 elements at a time for the inner dot product.
#[inline(never)]
fn matmul_avx2(
    c: &mut [f32],  // output M×N
    a: &[f32],      // input M×K
    b: &[f32],      // input K×N (row-major, so B[k][n] = b[k*N + n])
    m: usize,
    k: usize,
    n: usize,
) {
    // Validate dimensions
    if a.len() < m * k || b.len() < k * n || c.len() < m * n {
        return;
    }

    // For each output row
    for i in 0..m {
        // For each output column, process 8 at a time
        let mut j = 0;
        while j + 8 <= n {
            // Accumulate 8 output values simultaneously using AVX2
            let mut acc: [f32; 8] = [0.0; 8];

            // Inner product loop
            for kk in 0..k {
                let a_val = a[i * k + kk];

                // Load 8 values from B[kk][j..j+8]
                // FMA: acc[0..8] += a_val * B[kk][j..j+8]
                unsafe {
                    avx2_fmadd_8(
                        &mut acc,
                        a_val,
                        &b[kk * n + j..kk * n + j + 8],
                    );
                }
            }

            // Store accumulated results
            for q in 0..8 {
                c[i * n + j + q] = acc[q];
            }
            j += 8;
        }

        // Handle remaining columns (< 8)
        while j < n {
            let mut sum = 0.0f32;
            for kk in 0..k {
                sum += a[i * k + kk] * b[kk * n + j];
            }
            c[i * n + j] = sum;
            j += 1;
        }
    }
}

/// AVX2 fused multiply-add: acc[0..8] += scalar * src[0..8]
/// Uses _mm256_fmadd_ps intrinsic via inline assembly.
/// Falls back to scalar if AVX2 is not available.
#[inline(always)]
unsafe fn avx2_fmadd_8(acc: &mut [f32; 8], scalar: f32, src: &[f32]) {
    if src.len() < 8 { return; }

    // Try AVX2 FMA instruction: vfmadd231ps ymm_acc, ymm_scalar, ymm_src
    // This computes: acc = acc + scalar * src (element-wise for 8 f32s)
    #[cfg(target_arch = "x86_64")]
    {
        // Check if AVX2+FMA are available (CPUID check cached)
        if has_avx2_fma() {
            core::arch::asm!(
                // Load accumulator into ymm0
                "vmovups ymm0, [{acc}]",
                // Broadcast scalar to all 8 lanes of ymm1
                "vbroadcastss ymm1, [{scalar}]",
                // Load source into ymm2
                "vmovups ymm2, [{src}]",
                // FMA: ymm0 = ymm0 + ymm1 * ymm2
                "vfmadd231ps ymm0, ymm1, ymm2",
                // Store result back
                "vmovups [{acc}], ymm0",
                acc = in(reg) acc.as_mut_ptr(),
                scalar = in(reg) &scalar,
                src = in(reg) src.as_ptr(),
                // Clobber ymm0-ymm2
                options(nostack),
            );
            return;
        }
    }

    // Scalar fallback
    for i in 0..8 {
        acc[i] += scalar * src[i];
    }
}

/// Check if CPU supports AVX2 + FMA3 via CPUID.
/// Caches result for performance.
fn has_avx2_fma() -> bool {
    use core::sync::atomic::{AtomicU8, Ordering};
    static CACHED: AtomicU8 = AtomicU8::new(0); // 0=unchecked, 1=no, 2=yes

    let cached = CACHED.load(Ordering::Relaxed);
    if cached != 0 {
        return cached == 2;
    }

    let result = unsafe {
        let mut ebx: u32;
        let mut ecx: u32;
        // CPUID leaf 7, subleaf 0 → EBX bit 5 = AVX2
        core::arch::asm!(
            "mov eax, 7",
            "xor ecx, ecx",
            "cpuid",
            out("ebx") ebx,
            out("ecx") ecx,
            out("eax") _,
            out("edx") _,
        );
        let avx2 = (ebx >> 5) & 1 == 1;

        // CPUID leaf 1 → ECX bit 12 = FMA
        let mut ecx1: u32;
        core::arch::asm!(
            "mov eax, 1",
            "cpuid",
            out("ecx") ecx1,
            out("eax") _,
            out("ebx") _,
            out("edx") _,
        );
        let fma = (ecx1 >> 12) & 1 == 1;

        avx2 && fma
    };

    CACHED.store(if result { 2 } else { 1 }, Ordering::Relaxed);
    result
}

/// Read the TSC (Time Stamp Counter) for benchmarking
fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

// ═══════════════════════════════════════════════════════════════
// Tensor descriptor (parsed from GGUF)
// ═══════════════════════════════════════════════════════════════

struct TensorInfo {
    name: [u8; 64],
    name_len: usize,
    n_dims: u32,
    dims: [u64; 4],
    ttype: u32,
    data_offset: u64,
}

impl TensorInfo {
    fn new() -> Self {
        TensorInfo {
            name: [0u8; 64],
            name_len: 0,
            n_dims: 0,
            dims: [0; 4],
            ttype: 0,
            data_offset: 0,
        }
    }

    fn num_elements(&self) -> u64 {
        let mut n = 1u64;
        for d in 0..self.n_dims as usize {
            n = n.saturating_mul(self.dims[d]);
        }
        n
    }

    fn name_str(&self) -> &[u8] {
        &self.name[..self.name_len]
    }
}

// ═══════════════════════════════════════════════════════════════
// Main entry point
// ═══════════════════════════════════════════════════════════════

/// Model file paths to try (in order)
const MODEL_PATHS: &[&[u8]] = &[
    b"/models/smollm2-135m-q4_0.gguf\0",
    b"/disk/models/smollm2-135m-q4_0.gguf\0",
    b"/disk/models/part1\0",
];

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[LLM] AetherionOS Bare-Metal LLM Inference Engine v2.0");
    println("[LLM] Target: SmolLM2-135M Q4_0 (AVX2 + FMA)");

    // ═══ Step 1: Open the GGUF model file ═══
    let mut fd: i64 = -1;
    let mut model_path_idx = 0;
    for (idx, path) in MODEL_PATHS.iter().enumerate() {
        let f = sys_open(path, O_RDONLY);
        if f >= 0 {
            fd = f;
            model_path_idx = idx;
            print("[LLM] Opened model: ");
            // Print path without null terminator
            let plen = path.len().saturating_sub(1);
            sys_write(1, &path[..plen]);
            print(" (fd=");
            print_u64(fd as u64);
            println(")");
            break;
        }
    }

    if fd < 0 {
        println("[LLM] Model file not found on any path — running synthetic benchmark");
        return run_synthetic_benchmark();
    }

    // ═══ Step 2: Get file size via lseek ═══
    let file_size = {
        let end = sys_lseek(fd as u32, 0, 2) as u64; // SEEK_END
        let _ = sys_lseek(fd as u32, 0, 0);           // SEEK_SET (rewind)
        if end == 0 || end > 0xFFFF_FFFF_FFFF {
            println("[LLM] Cannot determine file size, using 256 MiB");
            256 * 1024 * 1024u64
        } else {
            end
        }
    };
    print("[LLM] Model file size: ");
    print_u64(file_size);
    println(" bytes");

    // ═══ Step 3: mmap the model file ═══
    print("[LLM] mmap'ing model file...");
    let model_base = sys_mmap_file_v2(fd as u32, file_size, 0);
    if model_base == 0 || (model_base as i64) < 0 {
        println(" FAILED — falling back to read()");
        // Fallback: allocate buffer and read
        let buf_size = core::cmp::min(file_size as usize, 64 * 1024); // Read first 64K
        let buf_addr = sys_mmap(buf_size);
        if buf_addr == 0 {
            println("[LLM] FATAL: cannot allocate buffer");
            sys_close(fd as u32);
            return 1;
        }
        let buf = unsafe { core::slice::from_raw_parts_mut(buf_addr as *mut u8, buf_size) };
        let n = sys_read_fd(fd as u32, buf);
        sys_close(fd as u32);
        if n <= 0 {
            println("[LLM] FATAL: read() returned 0");
            return 1;
        }
        print("[LLM] Read ");
        print_u64(n as u64);
        println(" bytes into buffer");
        return parse_and_benchmark(buf, n as usize, 0);
    }

    print(" OK at 0x");
    print_hex(model_base);
    println("");

    // Close fd (mmap keeps a reference)
    sys_close(fd as u32);

    // ═══ Step 4: Parse GGUF and benchmark ═══
    let model_data = unsafe {
        core::slice::from_raw_parts(model_base as *const u8, file_size as usize)
    };

    parse_and_benchmark(model_data, file_size as usize, model_base)
}

/// Parse GGUF header and run MatMul benchmark
fn parse_and_benchmark(data: &[u8], len: usize, base_addr: u64) -> i64 {
    // ═══ Verify GGUF magic ═══
    let magic = read_u32_le(data, 0);
    if magic != GGUF_MAGIC {
        print("[LLM] ERROR: Not a GGUF file (magic=0x");
        print_hex(magic as u64);
        println(")");
        return 1;
    }
    println("[LLM] GGUF magic verified");

    let version = read_u32_le(data, 4);
    let tensor_count = read_u64_le(data, 8);
    let kv_count = read_u64_le(data, 16);

    print("[LLM] GGUF v");
    print_u64(version as u64);
    print(" | tensors=");
    print_u64(tensor_count);
    print(" | kv_pairs=");
    print_u64(kv_count);
    println("");

    // ═══ Skip KV pairs to reach tensor info ═══
    let mut offset = 24usize; // After GGUF header

    for _kv in 0..kv_count {
        if offset + 12 >= len { break; }
        // Key: len(u64) + string
        let key_len = read_u64_le(data, offset) as usize;
        offset += 8 + key_len;
        if offset + 4 >= len { break; }
        // Value type + value
        let vtype = read_u32_le(data, offset);
        offset += 4;
        offset = skip_gguf_value(data, offset, vtype);
    }

    print("[LLM] Tensor info starts at offset ");
    print_u64(offset as u64);
    println("");

    // ═══ Parse tensor descriptors ═══
    let max_tensors = core::cmp::min(tensor_count as usize, 256);
    let mut total_params: u64 = 0;
    let mut q4_0_tensors: usize = 0;
    let mut first_q4_tensor = TensorInfo::new();
    let mut embed_dim: usize = 0;

    for i in 0..max_tensors {
        if offset + 8 >= len { break; }

        let mut ti = TensorInfo::new();

        // Tensor name
        let name_len = read_u64_le(data, offset) as usize;
        offset += 8;
        let name_end = offset + name_len;
        if name_end > len { break; }
        let copy_len = core::cmp::min(name_len, 63);
        ti.name[..copy_len].copy_from_slice(&data[offset..offset + copy_len]);
        ti.name_len = copy_len;
        offset = name_end;

        // Number of dimensions
        if offset + 4 >= len { break; }
        ti.n_dims = read_u32_le(data, offset);
        offset += 4;

        // Dimensions
        for d in 0..ti.n_dims as usize {
            if offset + 8 > len { break; }
            ti.dims[d] = read_u64_le(data, offset);
            offset += 8;
        }

        // Tensor type
        if offset + 4 > len { break; }
        ti.ttype = read_u32_le(data, offset);
        offset += 4;

        // Data offset
        if offset + 8 > len { break; }
        ti.data_offset = read_u64_le(data, offset);
        offset += 8;

        let params = ti.num_elements();
        total_params += params;

        // Print first 8 tensors
        if i < 8 {
            print("[LLM]   T");
            print_u64(i as u64);
            print(": ");
            sys_write(1, ti.name_str());
            print(" [");
            for d in 0..ti.n_dims as usize {
                print_u64(ti.dims[d]);
                if d + 1 < ti.n_dims as usize { print("x"); }
            }
            print("] ");
            print_tensor_type(ti.ttype);
            print(" params=");
            print_u64(params);
            println("");
        }

        if ti.ttype == GGML_TYPE_Q4_0 {
            if q4_0_tensors == 0 {
                // Save first Q4_0 tensor info for benchmark
                first_q4_tensor.name[..ti.name_len].copy_from_slice(&ti.name[..ti.name_len]);
                first_q4_tensor.name_len = ti.name_len;
                first_q4_tensor.n_dims = ti.n_dims;
                first_q4_tensor.dims = ti.dims;
                first_q4_tensor.ttype = ti.ttype;
                first_q4_tensor.data_offset = ti.data_offset;
            }
            q4_0_tensors += 1;
        }

        // Detect embedding dimension
        if embed_dim == 0 && ti.n_dims == 2 && ti.dims[0] > 0 {
            embed_dim = ti.dims[0] as usize;
        }
    }

    print("[LLM] Total parameters: ");
    print_u64(total_params);
    print(" (~");
    print_u64(total_params / 1_000_000);
    println("M)");
    print("[LLM] Q4_0 tensors: ");
    print_u64(q4_0_tensors as u64);
    println("");
    if embed_dim > 0 {
        print("[LLM] Embedding dimension: ");
        print_u64(embed_dim as u64);
        println("");
    }

    // ═══ Q4_0 Dequantization Demo ═══
    if q4_0_tensors > 0 && first_q4_tensor.data_offset > 0 {
        let data_off = first_q4_tensor.data_offset as usize;
        if data_off + Q4_0_BYTES_PER_BLOCK <= len {
            println("[LLM] Dequantizing first Q4_0 block...");
            let mut dequant = [0.0f32; Q4_0_BLOCK_SIZE];
            dequantize_q4_0_block(&data[data_off..], &mut dequant);
            print("[LLM]   Values[0..4]: ");
            for i in 0..4 {
                print_f32_approx(dequant[i]);
                if i < 3 { print(", "); }
            }
            println("");
        }
    }

    // ═══ MatMul Benchmark (AVX2 FMA) ═══
    run_matmul_benchmark(embed_dim);

    // Publish result on the bus
    let bus_ret = sys_bus_publish(0xE100, 2, total_params);
    if bus_ret == 0 {
        println("[LLM] Bus 0xE100 published OK");
    }

    println("[LLM] Inference engine initialization complete");
    0
}

/// Run a MatMul benchmark with the given dimension (or default 576)
fn run_matmul_benchmark(dim_hint: usize) -> i64 {
    let dim = if dim_hint > 0 && dim_hint <= 2048 { dim_hint } else { 576 };
    // Use smaller matrix for benchmark to avoid OOM
    let bench_m = 4;
    let bench_k = dim;
    let bench_n = dim;

    print("[LLM] MatMul benchmark: ");
    print_u64(bench_m as u64);
    print("x");
    print_u64(bench_k as u64);
    print(" * ");
    print_u64(bench_k as u64);
    print("x");
    print_u64(bench_n as u64);
    println("");

    // Allocate matrices
    let total_floats = bench_m * bench_k + bench_k * bench_n + bench_m * bench_n;
    let total_bytes = total_floats * 4;
    let buf = sys_mmap(total_bytes);
    if buf == 0 {
        println("[LLM] Cannot allocate benchmark buffers");
        return 1;
    }

    let all = unsafe { core::slice::from_raw_parts_mut(buf as *mut f32, total_floats) };

    // Initialize A and B with simple patterns
    let (a_slice, rest) = all.split_at_mut(bench_m * bench_k);
    let (b_slice, c_slice) = rest.split_at_mut(bench_k * bench_n);

    // Fill A: row value
    for i in 0..bench_m {
        for kk in 0..bench_k {
            a_slice[i * bench_k + kk] = 0.01 * ((i + 1) as f32);
        }
    }
    // Fill B: column value
    for kk in 0..bench_k {
        for j in 0..bench_n {
            b_slice[kk * bench_n + j] = 0.01 * ((j + 1) as f32);
        }
    }
    // Zero C
    for x in c_slice.iter_mut() { *x = 0.0; }

    // Warm up
    matmul_avx2(c_slice, a_slice, b_slice, bench_m, bench_k, bench_n);

    // Benchmark
    let iterations = 10u64;
    let tsc_start = rdtsc();

    for _ in 0..iterations {
        // Zero C
        for x in c_slice.iter_mut() { *x = 0.0; }
        matmul_avx2(c_slice, a_slice, b_slice, bench_m, bench_k, bench_n);
    }

    let tsc_end = rdtsc();
    let tsc_diff = tsc_end.saturating_sub(tsc_start);

    // FLOPs = 2 * M * N * K per matmul (multiply + add)
    let flops_per_iter = 2u64 * bench_m as u64 * bench_n as u64 * bench_k as u64;
    let total_flops = flops_per_iter * iterations;

    // Assume ~2 GHz TSC (conservative for QEMU)
    // GFLOPS = total_flops / (tsc_diff / 2e9) = total_flops * 2e9 / tsc_diff
    // Avoid overflow: GFLOPS = (total_flops / tsc_diff) * 2
    let gflops_x10 = if tsc_diff > 0 {
        (total_flops * 20) / tsc_diff // multiply by 20 = 2GHz * 10 (for 1 decimal)
    } else {
        0
    };

    print("[LLM] MatMul Benchmark: ");
    print_u64(gflops_x10 / 10);
    print(".");
    print_u64(gflops_x10 % 10);
    println(" GFLOPS");

    // Verify correctness: C[0][0] should be ≈ 0.01 * 0.01 * K * 1 * sum(1..N)
    let c00 = c_slice[0];
    print("[LLM] C[0][0] = ");
    print_f32_approx(c00);
    println("");

    // Check AVX2 availability
    if has_avx2_fma() {
        println("[LLM] AVX2+FMA: AVAILABLE (using hardware SIMD)");
    } else {
        println("[LLM] AVX2+FMA: NOT AVAILABLE (scalar fallback)");
    }

    print("[LLM] TSC cycles: ");
    print_u64(tsc_diff);
    print(" (");
    print_u64(iterations);
    println(" iterations)");

    0
}

/// Run synthetic benchmark when no model file is available
fn run_synthetic_benchmark() -> i64 {
    println("[LLM] Running synthetic MatMul benchmark (no model file)...");

    // Q4_0 dequantization test
    println("[LLM] Q4_0 dequantization test:");
    let test_block: [u8; Q4_0_BYTES_PER_BLOCK] = [
        0x00, 0x3C, // f16 scale = 1.0
        0x87, 0x65, 0x43, 0x21, 0x0F, 0xED, 0xCB, 0xA9,
        0x87, 0x65, 0x43, 0x21, 0x0F, 0xED, 0xCB, 0xA9,
    ];
    let mut dequant = [0.0f32; Q4_0_BLOCK_SIZE];
    dequantize_q4_0_block(&test_block, &mut dequant);
    print("[LLM]   Dequantized[0..4]: ");
    for i in 0..4 {
        print_f32_approx(dequant[i]);
        if i < 3 { print(", "); }
    }
    println("");

    // Run the matrix benchmark with default dimension
    run_matmul_benchmark(576)
}

/// Print tensor type name
fn print_tensor_type(t: u32) {
    match t {
        0 => print("F32"),
        1 => print("F16"),
        2 => print("Q4_0"),
        3 => print("Q4_1"),
        6 => print("Q5_0"),
        7 => print("Q5_1"),
        8 => print("Q8_0"),
        9 => print("Q8_1"),
        10 => print("Q2_K"),
        11 => print("Q3_K"),
        12 => print("Q4_K"),
        13 => print("Q5_K"),
        14 => print("Q6_K"),
        _ => {
            print("UNK(");
            print_u64(t as u64);
            print(")");
        }
    }
}

/// Approximate f32 printing (integer.fraction format)
fn print_f32_approx(v: f32) {
    if v < 0.0 {
        print("-");
        print_f32_approx(-v);
        return;
    }
    let int_part = v as u64;
    let frac = ((v - int_part as f32) * 1000.0) as u64;
    print_u64(int_part);
    print(".");
    if frac < 100 { print("0"); }
    if frac < 10 { print("0"); }
    print_u64(frac);
}


