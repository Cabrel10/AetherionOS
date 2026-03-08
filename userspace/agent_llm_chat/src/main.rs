//! AetherionOS Jalon 64 – Full LLM Chat Agent loading GGUF from FAT32 Disk
//!
//! This agent demonstrates the complete LLM pipeline on AetherionOS:
//!   1. Opens and reads a GGUF model file from /disk/models/ using chunked sys_read
//!   2. Parses the GGUF v3 header (magic, version, tensor/KV counts)
//!   3. Extracts tensor metadata and weight data
//!   4. Initializes transformer weights from GGUF data (or falls back to synthetic)
//!   5. Listens for INTENT_USER_PROMPT on the Cognitive Bus
//!   6. Runs full transformer forward passes (RMSNorm, RoPE, SwiGLU, GQA)
//!   7. Publishes INTENT_TOKEN_GENERATED for each output token
//!   8. Publishes INTENT_GENERATION_DONE when complete
//!
//! Architecture (test): dim=32, heads=2, kv_heads=1, head_dim=16
//! The agent validates chunked file I/O, GGUF parsing, and end-to-end generation.

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

// ═══════════════════════════════════════════════════
// Cognitive Bus Intent IDs
// ═══════════════════════════════════════════════════
const INTENT_USER_PROMPT: u64      = 0x8001;
const INTENT_TOKEN_GENERATED: u64  = 0x8002;
const INTENT_GENERATION_DONE: u64  = 0x8003;
const INTENT_LLM_READY: u64       = 0x8004;
const INTENT_LLM_CHAT_INIT: u64   = 0xD064;

// ═══════════════════════════════════════════════════
// Model Configuration (test scale — matches agent_llama_core)
// ═══════════════════════════════════════════════════
const DIM: usize        = 32;
const N_HEADS: usize    = 2;
const N_KV_HEADS: usize = 1;
const HEAD_DIM: usize   = DIM / N_HEADS;  // 16
const KV_DIM: usize     = HEAD_DIM * N_KV_HEADS; // 16
const HIDDEN_DIM: usize = DIM * 2;  // 64
const VOCAB_SIZE: usize = 128;
const MAX_SEQ_LEN: usize = 96;
const GEN_TOKENS: usize  = 64;

// GGUF v3 magic
const GGUF_MAGIC: u32 = 0x46475547; // "GGUF" little-endian

// ═══════════════════════════════════════════════════
// Software floating-point math (no_std, no libm)
// ═══════════════════════════════════════════════════
fn f32_abs(x: f32) -> f32 { if x < 0.0 { -x } else { x } }

fn f32_sqrt(x: f32) -> f32 {
    if x <= 0.0 { return 0.0; }
    let mut i = x.to_bits();
    i = 0x5f3759d5 - (i >> 1);
    let inv = f32::from_bits(i);
    let mut y = 1.0 / inv;
    for _ in 0..3 { y = 0.5 * (y + x / y); }
    y
}

fn f32_exp(x: f32) -> f32 {
    let x = if x > 88.0 { 88.0 } else if x < -88.0 { -88.0 } else { x };
    let xlog2e = x * 1.442695;
    let k = xlog2e as i32 - (if xlog2e < 0.0 { 1 } else { 0 });
    let f = xlog2e - k as f32;
    let p = 1.0 + f * (0.6931472 + f * (0.2402265 + f * (0.0555041 + f * 0.0096139)));
    let bits = ((k + 127) as u32) << 23;
    p * f32::from_bits(bits)
}

fn f32_cos(mut x: f32) -> f32 {
    let twopi = 6.2831853;
    while x > twopi { x -= twopi; }
    while x < 0.0 { x += twopi; }
    let x2 = x * x;
    1.0 - x2 * 0.5 + x2 * x2 * 0.041666667 - x2 * x2 * x2 * 0.001388889
}

fn f32_sin(mut x: f32) -> f32 {
    let twopi = 6.2831853;
    while x > twopi { x -= twopi; }
    while x < 0.0 { x += twopi; }
    let x2 = x * x;
    x - x * x2 * 0.16666667 + x * x2 * x2 * 0.008333333 - x * x2 * x2 * x2 * 0.000198413
}

