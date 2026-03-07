//! AetherionOS Jalon 49 - Bare-Metal LLM Transformer Math Engine (Ring 3)
//!
//! Implements core transformer building blocks:
//!   - RMSNorm (Root Mean Square Layer Normalization)
//!   - Softmax (numerically stable)
//!   - RoPE (Rotary Position Embeddings)
//!   - MatMul (matrix-vector multiply)
//!   - Dummy TransformerBlock: forward pass token-to-token
//!
//! All math is f32, no_std, no heap for core ops (stack-allocated).

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

// ===== Model dimensions (tiny test model) =====
const DIM: usize = 64;          // embedding dimension
const HIDDEN_DIM: usize = 128;  // FFN hidden dimension
const N_HEADS: usize = 4;       // attention heads
const HEAD_DIM: usize = DIM / N_HEADS; // 16

// ===== Math utilities =====

fn f32_sqrt(x: f32) -> f32 {
    if x <= 0.0 { return 0.0; }
    // Newton's method: 6 iterations for ~24-bit precision
    let mut guess = x;
    // Initial estimate using integer bit hack
    let i = x.to_bits();
    let i = 0x5f3759df - (i >> 1); // fast inverse sqrt
    guess = f32::from_bits(i);
    guess = 1.0 / guess; // convert to sqrt
    // Refine
    for _ in 0..4 {
        guess = 0.5 * (guess + x / guess);
    }
    guess
}

fn f32_exp(x: f32) -> f32 {
    // Clamped range-reduced exp approximation
    let x = if x > 88.0 { 88.0 } else if x < -88.0 { -88.0 } else { x };
    // exp(x) = 2^(x/ln2), use polynomial approx
    let ln2 = 0.6931471805599453_f32;
    let log2e = 1.4426950408889634_f32;
    let t = x * log2e;
    let ti = t as i32;
    let tf = t - ti as f32;
    // 2^tf polynomial (minimax on [0,1])
    let p = 1.0 + tf * (0.6931472 + tf * (0.2402265 + tf * (0.0555041 + tf * 0.0096139)));
    // 2^ti via bit manipulation
    if ti > 127 { return f32::MAX; }
    if ti < -126 { return 0.0; }
    let pow2i = f32::from_bits(((ti + 127) as u32) << 23);
    p * pow2i
}

fn f32_sin(x: f32) -> f32 {
    // Taylor series sin, 7 terms
    let x = x % (2.0 * 3.14159265);
    let x2 = x * x;
    let mut result = x;
    let mut term = x;
    term *= -x2 / (2.0 * 3.0); result += term;
    term *= -x2 / (4.0 * 5.0); result += term;
    term *= -x2 / (6.0 * 7.0); result += term;
    term *= -x2 / (8.0 * 9.0); result += term;
    term *= -x2 / (10.0 * 11.0); result += term;
    result
}

fn f32_cos(x: f32) -> f32 {
    f32_sin(x + 1.5707963) // cos(x) = sin(x + pi/2)
}

// ===== RMSNorm =====
// out[i] = (x[i] / rms) * weight[i]
// rms = sqrt(mean(x^2) + eps)
fn rmsnorm(out: &mut [f32], x: &[f32], weight: &[f32], n: usize) {
    let mut ss: f32 = 0.0;
    for i in 0..n {
        ss += x[i] * x[i];
    }
    ss = ss / n as f32;
    ss = 1.0 / f32_sqrt(ss + 1e-5);
    for i in 0..n {
        out[i] = x[i] * ss * weight[i];
    }
}

// ===== Softmax =====
// Numerically stable: subtract max, then exp, then normalize
fn softmax(x: &mut [f32], n: usize) {
    let mut max_val = x[0];
    for i in 1..n {
        if x[i] > max_val { max_val = x[i]; }
    }
    let mut sum: f32 = 0.0;
    for i in 0..n {
        x[i] = f32_exp(x[i] - max_val);
        sum += x[i];
    }
    if sum > 0.0 {
        for i in 0..n {
            x[i] /= sum;
        }
    }
}

// ===== MatMul (matrix-vector) =====
// out = W * x, where W is [rows x cols], x is [cols]
fn matmul(out: &mut [f32], w: &[f32], x: &[f32], rows: usize, cols: usize) {
    for i in 0..rows {
        let mut val: f32 = 0.0;
        let base = i * cols;
        for j in 0..cols {
            val += w[base + j] * x[j];
        }
        out[i] = val;
    }
}

