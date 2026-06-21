// kernel/src/llm/inference.rs — Transformer Inference Engine for AetherionOS (Layer 8)
//
// Implements a complete Llama-style transformer forward pass for SmolLM2-135M.
// Architecture: 30 layers, dim=576, n_heads=9, n_kv_heads=3, head_dim=64
//
// This runs entirely bare-metal with no OS overhead.

use alloc::vec;
use alloc::vec::Vec;
use super::matmul::*;

/// Model hyperparameters (SmolLM2-135M defaults)
#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub dim: usize,         // 576
    pub hidden_dim: usize,  // 1536
    pub n_layers: usize,    // 30
    pub n_heads: usize,     // 9
    pub n_kv_heads: usize,  // 3
    pub vocab_size: usize,  // 49152
    pub max_seq_len: usize, // 2048
    pub rope_theta: f32,    // 10000.0
    pub norm_eps: f32,      // 1e-5
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            dim: 576,
            hidden_dim: 1536,
            n_layers: 30,
            n_heads: 9,
            n_kv_heads: 3,
            vocab_size: 49152,
            max_seq_len: 2048,
            rope_theta: 10000.0,
            norm_eps: 1e-5,
        }
    }
}

impl ModelConfig {
    pub fn head_dim(&self) -> usize {
        self.dim / self.n_heads
    }

    pub fn kv_dim(&self) -> usize {
        self.head_dim() * self.n_kv_heads
    }

    pub fn n_rep(&self) -> usize {
        self.n_heads / self.n_kv_heads
    }
}

/// KV cache for a single layer
pub struct KVCache {
    pub key: Vec<f32>,    // [seq_len, kv_dim]
    pub value: Vec<f32>,  // [seq_len, kv_dim]
    pub len: usize,
}

impl KVCache {
    pub fn new(max_seq: usize, kv_dim: usize) -> Self {
        Self {
            key: vec![0.0; max_seq * kv_dim],
            value: vec![0.0; max_seq * kv_dim],
            len: 0,
        }
    }

    /// Store key/value at current position
    pub fn store(&mut self, pos: usize, k: &[f32], v: &[f32], kv_dim: usize) {
        let offset = pos * kv_dim;
        if offset + kv_dim <= self.key.len() {
            self.key[offset..offset + kv_dim].copy_from_slice(&k[..kv_dim]);
            self.value[offset..offset + kv_dim].copy_from_slice(&v[..kv_dim]);
            self.len = pos + 1;
        }
    }
}

/// Full model state
pub struct TransformerState {
    pub config: ModelConfig,
    pub kv_caches: Vec<KVCache>,
    // Working buffers
    pub x: Vec<f32>,      // [dim]
    pub xb: Vec<f32>,     // [dim] — pre-norm buffer
    pub xb2: Vec<f32>,    // [dim] — second buffer
    pub hb: Vec<f32>,     // [hidden_dim]
    pub hb2: Vec<f32>,    // [hidden_dim]
    pub q: Vec<f32>,      // [dim]
    pub k: Vec<f32>,      // [kv_dim]
    pub v: Vec<f32>,      // [kv_dim]
    pub att: Vec<f32>,    // [n_heads, max_seq_len]
    pub logits: Vec<f32>, // [vocab_size]
}

impl TransformerState {
    pub fn new(config: ModelConfig) -> Self {
        let dim = config.dim;
        let hidden_dim = config.hidden_dim;
        let kv_dim = config.kv_dim();
        let max_seq = config.max_seq_len;
        let vocab_size = config.vocab_size;
        let n_layers = config.n_layers;
        let n_heads = config.n_heads;

        let mut kv_caches = Vec::with_capacity(n_layers);
        for _ in 0..n_layers {
            kv_caches.push(KVCache::new(max_seq, kv_dim));
        }

        Self {
            config,
            kv_caches,
            x: vec![0.0; dim],
            xb: vec![0.0; dim],
            xb2: vec![0.0; dim],
            hb: vec![0.0; hidden_dim],
            hb2: vec![0.0; hidden_dim],
            q: vec![0.0; dim],
            k: vec![0.0; kv_dim],
            v: vec![0.0; kv_dim],
            att: vec![0.0; n_heads * max_seq],
            logits: vec![0.0; vocab_size],
        }
    }

    /// Allocator info: total bytes needed for state + KV cache
    pub fn memory_usage(&self) -> usize {
        let dim = self.config.dim;
        let kv_dim = self.config.kv_dim();
        let max_seq = self.config.max_seq_len;
        let n_layers = self.config.n_layers;
        let vocab_size = self.config.vocab_size;
        let n_heads = self.config.n_heads;
        let hidden_dim = self.config.hidden_dim;

        let kv_bytes = n_layers * 2 * max_seq * kv_dim * 4; // key + value
        let buf_bytes = (dim * 5 + hidden_dim * 2 + kv_dim * 2 + n_heads * max_seq + vocab_size) * 4;
        kv_bytes + buf_bytes
    }

    /// Reset KV caches for a new generation
    pub fn reset(&mut self) {
        for cache in &mut self.kv_caches {
            cache.len = 0;
        }
    }
}

/// Dequantize a single embedding row from Q4_0 data on-the-fly.
/// Q4_0 format: blocks of 32 elements, 18 bytes each (2 byte f16 scale + 16 byte nibbles).
/// The embedding matrix is stored row-major: row[tok_idx] starts at tok_idx*dim elements.
fn dequant_embedding_row_q4(q4_data: &[u8], tok_idx: usize, dim: usize, out: &mut [f32]) {
    use super::matmul::f16_to_f32;
    let blocks_per_row = dim / 32;
    let bytes_per_row = blocks_per_row * 18;
    let row_start = tok_idx * bytes_per_row;

    if row_start + bytes_per_row > q4_data.len() {
        for v in out.iter_mut().take(dim) { *v = 0.0; }
        return;
    }

    for b in 0..blocks_per_row {
        let off = row_start + b * 18;
        let raw_scale = f16_to_f32(u16::from_le_bytes([q4_data[off], q4_data[off + 1]]));
        // Scale-level sanitization: corrupt GGUF blocks have |scale| >> 1.0 (e.g. -61408)
        // Healthy scales observed in [0.0001, 0.04]. Threshold 1.0 gives 25x safety margin.
        let scale = if !raw_scale.is_finite() || raw_scale.abs() > 1000.0 { 0.0 } else { raw_scale };
        for i in 0..32 {
            let byte_idx = i / 2;
            let nibble = if i % 2 == 0 {
                (q4_data[off + 2 + byte_idx] & 0x0F) as i8 - 8
            } else {
                ((q4_data[off + 2 + byte_idx] >> 4) & 0x0F) as i8 - 8
            };
            let elem_idx = b * 32 + i;
            if elem_idx < dim {
                out[elem_idx] = scale * nibble as f32;
            }
        }
    }
}

/// Dequantize a single embedding row from Q8_0 data on-the-fly.
/// Q8_0 format: blocks of 32 elements, 34 bytes each (2 byte f16 scale + 32 byte int8 values).
fn dequant_embedding_row_q8(q8_data: &[u8], tok_idx: usize, dim: usize, out: &mut [f32]) {
    use super::matmul::f16_to_f32;
    let blocks_per_row = dim / 32;
    let bytes_per_row = blocks_per_row * 34;
    let row_start = tok_idx * bytes_per_row;

    if row_start + bytes_per_row > q8_data.len() {
        for v in out.iter_mut().take(dim) { *v = 0.0; }
        return;
    }

    for b in 0..blocks_per_row {
        let off = row_start + b * 34;
        let raw_scale = f16_to_f32(u16::from_le_bytes([q8_data[off], q8_data[off + 1]]));
        // Scale-level sanitization: corrupt GGUF blocks have |scale| >> 1.0 (e.g. -61408)
        let scale = if !raw_scale.is_finite() || raw_scale.abs() > 1000.0 { 0.0 } else { raw_scale };
        for i in 0..32 {
            let val = q8_data[off + 2 + i] as i8;
            let elem_idx = b * 32 + i;
            if elem_idx < dim {
                out[elem_idx] = scale * val as f32;
            }
        }
    }
}

/// Dispatch embedding row dequantization based on dtype.
/// is_q8: true for Q8_0, false for Q4_0.
fn dequant_embedding_row(raw_data: &[u8], tok_idx: usize, dim: usize, out: &mut [f32], is_q8: bool) {
    if is_q8 {
        dequant_embedding_row_q8(raw_data, tok_idx, dim, out);
    } else {
        dequant_embedding_row_q4(raw_data, tok_idx, dim, out);
    }
}