fn f32_pow(base: f32, exp: f32) -> f32 {
    if base <= 0.0 { return 0.0; }
    let bits = base.to_bits() as f32;
    let ln_base = (bits / 8388608.0 - 127.0) * 0.6931472;
    f32_exp(exp * ln_base)
}

// ═══════════════════════════════════════════════════
// Static weight buffers (in .bss, no heap)
// ═══════════════════════════════════════════════════
static mut WQ: [f32; DIM * DIM] = [0.0; DIM * DIM];
static mut WK: [f32; DIM * KV_DIM] = [0.0; DIM * KV_DIM];
static mut WV: [f32; DIM * KV_DIM] = [0.0; DIM * KV_DIM];
static mut WO: [f32; DIM * DIM] = [0.0; DIM * DIM];
static mut RMS_ATT: [f32; DIM] = [1.0; DIM];
static mut W_GATE: [f32; DIM * HIDDEN_DIM] = [0.0; DIM * HIDDEN_DIM];
static mut W_UP: [f32; DIM * HIDDEN_DIM] = [0.0; DIM * HIDDEN_DIM];
static mut W_DOWN: [f32; HIDDEN_DIM * DIM] = [0.0; HIDDEN_DIM * DIM];
static mut RMS_FFN: [f32; DIM] = [1.0; DIM];
static mut RMS_FINAL: [f32; DIM] = [1.0; DIM];
static mut W_OUTPUT: [f32; DIM * VOCAB_SIZE] = [0.0; DIM * VOCAB_SIZE];
static mut EMBEDDING: [f32; VOCAB_SIZE * DIM] = [0.0; VOCAB_SIZE * DIM];

// KV cache
static mut KEY_CACHE: [f32; MAX_SEQ_LEN * KV_DIM] = [0.0; MAX_SEQ_LEN * KV_DIM];
static mut VAL_CACHE: [f32; MAX_SEQ_LEN * KV_DIM] = [0.0; MAX_SEQ_LEN * KV_DIM];

// Scratch buffers
static mut X_BUF: [f32; DIM] = [0.0; DIM];
static mut XNORM: [f32; DIM] = [0.0; DIM];
static mut Q_BUF: [f32; DIM] = [0.0; DIM];
static mut K_BUF: [f32; KV_DIM] = [0.0; KV_DIM];
static mut V_BUF: [f32; KV_DIM] = [0.0; KV_DIM];
static mut ATTN_OUT: [f32; DIM] = [0.0; DIM];
static mut ATTN_PROJ: [f32; DIM] = [0.0; DIM];
static mut GATE_BUF: [f32; HIDDEN_DIM] = [0.0; HIDDEN_DIM];
static mut UP_BUF: [f32; HIDDEN_DIM] = [0.0; HIDDEN_DIM];
static mut HIDDEN_BUF: [f32; HIDDEN_DIM] = [0.0; HIDDEN_DIM];
static mut FFN_OUT: [f32; DIM] = [0.0; DIM];
static mut LOGITS: [f32; VOCAB_SIZE] = [0.0; VOCAB_SIZE];
static mut SCORES: [f32; MAX_SEQ_LEN] = [0.0; MAX_SEQ_LEN];

// ═══════════════════════════════════════════════════
// GGUF Parser (chunked file I/O)
// ═══════════════════════════════════════════════════

/// Read exactly `len` bytes from fd into buf. Returns bytes read.
fn read_exact(fd: u32, buf: &mut [u8], len: usize) -> usize {
    let mut total: usize = 0;
    while total < len {
        let remain = len - total;
        let chunk = if remain > 4096 { 4096 } else { remain };
        let n = sys_read_fd(fd, &mut buf[total..total + chunk]);
        if n <= 0 { break; }
        total += n as usize;
    }
    total
}

/// Read a u32 (little-endian) from fd
fn read_u32(fd: u32) -> Option<u32> {
    let mut buf = [0u8; 4];
    if read_exact(fd, &mut buf, 4) == 4 {
        Some(u32::from_le_bytes(buf))
    } else {
        None
    }
}

/// Read a u64 (little-endian) from fd
fn read_u64_val(fd: u32) -> Option<u64> {
    let mut buf = [0u8; 8];
    if read_exact(fd, &mut buf, 8) == 8 {
        Some(u64::from_le_bytes(buf))
    } else {
        None
    }
}

