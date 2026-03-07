//! AetherionOS Jalon 50 - LLM Chat Agent via Cognitive Bus (Ring 3)
//!
//! Demonstrates the LLM-to-Terminal communication pattern:
//!   1. Publishes INTENT_USER_PROMPT (0x8001) to signal incoming prompt
//!   2. Runs transformer forward passes to generate tokens
//!   3. Publishes INTENT_TOKEN_GENERATED (0x8002) for each output token
//!   4. Publishes INTENT_GENERATION_DONE (0x8003) when complete
//!
//! This establishes the protocol for connecting the LLM engine
//! to agent_terminal via the Cognitive Bus.

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

// ===== Cognitive Bus Intent IDs =====
const INTENT_USER_PROMPT: u64 = 0x8001;
const INTENT_TOKEN_GENERATED: u64 = 0x8002;
const INTENT_GENERATION_DONE: u64 = 0x8003;
const INTENT_LLM_READY: u64 = 0x8004;

// ===== Mini transformer (reusing J49 math) =====
const DIM: usize = 32;
const HIDDEN_DIM: usize = 64;
const N_HEADS: usize = 2;
const HEAD_DIM: usize = DIM / N_HEADS;
const VOCAB_SIZE: usize = 128; // ASCII range

fn f32_sqrt(x: f32) -> f32 {
    if x <= 0.0 { return 0.0; }
    let i = x.to_bits();
    let i = 0x5f3759df - (i >> 1);
    let mut g = 1.0 / f32::from_bits(i);
    for _ in 0..4 { g = 0.5 * (g + x / g); }
    g
}

fn f32_exp(x: f32) -> f32 {
    let x = if x > 88.0 { 88.0 } else if x < -88.0 { -88.0 } else { x };
    let log2e = 1.4426950408889634_f32;
    let t = x * log2e;
    let ti = t as i32;
    let tf = t - ti as f32;
    let p = 1.0 + tf * (0.6931472 + tf * (0.2402265 + tf * (0.0555041 + tf * 0.0096139)));
    if ti > 127 { return f32::MAX; }
    if ti < -126 { return 0.0; }
    let pow2i = f32::from_bits(((ti + 127) as u32) << 23);
    p * pow2i
}

fn rmsnorm(out: &mut [f32], x: &[f32], n: usize) {
    let mut ss: f32 = 0.0;
    for i in 0..n { ss += x[i] * x[i]; }
    ss = 1.0 / f32_sqrt(ss / n as f32 + 1e-5);
    for i in 0..n { out[i] = x[i] * ss; }
}

fn matmul(out: &mut [f32], w: &[f32], x: &[f32], rows: usize, cols: usize) {
    for i in 0..rows {
        let mut val: f32 = 0.0;
        let base = i * cols;
        for j in 0..cols { val += w[base + j] * x[j]; }
        out[i] = val;
    }
}

fn softmax(x: &mut [f32], n: usize) {
    let mut max_val = x[0];
    for i in 1..n { if x[i] > max_val { max_val = x[i]; } }
    let mut sum: f32 = 0.0;
    for i in 0..n {
        x[i] = f32_exp(x[i] - max_val);
        sum += x[i];
    }
    if sum > 0.0 {
        for i in 0..n { x[i] /= sum; }
    }
}

/// Simple token sampling: argmax
fn sample_argmax(logits: &[f32], n: usize) -> usize {
    let mut best = 0;
    let mut best_val = logits[0];
    for i in 1..n {
        if logits[i] > best_val {
            best_val = logits[i];
            best = i;
        }
    }
    best
}