/// Compute logits = x × embedding_matrix^T using quantized data on-the-fly.
/// For each vocab token i, computes logits[i] = dot(x, embed_row[i]).
/// Dequantizes each row on-the-fly to avoid storing full f32 embedding.
/// Supports both Q4_0 (is_q8=false) and Q8_0 (is_q8=true) formats.
///
/// OPTIMIZED: 4x loop unrolling + scale-hoisted accumulation for ~2x speedup in TCG.
fn matmul_quant_logits(logits: &mut [f32], x: &[f32], raw_data: &[u8], dim: usize, vocab_size: usize, is_q8: bool) {
    use super::matmul::f16_to_f32;
    let blocks_per_row = dim / 32;
    let bytes_per_block: usize = if is_q8 { 34 } else { 18 };
    let bytes_per_row = blocks_per_row * bytes_per_block;

    for tok in 0..vocab_size {
        let row_start = tok * bytes_per_row;
        if row_start + bytes_per_row > raw_data.len() {
            logits[tok] = 0.0;
            continue;
        }

        let mut dot = 0.0f32;
        for b in 0..blocks_per_row {
            let off = row_start + b * bytes_per_block;
            let raw_scale = f16_to_f32(u16::from_le_bytes([raw_data[off], raw_data[off + 1]]));
            let scale = if !raw_scale.is_finite() || raw_scale.abs() > 1000.0 { 0.0 } else { raw_scale };
            if is_q8 {
                // Q8_0: 32 int8 values — 4x unrolled with scale-hoisted accumulation
                let base_idx = b * 32;
                let mut block_acc = 0.0f32;
                let mut i = 0;
                while i + 3 < 32 {
                    let v0 = raw_data[off + 2 + i] as i8;
                    let v1 = raw_data[off + 2 + i + 1] as i8;
                    let v2 = raw_data[off + 2 + i + 2] as i8;
                    let v3 = raw_data[off + 2 + i + 3] as i8;
                    block_acc += v0 as f32 * x[base_idx + i]
                              + v1 as f32 * x[base_idx + i + 1]
                              + v2 as f32 * x[base_idx + i + 2]
                              + v3 as f32 * x[base_idx + i + 3];
                    i += 4;
                }
                while i < 32 {
                    block_acc += raw_data[off + 2 + i] as i8 as f32 * x[base_idx + i];
                    i += 1;
                }
                dot += scale * block_acc;  // single multiply per block instead of per-element
            } else {
                // Q4_0: 16 nibble-pair bytes after 2-byte scale
                let base_idx = b * 32;
                let mut block_acc = 0.0f32;
                for i in 0..16 {
                    let byte_val = raw_data[off + 2 + i];
                    let lo = (byte_val & 0x0F) as i8 - 8;
                    let hi = ((byte_val >> 4) & 0x0F) as i8 - 8;
                    let idx = i * 2;
                    block_acc += lo as f32 * x[base_idx + idx]
                               + hi as f32 * x[base_idx + idx + 1];
                }
                dot += scale * block_acc;
            }
        }
        logits[tok] = if dot.is_finite() { dot } else { 0.0 };
    }
}

/// Fused argmax over quantized logits — computes dot products and tracks
/// the running maximum inline, without writing intermediate logits.
///
/// This is mathematically equivalent to:
///   matmul_quant_logits(logits, x, raw_data, dim, vocab_size, is_q8);
///   sample_greedy(logits)
///
/// Key insight: argmax(logits) = argmax(softmax(logits)), so greedy
/// sampling never needs softmax. And since we only need the argmax,
/// we don't need to store all 49152 logit values — just track the best.
///
/// Performance: ~2× faster than separate matmul+argmax because:
///   - Eliminates 49152 f32 writes + reads (saves ~384 KB memory traffic)
///   - 4x loop unrolling with scale-hoisted accumulation
///   - Progress logging every 8192 rows for TCG monitoring
///   - is_finite() guard on each dot product
pub fn argmax_quant_logits(x: &[f32], raw_data: &[u8], dim: usize, vocab_size: usize, is_q8: bool) -> (u32, f32) {
    use super::matmul::f16_to_f32;
    let blocks_per_row = dim / 32;
    let bytes_per_block: usize = if is_q8 { 34 } else { 18 };
    let bytes_per_row = blocks_per_row * bytes_per_block;

    let mut best_tok: u32 = 0;
    let mut best_val: f32 = f32::NEG_INFINITY;

    for tok in 0..vocab_size {
        let row_start = tok * bytes_per_row;
        if row_start + bytes_per_row > raw_data.len() {
            continue;
        }

        let mut dot = 0.0f32;
        if is_q8 {
            for b in 0..blocks_per_row {
                let off = row_start + b * 34;
                let raw_scale = f16_to_f32(u16::from_le_bytes([raw_data[off], raw_data[off + 1]]));
                let scale = if !raw_scale.is_finite() || raw_scale.abs() > 1000.0 { 0.0 } else { raw_scale };
                let base_idx = b * 32;
                let mut block_acc = 0.0f32;
                let mut i = 0;
                while i + 3 < 32 {
                    let v0 = raw_data[off + 2 + i] as i8;
                    let v1 = raw_data[off + 2 + i + 1] as i8;
                    let v2 = raw_data[off + 2 + i + 2] as i8;
                    let v3 = raw_data[off + 2 + i + 3] as i8;
                    block_acc += v0 as f32 * x[base_idx + i]
                              + v1 as f32 * x[base_idx + i + 1]
                              + v2 as f32 * x[base_idx + i + 2]
                              + v3 as f32 * x[base_idx + i + 3];
                    i += 4;
                }
                dot += scale * block_acc;
            }
        } else {
            for b in 0..blocks_per_row {
                let off = row_start + b * 18;
                let raw_scale = f16_to_f32(u16::from_le_bytes([raw_data[off], raw_data[off + 1]]));
                let scale = if !raw_scale.is_finite() || raw_scale.abs() > 1000.0 { 0.0 } else { raw_scale };
                let base_idx = b * 32;
                let mut block_acc = 0.0f32;
                for i in 0..16 {
                    let byte_val = raw_data[off + 2 + i];
                    let lo = (byte_val & 0x0F) as i8 - 8;
                    let hi = ((byte_val >> 4) & 0x0F) as i8 - 8;
                    let idx = i * 2;
                    block_acc += lo as f32 * x[base_idx + idx]
                               + hi as f32 * x[base_idx + idx + 1];
                }
                dot += scale * block_acc;
            }
        }

        if dot.is_finite() && dot > best_val {
            best_val = dot;
            best_tok = tok as u32;
        }

        // Progress logging for TCG debugging (every 8192 vocab entries)
        if tok & 8191 == 0 && tok > 0 {
            crate::serial_println!("[LOGIT-PROGRESS] {}/{} best_so_far=tok{} val_i64={}",
                tok, vocab_size, best_tok, best_val as i64);
        }
    }

    crate::serial_println!("[LOGIT-DONE] argmax=tok{} val_i64={} bits=0x{:08X}",
        best_tok, best_val as i64, best_val.to_bits());
    (best_tok, best_val)
}

/// Transformer forward pass for a single token at position `pos`.
///
/// This implements the full Llama-style architecture:
///   1. Token embedding lookup
///   2. For each layer:
///      a. RMSNorm pre-attention
///      b. Q/K/V projections
///      c. RoPE on Q and K
///      d. KV cache update
///      e. Grouped Multi-Query Attention (GQA)
///      f. Residual connection
///      g. RMSNorm pre-FFN
///      h. SwiGLU FFN (gate * up * silu)
///      i. Residual connection
///   3. Final RMSNorm
///   4. Logits projection
///
/// `weights` is a flat weight buffer. In production, offsets are computed from ModelConfig.
/// For self-test, we use dummy weights.
/// NaN/Inf checker — prints first bad value's index and value, returns count of non-finite elements.
/// Only logs for the FIRST call that detects a problem to avoid flooding serial output.
#[inline(never)]
fn check_nan(name: &str, v: &[f32]) -> usize {
    let mut bad_count = 0usize;
    let mut first_bad_idx = 0usize;
    let mut first_bad_val = 0.0f32;
    for (i, &x) in v.iter().enumerate() {
        if !x.is_finite() {
            if bad_count == 0 {
                first_bad_idx = i;
                first_bad_val = x;
            }
            bad_count += 1;
        }
    }
    if bad_count > 0 {
        crate::serial_println!("[NAN] {} — {}/{} non-finite, first at [{}] = {} bits=0x{:08X}",
            name, bad_count, v.len(), first_bad_idx,
            first_bad_val as i64, first_bad_val.to_bits());
    }
    bad_count
}

/// Quick stats for a vector — min, max, mean, and count of zeros
#[inline(never)]
fn check_stats(name: &str, v: &[f32]) {
    if v.is_empty() { return; }
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut sum = 0.0f64;
    let mut zeros = 0usize;
    for &x in v.iter() {
        if x < min { min = x; }
        if x > max { max = x; }
        sum += x as f64;
        if x == 0.0 { zeros += 1; }
    }
    let mean = sum / v.len() as f64;
    // Use to_bits() to show exact f32 representation (avoids `as i64` truncation for small values)
    crate::serial_println!("[STATS] {} — len={} min_bits=0x{:08X} max_bits=0x{:08X} mean_x1000={} zeros={}",
        name, v.len(), min.to_bits(), max.to_bits(), (mean * 1000.0) as i64, zeros);
}

