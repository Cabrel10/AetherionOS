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
fn dequant_embedding_row(q4_data: &[u8], tok_idx: usize, dim: usize, out: &mut [f32]) {
    use super::matmul::f16_to_f32;
    // Each row has `dim` elements. In Q4_0, each block of 32 elements = 18 bytes.
    let blocks_per_row = dim / 32;
    let bytes_per_row = blocks_per_row * 18;
    let row_start = tok_idx * bytes_per_row;

    if row_start + bytes_per_row > q4_data.len() {
        // Out of bounds — fill with zeros
        for v in out.iter_mut().take(dim) { *v = 0.0; }
        return;
    }

    for b in 0..blocks_per_row {
        let off = row_start + b * 18;
        let scale = f16_to_f32(u16::from_le_bytes([q4_data[off], q4_data[off + 1]]));
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

/// Compute logits = x × embedding_matrix^T using Q4_0 data on-the-fly.
/// For each vocab token i, computes logits[i] = dot(x, embed_row[i]).
/// Dequantizes each row on-the-fly to avoid storing full f32 embedding (112 MB).
fn matmul_q4_0_logits(logits: &mut [f32], x: &[f32], q4_data: &[u8], dim: usize, vocab_size: usize) {
    use super::matmul::f16_to_f32;
    let blocks_per_row = dim / 32;
    let bytes_per_row = blocks_per_row * 18;

    for tok in 0..vocab_size {
        let row_start = tok * bytes_per_row;
        if row_start + bytes_per_row > q4_data.len() {
            logits[tok] = 0.0;
            continue;
        }

        let mut dot = 0.0f32;
        for b in 0..blocks_per_row {
            let off = row_start + b * 18;
            let scale = f16_to_f32(u16::from_le_bytes([q4_data[off], q4_data[off + 1]]));
            for i in 0..32 {
                let byte_idx = i / 2;
                let nibble = if i % 2 == 0 {
                    (q4_data[off + 2 + byte_idx] & 0x0F) as i8 - 8
                } else {
                    ((q4_data[off + 2 + byte_idx] >> 4) & 0x0F) as i8 - 8
                };
                let elem_idx = b * 32 + i;
                dot += x[elem_idx] * scale * nibble as f32;
            }
        }
        logits[tok] = dot;
    }
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

    // 1. Token embedding (supports Q4_0 on-the-fly dequant or f32 direct)
    let tok_idx = token as usize;
    if tok_idx < config.vocab_size {
        if !weights.token_embd_q4.is_empty() {
            // Q4_0 on-the-fly: dequantize only the row we need
            dequant_embedding_row(&weights.token_embd_q4, tok_idx, dim, &mut state.x);
        } else if !weights.token_embedding.is_empty() {
            let emb_offset = tok_idx * dim;
            if emb_offset + dim <= weights.token_embedding.len() {
                state.x.copy_from_slice(&weights.token_embedding[emb_offset..emb_offset + dim]);
            }
        }
    }

    // 2. Transformer layers
    for layer in 0..config.n_layers {
        // 2a. RMSNorm pre-attention
        state.xb.copy_from_slice(&state.x);
        let norm_w = weights.get_attn_norm(layer, dim);
        rmsnorm(&mut state.xb, norm_w, config.norm_eps);

        // 2b. Q/K/V projections
        let wq = weights.get_wq(layer, dim);
        let wk = weights.get_wk(layer, dim, kv_dim);
        let wv = weights.get_wv(layer, dim, kv_dim);

        matmul_f32(&mut state.q, &state.xb, wq, dim, dim);
        matmul_f32(&mut state.k, &state.xb, wk, dim, kv_dim);
        matmul_f32(&mut state.v, &state.xb, wv, dim, kv_dim);

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
        let wo = weights.get_wo(layer, dim);
        state.xb.fill(0.0);
        matmul_f32(&mut state.xb, &state.xb2, wo, dim, dim);

        // Residual connection
        for i in 0..dim {
            state.x[i] += state.xb[i];
        }

        // 2g. RMSNorm pre-FFN
        state.xb.copy_from_slice(&state.x);
        let ffn_norm_w = weights.get_ffn_norm(layer, dim);
        rmsnorm(&mut state.xb, ffn_norm_w, config.norm_eps);

        // 2h. SwiGLU FFN
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

        // down = W2 * hb
        state.xb.fill(0.0);
        matmul_f32(&mut state.xb, &state.hb, w2, config.hidden_dim, dim);

        // Residual connection
        for i in 0..dim {
            state.x[i] += state.xb[i];
        }
    }

    // 3. Final RMSNorm
    rmsnorm(&mut state.x, &weights.final_norm, config.norm_eps);

    // 4. Logits projection (supports Q4_0 tied weights to save memory)
    if weights.tied_output && !weights.token_embd_q4.is_empty() {
        // Q4_0 on-the-fly: compute logits[i] = dot(x, embd_row[i]) for each vocab token
        matmul_q4_0_logits(&mut state.logits, &state.x, &weights.token_embd_q4, dim, config.vocab_size);
    } else if weights.tied_output && !weights.token_embedding.is_empty() {
        matmul_f32(&mut state.logits, &state.x, &weights.token_embedding, dim, config.vocab_size);
    } else if !weights.output_proj.is_empty() {
        matmul_f32(&mut state.logits, &state.x, &weights.output_proj, dim, config.vocab_size);
    }
}

/// Transformer weights container
/// For production, this would point into mmap'd GGUF data.
/// For testing, we create small dummy weights.
pub struct TransformerWeights {
    pub token_embedding: Vec<f32>,
    pub final_norm: Vec<f32>,
    pub output_proj: Vec<f32>,
    // Per-layer weights packed sequentially
    pub layer_weights: Vec<f32>,
    // Config for offset calculation
    pub(crate) dim: usize,
    pub(crate) hidden_dim: usize,
    pub(crate) kv_dim: usize,
    pub(crate) n_layers: usize,
    /// When true, output_proj is empty and token_embedding should be used for logits.
    /// Saves ~112 MB in 512 MB QEMU by not duplicating the embedding table.
    pub tied_output: bool,
    /// Raw Q4_0 data for token embedding (avoids 112 MB f32 dequant).
    /// When non-empty, token_embedding is empty and this is used for on-the-fly lookup.
    pub token_embd_q4: Vec<u8>,
    /// vocab_size for Q4_0 embedding lookup
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
            dim,
            hidden_dim,
            kv_dim,
            n_layers,
            tied_output: false,
            token_embd_q4: Vec::new(),
            vocab_size: config.vocab_size,
        }
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
                        out.push(f32::from_le_bytes([data[off], data[off+1], data[off+2], data[off+3]]));
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
                        let scale = f16_to_f32(u16::from_le_bytes([data[off], data[off+1]]));
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

        // Load token embedding as RAW Q4_0 data (saves 112 MB vs f32 dequant)
        // Q4_0: vocab_size*dim elements, 18 bytes per 32-element block
        // = 576*49152/32*18 ≈ 16 MB raw vs 112 MB f32
        crate::serial_write("[LLM] Loading token_embd (raw Q4_0)...\n");
        let mut token_embd_q4 = Vec::new();
        let token_embedding = Vec::new(); // empty — we use Q4_0 on-the-fly
        if let Some(info) = model.tensors.get("token_embd.weight") {
            if let Some(data) = model.tensor_data("token_embd.weight", file_data) {
                token_embd_q4 = data.to_vec();
                crate::serial_println!("[LLM] token_embd: {} bytes raw {:?}", token_embd_q4.len(), info.dtype);
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
        crate::serial_write("[LLM] output tied to token_embd Q4_0 (saves 112 MB)\n");

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
            dim,
            hidden_dim,
            kv_dim,
            n_layers,
            tied_output: tied,
            token_embd_q4,
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
        let mut tokens = vec![self.bos_id];
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

    // 1. Token embedding (supports Q4_0 on-the-fly dequant)
    let tok_idx = token as usize;
    if tok_idx < config.vocab_size {
        if !weights.token_embd_q4.is_empty() {
            dequant_embedding_row(&weights.token_embd_q4, tok_idx, dim, &mut state.x);
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

    // 4. Logits projection (fast, supports Q4_0 tied weights)
    if weights.tied_output && !weights.token_embd_q4.is_empty() {
        matmul_q4_0_logits(&mut state.logits, &state.x, &weights.token_embd_q4, dim, config.vocab_size);
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