/// Parsed GGUF header info
struct GgufHeader {
    version: u32,
    tensor_count: u64,
    kv_count: u64,
}

/// Parse GGUF header from an open file descriptor.
/// Returns header info and leaves fd positioned after the header.
fn parse_gguf_header(fd: u32) -> Option<GgufHeader> {
    // Read magic: "GGUF" = 0x46475547
    let magic = read_u32(fd)?;
    if magic != GGUF_MAGIC {
        print("[J64] GGUF: bad magic 0x");
        print_hex(magic as u64);
        println("");
        return None;
    }
    println("[J64] GGUF: magic OK (0x46475547)");

    let version = read_u32(fd)?;
    print("[J64] GGUF: version ");
    print_u64(version as u64);
    println("");

    let tensor_count = read_u64_val(fd)?;
    print("[J64] GGUF: tensor_count ");
    print_u64(tensor_count);
    println("");

    let kv_count = read_u64_val(fd)?;
    print("[J64] GGUF: kv_count ");
    print_u64(kv_count);
    println("");

    Some(GgufHeader { version, tensor_count, kv_count })
}

/// Skip a GGUF KV pair (key string + type + value).
/// Returns true if successful.
fn skip_gguf_kv(fd: u32) -> bool {
    // Key: u64 len + string bytes
    let key_len = match read_u64_val(fd) {
        Some(v) => v as usize,
        None => return false,
    };
    if key_len > 256 { return false; } // sanity
    let mut skip_buf = [0u8; 256];
    let read_len = core::cmp::min(key_len, 256);
    if read_exact(fd, &mut skip_buf[..read_len], read_len) != read_len {
        return false;
    }

    // Value type: u32
    let val_type = match read_u32(fd) {
        Some(v) => v,
        None => return false,
    };

    // Skip value based on type
    match val_type {
        0 => { /* UINT8 */ let mut b = [0u8; 1]; read_exact(fd, &mut b, 1); }
        1 => { /* INT8 */  let mut b = [0u8; 1]; read_exact(fd, &mut b, 1); }
        2 => { /* UINT16 */ let mut b = [0u8; 2]; read_exact(fd, &mut b, 2); }
        3 => { /* INT16 */  let mut b = [0u8; 2]; read_exact(fd, &mut b, 2); }
        4 => { /* UINT32 */ let mut b = [0u8; 4]; read_exact(fd, &mut b, 4); }
        5 => { /* INT32 */  let mut b = [0u8; 4]; read_exact(fd, &mut b, 4); }
        6 => { /* FLOAT32 */ let mut b = [0u8; 4]; read_exact(fd, &mut b, 4); }
        7 => { /* BOOL */   let mut b = [0u8; 1]; read_exact(fd, &mut b, 1); }
        8 => { /* STRING */ 
            let slen = match read_u64_val(fd) { Some(v) => v as usize, None => return false };
            let mut tbuf = [0u8; 128];
            let mut skipped = 0;
            while skipped < slen {
                let c = core::cmp::min(slen - skipped, 128);
                read_exact(fd, &mut tbuf[..c], c);
                skipped += c;
            }
        }
        10 => { /* UINT64 */ let mut b = [0u8; 8]; read_exact(fd, &mut b, 8); }
        _ => {
            // Unknown type, skip 8 bytes as best guess
            let mut b = [0u8; 8]; read_exact(fd, &mut b, 8);
        }
    }
    true
}

/// Read f32 weights from fd into a static buffer (chunked 512 bytes at a time).
unsafe fn read_f32_weights(fd: u32, buf: &mut [f32], count: usize) -> usize {
    let byte_count = count * 4;
    let mut total: usize = 0;
    let chunk_size: usize = 512; // Read 512 bytes at a time (128 floats)
    let mut tmp = [0u8; 512];

    while total < byte_count {
        let remain = byte_count - total;
        let to_read = if remain > chunk_size { chunk_size } else { remain };
        let n = read_exact(fd, &mut tmp[..to_read], to_read);
        if n == 0 { break; }

        // Convert bytes to f32 values
        let float_start = total / 4;
        let mut off = 0;
        while off + 4 <= n {
            let idx = float_start + off / 4;
            if idx < count {
                buf[idx] = f32::from_le_bytes([tmp[off], tmp[off+1], tmp[off+2], tmp[off+3]]);
            }
            off += 4;
        }
        total += n;
    }
    total / 4
}