pub fn forward(
    state: &mut TransformerState,
    token: u32,
    pos: usize,
    weights: &TransformerWeights,
) {
    let config = &state.config;
    let dim = config.dim;
    let kv_dim = config.kv_dim();
    let head_dim = config.head_dim();
    let n_heads = config.n_heads;
    let n_kv_heads = config.n_kv_heads;
    let n_rep = config.n_rep();
    // Only instrument first token to avoid flooding
    let instrument = pos == 0;

    // 1. Token embedding (supports Q8_0/Q4_0 on-the-fly dequant or f32 direct)
    let tok_idx = token as usize;
    if tok_idx < config.vocab_size {
        if !weights.token_embd_raw.is_empty() {
            // Quantized on-the-fly: dequantize only the row we need
            dequant_embedding_row(&weights.token_embd_raw, tok_idx, dim, &mut state.x, weights.token_embd_is_q8);

        } else if !weights.token_embedding.is_empty() {
            let emb_offset = tok_idx * dim;
            if emb_offset + dim <= weights.token_embedding.len() {
                state.x.copy_from_slice(&weights.token_embedding[emb_offset..emb_offset + dim]);
            }
        }
    }
    if instrument {
        check_nan("token_embd", &state.x);
        check_stats("token_embd", &state.x);
    }

    // 2. Transformer layers
    for layer in 0..config.n_layers {
        if instrument {
            crate::serial_println!("[DIAG] Enter Layer {}", layer);
        }

        // 2a. RMSNorm pre-attention
        state.xb.copy_from_slice(&state.x);
        let norm_w = weights.get_attn_norm(layer, dim);
        rmsnorm(&mut state.xb, norm_w, config.norm_eps);

        if instrument {
            crate::serial_println!("[DIAG] After RMSNorm {}", layer);
        }

        // 2b. Q/K/V projections (supports both f32 and zero-copy Q8_0)
        if weights.is_q8_mode() {
            use super::matmul::matmul_q8_0;
            matmul_q8_0(&mut state.q, &state.xb, weights.get_wq_q8(layer), dim, dim);
            matmul_q8_0(&mut state.k, &state.xb, weights.get_wk_q8(layer), dim, kv_dim);
            matmul_q8_0(&mut state.v, &state.xb, weights.get_wv_q8(layer), dim, kv_dim);
        } else {
            let wq = weights.get_wq(layer, dim);
            let wk = weights.get_wk(layer, dim, kv_dim);
            let wv = weights.get_wv(layer, dim, kv_dim);
            matmul_f32(&mut state.q, &state.xb, wq, dim, dim);
            matmul_f32(&mut state.k, &state.xb, wk, dim, kv_dim);
            matmul_f32(&mut state.v, &state.xb, wv, dim, kv_dim);
        }
        if instrument && layer == 0 {
            check_nan("L0.rmsnorm", &state.xb);
            check_nan("L0.q_proj", &state.q);
            check_nan("L0.k_proj", &state.k);
            check_nan("L0.v_proj", &state.v);
            check_stats("L0.rmsnorm", &state.xb);
            check_stats("L0.q_proj", &state.q);
        }
        if instrument {
            crate::serial_println!("[DIAG] After QKV {}", layer);
        }

        // 2c. RoPE on Q heads and K heads separately
        // First: apply RoPE to all K heads (fewer heads, do first)
        for kh in 0..n_kv_heads {
            let k_start = kh * head_dim;
            // apply_rope: pass K slice as both q and k; or use empty for q
            let k_slice = &mut state.k[k_start..k_start + head_dim];
            apply_rope(&mut [], k_slice, pos, head_dim, config.rope_theta);
        }
        // Then: apply RoPE to all Q heads
        for h in 0..n_heads {
            let q_start = h * head_dim;
            let q_slice = &mut state.q[q_start..q_start + head_dim];
            apply_rope(q_slice, &mut [], pos, head_dim, config.rope_theta);
        }

        // 2d. KV cache store
        state.kv_caches[layer].store(pos, &state.k, &state.v, kv_dim);

        // 2e. Grouped Multi-Query Attention
        let seq_len = pos + 1;
        for h in 0..n_heads {
            let kv_h = h / n_rep; // which KV head this query head maps to
            let q_start = h * head_dim;

            // Compute attention scores: att[t] = Q . K[t] / sqrt(head_dim)
            let scale = 1.0 / sqrt_head_dim(head_dim);
            let att_start = h * config.max_seq_len;
            for t in 0..seq_len {
                let k_offset = t * kv_dim + kv_h * head_dim;
                let mut score = 0.0f32;
                for d in 0..head_dim {
                    score += state.q[q_start + d] * state.kv_caches[layer].key[k_offset + d];
                }
                state.att[att_start + t] = score * scale;
            }

            // Softmax over attention scores
            softmax(&mut state.att[att_start..att_start + seq_len]);

            // Weighted sum of values
            let xb_start = h * head_dim;
            for d in 0..head_dim {
                let mut val = 0.0f32;
                for t in 0..seq_len {
                    let v_offset = t * kv_dim + kv_h * head_dim;
                    val += state.att[att_start + t] * state.kv_caches[layer].value[v_offset + d];
                }
                state.xb2[xb_start + d] = val;
            }
        }

        // Output projection: xb = Wo * xb2
        state.xb.fill(0.0);
        if weights.is_q8_mode() {
            use super::matmul::matmul_q8_0;
            matmul_q8_0(&mut state.xb, &state.xb2, weights.get_wo_q8(layer), dim, dim);
        } else {
            let wo = weights.get_wo(layer, dim);
            matmul_f32(&mut state.xb, &state.xb2, wo, dim, dim);
        }
        if instrument && layer == 0 {
            check_nan("L0.attn_out", &state.xb);
            check_stats("L0.attn_out", &state.xb);
        }
        if instrument {
            crate::serial_println!("[DIAG] After Attention {}", layer);
        }

        // Residual connection
        for i in 0..dim {
            state.x[i] += state.xb[i];
        }
        if instrument && layer == 0 {
            check_nan("L0.residual1", &state.x);
        }

        // 2g. RMSNorm pre-FFN
        state.xb.copy_from_slice(&state.x);
        let ffn_norm_w = weights.get_ffn_norm(layer, dim);
        rmsnorm(&mut state.xb, ffn_norm_w, config.norm_eps);

        // 2h. SwiGLU FFN
        if weights.is_q8_mode() {
            use super::matmul::matmul_q8_0;
            // gate = W1 * xb, up = W3 * xb
            matmul_q8_0(&mut state.hb, &state.xb, weights.get_w1_q8(layer), dim, config.hidden_dim);
            matmul_q8_0(&mut state.hb2, &state.xb, weights.get_w3_q8(layer), dim, config.hidden_dim);

            // SwiGLU: hb = silu(gate) * up
            for i in 0..config.hidden_dim {
                state.hb[i] = silu(state.hb[i]) * state.hb2[i];
            }
            if instrument && layer == 0 {
                check_nan("L0.ffn_gate_silu", &state.hb);
            }

            // down = W2 * hb
            state.xb.fill(0.0);
            matmul_q8_0(&mut state.xb, &state.hb, weights.get_w2_q8(layer), config.hidden_dim, dim);
        } else {
            let w1 = weights.get_w1(layer, dim, config.hidden_dim);
            let w2 = weights.get_w2(layer, dim, config.hidden_dim);
            let w3 = weights.get_w3(layer, dim, config.hidden_dim);

            // gate = W1 * xb, up = W3 * xb
            matmul_f32(&mut state.hb, &state.xb, w1, dim, config.hidden_dim);
            matmul_f32(&mut state.hb2, &state.xb, w3, dim, config.hidden_dim);

            // SwiGLU: hb = silu(gate) * up
            for i in 0..config.hidden_dim {
                state.hb[i] = silu(state.hb[i]) * state.hb2[i];
            }
            if instrument && layer == 0 {
                check_nan("L0.ffn_gate_silu", &state.hb);
            }

            // down = W2 * hb
            state.xb.fill(0.0);
            matmul_f32(&mut state.xb, &state.hb, w2, config.hidden_dim, dim);
        }
        if instrument && layer == 0 {
            check_nan("L0.ffn_down", &state.xb);
        }

        // Residual connection
        for i in 0..dim {
            state.x[i] += state.xb[i];
        }
        if instrument {
            crate::serial_println!("[DIAG] After FFN {}", layer);
        }
        if instrument && layer <= 1 {
            check_nan(&alloc::format!("L{}.residual2", layer), &state.x);
            check_stats(&alloc::format!("L{}.residual2", layer), &state.x);
        }
        // Per-layer progress for TCG monitoring
        crate::serial_println!("[FWD] Layer {}/{} done (pos={})", layer + 1, config.n_layers, pos);
    }

    // 3. Final RMSNorm
    if instrument {
        check_nan("pre_final_norm", &state.x);
        check_stats("pre_final_norm", &state.x);
    }
    rmsnorm(&mut state.x, &weights.final_norm, config.norm_eps);
    if instrument {
        check_nan("final_norm", &state.x);
        check_stats("final_norm", &state.x);
        // Also check final_norm weights
        check_nan("final_norm_weights", &weights.final_norm);
    }

    // 4. Logits projection (supports Q8_0/Q4_0 tied weights to save memory)
    if weights.tied_output && !weights.token_embd_raw.is_empty() {
        // Quantized on-the-fly: compute logits[i] = dot(x, embd_row[i])
        matmul_quant_logits(&mut state.logits, &state.x, &weights.token_embd_raw, dim, config.vocab_size, weights.token_embd_is_q8);
    } else if weights.tied_output && !weights.token_embedding.is_empty() {
        matmul_f32(&mut state.logits, &state.x, &weights.token_embedding, dim, config.vocab_size);
    } else if !weights.output_proj.is_empty() {
        matmul_f32(&mut state.logits, &state.x, &weights.output_proj, dim, config.vocab_size);
    }
    if instrument {
        check_nan("logits", &state.logits);
        // Show first 10 logit values
        let show = state.logits.len().min(10);
        for i in 0..show {
            crate::serial_println!("[LOGIT] [{}] = {} (bits=0x{:08X})",
                i, state.logits[i] as i64, state.logits[i].to_bits());
        }
        let max_l = state.logits.iter().cloned().fold(f32::NEG_INFINITY, |a, b| if b > a { b } else { a });
        let min_l = state.logits.iter().cloned().fold(f32::INFINITY, |a, b| if b < a { b } else { a });
        crate::serial_println!("[LOGIT] max_bits=0x{:08X} min_bits=0x{:08X} max_x1000={} min_x1000={}",
            max_l.to_bits(), min_l.to_bits(), (max_l as f64 * 1000.0) as i64, (min_l as f64 * 1000.0) as i64);
    }
}

