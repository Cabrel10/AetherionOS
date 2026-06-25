//! Multi-Model Runtime — GGUF/ONNX/PyTorch/TF/SafeTensors/TFLite
//!
//! # Architecture (2026-06-25)
//!
//! AetherionOS is format-agnostic: it can load and run models from ANY
//! popular format by converting them to a common internal representation
//! (the ComputeGraph). This module handles:
//!
//! 1. **Format detection**: Identify model format from magic bytes
//! 2. **Metadata parsing**: Extract architecture, shapes, quantization info
//! 3. **Weight loading**: Map weights to compute graph tensors
//! 4. **Runtime dispatch**: Route inference to the right engine
//!
//! ```text
//! ┌────────────────────────────────────────────────────────┐
//! │                 Multi-Model Runtime                    │
//! │                                                        │
//! │  ┌──────┐ ┌──────┐ ┌────────┐ ┌────┐ ┌────────────┐  │
//! │  │ GGUF │ │ ONNX │ │PyTorch │ │ TF │ │SafeTensors│  │
//! │  │Parser│ │Parser│ │ Parser │ │Par.│ │  Parser    │  │
//! │  └──┬───┘ └──┬───┘ └───┬────┘ └─┬──┘ └─────┬──────┘  │
//! │     │        │         │        │           │          │
//! │     ▼        ▼         ▼        ▼           ▼          │
//! │  ┌──────────────────────────────────────────────────┐  │
//! │  │           Unified Model Descriptor               │  │
//! │  │  (arch, shapes, dtypes, tensor map, metadata)    │  │
//! │  └──────────────────────┬───────────────────────────┘  │
//! │                         │                              │
//! │                         ▼                              │
//! │  ┌──────────────────────────────────────────────────┐  │
//! │  │              ComputeGraph Builder                │  │
//! │  │  Converts model descriptor → executable graph    │  │
//! │  └──────────────────────────────────────────────────┘  │
//! └────────────────────────────────────────────────────────┘
//! ```

use alloc::vec::Vec;
use alloc::string::String;
use alloc::format;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU32, Ordering};
use spin::Mutex;
use lazy_static::lazy_static;

// ═══════════════════════════════════════════════════════════════════════════
// Model Format Detection
// ═══════════════════════════════════════════════════════════════════════════

/// Supported model file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ModelFormat {
    /// GGML/GGUF format (llama.cpp ecosystem)
    /// Magic: "GGUF" (0x46554747) at offset 0
    Gguf = 0,
    /// ONNX (Open Neural Network Exchange)
    /// Magic: 0x08 at offset 0 (protobuf varint for field 1)
    Onnx = 1,
    /// PyTorch (pickle + zip archive)
    /// Magic: "PK" (0x504B) at offset 0 (ZIP header)
    PyTorch = 2,
    /// TensorFlow SavedModel (protobuf)
    /// Directory with saved_model.pb
    TensorFlow = 3,
    /// SafeTensors (HuggingFace)
    /// Starts with 8-byte little-endian header length
    SafeTensors = 4,
    /// TensorFlow Lite (flatbuffers)
    /// Magic: "TFL3" at offset 4
    TfLite = 5,
    /// Unknown format
    Unknown = 255,
}

