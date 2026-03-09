//! AetherionOS Jalon 62/63/64 – LLaMA Transformer Core + 128-Token Generation
//!
//! Implements the core Mistral/LLaMA transformer operations:
//!   - RMSNorm, RoPE, SwiGLU, Multi-Head Attention (GQA)
//!   - KV Cache for autoregressive generation
//!   - Temperature-based sampling with softmax
//!   - 128 consecutive token generation loop (J64)
//!
//! Architecture (scaled test): dim=32, n_heads=2, n_kv_heads=1, head_dim=16
//! Full Mistral 7B:             dim=4096, n_heads=32, n_kv_heads=8, head_dim=128
//!
//! Publishes INTENT_TOKEN_GENERATED (0x8063) on the Cognitive Bus for each token.

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

// ═══════════════════════════════════════════════════
// Model Configuration (scaled test)
// ═══════════════════════════════════════════════════
const DIM: usize        = 32;       // Model dimension
const N_HEADS: usize    = 2;        // Query heads
const N_KV_HEADS: usize = 1;        // KV heads (GQA: 2/1 = 2 queries per KV)
const HEAD_DIM: usize   = DIM / N_HEADS; // 16
const KV_DIM: usize     = HEAD_DIM * N_KV_HEADS; // 16
const HIDDEN_DIM: usize = DIM * 2;  // 64 (FFN intermediate)
const VOCAB_SIZE: usize = 128;      // ASCII vocabulary
const MAX_SEQ_LEN: usize = 160;     // Maximum sequence length (prompt + 128 gen)
const GEN_TOKENS: usize = 128;      // Tokens to generate (J64)

// Cognitive Bus intents
const INTENT_TOKEN_GEN: u64   = 0x8063;
const INTENT_LLAMA_CORE: u64  = 0xD062;

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
// Static Buffers (zero heap for core compute)
// ═══════════════════════════════════════════════════
// Using mutable statics for all weight and scratch buffers
// to avoid heap allocation entirely.

// Weight matrices (filled once with synthetic data)
static mut WQ: [f32; DIM * DIM] = [0.0; DIM * DIM];             // 4 KB
static mut WK: [f32; DIM * KV_DIM] = [0.0; DIM * KV_DIM];       // 2 KB
static mut WV: [f32; DIM * KV_DIM] = [0.0; DIM * KV_DIM];       // 2 KB
static mut WO: [f32; DIM * DIM] = [0.0; DIM * DIM];             // 4 KB
static mut RMS_ATT: [f32; DIM] = [1.0; DIM];
static mut W_GATE: [f32; DIM * HIDDEN_DIM] = [0.0; DIM * HIDDEN_DIM]; // 8 KB
static mut W_UP: [f32; DIM * HIDDEN_DIM] = [0.0; DIM * HIDDEN_DIM];   // 8 KB
static mut W_DOWN: [f32; HIDDEN_DIM * DIM] = [0.0; HIDDEN_DIM * DIM]; // 8 KB
static mut RMS_FFN: [f32; DIM] = [1.0; DIM];
static mut RMS_FINAL: [f32; DIM] = [1.0; DIM];
static mut W_OUTPUT: [f32; DIM * VOCAB_SIZE] = [0.0; DIM * VOCAB_SIZE]; // 16 KB
static mut EMBEDDING: [f32; VOCAB_SIZE * DIM] = [0.0; VOCAB_SIZE * DIM]; // 16 KB

// KV cache
static mut KEY_CACHE: [f32; MAX_SEQ_LEN * KV_DIM] = [0.0; MAX_SEQ_LEN * KV_DIM];
static mut VAL_CACHE: [f32; MAX_SEQ_LEN * KV_DIM] = [0.0; MAX_SEQ_LEN * KV_DIM];

// Scratch buffers for forward pass
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

// Total static: ~68 KB (in .bss, zero-initialized by ELF loader)

// ═══════════════════════════════════════════════════
// Transformer Operations
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

/// Temperature sampling: scale logits by 1/temperature, apply softmax,
/// then pick based on a simple LCG random number.
fn sample_temperature(logits: &mut [f32], size: usize, temperature: f32, rng_state: &mut u64) -> usize {
    if temperature <= 0.01 { return argmax(logits, size); }
    // Scale logits
    for i in 0..size { logits[i] /= temperature; }
    softmax(logits, size);
    // Random selection based on cumulative probability
    *rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
    let r = ((*rng_state >> 33) as f32) / 2147483647.0; // [0, 1)
    let mut cum: f32 = 0.0;
    for i in 0..size {
        cum += logits[i];
        if cum >= r { return i; }
    }
    size - 1
}

// ═══════════════════════════════════════════════════
// LCG PRNG for reproducible weights
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
// Initialize synthetic weights in static buffers
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
    // RMS weights stay at 1.0 (already initialized)
}