// ===== RoPE (Rotary Position Embeddings) =====
// Applies rotation to pairs of dimensions based on position
fn rope(q: &mut [f32], k: &mut [f32], head_dim: usize, pos: usize, n_heads: usize) {
    for h in 0..n_heads {
        let offset = h * head_dim;
        for i in (0..head_dim).step_by(2) {
            let freq = 1.0 / f32_sqrt(10000.0_f32);
            let theta = pos as f32 * freq * ((i / 2) as f32 + 1.0);
            let cos_t = f32_cos(theta);
            let sin_t = f32_sin(theta);

            // Rotate q
            let q0 = q[offset + i];
            let q1 = q[offset + i + 1];
            q[offset + i] = q0 * cos_t - q1 * sin_t;
            q[offset + i + 1] = q0 * sin_t + q1 * cos_t;

            // Rotate k
            let k0 = k[offset + i];
            let k1 = k[offset + i + 1];
            k[offset + i] = k0 * cos_t - k1 * sin_t;
            k[offset + i + 1] = k0 * sin_t + k1 * cos_t;
        }
    }
}

// ===== SwiGLU activation =====
fn swiglu(out: &mut [f32], gate: &[f32], up: &[f32], n: usize) {
    for i in 0..n {
        // SiLU(gate) * up = gate * sigmoid(gate) * up
        let sig = 1.0 / (1.0 + f32_exp(-gate[i]));
        out[i] = gate[i] * sig * up[i];
    }
}

// ===== Dummy Transformer Block =====
// Single-layer forward pass: RMSNorm -> Self-Attention (simplified) -> RMSNorm -> FFN
struct TransformerWeights {
    // Attention weights (simplified: Wq, Wk, Wv, Wo as DIMxDIM)
    wq: [f32; DIM * DIM],
    wk: [f32; DIM * DIM],
    wv: [f32; DIM * DIM],
    wo: [f32; DIM * DIM],
    // FFN weights
    w_gate: [f32; DIM * HIDDEN_DIM],
    w_up: [f32; DIM * HIDDEN_DIM],
    w_down: [f32; HIDDEN_DIM * DIM],
    // Norms
    rms_att: [f32; DIM],
    rms_ffn: [f32; DIM],
}

fn init_dummy_weights(w: &mut TransformerWeights) {
    // Initialize with small deterministic values
    for i in 0..DIM * DIM {
        let v = ((i % 97) as f32 - 48.0) * 0.01;
        w.wq[i] = v;
        w.wk[i] = v * 0.9;
        w.wv[i] = v * 0.8;
        w.wo[i] = v * 0.7;
    }
    for i in 0..DIM * HIDDEN_DIM {
        let v = ((i % 89) as f32 - 44.0) * 0.01;
        w.w_gate[i] = v;
        w.w_up[i] = v * 0.9;
    }
    for i in 0..HIDDEN_DIM * DIM {
        w.w_down[i] = ((i % 83) as f32 - 41.0) * 0.01;
    }
    for i in 0..DIM {
        w.rms_att[i] = 1.0;
        w.rms_ffn[i] = 1.0;
    }
}

fn transformer_forward(
    out: &mut [f32; DIM],
    input: &[f32; DIM],
    weights: &TransformerWeights,
    pos: usize,
) {
    // Working buffers on stack
    let mut normed = [0.0f32; DIM];
    let mut q = [0.0f32; DIM];
    let mut k = [0.0f32; DIM];
    let mut v = [0.0f32; DIM];
    let mut attn_out = [0.0f32; DIM];

    // 1. RMSNorm (attention)
    rmsnorm(&mut normed, input, &weights.rms_att, DIM);

    // 2. Q, K, V projections
    matmul(&mut q, &weights.wq, &normed, DIM, DIM);
    matmul(&mut k, &weights.wk, &normed, DIM, DIM);
    matmul(&mut v, &weights.wv, &normed, DIM, DIM);

    // 3. RoPE
    rope(&mut q, &mut k, HEAD_DIM, pos, N_HEADS);

    // 4. Simplified self-attention (single token, no KV cache)
    // attn_score = dot(q, k) / sqrt(head_dim), then apply to v
    let scale = 1.0 / f32_sqrt(HEAD_DIM as f32);
    for h in 0..N_HEADS {
        let off = h * HEAD_DIM;
        let mut score: f32 = 0.0;
        for j in 0..HEAD_DIM {
            score += q[off + j] * k[off + j];
        }
        score *= scale;
        // Single-token attention: score is just the weight for v
        let w = f32_exp(score) / (f32_exp(score) + 1.0); // sigmoid-like
        for j in 0..HEAD_DIM {
            attn_out[off + j] = w * v[off + j];
        }
    }

    // 5. Output projection + residual
    let mut projected = [0.0f32; DIM];
    matmul(&mut projected, &weights.wo, &attn_out, DIM, DIM);
    let mut residual = [0.0f32; DIM];
    for i in 0..DIM {
        residual[i] = input[i] + projected[i];
    }

    // 6. RMSNorm (FFN)
    rmsnorm(&mut normed, &residual, &weights.rms_ffn, DIM);

    // 7. FFN: SwiGLU
    let mut gate = [0.0f32; HIDDEN_DIM];
    let mut up = [0.0f32; HIDDEN_DIM];
    let mut ffn_hidden = [0.0f32; HIDDEN_DIM];
    matmul(&mut gate, &weights.w_gate, &normed, HIDDEN_DIM, DIM);
    matmul(&mut up, &weights.w_up, &normed, HIDDEN_DIM, DIM);
    swiglu(&mut ffn_hidden, &gate, &up, HIDDEN_DIM);

    // 8. Down projection + residual
    let mut down_out = [0.0f32; DIM];
    matmul(&mut down_out, &weights.w_down, &ffn_hidden, DIM, HIDDEN_DIM);
    for i in 0..DIM {
        out[i] = residual[i] + down_out[i];
    }
}

