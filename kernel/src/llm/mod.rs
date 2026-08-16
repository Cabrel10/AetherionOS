// kernel/src/llm/mod.rs — LLM Inference Engine for AetherionOS (Block M)
//
// Provides bare-metal GGUF model loading and transformer inference.
// Designed for AVX2-accelerated Q4_0 quantized models.
//
// Architecture:
//   1. GGUF parser reads model metadata and tensor offsets from mmap'd memory
//   2. Dequantization converts Q4_0 blocks to f32 for computation
//   3. Transformer forward pass implements attention + FFN with RoPE
//   4. Sampling selects next token via greedy, top-k, or top-p
//
// Prerequisites (Block B + scheduler):
//   - XSAVE/XRSTOR for AVX2/AVX-512 state in context switch
//   - HugePages (2 MiB / 1 GiB) for model mmap
//   - Sufficient physical RAM for model + KV cache

pub mod gguf;
pub mod matmul;
pub mod inference;
pub mod parallel;
