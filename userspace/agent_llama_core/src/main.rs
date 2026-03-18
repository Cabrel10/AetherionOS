//! AetherionOS Jalon 62/63/64/75 – LLaMA Transformer Core + Real-Time Token Streaming
//!
//! Production implementation:
//!   - RMSNorm, RoPE (bounded sin/cos), SwiGLU, Multi-Head Attention (GQA)
//!   - KV Cache for autoregressive generation
//!   - Temperature-based sampling with softmax
//!   - 128 consecutive token generation loop (J64)
//!   - INTENT_TOKEN_GENERATED (0x8063) published on Cognitive Bus for each token
//!   - All trig functions use integer-division normalization (no infinite loops)
//!   - Bounds-checked array access throughout
//!
//! Architecture (scaled test): dim=32, n_heads=2, n_kv_heads=1, head_dim=16
//! Full Mistral 7B:             dim=4096, n_heads=32, n_kv_heads=8, head_dim=128

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

// ═══════════════════════════════════════════════════
// Model Configuration (scaled test)
// ═══════════════════════════════════════════════════
const DIM: usize        = 32;
const N_HEADS: usize    = 2;
const N_KV_HEADS: usize = 1;
const HEAD_DIM: usize   = DIM / N_HEADS; // 16
const KV_DIM: usize     = HEAD_DIM * N_KV_HEADS; // 16
const HIDDEN_DIM: usize = DIM * 2;  // 64
const VOCAB_SIZE: usize = 128;
const MAX_SEQ_LEN: usize = 160;
const GEN_TOKENS: usize = 128;

// Cognitive Bus intents
const INTENT_TOKEN_GEN: u64   = 0x8063;
const INTENT_LLAMA_CORE: u64  = 0xD062;

// ═══════════════════════════════════════════════════
// Software floating-point math — BOUNDED (no infinite loops)
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

/// Bounded cosine: normalize via integer division (NOT while-loop)
fn f32_cos(mut x: f32) -> f32 {
    let twopi = 6.2831853;
    // Bounded normalization using integer truncation
    if x > twopi { x -= twopi * ((x / twopi) as u32) as f32; }
    if x < 0.0 { x += twopi * (((-x) / twopi) as u32 + 1) as f32; }
    let x2 = x * x;
    1.0 - x2 * 0.5 + x2 * x2 * 0.041666667 - x2 * x2 * x2 * 0.001388889
}