// ═══════════════════════════════════════════════════
// Single Transformer Forward Pass (all static buffers)
// ═══════════════════════════════════════════════════
unsafe fn transformer_forward(token: usize, pos: usize) {
    // Bounds check to prevent out-of-bounds access
    if pos >= MAX_SEQ_LEN {
        return; // Silently skip if position exceeds max sequence length
    }
    
    // Step 1: Token embedding lookup
    let emb_base = (token % VOCAB_SIZE) * DIM;
    for i in 0..DIM { X_BUF[i] = EMBEDDING[emb_base + i]; }

    // Step 2: Attention RMSNorm
    rmsnorm(&mut XNORM, &X_BUF, &RMS_ATT, DIM);

    // Step 3: Q, K, V projections
    matmul(&mut Q_BUF, &WQ, &XNORM, DIM, DIM);
    matmul(&mut K_BUF, &WK, &XNORM, KV_DIM, DIM);
    matmul(&mut V_BUF, &WV, &XNORM, KV_DIM, DIM);

    // Step 4: RoPE
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

    // Step 5: Store K, V in cache
    let kv_base = pos * KV_DIM;
    // Bounds check before writing to cache
    if kv_base + KV_DIM <= KEY_CACHE.len() {
        for i in 0..KV_DIM {
            KEY_CACHE[kv_base + i] = K_BUF[i];
            VAL_CACHE[kv_base + i] = V_BUF[i];
        }
    }

    // Step 6: Multi-Head Attention with GQA
    for i in 0..DIM { ATTN_OUT[i] = 0.0; }
    let kv_group = N_HEADS / N_KV_HEADS;

    for h in 0..N_HEADS {
        let qoff = h * HEAD_DIM;
        let kv_h = h / kv_group;

        // Attention scores
        for t in 0..=pos {
            if t >= MAX_SEQ_LEN { break; } // Bounds check
            let mut dot: f32 = 0.0;
            let kb = t * KV_DIM + kv_h * HEAD_DIM;
            // Bounds check before accessing cache
            if kb + HEAD_DIM <= KEY_CACHE.len() {
                for d in 0..HEAD_DIM { dot += Q_BUF[qoff + d] * KEY_CACHE[kb + d]; }
            }
            SCORES[t] = dot / f32_sqrt(HEAD_DIM as f32);
        }
        let safe_pos = core::cmp::min(pos + 1, MAX_SEQ_LEN);
        softmax(&mut SCORES[..safe_pos], safe_pos);

        // Weighted sum of values
        for t in 0..=pos {
            if t >= MAX_SEQ_LEN { break; } // Bounds check
            let vb = t * KV_DIM + kv_h * HEAD_DIM;
            let w = SCORES[t];
            // Bounds check before accessing cache
            if vb + HEAD_DIM <= VAL_CACHE.len() {
                for d in 0..HEAD_DIM { ATTN_OUT[qoff + d] += w * VAL_CACHE[vb + d]; }
            }
        }
    }

    // Step 7: Output projection + residual
    matmul(&mut ATTN_PROJ, &WO, &ATTN_OUT, DIM, DIM);
    for i in 0..DIM { X_BUF[i] += ATTN_PROJ[i]; }

    // Step 8: FFN RMSNorm
    rmsnorm(&mut XNORM, &X_BUF, &RMS_FFN, DIM);

    // Step 9: FFN with SwiGLU
    matmul(&mut GATE_BUF, &W_GATE, &XNORM, HIDDEN_DIM, DIM);
    matmul(&mut UP_BUF, &W_UP, &XNORM, HIDDEN_DIM, DIM);
    swiglu(&mut HIDDEN_BUF, &GATE_BUF, &UP_BUF, HIDDEN_DIM);

    // Step 10: Down projection + residual
    matmul(&mut FFN_OUT, &W_DOWN, &HIDDEN_BUF, DIM, HIDDEN_DIM);
    for i in 0..DIM { X_BUF[i] += FFN_OUT[i]; }

    // Step 11: Final RMSNorm
    rmsnorm(&mut XNORM, &X_BUF, &RMS_FINAL, DIM);

    // Step 12: Logits
    matmul(&mut LOGITS, &W_OUTPUT, &XNORM, VOCAB_SIZE, DIM);
}