/// Greedy forward pass: runs the transformer and returns the argmax token directly.
///
/// Identical to `forward()` for layers 0..n_layers + final RMSNorm, but replaces the
/// logits projection + sample_greedy() pair with a **fused argmax_quant_logits()** that:
///   - Never allocates or writes 49152 f32 logits
///   - Tracks running maximum inline (saves ~384 KB memory traffic)
///   - 4x loop-unrolled inner product with scale hoisting
///   - Reports progress every 8192 vocab entries
///
/// For non-quantized paths, falls back to standard matmul + sample_greedy.
///
/// Returns: (token_id, logit_value) of the greedy-best token.
pub fn forward_greedy(
    state: &mut TransformerState,
    token: u32,
    pos: usize,
    weights: &TransformerWeights,
) -> (u32, f32) {
    let config = &state.config;
    let dim = config.dim;
    let kv_dim = config.kv_dim();
    let head_dim = config.head_dim();
    let n_heads = config.n_heads;
    let n_kv_heads = config.n_kv_heads;
    let n_rep = config.n_rep();

    // 1. Token embedding
    let tok_idx = token as usize;
    if tok_idx < config.vocab_size {
        if !weights.token_embd_raw.is_empty() {
            dequant_embedding_row(&weights.token_embd_raw, tok_idx, dim, &mut state.x, weights.token_embd_is_q8);
        } else if !weights.token_embedding.is_empty() {
            let emb_offset = tok_idx * dim;
            if emb_offset + dim <= weights.token_embedding.len() {
                state.x.copy_from_slice(&weights.token_embedding[emb_offset..emb_offset + dim]);
            }
        }
    }

    // 2. Transformer layers
    for layer in 0..config.n_layers {
        // Pre-attention RMSNorm
        state.xb.copy_from_slice(&state.x);
        let norm_w = weights.get_attn_norm(layer, dim);
        rmsnorm(&mut state.xb, norm_w, config.norm_eps);

        // Q/K/V projections
        if weights.is_q8_mode() {
            use super::matmul::matmul_q8_0;
            matmul_q8_0(&mut state.q, &state.xb, weights.get_wq_q8(layer), dim, dim);
            matmul_q8_0(&mut state.k, &state.xb, weights.get_wk_q8(layer), dim, kv_dim);
            matmul_q8_0(&mut state.v, &state.xb, weights.get_wv_q8(layer), dim, kv_dim);
        } else {
            let wq = weights.get_wq(layer, dim);
            let wk = weights.get_wk(layer, dim, kv_dim);
            let wv = weights.get_wv(layer, dim, kv_dim);
            matmul_f32(&mut state.q, &state.xb, wq, dim, dim);
            matmul_f32(&mut state.k, &state.xb, wk, dim, kv_dim);
            matmul_f32(&mut state.v, &state.xb, wv, dim, kv_dim);
        }

        // RoPE on K heads then Q heads
        for kh in 0..n_kv_heads {
            let k_start = kh * head_dim;
            let k_slice = &mut state.k[k_start..k_start + head_dim];
            apply_rope(&mut [], k_slice, pos, head_dim, config.rope_theta);
        }
        for h in 0..n_heads {
            let q_start = h * head_dim;
            let q_slice = &mut state.q[q_start..q_start + head_dim];
            apply_rope(q_slice, &mut [], pos, head_dim, config.rope_theta);
        }

        // KV cache
        state.kv_caches[layer].store(pos, &state.k, &state.v, kv_dim);

        // GQA attention
        let seq_len = pos + 1;
        for h in 0..n_heads {
            let kv_h = h / n_rep;
            let q_start = h * head_dim;
            let scale = 1.0 / sqrt_head_dim(head_dim);
            let att_start = h * config.max_seq_len;
            for t in 0..seq_len {
                let k_offset = t * kv_dim + kv_h * head_dim;
                let mut score = 0.0f32;
                for d in 0..head_dim {
                    score += state.q[q_start + d] * state.kv_caches[layer].key[k_offset + d];
                }
                state.att[att_start + t] = score * scale;
            }
            softmax(&mut state.att[att_start..att_start + seq_len]);
            let xb_start = h * head_dim;
            for d in 0..head_dim {
                let mut val = 0.0f32;
                for t in 0..seq_len {
                    let v_offset = t * kv_dim + kv_h * head_dim;
                    val += state.att[att_start + t] * state.kv_caches[layer].value[v_offset + d];
                }
                state.xb2[xb_start + d] = val;
            }
        }

        // Output projection
        state.xb.fill(0.0);
        if weights.is_q8_mode() {
            use super::matmul::matmul_q8_0;
            matmul_q8_0(&mut state.xb, &state.xb2, weights.get_wo_q8(layer), dim, dim);
        } else {
            let wo = weights.get_wo(layer, dim);
            matmul_f32(&mut state.xb, &state.xb2, wo, dim, dim);
        }
        for i in 0..dim { state.x[i] += state.xb[i]; }

        // Pre-FFN RMSNorm + SwiGLU FFN
        state.xb.copy_from_slice(&state.x);
        let ffn_norm_w = weights.get_ffn_norm(layer, dim);
        rmsnorm(&mut state.xb, ffn_norm_w, config.norm_eps);

        if weights.is_q8_mode() {
            use super::matmul::matmul_q8_0;
            matmul_q8_0(&mut state.hb, &state.xb, weights.get_w1_q8(layer), dim, config.hidden_dim);
            matmul_q8_0(&mut state.hb2, &state.xb, weights.get_w3_q8(layer), dim, config.hidden_dim);
            for i in 0..config.hidden_dim {
                state.hb[i] = silu(state.hb[i]) * state.hb2[i];
            }
            state.xb.fill(0.0);
            matmul_q8_0(&mut state.xb, &state.hb, weights.get_w2_q8(layer), config.hidden_dim, dim);
        } else {
            let w1 = weights.get_w1(layer, dim, config.hidden_dim);
            let w2 = weights.get_w2(layer, dim, config.hidden_dim);
            let w3 = weights.get_w3(layer, dim, config.hidden_dim);
            matmul_f32(&mut state.hb, &state.xb, w1, dim, config.hidden_dim);
            matmul_f32(&mut state.hb2, &state.xb, w3, dim, config.hidden_dim);
            for i in 0..config.hidden_dim {
                state.hb[i] = silu(state.hb[i]) * state.hb2[i];
            }
            state.xb.fill(0.0);
            matmul_f32(&mut state.xb, &state.hb, w2, config.hidden_dim, dim);
        }
        for i in 0..dim { state.x[i] += state.xb[i]; }

        // Per-layer progress (important for TCG monitoring)
        if layer % 5 == 4 || layer == config.n_layers - 1 {
            crate::serial_println!("[FWD-GREEDY] Layer {}/{} done", layer + 1, config.n_layers);
        }
    }

    // 3. Final RMSNorm
    rmsnorm(&mut state.x, &weights.final_norm, config.norm_eps);

    // 4. Fused argmax logits projection (THE TCG OPTIMIZATION)
    //    Instead of computing 49152 logits then scanning for max,
    //    we compute each dot product and track the running maximum inline.
    if weights.tied_output && !weights.token_embd_raw.is_empty() {
        crate::serial_println!("[FWD-GREEDY] Starting fused argmax over {} vocab (dim={})", config.vocab_size, dim);
        let (tok, val) = argmax_quant_logits(&state.x, &weights.token_embd_raw, dim, config.vocab_size, weights.token_embd_is_q8);
        return (tok, val);
    } else if weights.tied_output && !weights.token_embedding.is_empty() {
        matmul_f32(&mut state.logits, &state.x, &weights.token_embedding, dim, config.vocab_size);
    } else if !weights.output_proj.is_empty() {
        matmul_f32(&mut state.logits, &state.x, &weights.output_proj, dim, config.vocab_size);
    }

    // Fallback: standard argmax on computed logits
    let tok = sample_greedy(&state.logits);
    let val = if (tok as usize) < state.logits.len() { state.logits[tok as usize] } else { 0.0 };
    (tok, val)
}