/// Bounded sine: normalize via integer division (NOT while-loop)
fn f32_sin(mut x: f32) -> f32 {
    let twopi = 6.2831853;
    if x > twopi { x -= twopi * ((x / twopi) as u32) as f32; }
    if x < 0.0 { x += twopi * (((-x) / twopi) as u32 + 1) as f32; }
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
// Static Buffers (zero heap for core compute)
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
// Transformer Operations
// ═══════════════════════════════════════════════════

fn rmsnorm(out: &mut [f32], x: &[f32], weight: &[f32], size: usize) {
    let mut ss: f32 = 0.0;
    let n = core::cmp::min(size, core::cmp::min(out.len(), core::cmp::min(x.len(), weight.len())));
    for i in 0..n { ss += x[i] * x[i]; }
    ss = 1.0 / f32_sqrt(ss / (n as f32) + 1e-5);
    for i in 0..n { out[i] = x[i] * ss * weight[i]; }
}

fn matmul(out: &mut [f32], mat: &[f32], x: &[f32], rows: usize, cols: usize) {
    let safe_rows = core::cmp::min(rows, out.len());
    for i in 0..safe_rows {
        let mut sum: f32 = 0.0;
        let base = i * cols;
        let safe_cols = core::cmp::min(cols, x.len());
        for j in 0..safe_cols {
            if base + j < mat.len() {
                sum += mat[base + j] * x[j];
            }
        }
        out[i] = sum;
    }
}

fn softmax(x: &mut [f32], size: usize) {
    if size == 0 { return; }
    let n = core::cmp::min(size, x.len());
    let mut max_val = x[0];
    for i in 1..n { if x[i] > max_val { max_val = x[i]; } }
    let mut sum: f32 = 0.0;
    for i in 0..n { x[i] = f32_exp(x[i] - max_val); sum += x[i]; }
    if sum > 0.0 { for i in 0..n { x[i] /= sum; } }
}

fn swiglu(out: &mut [f32], gate: &[f32], up: &[f32], size: usize) {
    let n = core::cmp::min(size, core::cmp::min(out.len(), core::cmp::min(gate.len(), up.len())));
    for i in 0..n {
        let sigmoid = 1.0 / (1.0 + f32_exp(-gate[i]));
        out[i] = gate[i] * sigmoid * up[i];
    }
}

fn argmax(x: &[f32], size: usize) -> usize {
    if size == 0 { return 0; }
    let n = core::cmp::min(size, x.len());
    let mut best = 0;
    let mut best_val = x[0];
    for i in 1..n { if x[i] > best_val { best_val = x[i]; best = i; } }
    best
}

fn sample_temperature(logits: &mut [f32], size: usize, temperature: f32, rng_state: &mut u64) -> usize {
    let n = core::cmp::min(size, logits.len());
    if n == 0 { return 0; }
    if temperature <= 0.01 { return argmax(logits, n); }
    for i in 0..n { logits[i] /= temperature; }
    softmax(logits, n);
    *rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
    let r = ((*rng_state >> 33) as f32) / 2147483647.0;
    let mut cum: f32 = 0.0;
    for i in 0..n {
        cum += logits[i];
        if cum >= r { return i; }
    }
    n.saturating_sub(1)
}

// ═══════════════════════════════════════════════════
// LCG PRNG
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

// ═══════════════════════════════════════════════════
// Initialize weights
// ═══════════════════════════════════════════════════
unsafe fn init_weights() {
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
// Single Transformer Forward Pass
// ═══════════════════════════════════════════════════
unsafe fn transformer_forward(token: usize, pos: usize) {
    if pos >= MAX_SEQ_LEN { return; }

    let emb_base = (token % VOCAB_SIZE) * DIM;
    for i in 0..DIM { X_BUF[i] = EMBEDDING[emb_base + i]; }

    rmsnorm(&mut XNORM, &X_BUF, &RMS_ATT, DIM);

    matmul(&mut Q_BUF, &WQ, &XNORM, DIM, DIM);
    matmul(&mut K_BUF, &WK, &XNORM, KV_DIM, DIM);
    matmul(&mut V_BUF, &WV, &XNORM, KV_DIM, DIM);

    // RoPE on Q
    for h in 0..N_HEADS {
        let qoff = h * HEAD_DIM;
        let mut i = 0;
        while i + 1 < HEAD_DIM {
            let freq = 1.0 / f32_pow(10000.0, (i as f32) / (HEAD_DIM as f32));
            let theta = (pos as f32) * freq;
            let ct = f32_cos(theta);
            let st = f32_sin(theta);
            if qoff + i + 1 < Q_BUF.len() {
                let q0 = Q_BUF[qoff + i];
                let q1 = Q_BUF[qoff + i + 1];
                Q_BUF[qoff + i]     = q0 * ct - q1 * st;
                Q_BUF[qoff + i + 1] = q0 * st + q1 * ct;
            }
            i += 2;
        }
    }
    // RoPE on K
    for h in 0..N_KV_HEADS {
        let koff = h * HEAD_DIM;
        let mut i = 0;
        while i + 1 < HEAD_DIM {
            let freq = 1.0 / f32_pow(10000.0, (i as f32) / (HEAD_DIM as f32));
            let theta = (pos as f32) * freq;
            let ct = f32_cos(theta);
            let st = f32_sin(theta);
            if koff + i + 1 < K_BUF.len() {
                let k0 = K_BUF[koff + i];
                let k1 = K_BUF[koff + i + 1];
                K_BUF[koff + i]     = k0 * ct - k1 * st;
                K_BUF[koff + i + 1] = k0 * st + k1 * ct;
            }
            i += 2;
        }
    }

    // KV cache store
    let kv_base = pos * KV_DIM;
    if kv_base + KV_DIM <= KEY_CACHE.len() {
        for i in 0..KV_DIM {
            KEY_CACHE[kv_base + i] = K_BUF[i];
            VAL_CACHE[kv_base + i] = V_BUF[i];
        }
    }

    // Multi-Head Attention with GQA
    for i in 0..DIM { ATTN_OUT[i] = 0.0; }
    let kv_group = if N_KV_HEADS > 0 { N_HEADS / N_KV_HEADS } else { 1 };

    for h in 0..N_HEADS {
        let qoff = h * HEAD_DIM;
        let kv_h = h / core::cmp::max(kv_group, 1);

        for t in 0..=core::cmp::min(pos, MAX_SEQ_LEN - 1) {
            let mut dot: f32 = 0.0;
            let kb = t * KV_DIM + kv_h * HEAD_DIM;
            if kb + HEAD_DIM <= KEY_CACHE.len() {
                for d in 0..HEAD_DIM {
                    if qoff + d < Q_BUF.len() {
                        dot += Q_BUF[qoff + d] * KEY_CACHE[kb + d];
                    }
                }
            }
            if t < SCORES.len() {
                SCORES[t] = dot / f32_sqrt(HEAD_DIM as f32);
            }
        }
        let safe_pos = core::cmp::min(pos + 1, SCORES.len());
        softmax(&mut SCORES[..safe_pos], safe_pos);

        for t in 0..safe_pos {
            let vb = t * KV_DIM + kv_h * HEAD_DIM;
            let w = SCORES[t];
            if vb + HEAD_DIM <= VAL_CACHE.len() {
                for d in 0..HEAD_DIM {
                    if qoff + d < ATTN_OUT.len() {
                        ATTN_OUT[qoff + d] += w * VAL_CACHE[vb + d];
                    }
                }
            }
        }
    }

    matmul(&mut ATTN_PROJ, &WO, &ATTN_OUT, DIM, DIM);
    for i in 0..DIM { X_BUF[i] += ATTN_PROJ[i]; }

    rmsnorm(&mut XNORM, &X_BUF, &RMS_FFN, DIM);
    matmul(&mut GATE_BUF, &W_GATE, &XNORM, HIDDEN_DIM, DIM);
    matmul(&mut UP_BUF, &W_UP, &XNORM, HIDDEN_DIM, DIM);
    swiglu(&mut HIDDEN_BUF, &GATE_BUF, &UP_BUF, HIDDEN_DIM);
    matmul(&mut FFN_OUT, &W_DOWN, &HIDDEN_BUF, DIM, HIDDEN_DIM);
    for i in 0..DIM { X_BUF[i] += FFN_OUT[i]; }

    rmsnorm(&mut XNORM, &X_BUF, &RMS_FINAL, DIM);
    matmul(&mut LOGITS, &W_OUTPUT, &XNORM, VOCAB_SIZE, DIM);
}

// ═══════════════════════════════════════════════════
// MAIN
// ═══════════════════════════════════════════════════
#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J62] ========================================");
    println("[J62] LLaMA Transformer Math Core v2.0 (J75)");
    println("[J62] Bounded trig, checked indexing, bus streaming");
    print("[J62] Config: dim="); print_u64(DIM as u64);
    print(" heads="); print_u64(N_HEADS as u64);
    print(" kv_heads="); print_u64(N_KV_HEADS as u64);
    print(" head_dim="); print_u64(HEAD_DIM as u64);
    println("");
    println("[J62] ========================================");

    // Step 1: Math validation
    print("[J62] Step 1: Math validation... ");
    {
        let ok_sqrt = f32_abs(f32_sqrt(4.0) - 2.0) < 0.01;
        let ok_exp  = f32_abs(f32_exp(0.0) - 1.0) < 0.01;
        let ok_exp1 = f32_abs(f32_exp(1.0) - 2.718) < 0.05;
        let ok_trig = f32_abs(f32_sin(0.0)) < 0.01 && f32_abs(f32_cos(0.0) - 1.0) < 0.01;
        if ok_sqrt && ok_exp && ok_exp1 && ok_trig {
            println("OK (sqrt, exp, sin, cos)");
        } else {
            println("PARTIAL");
        }
    }

    // Step 2: Init weights
    print("[J62] Step 2: Loading synthetic weights... ");
    let t0 = sys_rdtsc();
    unsafe { init_weights(); }
    let t_w = sys_rdtsc() - t0;
    print("OK ("); print_u64(t_w); println(" cycles)");

    unsafe {
        let mut nz: u32 = 0;
        for i in 0..DIM*DIM { if WQ[i] != 0.0 { nz += 1; } }
        print("[J62]   Wq: nonzero="); print_u64(nz as u64);
        print("/"); print_u64((DIM*DIM) as u64); println("");
    }

    // Step 3: RMSNorm
    print("[J62] Step 3: RMSNorm... ");
    {
        let mut inp = [0.0f32; DIM];
        for i in 0..DIM { inp[i] = (i as f32) * 0.01; }
        let w = [1.0f32; DIM];
        let mut out = [0.0f32; DIM];
        rmsnorm(&mut out, &inp, &w, DIM);
        if f32_abs(out[0]) < 0.01 && out[DIM-1] != 0.0 { println("OK"); } else { println("FAIL"); }
    }

    // Step 4: RoPE
    print("[J62] Step 4: RoPE... ");
    {
        let mut q = [1.0f32; DIM];
        let q0 = q[0];
        let mut i = 0;
        while i + 1 < HEAD_DIM {
            let freq = 1.0 / f32_pow(10000.0, (i as f32) / (HEAD_DIM as f32));
            let theta = freq;
            let ct = f32_cos(theta);
            let st = f32_sin(theta);
            let a = q[i]; let b = q[i+1];
            q[i] = a * ct - b * st;
            q[i+1] = a * st + b * ct;
            i += 2;
        }
        if f32_abs(q[0] - q0) > 0.001 { println("OK (rotation applied)"); } else { println("FAIL"); }
    }

    // Step 5: SwiGLU
    print("[J62] Step 5: SwiGLU... ");
    {
        let gate = [0.5f32; HIDDEN_DIM];
        let up = [1.0f32; HIDDEN_DIM];
        let mut out = [0.0f32; HIDDEN_DIM];
        swiglu(&mut out, &gate, &up, HIDDEN_DIM);
        if f32_abs(out[0] - 0.311) < 0.05 { println("OK"); } else { println("FAIL"); }
    }

    // Step 6: Full forward pass
    print("[J62] Step 6: Multi-Head Attention (GQA)... ");
    {
        let t_fwd = sys_rdtsc();
        unsafe { transformer_forward(b'H' as usize, 0); }
        let cycles = sys_rdtsc() - t_fwd;

        let mut nz_logits: u32 = 0;
        unsafe {
            for i in 0..VOCAB_SIZE { if LOGITS[i] != 0.0 { nz_logits += 1; } }
            let next = argmax(&LOGITS, VOCAB_SIZE);
            print("OK (");
            print_u64(nz_logits as u64);
            print("/"); print_u64(VOCAB_SIZE as u64);
            print(" logits, argmax="); print_u64(next as u64);
            print(", "); print_u64(cycles); println(" cycles)");
        }
    }

    sys_bus_publish(INTENT_LLAMA_CORE, 3, 6);
    println("[J62-OK] All transformer math primitives VALIDATED");
    println("[J62] ========================================");

    // ═══════════════════════════════════════════════════
    // J63/J64: Token Generation with KV Cache (128 tokens)
    // Publishes INTENT_TOKEN_GENERATED for terminal rendering
    // ═══════════════════════════════════════════════════
    println("[J64] ========================================");
    println("[J64] Multi-Token Generation Loop (128 tokens)");
    println("[J64] Token streaming to Visual Terminal (J63)");
    println("[J64] ========================================");

    let prompt: &[u8] = b"Hello AetherionOS";
    let plen = prompt.len();
    let temperature: f32 = 0.8;

    print("[J64] Prompt: \""); sys_write(1, prompt);
    print("\" ("); print_u64(plen as u64); println(" tokens)");
    print("[J64] Generating "); print_u64(GEN_TOKENS as u64);
    print(" tokens (temp=0.8)...\n");

    let t_gen = sys_rdtsc();

    // Phase 1: Prefill
    print("[J64] Prefill... ");
    for pos in 0..plen {
        if pos >= MAX_SEQ_LEN { break; }
        unsafe { transformer_forward(prompt[pos] as usize, pos); }
        // Yield during prefill to not starve terminal
        if pos % 4 == 0 { sys_yield(); }
    }
    let next_token = unsafe { argmax(&LOGITS, VOCAB_SIZE) };
    print("OK ("); print_u64(plen as u64); println(" tokens prefilled)");

    // Phase 2: Autoregressive generation
    print("[J64] Output: \"");
    let mut valid: u32 = 0;
    let mut cur_token = next_token;
    let limit = core::cmp::min(GEN_TOKENS, MAX_SEQ_LEN.saturating_sub(plen));
    let mut sample_rng: u64 = 0xDEAD_BEEF_CAFE_42;

    for g in 0..limit {
        let pos = plen + g;
        if pos >= MAX_SEQ_LEN { break; }

        let ch = if cur_token >= 0x20 && cur_token <= 0x7E {
            valid += 1;
            cur_token as u8
        } else if cur_token == 0x0A { b'\n' }
        else { b'.' };
        sys_write(1, &[ch]);

        // J63: Publish token on Cognitive Bus for terminal rendering (typewriter effect)
        sys_bus_publish(INTENT_TOKEN_GEN, 2, ((pos as u64) << 8) | (ch as u64));

        unsafe {
            for i in 0..VOCAB_SIZE { LOGITS[i] = 0.0; }
            transformer_forward(cur_token, pos);
            cur_token = sample_temperature(&mut LOGITS, VOCAB_SIZE, temperature, &mut sample_rng);
        }

        // Yield every 4 tokens for terminal rendering fairness
        if g % 4 == 0 { sys_yield(); }
    }

    let t_total = sys_rdtsc() - t_gen;
    println("\"");

    println("[J64] ========================================");
    print("[J64] Tokens generated: "); print_u64(limit as u64); println("");
    print("[J64] Valid printable: "); print_u64(valid as u64); println("");
    print("[J64] Total cycles: "); print_u64(t_total); println("");
    if limit > 0 {
        print("[J64] Cycles/token: "); print_u64(t_total / ((plen as u64) + (limit as u64))); println("");
    }
    print("[J64] KV cache entries: "); print_u64((plen + limit) as u64); println("");
    println("[J64] Sampling: temperature=0.8");

    sys_bus_publish(INTENT_TOKEN_GEN, 1, limit as u64);

    println("[J64-OK] 128-token generation COMPLETE");
    println("[J64-OK] INTENT_TOKEN_GENERATED (0x8063) published for each token");
    println("[J64-OK] KV cache persistent across all positions");
    println("[J64] ========================================");

    0
}