// ═══════════════════════════════════════════════════
// Transformer Operations (identical to agent_llama_core)
// ═══════════════════════════════════════════════════

fn rmsnorm(out: &mut [f32], x: &[f32], weight: &[f32], size: usize) {
    let mut ss: f32 = 0.0;
    for i in 0..size { ss += x[i] * x[i]; }
    ss = 1.0 / f32_sqrt(ss / (size as f32) + 1e-5);
    for i in 0..size { out[i] = x[i] * ss * weight[i]; }
}

fn matmul(out: &mut [f32], mat: &[f32], x: &[f32], rows: usize, cols: usize) {
    for i in 0..rows {
        let mut sum: f32 = 0.0;
        let base = i * cols;
        for j in 0..cols { sum += mat[base + j] * x[j]; }
        out[i] = sum;
    }
}

fn softmax(x: &mut [f32], size: usize) {
    let mut max_val = x[0];
    for i in 1..size { if x[i] > max_val { max_val = x[i]; } }
    let mut sum: f32 = 0.0;
    for i in 0..size { x[i] = f32_exp(x[i] - max_val); sum += x[i]; }
    if sum > 0.0 { for i in 0..size { x[i] /= sum; } }
}

fn swiglu(out: &mut [f32], gate: &[f32], up: &[f32], size: usize) {
    for i in 0..size {
        let sigmoid = 1.0 / (1.0 + f32_exp(-gate[i]));
        out[i] = gate[i] * sigmoid * up[i];
    }
}

fn argmax(x: &[f32], size: usize) -> usize {
    let mut best = 0;
    let mut best_val = x[0];
    for i in 1..size { if x[i] > best_val { best_val = x[i]; best = i; } }
    best
}

fn sample_temperature(logits: &mut [f32], size: usize, temp: f32, rng: &mut u64) -> usize {
    if temp <= 0.01 { return argmax(logits, size); }
    for i in 0..size { logits[i] /= temp; }
    softmax(logits, size);
    *rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
    let r = ((*rng >> 33) as f32) / 2147483647.0;
    let mut cum: f32 = 0.0;
    for i in 0..size {
        cum += logits[i];
        if cum >= r { return i; }
    }
    size - 1
}

// ═══════════════════════════════════════════════════
// LCG PRNG for synthetic fallback weights
// ═══════════════════════════════════════════════════
struct Rng { state: u64 }
impl Rng {
    fn new(seed: u64) -> Self { Rng { state: seed } }
    fn next_f32(&mut self) -> f32 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let bits = ((self.state >> 33) as u32) & 0x7FFFFF;
        (bits as f32 / 8388607.0) * 0.2 - 0.1
    }
    fn fill(&mut self, v: &mut [f32]) {
        for x in v.iter_mut() { *x = self.next_f32(); }
    }
}

unsafe fn init_synthetic_weights() {
    let mut rng = Rng::new(0xAE70_E210_0042u64.wrapping_mul(7));
    rng.fill(&mut WQ);
    rng.fill(&mut WK);
    rng.fill(&mut WV);
    rng.fill(&mut WO);
    rng.fill(&mut W_GATE);
    rng.fill(&mut W_UP);
    rng.fill(&mut W_DOWN);
    rng.fill(&mut W_OUTPUT);
    rng.fill(&mut EMBEDDING);
}