impl core::fmt::Display for ModelFormat {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::Gguf => write!(f, "GGUF"),
            Self::Onnx => write!(f, "ONNX"),
            Self::PyTorch => write!(f, "PyTorch"),
            Self::TensorFlow => write!(f, "TensorFlow"),
            Self::SafeTensors => write!(f, "SafeTensors"),
            Self::TfLite => write!(f, "TFLite"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Detect model format from the first bytes of a file.
pub fn detect_format(header: &[u8]) -> ModelFormat {
    if header.len() < 8 {
        return ModelFormat::Unknown;
    }

    // GGUF: magic "GGUF" at offset 0 (little-endian: 0x46554747)
    if header[0] == 0x47 && header[1] == 0x47 && header[2] == 0x55 && header[3] == 0x46 {
        return ModelFormat::Gguf;
    }

    // PyTorch/ZIP: magic "PK\x03\x04"
    if header[0] == 0x50 && header[1] == 0x4B && header[2] == 0x03 && header[3] == 0x04 {
        return ModelFormat::PyTorch;
    }

    // TFLite: "TFL3" at offset 4
    if header.len() >= 8 && header[4] == b'T' && header[5] == b'F' && header[6] == b'L' && header[7] == b'3' {
        return ModelFormat::TfLite;
    }

    // SafeTensors: first 8 bytes are a little-endian u64 header length
    // Typical values: < 10MB for the JSON header
    let header_len = u64::from_le_bytes([
        header[0], header[1], header[2], header[3],
        header[4], header[5], header[6], header[7],
    ]);
    if header_len > 0 && header_len < 100_000_000 {
        // Heuristic: if byte 8+ looks like '{' (JSON), it's likely SafeTensors
        if header.len() > 8 && header[8] == b'{' {
            return ModelFormat::SafeTensors;
        }
    }

    // ONNX: protobuf with first field being an int64 (ir_version)
    // This is a weak heuristic; proper detection needs protobuf parsing
    if header[0] == 0x08 {
        return ModelFormat::Onnx;
    }

    ModelFormat::Unknown
}

// ═══════════════════════════════════════════════════════════════════════════
// Unified Model Descriptor
// ═══════════════════════════════════════════════════════════════════════════

/// Architecture type of the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ModelArch {
    /// LLaMA-style decoder-only transformer
    Llama = 0,
    /// GPT-2/GPT-J style
    Gpt2 = 1,
    /// BERT-style encoder
    Bert = 2,
    /// T5-style encoder-decoder
    T5 = 3,
    /// Mixture-of-Experts (Mixtral, DeepSeek)
    Moe = 4,
    /// Vision Transformer (ViT)
    Vit = 5,
    /// Whisper (audio)
    Whisper = 6,
    /// Stable Diffusion (image generation)
    Diffusion = 7,
    /// Generic/unknown architecture
    Generic = 255,
}

/// Unified model descriptor — format-agnostic representation of a model.
#[derive(Debug, Clone)]
pub struct ModelDescriptor {
    /// Unique model ID (auto-assigned)
    pub model_id: u32,
    /// Original file format
    pub format: ModelFormat,
    /// Architecture type
    pub arch: ModelArch,
    /// Model name (from metadata or filename)
    pub name: String,
    /// Number of transformer layers
    pub n_layers: usize,
    /// Hidden dimension
    pub dim: usize,
    /// Number of attention heads
    pub n_heads: usize,
    /// Number of KV heads (for GQA; equal to n_heads for MHA)
    pub n_kv_heads: usize,
    /// Vocabulary size
    pub vocab_size: usize,
    /// FFN hidden dimension
    pub ffn_dim: usize,
    /// Maximum sequence length
    pub max_seq_len: usize,
    /// RoPE theta
    pub rope_theta: f32,
    /// Normalization epsilon
    pub norm_eps: f32,
    /// Quantization type (e.g., "Q4_0", "Q8_0", "F16", "F32")
    pub quant_type: String,
    /// Total parameter count
    pub param_count: u64,
    /// Total file size in bytes
    pub file_size: u64,
    /// Tensor name → (offset, size, dtype, shape) mapping
    pub tensor_map: BTreeMap<String, TensorInfo>,
    /// MoE configuration (if applicable)
    pub moe_config: Option<MoeInfo>,
}

/// Information about a single tensor in the model file.
#[derive(Debug, Clone)]
pub struct TensorInfo {
    pub offset: u64,
    pub size_bytes: u64,
    pub dtype: super::compute_graph::DType,
    pub shape: Vec<usize>,
}

/// MoE-specific configuration.
#[derive(Debug, Clone)]
pub struct MoeInfo {
    pub n_experts: usize,
    pub top_k: usize,
    pub expert_layers: Vec<usize>,  // Which layers have MoE
}

// ═══════════════════════════════════════════════════════════════════════════
// Model Registry
// ═══════════════════════════════════════════════════════════════════════════

static NEXT_MODEL_ID: AtomicU32 = AtomicU32::new(1);

struct ModelRegistry {
    models: BTreeMap<u32, ModelDescriptor>,
}

impl ModelRegistry {
    fn new() -> Self {
        Self {
            models: BTreeMap::new(),
        }
    }
}

lazy_static! {
    static ref REGISTRY: Mutex<ModelRegistry> = Mutex::new(ModelRegistry::new());
}

// ═══════════════════════════════════════════════════════════════════════════
// Format-Specific Parsers (Stubs)
// ═══════════════════════════════════════════════════════════════════════════

/// Parse a GGUF file header and return a model descriptor.
///
/// GGUF is the primary format — it's already deeply supported by the
/// existing `llm::gguf` module. This function wraps that parser into
/// the unified descriptor format.
pub fn parse_gguf(data: &[u8], file_size: u64) -> Option<ModelDescriptor> {
    // Validate GGUF magic
    if data.len() < 16 || detect_format(&data[..8]) != ModelFormat::Gguf {
        return None;
    }

    let model_id = NEXT_MODEL_ID.fetch_add(1, Ordering::Relaxed);

    // The existing llm::gguf module handles detailed parsing.
    // Here we create a high-level descriptor for the multi-model registry.
    Some(ModelDescriptor {
        model_id,
        format: ModelFormat::Gguf,
        arch: ModelArch::Llama, // Most GGUF models are LLaMA-family
        name: String::from("gguf-model"),
        n_layers: 0,  // Filled by gguf parser
        dim: 0,
        n_heads: 0,
        n_kv_heads: 0,
        vocab_size: 0,
        ffn_dim: 0,
        max_seq_len: 2048,
        rope_theta: 10000.0,
        norm_eps: 1e-5,
        quant_type: String::from("unknown"),
        param_count: 0,
        file_size,
        tensor_map: BTreeMap::new(),
        moe_config: None,
    })
}

/// Parse SafeTensors format header.
pub fn parse_safetensors(data: &[u8], file_size: u64) -> Option<ModelDescriptor> {
    if data.len() < 16 || detect_format(&data[..8]) != ModelFormat::SafeTensors {
        return None;
    }

    let model_id = NEXT_MODEL_ID.fetch_add(1, Ordering::Relaxed);

    // SafeTensors format: 8-byte header length + JSON header + raw tensor data
    let header_len = u64::from_le_bytes([
        data[0], data[1], data[2], data[3],
        data[4], data[5], data[6], data[7],
    ]) as usize;

    if 8 + header_len > data.len() {
        return None;
    }

    // The JSON header contains tensor names, shapes, dtypes, and offsets.
    // Full JSON parsing is TODO; for now create a descriptor stub.

    Some(ModelDescriptor {
        model_id,
        format: ModelFormat::SafeTensors,
        arch: ModelArch::Generic,
        name: String::from("safetensors-model"),
        n_layers: 0,
        dim: 0,
        n_heads: 0,
        n_kv_heads: 0,
        vocab_size: 0,
        ffn_dim: 0,
        max_seq_len: 2048,
        rope_theta: 10000.0,
        norm_eps: 1e-5,
        quant_type: String::from("F16"),
        param_count: 0,
        file_size,
        tensor_map: BTreeMap::new(),
        moe_config: None,
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Load a model from raw file data. Detects format and returns a model ID.
pub fn load_model(data: &[u8], file_size: u64) -> Option<u32> {
    let format = detect_format(&data[..data.len().min(16)]);

    let descriptor = match format {
        ModelFormat::Gguf => parse_gguf(data, file_size),
        ModelFormat::SafeTensors => parse_safetensors(data, file_size),
        // Other formats return stub descriptors for now
        other => {
            let model_id = NEXT_MODEL_ID.fetch_add(1, Ordering::Relaxed);
            Some(ModelDescriptor {
                model_id,
                format: other,
                arch: ModelArch::Generic,
                name: format!("{}-model", other),
                n_layers: 0, dim: 0, n_heads: 0, n_kv_heads: 0,
                vocab_size: 0, ffn_dim: 0, max_seq_len: 0,
                rope_theta: 0.0, norm_eps: 0.0,
                quant_type: String::from("unknown"),
                param_count: 0, file_size,
                tensor_map: BTreeMap::new(), moe_config: None,
            })
        }
    };

    if let Some(desc) = descriptor {
        let model_id = desc.model_id;
        crate::serial_println!(
            "[MULTI-MODEL] loaded: {} (format={}, arch={:?}, file={} MB)",
            desc.name, desc.format, desc.arch, file_size / (1024 * 1024)
        );
        REGISTRY.lock().models.insert(model_id, desc);
        Some(model_id)
    } else {
        crate::serial_println!("[MULTI-MODEL] failed to parse model (format={:?})", format);
        None
    }
}

/// Get a model descriptor by ID.
pub fn get_model(model_id: u32) -> Option<ModelDescriptor> {
    REGISTRY.lock().models.get(&model_id).cloned()
}

/// List all loaded models.
pub fn list_models() -> Vec<(u32, String, ModelFormat, u64)> {
    REGISTRY
        .lock()
        .models
        .iter()
        .map(|(&id, desc)| (id, desc.name.clone(), desc.format, desc.file_size))
        .collect()
}

/// Unload a model.
pub fn unload_model(model_id: u32) -> bool {
    REGISTRY.lock().models.remove(&model_id).is_some()
}

/// Initialize the multi-model runtime.
pub fn init() {
    crate::serial_println!(
        "[MULTI-MODEL] Runtime initialized (supports: GGUF, ONNX, PyTorch, TF, SafeTensors, TFLite)"
    );
}