/// Prefill a single prompt token: run the full transformer stack to update the
/// KV cache at `pos`, WITHOUT projecting to the vocabulary or sampling.
///
/// This is `forward_greedy()` minus the final RMSNorm + logits/argmax stage.
/// During prompt ingestion we only care about populating the KV cache for every
/// position except the last one (the last prompt token goes through
/// `forward_greedy()` so we get the first generated token). Skipping the
/// ~49k-wide vocab projection on each prefill position is a large saving — on
/// the bare-metal i3 Gen11 (KVM + AVX2) an 8-token prefill drops from "hours of
/// TCG" to a few seconds.
///
/// The updated state lives in `state.kv_caches[..]`; this function returns `()`.
pub fn forward_prefill(
    state: &mut TransformerState,
    token: u32,
    pos: usize,
    weights: &TransformerWeights,
) {
    let config = &state.config;
    let dim = config.dim;
    let kv_dim = config.kv_dim();
    let head_dim = config.head_dim();
    let n_heads = config.n_heads;
    let n_kv_heads = config.n_kv_heads;
    let n_rep = config.n_rep();

    // 1. Token embedding
    let tok_idx = token as usize;
    if tok_idx < config.vocab_size {
        if !weights.token_embd_raw.is_empty() {
            dequant_embedding_row(&weights.token_embd_raw, tok_idx, dim, &mut state.x, weights.token_embd_is_q8);
        } else if !weights.token_embedding.is_empty() {
            let emb_offset = tok_idx * dim;
            if emb_offset + dim <= weights.token_embedding.len() {
                state.x.copy_from_slice(&weights.token_embedding[emb_offset..emb_offset + dim]);
            }
        }
    }

    // 2. Transformer layers (identical to forward_greedy)
    for layer in 0..config.n_layers {
        // Pre-attention RMSNorm
        state.xb.copy_from_slice(&state.x);
        let norm_w = weights.get_attn_norm(layer, dim);
        rmsnorm(&mut state.xb, norm_w, config.norm_eps);

        // Q/K/V projections
        if weights.is_q8_mode() {
            use super::matmul::matmul_q8_0;
            matmul_q8_0(&mut state.q, &state.xb, weights.get_wq_q8(layer), dim, dim);
            matmul_q8_0(&mut state.k, &state.xb, weights.get_wk_q8(layer), dim, kv_dim);
            matmul_q8_0(&mut state.v, &state.xb, weights.get_wv_q8(layer), dim, kv_dim);
        } else {
            let wq = weights.get_wq(layer, dim);
            let wk = weights.get_wk(layer, dim, kv_dim);
            let wv = weights.get_wv(layer, dim, kv_dim);
            matmul_f32(&mut state.q, &state.xb, wq, dim, dim);
            matmul_f32(&mut state.k, &state.xb, wk, dim, kv_dim);
            matmul_f32(&mut state.v, &state.xb, wv, dim, kv_dim);
        }

        // RoPE on K heads then Q heads
        for kh in 0..n_kv_heads {
            let k_start = kh * head_dim;
            let k_slice = &mut state.k[k_start..k_start + head_dim];
            apply_rope(&mut [], k_slice, pos, head_dim, config.rope_theta);
        }
        for h in 0..n_heads {
            let q_start = h * head_dim;
            let q_slice = &mut state.q[q_start..q_start + head_dim];
            apply_rope(q_slice, &mut [], pos, head_dim, config.rope_theta);
        }

        // KV cache store — THE point of prefill
        state.kv_caches[layer].store(pos, &state.k, &state.v, kv_dim);

        // GQA attention
        let seq_len = pos + 1;
        for h in 0..n_heads {
            let kv_h = h / n_rep;
            let q_start = h * head_dim;
            let scale = 1.0 / sqrt_head_dim(head_dim);
            let att_start = h * config.max_seq_len;
            for t in 0..seq_len {
                let k_offset = t * kv_dim + kv_h * head_dim;
                let mut score = 0.0f32;
                for d in 0..head_dim {
                    score += state.q[q_start + d] * state.kv_caches[layer].key[k_offset + d];
                }
                state.att[att_start + t] = score * scale;
            }
            softmax(&mut state.att[att_start..att_start + seq_len]);
            let xb_start = h * head_dim;
            for d in 0..head_dim {
                let mut val = 0.0f32;
                for t in 0..seq_len {
                    let v_offset = t * kv_dim + kv_h * head_dim;
                    val += state.att[att_start + t] * state.kv_caches[layer].value[v_offset + d];
                }
                state.xb2[xb_start + d] = val;
            }
        }

        // Output projection
        state.xb.fill(0.0);
        if weights.is_q8_mode() {
            use super::matmul::matmul_q8_0;
            matmul_q8_0(&mut state.xb, &state.xb2, weights.get_wo_q8(layer), dim, dim);
        } else {
            let wo = weights.get_wo(layer, dim);
            matmul_f32(&mut state.xb, &state.xb2, wo, dim, dim);
        }
        for i in 0..dim { state.x[i] += state.xb[i]; }

        // Pre-FFN RMSNorm + SwiGLU FFN
        state.xb.copy_from_slice(&state.x);
        let ffn_norm_w = weights.get_ffn_norm(layer, dim);
        rmsnorm(&mut state.xb, ffn_norm_w, config.norm_eps);

        if weights.is_q8_mode() {
            use super::matmul::matmul_q8_0;
            matmul_q8_0(&mut state.hb, &state.xb, weights.get_w1_q8(layer), dim, config.hidden_dim);
            matmul_q8_0(&mut state.hb2, &state.xb, weights.get_w3_q8(layer), dim, config.hidden_dim);
            for i in 0..config.hidden_dim {
                state.hb[i] = silu(state.hb[i]) * state.hb2[i];
            }
            state.xb.fill(0.0);
            matmul_q8_0(&mut state.xb, &state.hb, weights.get_w2_q8(layer), config.hidden_dim, dim);
        } else {
            let w1 = weights.get_w1(layer, dim, config.hidden_dim);
            let w2 = weights.get_w2(layer, dim, config.hidden_dim);
            let w3 = weights.get_w3(layer, dim, config.hidden_dim);
            matmul_f32(&mut state.hb, &state.xb, w1, dim, config.hidden_dim);
            matmul_f32(&mut state.hb2, &state.xb, w3, dim, config.hidden_dim);
            for i in 0..config.hidden_dim {
                state.hb[i] = silu(state.hb[i]) * state.hb2[i];
            }
            state.xb.fill(0.0);
            matmul_f32(&mut state.xb, &state.hb, w2, config.hidden_dim, dim);
        }
        for i in 0..dim { state.x[i] += state.xb[i]; }
    }

    // NOTE: deliberately NO final RMSNorm and NO logits/argmax projection.
    // The only product of prefill is the updated KV cache (state.kv_caches).
    crate::serial_println!("[FWD-PREFILL] pos={} cached (KV updated, no logits)", pos);
}

/// Transformer weights container
/// Supports two modes:
/// 1. **f32 mode** (legacy/testing): layer_weights contains dequantized f32 data
/// 2. **Zero-copy Q8_0 mode** (production): layer_weights_q8 contains raw Q8_0 data,
///    and forward() uses matmul_q8_0 directly — saves 4× memory vs f32.
///
/// Norm weights (attn_norm, ffn_norm) are always f32 since they're tiny (dim elements).
pub struct TransformerWeights {
    pub token_embedding: Vec<f32>,
    pub final_norm: Vec<f32>,
    pub output_proj: Vec<f32>,
    // Per-layer weights packed sequentially (f32 mode — used for dummy/test)
    pub layer_weights: Vec<f32>,
    // Per-layer Q8_0 raw weights packed sequentially (zero-copy mode)
    // Layout per layer: [attn_q | attn_k | attn_v | attn_out | ffn_up | ffn_down | ffn_gate]
    // Each tensor stored as raw Q8_0 blocks (34 bytes per block of 32 elements)
    pub layer_weights_q8: Vec<u8>,
    // Per-layer f32 norm weights: [attn_norm(dim) | ffn_norm(dim)] per layer
    pub layer_norms: Vec<f32>,
    // Config for offset calculation
    pub(crate) dim: usize,
    pub(crate) hidden_dim: usize,
    pub(crate) kv_dim: usize,
    pub(crate) n_layers: usize,
    /// When true, output_proj is empty and token_embedding should be used for logits.
    pub tied_output: bool,
    /// Raw quantized data for token embedding (avoids 112 MB f32 dequant).
    pub token_embd_raw: Vec<u8>,
    /// True if token_embd_raw is Q8_0 format (34 bytes/block), false for Q4_0 (18 bytes/block).
    pub token_embd_is_q8: bool,
    /// vocab_size for quantized embedding lookup
    pub vocab_size: usize,
}

impl TransformerWeights {
    /// Create dummy weights for testing (all small random-ish values)
    pub fn dummy(config: &ModelConfig) -> Self {
        let dim = config.dim;
        let hidden_dim = config.hidden_dim;
        let kv_dim = config.kv_dim();
        let vocab_size = config.vocab_size;
        let n_layers = config.n_layers;

        // Per-layer weight sizes:
        // attn_norm: dim
        // wq: dim*dim, wk: dim*kv_dim, wv: dim*kv_dim, wo: dim*dim
        // ffn_norm: dim
        // w1: dim*hidden_dim, w2: hidden_dim*dim, w3: dim*hidden_dim
        let per_layer = dim + dim * dim + dim * kv_dim + dim * kv_dim + dim * dim
            + dim + dim * hidden_dim + hidden_dim * dim + dim * hidden_dim;
        let total_layer = per_layer * n_layers;

        // Use a simple PRNG to fill weights with small values
        let mut rng = 12345u32;
        let mut rand_f32 = || -> f32 {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            ((rng as f32) / 4294967296.0 - 0.5) * 0.02
        };

        let token_embedding: Vec<f32> = (0..vocab_size * dim).map(|_| rand_f32()).collect();
        let final_norm: Vec<f32> = (0..dim).map(|_| 1.0 + rand_f32() * 0.1).collect();
        let output_proj: Vec<f32> = (0..dim * vocab_size).map(|_| rand_f32()).collect();
        let layer_weights: Vec<f32> = (0..total_layer).map(|_| rand_f32()).collect();

        Self {
            token_embedding,
            final_norm,
            output_proj,
            layer_weights,
            layer_weights_q8: Vec::new(),
            layer_norms: Vec::new(),
            dim,
            hidden_dim,
            kv_dim,
            n_layers,
            tied_output: false,
            token_embd_raw: Vec::new(),
            token_embd_is_q8: false,
            vocab_size: config.vocab_size,
        }
    }

    /// Check if this weights container uses zero-copy Q8_0 mode
    pub fn is_q8_mode(&self) -> bool {
        !self.layer_weights_q8.is_empty()
    }

    fn per_layer_size(&self) -> usize {
        self.dim + self.dim * self.dim + self.dim * self.kv_dim + self.dim * self.kv_dim
            + self.dim * self.dim + self.dim + self.dim * self.hidden_dim
            + self.hidden_dim * self.dim + self.dim * self.hidden_dim
    }

    fn layer_offset(&self, layer: usize) -> usize {
        layer * self.per_layer_size()
    }

    pub fn get_attn_norm(&self, layer: usize, dim: usize) -> &[f32] {
        if !self.layer_norms.is_empty() {
            // Zero-copy mode: norms stored in layer_norms [attn_norm | ffn_norm] per layer
            let base = layer * 2 * dim;
            return &self.layer_norms[base..base + dim];
        }
        let base = self.layer_offset(layer);
        &self.layer_weights[base..base + dim]
    }

    pub fn get_wq(&self, layer: usize, dim: usize) -> &[f32] {
        let base = self.layer_offset(layer) + dim;
        &self.layer_weights[base..base + dim * dim]
    }

    pub fn get_wk(&self, layer: usize, dim: usize, kv_dim: usize) -> &[f32] {
        let base = self.layer_offset(layer) + dim + dim * dim;
        &self.layer_weights[base..base + dim * kv_dim]
    }

    pub fn get_wv(&self, layer: usize, dim: usize, kv_dim: usize) -> &[f32] {
        let base = self.layer_offset(layer) + dim + dim * dim + dim * kv_dim;
        &self.layer_weights[base..base + dim * kv_dim]
    }

    pub fn get_wo(&self, layer: usize, dim: usize) -> &[f32] {
        let base = self.layer_offset(layer) + dim + dim * dim + 2 * dim * self.kv_dim;
        &self.layer_weights[base..base + dim * dim]
    }