/// Lightweight LLM weights (allocated via mmap)
#[repr(C)]
struct MiniLLM {
    // Embedding: VOCAB_SIZE x DIM
    embed: [f32; VOCAB_SIZE * DIM],
    // Single transformer layer
    wq: [f32; DIM * DIM],
    wk: [f32; DIM * DIM],
    wv: [f32; DIM * DIM],
    wo: [f32; DIM * DIM],
    w_gate: [f32; DIM * HIDDEN_DIM],
    w_up: [f32; DIM * HIDDEN_DIM],
    w_down: [f32; HIDDEN_DIM * DIM],
    // Output projection: DIM x VOCAB_SIZE
    w_output: [f32; DIM * VOCAB_SIZE],
}

fn init_mini_llm(m: &mut MiniLLM) {
    // Deterministic pseudo-random init
    for i in 0..VOCAB_SIZE * DIM {
        m.embed[i] = ((i % 97) as f32 - 48.0) * 0.02;
    }
    for i in 0..DIM * DIM {
        let v = ((i % 73) as f32 - 36.0) * 0.01;
        m.wq[i] = v;
        m.wk[i] = v * 0.9;
        m.wv[i] = v * 0.8;
        m.wo[i] = v * 0.7;
    }
    for i in 0..DIM * HIDDEN_DIM {
        m.w_gate[i] = ((i % 61) as f32 - 30.0) * 0.01;
        m.w_up[i] = ((i % 53) as f32 - 26.0) * 0.01;
    }
    for i in 0..HIDDEN_DIM * DIM {
        m.w_down[i] = ((i % 47) as f32 - 23.0) * 0.01;
    }
    for i in 0..DIM * VOCAB_SIZE {
        m.w_output[i] = ((i % 83) as f32 - 41.0) * 0.015;
    }
}

/// Forward pass: token_id -> logits[VOCAB_SIZE]
fn forward(m: &MiniLLM, token_id: usize, pos: usize, logits: &mut [f32; VOCAB_SIZE]) {
    // Embedding lookup
    let mut x = [0.0f32; DIM];
    let base = token_id * DIM;
    for i in 0..DIM {
        x[i] = if base + i < VOCAB_SIZE * DIM { m.embed[base + i] } else { 0.0 };
    }

    // RMSNorm
    let mut normed = [0.0f32; DIM];
    rmsnorm(&mut normed, &x, DIM);

    // Q, K, V
    let mut q = [0.0f32; DIM];
    let mut k = [0.0f32; DIM];
    let mut v = [0.0f32; DIM];
    matmul(&mut q, &m.wq, &normed, DIM, DIM);
    matmul(&mut k, &m.wk, &normed, DIM, DIM);
    matmul(&mut v, &m.wv, &normed, DIM, DIM);

    // Simplified attention (single token, no cache)
    let scale = 1.0 / f32_sqrt(HEAD_DIM as f32);
    let mut attn_out = [0.0f32; DIM];
    for h in 0..N_HEADS {
        let off = h * HEAD_DIM;
        let mut score: f32 = 0.0;
        for j in 0..HEAD_DIM { score += q[off + j] * k[off + j]; }
        score *= scale;
        let w = 1.0 / (1.0 + f32_exp(-score));
        for j in 0..HEAD_DIM { attn_out[off + j] = w * v[off + j]; }
    }

    // Output proj + residual
    let mut projected = [0.0f32; DIM];
    matmul(&mut projected, &m.wo, &attn_out, DIM, DIM);
    let mut residual = [0.0f32; DIM];
    for i in 0..DIM { residual[i] = x[i] + projected[i]; }

    // FFN
    rmsnorm(&mut normed, &residual, DIM);
    let mut gate = [0.0f32; HIDDEN_DIM];
    let mut up = [0.0f32; HIDDEN_DIM];
    matmul(&mut gate, &m.w_gate, &normed, HIDDEN_DIM, DIM);
    matmul(&mut up, &m.w_up, &normed, HIDDEN_DIM, DIM);
    let mut ffn = [0.0f32; HIDDEN_DIM];
    for i in 0..HIDDEN_DIM {
        let sig = 1.0 / (1.0 + f32_exp(-gate[i]));
        ffn[i] = gate[i] * sig * up[i];
    }
    let mut down_out = [0.0f32; DIM];
    matmul(&mut down_out, &m.w_down, &ffn, DIM, HIDDEN_DIM);
    for i in 0..DIM { residual[i] += down_out[i]; }

    // Final norm + output projection
    rmsnorm(&mut normed, &residual, DIM);
    // logits = w_output * normed
    for i in 0..VOCAB_SIZE {
        let mut val: f32 = 0.0;
        let base = i * DIM;
        for j in 0..DIM { val += m.w_output[base + j] * normed[j]; }
        logits[i] = val;
    }
}