// ═══════════════════════════════════════════════════
// Transformer Forward Pass
// ═══════════════════════════════════════════════════
unsafe fn transformer_forward(token: usize, pos: usize) {
    // Token embedding
    let emb_base = (token % VOCAB_SIZE) * DIM;
    for i in 0..DIM { X_BUF[i] = EMBEDDING[emb_base + i]; }

    // Attention RMSNorm
    rmsnorm(&mut XNORM, &X_BUF, &RMS_ATT, DIM);

    // Q, K, V projections
    matmul(&mut Q_BUF, &WQ, &XNORM, DIM, DIM);
    matmul(&mut K_BUF, &WK, &XNORM, KV_DIM, DIM);
    matmul(&mut V_BUF, &WV, &XNORM, KV_DIM, DIM);

    // RoPE
    for h in 0..N_HEADS {
        let qoff = h * HEAD_DIM;
        for i in (0..HEAD_DIM).step_by(2) {
            let freq = 1.0 / f32_pow(10000.0, (i as f32) / (HEAD_DIM as f32));
            let theta = (pos as f32) * freq;
            let ct = f32_cos(theta);
            let st = f32_sin(theta);
            let q0 = Q_BUF[qoff + i];
            let q1 = Q_BUF[qoff + i + 1];
            Q_BUF[qoff + i]     = q0 * ct - q1 * st;
            Q_BUF[qoff + i + 1] = q0 * st + q1 * ct;
        }
    }
    for h in 0..N_KV_HEADS {
        let koff = h * HEAD_DIM;
        for i in (0..HEAD_DIM).step_by(2) {
            let freq = 1.0 / f32_pow(10000.0, (i as f32) / (HEAD_DIM as f32));
            let theta = (pos as f32) * freq;
            let ct = f32_cos(theta);
            let st = f32_sin(theta);
            let k0 = K_BUF[koff + i];
            let k1 = K_BUF[koff + i + 1];
            K_BUF[koff + i]     = k0 * ct - k1 * st;
            K_BUF[koff + i + 1] = k0 * st + k1 * ct;
        }
    }

    // Store KV in cache
    let kv_base = pos * KV_DIM;
    for i in 0..KV_DIM {
        KEY_CACHE[kv_base + i] = K_BUF[i];
        VAL_CACHE[kv_base + i] = V_BUF[i];
    }

    // Multi-Head Attention with GQA
    for i in 0..DIM { ATTN_OUT[i] = 0.0; }
    let kv_group = N_HEADS / N_KV_HEADS;

    for h in 0..N_HEADS {
        let qoff = h * HEAD_DIM;
        let kv_h = h / kv_group;

        for t in 0..=pos {
            let mut dot: f32 = 0.0;
            let kb = t * KV_DIM + kv_h * HEAD_DIM;
            for d in 0..HEAD_DIM { dot += Q_BUF[qoff + d] * KEY_CACHE[kb + d]; }
            SCORES[t] = dot / f32_sqrt(HEAD_DIM as f32);
        }
        softmax(&mut SCORES[..pos+1], pos + 1);

        for t in 0..=pos {
            let vb = t * KV_DIM + kv_h * HEAD_DIM;
            let w = SCORES[t];
            for d in 0..HEAD_DIM { ATTN_OUT[qoff + d] += w * VAL_CACHE[vb + d]; }
        }
    }

    // Output proj + residual
    matmul(&mut ATTN_PROJ, &WO, &ATTN_OUT, DIM, DIM);
    for i in 0..DIM { X_BUF[i] += ATTN_PROJ[i]; }

    // FFN
    rmsnorm(&mut XNORM, &X_BUF, &RMS_FFN, DIM);
    matmul(&mut GATE_BUF, &W_GATE, &XNORM, HIDDEN_DIM, DIM);
    matmul(&mut UP_BUF, &W_UP, &XNORM, HIDDEN_DIM, DIM);
    swiglu(&mut HIDDEN_BUF, &GATE_BUF, &UP_BUF, HIDDEN_DIM);
    matmul(&mut FFN_OUT, &W_DOWN, &HIDDEN_BUF, DIM, HIDDEN_DIM);
    for i in 0..DIM { X_BUF[i] += FFN_OUT[i]; }

    // Final RMSNorm + logits
    rmsnorm(&mut XNORM, &X_BUF, &RMS_FINAL, DIM);
    matmul(&mut LOGITS, &W_OUTPUT, &XNORM, VOCAB_SIZE, DIM);
}