    pub fn get_ffn_norm(&self, layer: usize, dim: usize) -> &[f32] {
        if !self.layer_norms.is_empty() {
            // Zero-copy mode: norms stored in layer_norms [attn_norm | ffn_norm] per layer
            let base = layer * 2 * dim + dim;
            return &self.layer_norms[base..base + dim];
        }
        let base = self.layer_offset(layer) + dim + 2 * dim * dim + 2 * dim * self.kv_dim;
        &self.layer_weights[base..base + dim]
    }

    pub fn get_w1(&self, layer: usize, dim: usize, hidden_dim: usize) -> &[f32] {
        let base = self.layer_offset(layer) + 2 * dim + 2 * dim * dim + 2 * dim * self.kv_dim;
        &self.layer_weights[base..base + dim * hidden_dim]
    }

    pub fn get_w2(&self, layer: usize, dim: usize, hidden_dim: usize) -> &[f32] {
        let base = self.layer_offset(layer) + 2 * dim + 2 * dim * dim + 2 * dim * self.kv_dim
            + dim * hidden_dim;
        &self.layer_weights[base..base + hidden_dim * dim]
    }

    pub fn get_w3(&self, layer: usize, dim: usize, hidden_dim: usize) -> &[f32] {
        let base = self.layer_offset(layer) + 2 * dim + 2 * dim * dim + 2 * dim * self.kv_dim
            + dim * hidden_dim + hidden_dim * dim;
        &self.layer_weights[base..base + dim * hidden_dim]
    }

    // ═══════════════════════════════════════════════════════════
    // Zero-copy Q8_0 accessors — return raw Q8_0 byte slices
    // ═══════════════════════════════════════════════════════════
    // Q8_0 layout per layer in layer_weights_q8:
    //   [wq | wk | wv | wo | w1 | w2 | w3]
    //   Each tensor is (n_elements / 32) * 34 bytes
    // Norms (attn_norm, ffn_norm) are stored separately in layer_norms (f32).

    fn q8_bytes_for(n_elements: usize) -> usize {
        (n_elements / 32) * 34
    }

    fn q8_per_layer_bytes(&self) -> usize {
        let dim = self.dim;
        let kv_dim = self.kv_dim;
        let hidden_dim = self.hidden_dim;
        // wq: dim*dim, wk: dim*kv_dim, wv: dim*kv_dim, wo: dim*dim
        // w1: dim*hidden_dim, w2: hidden_dim*dim, w3: dim*hidden_dim
        Self::q8_bytes_for(dim * dim)        // wq
        + Self::q8_bytes_for(dim * kv_dim)   // wk
        + Self::q8_bytes_for(dim * kv_dim)   // wv
        + Self::q8_bytes_for(dim * dim)      // wo
        + Self::q8_bytes_for(dim * hidden_dim) // w1
        + Self::q8_bytes_for(hidden_dim * dim) // w2
        + Self::q8_bytes_for(dim * hidden_dim) // w3
    }

    pub fn get_wq_q8(&self, layer: usize) -> &[u8] {
        let per = self.q8_per_layer_bytes();
        let base = layer * per;
        let size = Self::q8_bytes_for(self.dim * self.dim);
        &self.layer_weights_q8[base..base + size]
    }

    pub fn get_wk_q8(&self, layer: usize) -> &[u8] {
        let per = self.q8_per_layer_bytes();
        let base = layer * per + Self::q8_bytes_for(self.dim * self.dim);
        let size = Self::q8_bytes_for(self.dim * self.kv_dim);
        &self.layer_weights_q8[base..base + size]
    }

    pub fn get_wv_q8(&self, layer: usize) -> &[u8] {
        let per = self.q8_per_layer_bytes();
        let base = layer * per
            + Self::q8_bytes_for(self.dim * self.dim)
            + Self::q8_bytes_for(self.dim * self.kv_dim);
        let size = Self::q8_bytes_for(self.dim * self.kv_dim);
        &self.layer_weights_q8[base..base + size]
    }

    pub fn get_wo_q8(&self, layer: usize) -> &[u8] {
        let per = self.q8_per_layer_bytes();
        let base = layer * per
            + Self::q8_bytes_for(self.dim * self.dim)
            + 2 * Self::q8_bytes_for(self.dim * self.kv_dim);
        let size = Self::q8_bytes_for(self.dim * self.dim);
        &self.layer_weights_q8[base..base + size]
    }

    pub fn get_w1_q8(&self, layer: usize) -> &[u8] {
        let per = self.q8_per_layer_bytes();
        let base = layer * per
            + 2 * Self::q8_bytes_for(self.dim * self.dim)
            + 2 * Self::q8_bytes_for(self.dim * self.kv_dim);
        let size = Self::q8_bytes_for(self.dim * self.hidden_dim);
        &self.layer_weights_q8[base..base + size]
    }

    pub fn get_w2_q8(&self, layer: usize) -> &[u8] {
        let per = self.q8_per_layer_bytes();
        let base = layer * per
            + 2 * Self::q8_bytes_for(self.dim * self.dim)
            + 2 * Self::q8_bytes_for(self.dim * self.kv_dim)
            + Self::q8_bytes_for(self.dim * self.hidden_dim);
        let size = Self::q8_bytes_for(self.hidden_dim * self.dim);
        &self.layer_weights_q8[base..base + size]
    }

    pub fn get_w3_q8(&self, layer: usize) -> &[u8] {
        let per = self.q8_per_layer_bytes();
        let base = layer * per
            + 2 * Self::q8_bytes_for(self.dim * self.dim)
            + 2 * Self::q8_bytes_for(self.dim * self.kv_dim)
            + Self::q8_bytes_for(self.dim * self.hidden_dim)
            + Self::q8_bytes_for(self.hidden_dim * self.dim);
        let size = Self::q8_bytes_for(self.dim * self.hidden_dim);
        &self.layer_weights_q8[base..base + size]
    }