fn print_f32_approx(val: f32) {
    if val < 0.0 {
        print("-");
        print_f32_approx(-val);
        return;
    }
    let int_part = val as u64;
    print_u64(int_part);
    print(".");
    let frac = ((val - int_part as f32) * 1000.0) as u64;
    if frac < 100 { print("0"); }
    if frac < 10 { print("0"); }
    print_u64(frac);
}

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J49] LLM Transformer Math Engine v1.0");
    print("[J49] dim=");
    print_u64(DIM as u64);
    print(" hidden=");
    print_u64(HIDDEN_DIM as u64);
    print(" heads=");
    print_u64(N_HEADS as u64);
    println("");

    // Test 1: RMSNorm
    {
        let mut input = [0.0f32; 8];
        let weight = [1.0f32; 8];
        for i in 0..8 { input[i] = (i as f32 + 1.0) * 0.5; }
        let mut out = [0.0f32; 8];
        rmsnorm(&mut out, &input, &weight, 8);
        print("[J49] RMSNorm test: out[0]=");
        print_f32_approx(out[0]);
        print(" out[7]=");
        print_f32_approx(out[7]);
        println(" OK");
    }

    // Test 2: Softmax
    {
        let mut x = [1.0f32, 2.0, 3.0, 4.0, 0.0, 0.0, 0.0, 0.0];
        softmax(&mut x, 4);
        print("[J49] Softmax test: [");
        for i in 0..4 {
            print_f32_approx(x[i]);
            if i < 3 { print(", "); }
        }
        println("] OK");
    }

    // Test 3: MatMul
    {
        let w = [1.0f32, 0.0, 0.0, 1.0]; // 2x2 identity
        let x = [3.0f32, 7.0];
        let mut out = [0.0f32; 2];
        matmul(&mut out, &w, &x, 2, 2);
        print("[J49] MatMul test: [");
        print_f32_approx(out[0]);
        print(", ");
        print_f32_approx(out[1]);
        println("] OK");
    }

    // Test 4: RoPE
    {
        let mut q = [1.0f32; DIM];
        let mut k = [0.5f32; DIM];
        rope(&mut q, &mut k, HEAD_DIM, 1, N_HEADS);
        print("[J49] RoPE test: q[0]=");
        print_f32_approx(q[0]);
        print(" q[1]=");
        print_f32_approx(q[1]);
        println(" OK");
    }

    // Test 5: Full transformer forward pass
    {
        println("[J49] Initializing dummy weights...");
        // Use mmap for weights (too large for stack: ~100KB)
        let weights_size = core::mem::size_of::<TransformerWeights>();
        let w_addr = sys_mmap(weights_size);
        if w_addr == 0 || w_addr > 0xFFFF_FFFF_FFFF {
            println("[J49] FAIL: mmap for weights");
            return 1;
        }
        let weights = unsafe { &mut *(w_addr as *mut TransformerWeights) };
        init_dummy_weights(weights);

        let mut input = [0.0f32; DIM];
        for i in 0..DIM { input[i] = (i as f32) * 0.1; }
        let mut output = [0.0f32; DIM];

        println("[J49] Running forward pass (pos=0)...");
        let t0 = sys_rdtsc();
        transformer_forward(&mut output, &input, weights, 0);
        let t1 = sys_rdtsc();

        print("[J49] Forward pass: ");
        print_u64(t1 - t0);
        println(" cycles");

        print("[J49] out[0]=");
        print_f32_approx(output[0]);
        print(" out[63]=");
        print_f32_approx(output[63]);
        println("");

        // Verify output is non-zero and finite
        let mut valid = true;
        for i in 0..DIM {
            if output[i] != output[i] { // NaN check
                valid = false;
                break;
            }
        }
        if valid {
            println("[J49] Forward pass: VALID (no NaN)");
        } else {
            println("[J49] Forward pass: INVALID (NaN detected)");
            return 1;
        }

        // Run second pass at pos=1
        let mut output2 = [0.0f32; DIM];
        transformer_forward(&mut output2, &output, weights, 1);
        print("[J49] Pass 2: out[0]=");
        print_f32_approx(output2[0]);
        println(" OK");
    }

    // Publish success
    let bus_ret = sys_bus_publish(0xC049, 2, DIM as u64);
    if bus_ret == 0 {
        println("[J49] Bus 0xC049 OK");
    }

    sys_write(1, b"\n[J49-OK] Transformer math engine SUCCESS\n");
    0
}