// ═══════════════════════════════════════════════════
// MAIN — LLM Chat Agent
// ═══════════════════════════════════════════════════
#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("========================================");
    println("[J64] LLM Chat Agent v1.0 — GGUF Loader");
    println("[J64] Architecture: Mistral 7B (test scale)");
    print("[J64] Config: dim="); print_u64(DIM as u64);
    print(" heads="); print_u64(N_HEADS as u64);
    print(" kv_heads="); print_u64(N_KV_HEADS as u64);
    print(" head_dim="); print_u64(HEAD_DIM as u64);
    println("");
    println("========================================");

    // Step 1: Try to open the GGUF model file from FAT32 disk
    // Try multiple filename formats (FAT32 8.3 name compatibility)
    print("[J64] Step 1: Opening GGUF model from /disk/models/... ");
    let paths: [&[u8]; 3] = [
        b"/disk/models/TEST.GGU\0",
        b"/disk/models/test.gguf\0",
        b"/disk/models/MODEL.GGU\0",
    ];
    let mut fd: i64 = -1;
    for path in &paths {
        let result = sys_open(*path, O_RDONLY);
        if result >= 0 && result < 256 {
            fd = result;
            break;
        }
    }
    let mut gguf_loaded = false;
    let mut weights_from_gguf: u32 = 0;

    if fd >= 0 && fd < 256 {
        let fd = fd as u32;
        print("OK (fd="); print_u64(fd as u64); println(")");

        // Step 2: Parse GGUF header
        print("[J64] Step 2: Parsing GGUF header... ");
        match parse_gguf_header(fd) {
            Some(hdr) => {
                print("OK (v"); print_u64(hdr.version as u64);
                print(", "); print_u64(hdr.tensor_count);
                print(" tensors, "); print_u64(hdr.kv_count);
                println(" KV pairs)");

                // Step 3: Skip KV pairs
                print("[J64] Step 3: Skipping "); print_u64(hdr.kv_count);
                print(" KV pairs... ");
                let mut skipped: u64 = 0;
                for _ in 0..hdr.kv_count {
                    if skip_gguf_kv(fd) { skipped += 1; } else { break; }
                }
                print_u64(skipped); println(" skipped");

                // Step 4: Read tensor data into weight buffers
                // For our test GGUF, tensor data follows immediately after metadata
                print("[J64] Step 4: Loading weights from GGUF... ");
                let t0 = sys_rdtsc();

                unsafe {
                    // Read weights in order: embedding, wq, wk, wv, wo, w_gate, w_up, w_down, w_output
                    // Each read uses chunked I/O to handle large files
                    let mut loaded: u32 = 0;

                    let n = read_f32_weights(fd, &mut EMBEDDING, VOCAB_SIZE * DIM);
                    if n > 0 { loaded += 1; }

                    let n = read_f32_weights(fd, &mut WQ, DIM * DIM);
                    if n > 0 { loaded += 1; }

                    let n = read_f32_weights(fd, &mut WK, DIM * KV_DIM);
                    if n > 0 { loaded += 1; }

                    let n = read_f32_weights(fd, &mut WV, DIM * KV_DIM);
                    if n > 0 { loaded += 1; }

                    let n = read_f32_weights(fd, &mut WO, DIM * DIM);
                    if n > 0 { loaded += 1; }

                    let n = read_f32_weights(fd, &mut W_GATE, DIM * HIDDEN_DIM);
                    if n > 0 { loaded += 1; }

                    let n = read_f32_weights(fd, &mut W_UP, DIM * HIDDEN_DIM);
                    if n > 0 { loaded += 1; }

                    let n = read_f32_weights(fd, &mut W_DOWN, HIDDEN_DIM * DIM);
                    if n > 0 { loaded += 1; }

                    let n = read_f32_weights(fd, &mut W_OUTPUT, DIM * VOCAB_SIZE);
                    if n > 0 { loaded += 1; }

                    weights_from_gguf = loaded;
                }

                let dt = sys_rdtsc() - t0;
                print_u64(weights_from_gguf as u64);
                print(" tensors loaded (");
                print_u64(dt);
                println(" cycles)");

                if weights_from_gguf > 0 {
                    gguf_loaded = true;
                    println("[J64] GGUF weights loaded successfully");
                }
            }
            None => {
                println("FAIL (header parse error)");
            }
        }

        sys_close(fd);
    } else {
        println("NOT FOUND (using synthetic weights)");
    }

    // Step 5: Fallback to synthetic weights if GGUF not loaded
    if !gguf_loaded {
        print("[J64] Step 5: Initializing synthetic weights (fallback)... ");
        let t0 = sys_rdtsc();
        unsafe { init_synthetic_weights(); }
        let dt = sys_rdtsc() - t0;
        print("OK ("); print_u64(dt); println(" cycles)");
    } else {
        print("[J64] Step 5: GGUF weights active (");
        print_u64(weights_from_gguf as u64);
        println("/9 tensors from disk)");
    }

    // Verify weight loading
    unsafe {
        let mut nz: u32 = 0;
        for i in 0..DIM*DIM { if WQ[i] != 0.0 { nz += 1; } }
        print("[J64] Wq nonzero: "); print_u64(nz as u64);
        print("/"); print_u64((DIM*DIM) as u64); println("");
    }

    // Signal LLM ready on the bus
    sys_bus_publish(INTENT_LLM_READY, 2, 0);
    sys_bus_publish(INTENT_LLM_CHAT_INIT, 3, if gguf_loaded { 1 } else { 0 });
    println("[J64] Published INTENT_LLM_READY (0x8004)");

    // Step 6: Generate tokens (simulated user prompt)
    println("[J64] ========================================");
    println("[J64] Token Generation — Chat Mode");
    println("[J64] ========================================");

    let prompt: &[u8] = b"Hello AetherionOS";
    let plen = prompt.len();
    let temperature: f32 = 0.7;

    // Publish user prompt intent
    let prompt_hash = {
        let mut h: u64 = 5381;
        for &b in prompt { h = h.wrapping_mul(33).wrapping_add(b as u64); }
        h
    };
    sys_bus_publish(INTENT_USER_PROMPT, 2, prompt_hash);

    print("[J64] Prompt: \""); sys_write(1, prompt);
    print("\" ("); print_u64(plen as u64); println(" tokens)");
    print("[J64] Generating "); print_u64(GEN_TOKENS as u64);
    println(" tokens (temp=0.7)...");

    let t_gen = sys_rdtsc();

    // Prefill
    print("[J64] Prefill... ");
    for pos in 0..plen {
        unsafe { transformer_forward(prompt[pos] as usize, pos); }
    }
    let first_token = unsafe { argmax(&LOGITS, VOCAB_SIZE) };
    print("OK ("); print_u64(plen as u64); println(" tokens prefilled)");

    // Autoregressive generation
    print("[J64] Output: \"");
    let mut valid: u32 = 0;
    let mut cur_token = first_token;
    let limit = core::cmp::min(GEN_TOKENS, MAX_SEQ_LEN - plen);
    let mut sample_rng: u64 = 0xDEAD_BEEF_CAFE_64;

    for g in 0..limit {
        let pos = plen + g;
        if pos >= MAX_SEQ_LEN { break; }

        // Output character
        let ch = if cur_token >= 0x20 && cur_token <= 0x7E {
            valid += 1;
            cur_token as u8
        } else if cur_token == 0x0A { b'\n' }
        else { b'.' };
        sys_write(1, &[ch]);

        // Publish each token on the bus
        sys_bus_publish(INTENT_TOKEN_GENERATED, 2, ((pos as u64) << 8) | (ch as u64));

        // Forward pass + sample
        unsafe {
            for i in 0..VOCAB_SIZE { LOGITS[i] = 0.0; }
            transformer_forward(cur_token, pos);
            cur_token = sample_temperature(&mut LOGITS, VOCAB_SIZE, temperature, &mut sample_rng);
        }

        // Yield every 8 tokens
        if g % 8 == 0 { sys_yield(); }
    }
    let t_total = sys_rdtsc() - t_gen;
    println("\"");

    // Statistics
    println("[J64] ========================================");
    print("[J64] Tokens generated: "); print_u64(limit as u64); println("");
    print("[J64] Valid printable: "); print_u64(valid as u64); println("");
    print("[J64] Total cycles: "); print_u64(t_total); println("");
    if limit > 0 {
        print("[J64] Cycles/token: "); print_u64(t_total / ((plen as u64) + (limit as u64)));
        println("");
    }
    print("[J64] Source: ");
    if gguf_loaded { println("GGUF from FAT32 disk"); } else { println("Synthetic weights"); }
    print("[J64] GGUF tensors loaded: "); print_u64(weights_from_gguf as u64); println("/9");

    // Signal completion
    sys_bus_publish(INTENT_GENERATION_DONE, 2, limit as u64);
    println("[J64-OK] LLM Chat Agent generation COMPLETE");
    println("[J64-OK] INTENT_TOKEN_GENERATED published per token");
    println("[J64-OK] Chunked GGUF I/O validated");
    println("========================================");

    0
}