    /// Load real weights from a parsed GGUF model.
    /// Dequantizes Q4_0/Q8_0/F16 blocks on-the-fly.
    /// `n_layers_limit`: max layers to load (to fit in memory under QEMU).
    /// `file_data` must contain the full GGUF file (including tensor data region).
    pub fn from_gguf(
        model: &super::gguf::GgufModel,
        config: &ModelConfig,
        file_data: &[u8],
        n_layers_limit: usize,
    ) -> Option<Self> {
        use super::matmul::dequant_q4_0;
        use super::gguf::GgmlType;
        use super::matmul::f16_to_f32;

        let dim = config.dim;
        let hidden_dim = config.hidden_dim;
        let kv_dim = config.kv_dim();
        let vocab_size = config.vocab_size;
        let n_layers = n_layers_limit.min(config.n_layers);

        crate::serial_println!("[LLM] Loading weights: dim={}, hidden={}, layers={}/{}, vocab={}, kv_dim={}",
            dim, hidden_dim, n_layers, config.n_layers, vocab_size, kv_dim);

        // Helper: dequantize a tensor to f32 vec
        let dequant_tensor = |name: &str, expected: usize| -> Option<Vec<f32>> {
            let info = model.tensors.get(name)?;
            let data = model.tensor_data(name, file_data)?;
            let n_elem = info.n_elements() as usize;
            match info.dtype {
                GgmlType::F32 => {
                    let mut out = Vec::with_capacity(n_elem);
                    for i in 0..n_elem {
                        let off = i * 4;
                        if off + 4 > data.len() { break; }
                        let v = f32::from_le_bytes([data[off], data[off+1], data[off+2], data[off+3]]); out.push(if v.is_finite() { v } else { 0.0 });
                    }
                    Some(out)
                },
                GgmlType::F16 => {
                    let mut out = Vec::with_capacity(n_elem);
                    for i in 0..n_elem {
                        let off = i * 2;
                        if off + 2 > data.len() { break; }
                        out.push(f16_to_f32(u16::from_le_bytes([data[off], data[off+1]])));
                    }
                    Some(out)
                },
                GgmlType::Q4_0 => {
                    Some(dequant_q4_0(data, n_elem))
                },
                GgmlType::Q8_0 => {
                    // Q8_0: blocks of 32 elements, 2 bytes scale (f16) + 32 bytes data (int8)
                    let block_size = 32;
                    let bytes_per_block = 34;
                    let n_blocks = n_elem / block_size;
                    let mut out = vec![0.0f32; n_elem];
                    for b in 0..n_blocks {
                        let off = b * bytes_per_block;
                        if off + bytes_per_block > data.len() { break; }
                        let raw_scale = f16_to_f32(u16::from_le_bytes([data[off], data[off+1]]));
                        // Scale-level sanitization: aberrant scales (exp=30, |val|>1) are corrupted GGUF blocks
                        let scale = if raw_scale.is_nan() || raw_scale.is_infinite() || raw_scale.abs() > 1.0 { 0.0 } else { raw_scale };
                        for i in 0..32 {
                            let val = data[off + 2 + i] as i8;
                            let idx = b * block_size + i;
                            if idx < n_elem {
                                out[idx] = scale * val as f32;
                            }
                        }
                    }
                    Some(out)
                },
                _other => {
                    // Return zeros as fallback for unsupported types
                    Some(vec![0.0f32; expected])
                }
            }
        };

        // Load token embedding as RAW quantized data (saves 112 MB vs f32 dequant)
        // Supports both Q4_0 (18 bytes/block) and Q8_0 (34 bytes/block)
        crate::serial_write("[LLM] Loading token_embd (raw quantized)...\n");
        let mut token_embd_raw = Vec::new();
        let mut token_embd_is_q8 = false;
        let token_embedding = Vec::new(); // empty — we use quantized on-the-fly
        if let Some(info) = model.tensors.get("token_embd.weight") {
            token_embd_is_q8 = info.dtype == GgmlType::Q8_0;
            if let Some(data) = model.tensor_data("token_embd.weight", file_data) {
                token_embd_raw = data.to_vec();
                crate::serial_println!("[LLM] token_embd: {} bytes raw {:?} (is_q8={})",
                    token_embd_raw.len(), info.dtype, token_embd_is_q8);
            } else {
                crate::serial_write("[LLM] WARN: token_embd.weight data not found\n");
            }
        } else {
            crate::serial_write("[LLM] WARN: token_embd.weight tensor not found\n");
        }

        // Load final norm
        let final_norm = dequant_tensor("output_norm.weight", dim)
            .unwrap_or_else(|| vec![1.0f32; dim]);

        // Output projection: always tied in 64 MB heap mode (no room for 112 MB f32)
        let output_proj = Vec::new();
        let tied = true;
        let dtype_str = if token_embd_is_q8 { "Q8_0" } else { "Q4_0" };
        crate::serial_println!("[LLM] output tied to token_embd {} (saves 112 MB)", dtype_str);

        // Per-layer weight sizes
        let per_layer = dim + dim * dim + dim * kv_dim + dim * kv_dim + dim * dim
            + dim + dim * hidden_dim + hidden_dim * dim + dim * hidden_dim;
        let total_layer = per_layer * n_layers;

        crate::serial_println!("[LLM] Allocating layer weights: {} floats ({} MB)",
            total_layer, total_layer * 4 / 1024 / 1024);

        let mut layer_weights = vec![0.0f32; total_layer];

        for l in 0..n_layers {
            let base = l * per_layer;

            // Attention norm
            let name = alloc::format!("blk.{}.attn_norm.weight", l);
            if let Some(w) = dequant_tensor(&name, dim) {
                let copy_len = w.len().min(dim);
                layer_weights[base..base + copy_len].copy_from_slice(&w[..copy_len]);
            }

            // Q, K, V, O projections
            let name = alloc::format!("blk.{}.attn_q.weight", l);
            let wq_off = base + dim;
            if let Some(w) = dequant_tensor(&name, dim * dim) {
                let copy_len = w.len().min(dim * dim);
                layer_weights[wq_off..wq_off + copy_len].copy_from_slice(&w[..copy_len]);
            }

            let name = alloc::format!("blk.{}.attn_k.weight", l);
            let wk_off = wq_off + dim * dim;
            if let Some(w) = dequant_tensor(&name, dim * kv_dim) {
                let copy_len = w.len().min(dim * kv_dim);
                layer_weights[wk_off..wk_off + copy_len].copy_from_slice(&w[..copy_len]);
            }

            let name = alloc::format!("blk.{}.attn_v.weight", l);
            let wv_off = wk_off + dim * kv_dim;
            if let Some(w) = dequant_tensor(&name, dim * kv_dim) {
                let copy_len = w.len().min(dim * kv_dim);
                layer_weights[wv_off..wv_off + copy_len].copy_from_slice(&w[..copy_len]);
            }

            let name = alloc::format!("blk.{}.attn_output.weight", l);
            let wo_off = wv_off + dim * kv_dim;
            if let Some(w) = dequant_tensor(&name, dim * dim) {
                let copy_len = w.len().min(dim * dim);
                layer_weights[wo_off..wo_off + copy_len].copy_from_slice(&w[..copy_len]);
            }

            // FFN norm + gate + down + up
            let name = alloc::format!("blk.{}.ffn_norm.weight", l);
            let ffn_norm_off = wo_off + dim * dim;
            if let Some(w) = dequant_tensor(&name, dim) {
                let copy_len = w.len().min(dim);
                layer_weights[ffn_norm_off..ffn_norm_off + copy_len].copy_from_slice(&w[..copy_len]);
            }

            let name = alloc::format!("blk.{}.ffn_gate.weight", l);
            let w1_off = ffn_norm_off + dim;
            if let Some(w) = dequant_tensor(&name, dim * hidden_dim) {
                let copy_len = w.len().min(dim * hidden_dim);
                layer_weights[w1_off..w1_off + copy_len].copy_from_slice(&w[..copy_len]);
            }

            let name = alloc::format!("blk.{}.ffn_down.weight", l);
            let w2_off = w1_off + dim * hidden_dim;
            if let Some(w) = dequant_tensor(&name, hidden_dim * dim) {
                let copy_len = w.len().min(hidden_dim * dim);
                layer_weights[w2_off..w2_off + copy_len].copy_from_slice(&w[..copy_len]);
            }

            let name = alloc::format!("blk.{}.ffn_up.weight", l);
            let w3_off = w2_off + hidden_dim * dim;
            if let Some(w) = dequant_tensor(&name, dim * hidden_dim) {
                let copy_len = w.len().min(dim * hidden_dim);
                layer_weights[w3_off..w3_off + copy_len].copy_from_slice(&w[..copy_len]);
            }

            if l == 0 || l == n_layers - 1 {
                crate::serial_println!("[LLM] Layer {} loaded", l);
            }
        }

        crate::serial_println!("[LLM] All {} layers loaded", n_layers);

        Some(Self {
            token_embedding,
            final_norm,
            output_proj,
            layer_weights,
            layer_weights_q8: Vec::new(), // from_gguf loads f32 mode
            layer_norms: Vec::new(),       // from_gguf uses norms in layer_weights
            dim,
            hidden_dim,
            kv_dim,
            n_layers,
            tied_output: tied,
            token_embd_raw,
            token_embd_is_q8,
            vocab_size,
        })
    }
}

/// (head_dim as f32).sqrt() — fast integer sqrt for power-of-2 head dims
fn sqrt_head_dim(hd: usize) -> f32 {
    match hd {
        16 => 4.0,
        64 => 8.0,
        128 => 11.313708,
        32 => 5.656854,
        _ => {
            // fallback
            let x = hd as f32;
            let mut y = x * 0.5;
            for _ in 0..8 {
                y = (y + x / y) * 0.5;
            }
            y
        }
    }
}

/// Simple greedy token sampler
pub fn sample_greedy(logits: &[f32]) -> u32 {
    let mut max_val = f32::NEG_INFINITY;
    let mut max_idx = 0u32;
    for (i, &v) in logits.iter().enumerate() {
        if v > max_val {
            max_val = v;
            max_idx = i as u32;
        }
    }
    max_idx
}

/// Top-K sampling with temperature
pub fn sample_top_k(logits: &mut [f32], k: usize, temperature: f32) -> u32 {
    // Apply temperature
    if temperature != 1.0 && temperature > 0.0 {
        let inv_t = 1.0 / temperature;
        for v in logits.iter_mut() {
            *v *= inv_t;
        }
    }

    // Find top-K indices
    let mut indices: Vec<(usize, f32)> = logits.iter().enumerate()
        .map(|(i, &v)| (i, v))
        .collect();
    indices.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(core::cmp::Ordering::Equal));
    indices.truncate(k);

    // Softmax over top-K
    let max_val = indices[0].1;
    let mut probs: Vec<f32> = indices.iter().map(|(_, v)| {
        let e = v - max_val;
        if e < -88.0 { 0.0 } else { super::matmul::exp_f32_pub(e) }
    }).collect();
    let sum: f32 = probs.iter().sum();
    if sum > 0.0 {
        for p in &mut probs { *p /= sum; }
    }

    // Simple random sampling using TSC
    let tsc = unsafe {
        let lo: u32;
        let hi: u32;
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nostack));
        ((hi as u64) << 32 | lo as u64) as u32
    };
    let r = (tsc as f32 % 1000.0) / 1000.0;

    let mut cumulative = 0.0f32;
    for (i, &p) in probs.iter().enumerate() {
        cumulative += p;
        if r < cumulative {
            return indices[i].0 as u32;
        }
    }

    indices[0].0 as u32
}

/// Minimal BPE tokenizer for SmolLM2 (byte-level fallback)
pub struct SimpleTokenizer {
    pub bos_id: u32,
    pub eos_id: u32,
}

impl SimpleTokenizer {
    pub fn new() -> Self {
        Self {
            bos_id: 1,
            eos_id: 2,
        }
    }

    /// Encode a string to token IDs (byte-level fallback)
    pub fn encode(&self, text: &str) -> Vec<u32> {
        // SmolLM2 has no valid BOS token (tokens 0-5 are all-zero reserved).
        // Skip BOS to avoid feeding zero-embedding as first input.
        let mut tokens = Vec::new();
        for &b in text.as_bytes() {
            tokens.push(b as u32 + 3);
        }
        tokens
    }

    /// Decode a token ID back to a string fragment
    pub fn decode_token(&self, token: u32) -> Option<u8> {
        if token >= 3 && token < 259 {
            Some((token - 3) as u8)
        } else {
            None
        }
    }
}