const MAX_GEN_TOKENS: usize = 16;

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J50] LLM Chat Agent via Cognitive Bus v1.0");

    // Signal LLM engine is ready
    sys_bus_publish(INTENT_LLM_READY, 2, 0);
    println("[J50] Published INTENT_LLM_READY (0x8004)");

    // Allocate model weights
    let model_size = core::mem::size_of::<MiniLLM>();
    print("[J50] Model size: ");
    print_u64(model_size as u64);
    println(" bytes");

    let m_addr = sys_mmap(model_size);
    if m_addr == 0 || m_addr > 0xFFFF_FFFF_FFFF {
        println("[J50] FAIL: mmap for model");
        return 1;
    }
    let model = unsafe { &mut *(m_addr as *mut MiniLLM) };
    init_mini_llm(model);
    println("[J50] Model initialized");

    // Simulate user prompt: "Hi" = [72, 105]
    let prompt: [u8; 2] = [b'H', b'i'];
    print("[J50] User prompt: \"");
    sys_write(1, &prompt);
    println("\"");

    // Publish INTENT_USER_PROMPT with prompt hash
    let prompt_hash = (prompt[0] as u64) << 8 | prompt[1] as u64;
    sys_bus_publish(INTENT_USER_PROMPT, 2, prompt_hash);
    println("[J50] Published INTENT_USER_PROMPT (0x8001)");

    // Process prompt tokens through transformer
    let mut logits = [0.0f32; VOCAB_SIZE];
    let mut last_token: usize = prompt[1] as usize; // Start from last prompt token

    // Forward pass for prompt
    for (pos, &tok) in prompt.iter().enumerate() {
        forward(model, tok as usize, pos, &mut logits);
    }

    // Generate response tokens
    print("[J50] Generated: \"");
    let mut gen_tokens = [0u8; MAX_GEN_TOKENS];
    let mut gen_count: usize = 0;

    for step in 0..MAX_GEN_TOKENS {
        // Sample next token
        softmax(&mut logits, VOCAB_SIZE);
        let next_token = sample_argmax(&logits, VOCAB_SIZE);

        // Publish each generated token on the bus
        sys_bus_publish(INTENT_TOKEN_GENERATED, 1, next_token as u64);

        // Store and print (only printable ASCII)
        let ch = if next_token >= 32 && next_token < 127 {
            next_token as u8
        } else {
            b'.'
        };
        gen_tokens[step] = ch;
        gen_count += 1;
        sys_write(1, &[ch]);

        // Forward pass for next token
        forward(model, next_token, prompt.len() + step, &mut logits);
    }
    println("\"");

    print("[J50] Generated ");
    print_u64(gen_count as u64);
    println(" tokens");

    // Signal generation complete
    sys_bus_publish(INTENT_GENERATION_DONE, 2, gen_count as u64);
    println("[J50] Published INTENT_GENERATION_DONE (0x8003)");

    // Print token IDs
    print("[J50] Token IDs: [");
    for i in 0..gen_count {
        print_u64(gen_tokens[i] as u64);
        if i + 1 < gen_count { print(", "); }
    }
    println("]");

    sys_write(1, b"\n[J50-OK] LLM Chat via Cognitive Bus SUCCESS\n");
    0
}