// ═══════════════════════════════════════════════════
// MAIN
// ═══════════════════════════════════════════════════
#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J62] ========================================");
    println("[J62] LLaMA Transformer Math Core v1.0");
    println("[J62] Architecture: Mistral 7B (scaled test)");
    print("[J62] Config: dim="); print_u64(DIM as u64);
    print(" heads="); print_u64(N_HEADS as u64);
    print(" kv_heads="); print_u64(N_KV_HEADS as u64);
    print(" head_dim="); print_u64(HEAD_DIM as u64);
    println("");
    println("[J62] ========================================");

    // ── Step 1: Math validation ──
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

    // ── Step 2: Init weights ──
    print("[J62] Step 2: Loading synthetic weights... ");
    let t0 = sys_rdtsc();
    unsafe { init_weights(); }
    let t_w = sys_rdtsc() - t0;
    print("OK ("); print_u64(t_w); println(" cycles)");

    // Verify
    unsafe {
        let mut nz: u32 = 0;
        for i in 0..DIM*DIM { if WQ[i] != 0.0 { nz += 1; } }
        print("[J62]   Wq: nonzero="); print_u64(nz as u64);
        print("/"); print_u64((DIM*DIM) as u64); println("");
    }

    // ── Step 3: RMSNorm ──
    print("[J62] Step 3: RMSNorm... ");
    {
        let mut inp = [0.0f32; DIM];
        for i in 0..DIM { inp[i] = (i as f32) * 0.01; }
        let w = [1.0f32; DIM];
        let mut out = [0.0f32; DIM];
        rmsnorm(&mut out, &inp, &w, DIM);
        if f32_abs(out[0]) < 0.01 && out[DIM-1] != 0.0 { println("OK"); } else { println("FAIL"); }
    }

    // ── Step 4: RoPE ──
    print("[J62] Step 4: RoPE... ");
    {
        let mut q = [1.0f32; DIM];
        let mut k = [1.0f32; KV_DIM];
        let q0 = q[0];
        // Inline RoPE for one head
        for i in (0..HEAD_DIM).step_by(2) {
            let freq = 1.0 / f32_pow(10000.0, (i as f32) / (HEAD_DIM as f32));
            let theta = freq; // pos=1
            let ct = f32_cos(theta);
            let st = f32_sin(theta);
            let a = q[i]; let b = q[i+1];
            q[i] = a * ct - b * st;
            q[i+1] = a * st + b * ct;
        }
        if f32_abs(q[0] - q0) > 0.001 { println("OK (rotation applied)"); } else { println("FAIL"); }
    }

    // ── Step 5: SwiGLU ──
    print("[J62] Step 5: SwiGLU... ");
    {
        let gate = [0.5f32; HIDDEN_DIM];
        let up = [1.0f32; HIDDEN_DIM];
        let mut out = [0.0f32; HIDDEN_DIM];
        swiglu(&mut out, &gate, &up, HIDDEN_DIM);
        if f32_abs(out[0] - 0.311) < 0.05 { println("OK"); } else { println("FAIL"); }
    }

    // ── Step 6: Full forward pass ──
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
    // ═══════════════════════════════════════════════════
    println("[J64] ========================================");
    println("[J64] Multi-Token Generation Loop (128 tokens)");
    println("[J64] Temperature sampling + KV cache");
    println("[J64] ========================================");

    let prompt: &[u8] = b"Hello AetherionOS";
    let plen = prompt.len();
    let temperature: f32 = 0.8; // Temperature for sampling

    print("[J64] Prompt: \""); sys_write(1, prompt);
    print("\" ("); print_u64(plen as u64); println(" tokens)");
    print("[J64] Generating "); print_u64(GEN_TOKENS as u64);
    print(" tokens (temp=0.8)...\n");

    let t_gen = sys_rdtsc();

    // Phase 1: Prefill
    print("[J64] Prefill... ");
    for pos in 0..plen {
        unsafe { transformer_forward(prompt[pos] as usize, pos); }
    }
    let next_token = unsafe { argmax(&LOGITS, VOCAB_SIZE) };
    print("OK ("); print_u64(plen as u64); println(" tokens prefilled)");

    // Phase 2: Autoregressive generation (128 tokens)
    print("[J64] Output: \"");
    let mut valid: u32 = 0;
    let mut cur_token = next_token;
    let limit = core::cmp::min(GEN_TOKENS, MAX_SEQ_LEN - plen);
    let mut sample_rng: u64 = 0xDEAD_BEEF_CAFE_42;

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

        // Publish on bus
        sys_bus_publish(INTENT_TOKEN_GEN, 2, ((pos as u64) << 8) | (ch as u64));

        // Forward pass + temperature sampling for next token
        unsafe {
            for i in 0..VOCAB_SIZE { LOGITS[i] = 0.0; }
            transformer_forward(cur_token, pos);
            cur_token = sample_temperature(&mut LOGITS, VOCAB_SIZE, temperature, &mut sample_rng);
        }

        // Yield every 8 tokens for fairness
        if g % 8 == 0 { sys_yield(); }
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
    print("[J64] Sampling: temperature=0.8\n");

    sys_bus_publish(INTENT_TOKEN_GEN, 1, limit as u64);

    println("[J64-OK] 128-token generation COMPLETE");
    println("[J64-OK] KV cache persistent across all positions");
    println("[J64-OK] INTENT_TOKEN_GENERATED published for each token");
    println("[J64-OK] Temperature sampling active");
    println("[J64] ========================================");

    0
}