/// Forward pass using the fast (4-way unrolled) matmul.
/// Uses matmul_f32_fast for all matrix-vector multiplications,
/// providing ~2× throughput improvement on scalar hardware
/// and better ILP utilization with SIMD extensions.
pub fn forward_fast(
    state: &mut TransformerState,
    token: u32,
    pos: usize,
    weights: &TransformerWeights,
) {
    let config = &state.config;
    let dim = config.dim;
    let kv_dim = config.kv_dim();
    let head_dim = config.head_dim();
    let n_heads = config.n_heads;
    let n_kv_heads = config.n_kv_heads;
    let n_rep = config.n_rep();

    // 1. Token embedding (supports Q8_0/Q4_0 on-the-fly dequant)
    let tok_idx = token as usize;
    if tok_idx < config.vocab_size {
        if !weights.token_embd_raw.is_empty() {
            dequant_embedding_row(&weights.token_embd_raw, tok_idx, dim, &mut state.x, weights.token_embd_is_q8);
        } else if !weights.token_embedding.is_empty() {
            let emb_offset = tok_idx * dim;
            if emb_offset + dim <= weights.token_embedding.len() {
                state.x.copy_from_slice(&weights.token_embedding[emb_offset..emb_offset + dim]);
            }
        }
    }

    // 2. Transformer layers
    for layer in 0..config.n_layers {
        state.xb.copy_from_slice(&state.x);
        let norm_w = weights.get_attn_norm(layer, dim);
        rmsnorm(&mut state.xb, norm_w, config.norm_eps);

        let wq = weights.get_wq(layer, dim);
        let wk = weights.get_wk(layer, dim, kv_dim);
        let wv = weights.get_wv(layer, dim, kv_dim);

        // Use fast 4-way unrolled matmul
        matmul_f32_fast(&mut state.q, &state.xb, wq, dim, dim);
        matmul_f32_fast(&mut state.k, &state.xb, wk, dim, kv_dim);
        matmul_f32_fast(&mut state.v, &state.xb, wv, dim, kv_dim);

        // RoPE on K heads
        for kh in 0..n_kv_heads {
            let k_start = kh * head_dim;
            let k_slice = &mut state.k[k_start..k_start + head_dim];
            apply_rope(&mut [], k_slice, pos, head_dim, config.rope_theta);
        }
        // RoPE on Q heads
        for h in 0..n_heads {
            let q_start = h * head_dim;
            let q_slice = &mut state.q[q_start..q_start + head_dim];
            apply_rope(q_slice, &mut [], pos, head_dim, config.rope_theta);
        }

        state.kv_caches[layer].store(pos, &state.k, &state.v, kv_dim);

        // GQA attention
        let seq_len = pos + 1;
        for h in 0..n_heads {
            let kv_h = h / n_rep;
            let q_start = h * head_dim;
            let scale = 1.0 / sqrt_head_dim(head_dim);
            let att_start = h * config.max_seq_len;
            for t in 0..seq_len {
                let k_offset = t * kv_dim + kv_h * head_dim;
                let mut score = 0.0f32;
                for d in 0..head_dim {
                    score += state.q[q_start + d] * state.kv_caches[layer].key[k_offset + d];
                }
                state.att[att_start + t] = score * scale;
            }
            softmax(&mut state.att[att_start..att_start + seq_len]);
            let xb_start = h * head_dim;
            for d in 0..head_dim {
                let mut val = 0.0f32;
                for t in 0..seq_len {
                    let v_offset = t * kv_dim + kv_h * head_dim;
                    val += state.att[att_start + t] * state.kv_caches[layer].value[v_offset + d];
                }
                state.xb2[xb_start + d] = val;
            }
        }

        let wo = weights.get_wo(layer, dim);
        state.xb.fill(0.0);
        matmul_f32_fast(&mut state.xb, &state.xb2, wo, dim, dim);
        for i in 0..dim { state.x[i] += state.xb[i]; }

        // FFN
        state.xb.copy_from_slice(&state.x);
        let ffn_norm_w = weights.get_ffn_norm(layer, dim);
        rmsnorm(&mut state.xb, ffn_norm_w, config.norm_eps);
        let w1 = weights.get_w1(layer, dim, config.hidden_dim);
        let w2 = weights.get_w2(layer, dim, config.hidden_dim);
        let w3 = weights.get_w3(layer, dim, config.hidden_dim);
        matmul_f32_fast(&mut state.hb, &state.xb, w1, dim, config.hidden_dim);
        matmul_f32_fast(&mut state.hb2, &state.xb, w3, dim, config.hidden_dim);
        for i in 0..config.hidden_dim {
            state.hb[i] = silu(state.hb[i]) * state.hb2[i];
        }
        state.xb.fill(0.0);
        matmul_f32_fast(&mut state.xb, &state.hb, w2, config.hidden_dim, dim);
        for i in 0..dim { state.x[i] += state.xb[i]; }
    }

    // 3. Final RMSNorm
    rmsnorm(&mut state.x, &weights.final_norm, config.norm_eps);

    // 4. Logits projection (fast, supports Q8_0/Q4_0 tied weights)
    if weights.tied_output && !weights.token_embd_raw.is_empty() {
        matmul_quant_logits(&mut state.logits, &state.x, &weights.token_embd_raw, dim, config.vocab_size, weights.token_embd_is_q8);
    } else if weights.tied_output && !weights.token_embedding.is_empty() {
        matmul_f32_fast(&mut state.logits, &state.x, &weights.token_embedding, dim, config.vocab_size);
    } else if !weights.output_proj.is_empty() {
        matmul_f32_fast(&mut state.logits, &state.x, &weights.output_proj, dim, config.vocab_size);
    }
}

/// REST API endpoint for LLM inference
pub fn handle_llm_request(prompt: &str, max_tokens: usize) -> alloc::string::String {
    use alloc::string::String;

    let mut result = String::new();
    let tokenizer = SimpleTokenizer::new();
    let tokens = tokenizer.encode(prompt);

    crate::serial_println!("[LLM] Prompt: '{}' ({} tokens, generating up to {})",
        prompt, tokens.len(), max_tokens);

    result.push_str(&alloc::format!(
        "{{\"model\":\"SmolLM2-135M\",\"prompt\":\"{}\",\"tokens\":{},\"status\":\"pipeline_ready\"}}",
        prompt, tokens.len()
    ));

    result
}

/// Run LLM self-tests
pub fn run_tests() {
    crate::serial_println!("\n========================================");
    crate::serial_println!("[LLM TESTS] Inference Engine (Layer 8)");
    crate::serial_println!("========================================\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // Test 1: Model config
    crate::serial_write("  [TEST 1/8] Model config... ");
    let config = ModelConfig::default();
    if config.dim == 576 && config.n_heads == 9 && config.head_dim() == 64
        && config.kv_dim() == 192 && config.n_rep() == 3 {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_write("FAIL\n");
        failed += 1;
    }

    // Test 2: Transformer state allocation (use tiny config to avoid OOM)
    crate::serial_write("  [TEST 2/8] State alloc... ");
    {
        let tiny = ModelConfig {
            dim: 64,
            hidden_dim: 128,
            n_layers: 2,
            n_heads: 2,
            n_kv_heads: 1,
            vocab_size: 256,
            max_seq_len: 32,
            rope_theta: 10000.0,
            norm_eps: 1e-5,
        };
        let state = TransformerState::new(tiny);
        let mem = state.memory_usage();
        if state.x.len() == 64 && state.logits.len() == 256 && mem > 0 {
            crate::serial_println!("OK ({} bytes)", mem);
            passed += 1;
        } else {
            crate::serial_write("FAIL\n");
            failed += 1;
        }
    }

    // Test 3: Q4_0 dequantization
    crate::serial_write("  [TEST 3/8] Q4_0 dequant... ");
    let mut test_block = [0u8; 18];
    test_block[0] = 0x00; test_block[1] = 0x3C; // f16 1.0
    let result = dequant_q4_0(&test_block, 32);
    if result.len() == 32 && (result[0] - (-8.0)).abs() < 0.01 {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_println!("FAIL (got {} elements, first={})", result.len(), result[0]);
        failed += 1;
    }

    // Test 4: Softmax
    crate::serial_write("  [TEST 4/8] Softmax... ");
    let mut probs = vec![1.0f32, 2.0, 3.0];
    softmax(&mut probs);
    let sum: f32 = probs.iter().sum();
    if (sum - 1.0).abs() < 0.01 && probs[2] > probs[1] && probs[1] > probs[0] {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_println!("FAIL (sum={})", sum);
        failed += 1;
    }

    // Test 5: RMSNorm
    crate::serial_write("  [TEST 5/8] RMSNorm... ");
    let mut x = vec![1.0f32, 2.0, 3.0, 4.0];
    let w = vec![1.0f32; 4];
    rmsnorm(&mut x, &w, 1e-5);
    let rms: f32 = x.iter().map(|v| v * v).sum::<f32>() / 4.0;
    if (rms - 1.0).abs() < 0.1 {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_println!("FAIL (rms={})", rms);
        failed += 1;
    }

    // Test 6: Tokenizer
    crate::serial_write("  [TEST 6/8] Tokenizer... ");
    let tok = SimpleTokenizer::new();
    let encoded = tok.encode("Hello");
    if encoded.len() == 6 && encoded[0] == tok.bos_id {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_println!("FAIL (len={})", encoded.len());
        failed += 1;
    }

    // Test 7: RoPE computation
    crate::serial_write("  [TEST 7/8] RoPE... ");
    {
        let mut q = vec![1.0f32, 0.0, 1.0, 0.0];
        let mut k = vec![1.0f32, 0.0, 1.0, 0.0];
        apply_rope(&mut q, &mut k, 0, 4, 10000.0);
        // At pos=0, cos(0)=1, sin(0)=0, so values should be unchanged
        let ok = (q[0] - 1.0).abs() < 0.01 && q[1].abs() < 0.01;
        if ok {
            crate::serial_write("OK\n");
            passed += 1;
        } else {
            crate::serial_println!("FAIL (q[0]={}, q[1]={})", q[0], q[1]);
            failed += 1;
        }
    }

    // Test 8: SiLU activation
    crate::serial_write("  [TEST 8/8] SiLU activation... ");
    {
        let y0 = silu(0.0);
        let y_pos = silu(2.0);
        let y_neg = silu(-2.0);
        // silu(0) = 0, silu(2) ≈ 1.76, silu(-2) ≈ -0.24
        if y0.abs() < 0.01 && y_pos > 1.5 && y_pos < 2.0 && y_neg < 0.0 && y_neg > -0.5 {
            crate::serial_write("OK\n");
            passed += 1;
        } else {
            crate::serial_println!("FAIL (silu(0)={}, silu(2)={}, silu(-2)={})", y0, y_pos, y_neg);
            failed += 1;
        }
    }

    crate::serial_println!("\n========================================");
    crate::serial_println!("[LLM TESTS] {}/{} passed", passed, passed + failed);
    if failed == 0 && passed > 0 {
        crate::serial_write("[LLM TESTS] ALL PASSED!\n");
    }
    crate::serial_println!("========================================");
}
