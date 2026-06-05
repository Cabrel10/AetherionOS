// kernel/src/llm/gguf.rs — GGUF v3 Model Format Parser (Session 9: Complete Implementation)
//
// GGUF (GGML Universal Format) is the standard format for quantized LLMs.
// This parser extracts model metadata, tensor locations, and vocabulary
// from raw bytes, enabling direct inference from mmap'd model files.
//
// GGUF v3 file layout:
//   [Header 24 bytes] magic=GGUF version=3 n_tensors n_kv
//   [KV pairs n_kv]   key_string type value
//   [Tensor infos]    name n_dims dims type offset
//   [ALIGNMENT PAD]   align to 32 bytes
//   [Tensor data]     raw quantized weights at data_offset + tensor.offset

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// GGUF file magic number: "GGUF" in little-endian
const GGUF_MAGIC: u32 = 0x46554747;

/// Default alignment for GGUF v3
const GGUF_DEFAULT_ALIGNMENT: usize = 32;

/// GGUF file header (24 bytes)
#[derive(Debug, Clone)]
pub struct GgufHeader {
    pub magic: u32,
    pub version: u32,
    pub n_tensors: u64,
    pub n_kv: u64,
}

/// GGUF KV value types
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GgufValueType {
    Uint8 = 0,
    Int8 = 1,
    Uint16 = 2,
    Int16 = 3,
    Uint32 = 4,
    Int32 = 5,
    Float32 = 6,
    Bool = 7,
    String = 8,
    Array = 9,
    Uint64 = 10,
    Int64 = 11,
    Float64 = 12,
}

/// Quantization types supported by GGML
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GgmlType {
    F32 = 0,
    F16 = 1,
    Q4_0 = 2,
    Q4_1 = 3,
    Q5_0 = 6,
    Q5_1 = 7,
    Q8_0 = 8,
    Q8_1 = 9,
    Q2K = 10,
    Q3KS = 11,
    Q4KS = 14,
    Q4KM = 15,
    Q5KS = 16,
    Q5KM = 17,
    Q6K = 18,
    Q8K = 19,
    BF16 = 35,
}

impl GgmlType {
    /// Convert raw u32 to GgmlType
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::F32),
            1 => Some(Self::F16),
            2 => Some(Self::Q4_0),
            3 => Some(Self::Q4_1),
            6 => Some(Self::Q5_0),
            7 => Some(Self::Q5_1),
            8 => Some(Self::Q8_0),
            9 => Some(Self::Q8_1),
            10 => Some(Self::Q2K),
            11 => Some(Self::Q3KS),
            14 => Some(Self::Q4KS),
            15 => Some(Self::Q4KM),
            16 => Some(Self::Q5KS),
            17 => Some(Self::Q5KM),
            18 => Some(Self::Q6K),
            19 => Some(Self::Q8K),
            35 => Some(Self::BF16),
            _ => None,
        }
    }

    /// Block size (number of elements per quantization block)
    pub fn block_size(&self) -> usize {
        match self {
            Self::F32 | Self::F16 | Self::BF16 => 1,
            Self::Q4_0 | Self::Q4_1 => 32,
            Self::Q5_0 | Self::Q5_1 => 32,
            Self::Q8_0 | Self::Q8_1 => 32,
            Self::Q2K | Self::Q3KS | Self::Q4KS | Self::Q4KM
            | Self::Q5KS | Self::Q5KM | Self::Q6K | Self::Q8K => 256,
        }
    }

    /// Size in bytes for one block of this type
    pub fn type_size(&self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F16 | Self::BF16 => 2,
            Self::Q4_0 => 18,   // 2 (f16 scale) + 16 (data) per 32 elements
            Self::Q4_1 => 20,   // 2 (f16 scale) + 2 (f16 min) + 16 (data)
            Self::Q5_0 => 22,
            Self::Q5_1 => 24,
            Self::Q8_0 => 34,   // 2 (f16 scale) + 32 (data)
            Self::Q8_1 => 40,
            Self::Q2K => 64 + 16 + 1,  // ~81
            Self::Q3KS => 64 + 32 + 2 + 12, // ~110
            Self::Q4KS => 2 + 2 + 128, // 132
            Self::Q4KM => 2 + 2 + 12 + 128, // 144
            Self::Q5KS => 2 + 2 + 12 + 128 + 32, // 176
            Self::Q5KM => 2 + 2 + 12 + 128 + 32, // 176
            Self::Q6K => 2 + 192 + 64 + 16, // 274 approx
            Self::Q8K => 4 + 256 + 16, // 276
        }
    }

    /// Size in bytes per element for this type
    pub fn element_size(&self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F16 | Self::BF16 => 2,
            _ => 1, // quantized types use block-based sizing
        }
    }

    /// Compute total bytes for n elements of this type
    pub fn row_size(&self, n_elements: u64) -> u64 {
        let bs = self.block_size() as u64;
        let ts = self.type_size() as u64;
        let n_blocks = (n_elements + bs - 1) / bs;
        n_blocks * ts
    }
}

/// Information about a single tensor in the GGUF file
#[derive(Debug, Clone)]
pub struct TensorInfo {
    pub name: String,
    pub shape: Vec<u64>,
    pub dtype: GgmlType,
    pub offset: u64,
}

impl TensorInfo {
    /// Total number of elements in this tensor
    pub fn n_elements(&self) -> u64 {
        self.shape.iter().product()
    }

    /// Total size in bytes of this tensor's data
    pub fn data_size(&self) -> u64 {
        self.dtype.row_size(self.n_elements())
    }
}

/// Parsed GGUF file — full metadata + tensor index
#[derive(Debug)]
pub struct GgufModel {
    pub header: GgufHeader,
    pub metadata: BTreeMap<String, GgufValue>,
    pub tensors: BTreeMap<String, TensorInfo>,
    pub data_offset: usize,
}

/// GGUF metadata value (simplified for the values we need)
#[derive(Debug, Clone)]
pub enum GgufValue {
    Uint32(u32),
    Int32(i32),
    Float32(f32),
    Uint64(u64),
    Bool(bool),
    Str(String),
    ArrayStr(Vec<String>),
    ArrayF32(Vec<f32>),
    ArrayU32(Vec<u32>),
    Other,
}

/// GGUF parsing errors
#[derive(Debug)]
pub enum GgufError {
    TooShort,
    InvalidMagic,
    UnsupportedVersion,
    InvalidTensor,
    InvalidString,
    UnknownType(u32),
    ParseError(&'static str),
}

/// Cursor for parsing GGUF binary data
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        if self.pos >= self.data.len() { 0 } else { self.data.len() - self.pos }
    }

    fn read_u8(&mut self) -> Result<u8, GgufError> {
        if self.remaining() < 1 { return Err(GgufError::TooShort); }
        let v = self.data[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn read_u32(&mut self) -> Result<u32, GgufError> {
        if self.remaining() < 4 { return Err(GgufError::TooShort); }
        let v = u32::from_le_bytes([
            self.data[self.pos], self.data[self.pos+1],
            self.data[self.pos+2], self.data[self.pos+3],
        ]);
        self.pos += 4;
        Ok(v)
    }

    fn read_i32(&mut self) -> Result<i32, GgufError> {
        Ok(self.read_u32()? as i32)
    }

    fn read_u64(&mut self) -> Result<u64, GgufError> {
        if self.remaining() < 8 { return Err(GgufError::TooShort); }
        let v = u64::from_le_bytes([
            self.data[self.pos], self.data[self.pos+1],
            self.data[self.pos+2], self.data[self.pos+3],
            self.data[self.pos+4], self.data[self.pos+5],
            self.data[self.pos+6], self.data[self.pos+7],
        ]);
        self.pos += 8;
        Ok(v)
    }

    fn read_f32(&mut self) -> Result<f32, GgufError> {
        let bits = self.read_u32()?;
        Ok(f32::from_bits(bits))
    }

    fn read_string(&mut self) -> Result<String, GgufError> {
        let len = self.read_u64()? as usize;
        if len > 65536 || self.remaining() < len {
            return Err(GgufError::InvalidString);
        }
        let bytes = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    fn skip(&mut self, n: usize) -> Result<(), GgufError> {
        if self.remaining() < n { return Err(GgufError::TooShort); }
        self.pos += n;
        Ok(())
    }

    /// Skip a GGUF value based on its type tag
    fn skip_value(&mut self, vtype: u32) -> Result<(), GgufError> {
        match vtype {
            0 => self.skip(1),   // uint8
            1 => self.skip(1),   // int8
            2 => self.skip(2),   // uint16
            3 => self.skip(2),   // int16
            4 => self.skip(4),   // uint32
            5 => self.skip(4),   // int32
            6 => self.skip(4),   // float32
            7 => self.skip(1),   // bool
            8 => { self.read_string()?; Ok(()) }, // string
            9 => { // array
                let elem_type = self.read_u32()?;
                let count = self.read_u64()? as usize;
                for _ in 0..count {
                    self.skip_value(elem_type)?;
                }
                Ok(())
            },
            10 => self.skip(8),  // uint64
            11 => self.skip(8),  // int64
            12 => self.skip(8),  // float64
            _ => Err(GgufError::UnknownType(vtype)),
        }
    }

    /// Read a GGUF value
    fn read_value(&mut self, vtype: u32) -> Result<GgufValue, GgufError> {
        match vtype {
            0 => { let v = self.read_u8()?; Ok(GgufValue::Uint32(v as u32)) },
            1 => { let v = self.read_u8()?; Ok(GgufValue::Int32(v as i32)) },
            2 => { self.skip(2)?; Ok(GgufValue::Other) }, // uint16
            3 => { self.skip(2)?; Ok(GgufValue::Other) }, // int16
            4 => { let v = self.read_u32()?; Ok(GgufValue::Uint32(v)) },
            5 => { let v = self.read_i32()?; Ok(GgufValue::Int32(v)) },
            6 => { let v = self.read_f32()?; Ok(GgufValue::Float32(v)) },
            7 => { let v = self.read_u8()?; Ok(GgufValue::Bool(v != 0)) },
            8 => { let s = self.read_string()?; Ok(GgufValue::Str(s)) },
            9 => { // array
                let elem_type = self.read_u32()?;
                let count = self.read_u64()? as usize;
                match elem_type {
                    8 => { // array of strings
                        let mut arr = Vec::with_capacity(count.min(65536));
                        for _ in 0..count.min(65536) {
                            arr.push(self.read_string()?);
                        }
                        // Skip remaining if count > 65536
                        for _ in 65536..count {
                            self.read_string()?;
                        }
                        Ok(GgufValue::ArrayStr(arr))
                    },
                    6 => { // array of float32
                        let mut arr = Vec::with_capacity(count.min(65536));
                        for _ in 0..count.min(65536) {
                            arr.push(self.read_f32()?);
                        }
                        for _ in 65536..count {
                            self.read_f32()?;
                        }
                        Ok(GgufValue::ArrayF32(arr))
                    },
                    4 => { // array of uint32
                        let mut arr = Vec::with_capacity(count.min(65536));
                        for _ in 0..count.min(65536) {
                            arr.push(self.read_u32()?);
                        }
                        for _ in 65536..count {
                            self.read_u32()?;
                        }
                        Ok(GgufValue::ArrayU32(arr))
                    },
                    _ => {
                        // Skip unknown array types
                        for _ in 0..count {
                            self.skip_value(elem_type)?;
                        }
                        Ok(GgufValue::Other)
                    }
                }
            },
            10 => { let v = self.read_u64()?; Ok(GgufValue::Uint64(v)) },
            11 => { self.skip(8)?; Ok(GgufValue::Other) },
            12 => { self.skip(8)?; Ok(GgufValue::Other) },
            _ => Err(GgufError::UnknownType(vtype)),
        }
    }
}

impl GgufModel {
    /// Parse GGUF header from raw bytes.
    pub fn parse_header(data: &[u8]) -> Result<GgufHeader, GgufError> {
        if data.len() < 24 {
            return Err(GgufError::TooShort);
        }
        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if magic != GGUF_MAGIC {
            return Err(GgufError::InvalidMagic);
        }
        let version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        if version < 1 || version > 3 {
            return Err(GgufError::UnsupportedVersion);
        }
        let n_tensors = u64::from_le_bytes(data[8..16].try_into().unwrap());
        let n_kv = u64::from_le_bytes(data[16..24].try_into().unwrap());
        Ok(GgufHeader { magic, version, n_tensors, n_kv })
    }

    /// Parse the complete GGUF file: header + KV pairs + tensor info.
    /// Returns a fully indexed model that can look up tensor data by name.
    ///
    /// NOTE: `data` must contain at least the header + KV + tensor info sections.
    /// It does NOT need to contain the full tensor data (which can be 100+ MB).
    /// The `data_offset` field tells you where tensor data begins.
    pub fn parse(data: &[u8]) -> Result<Self, GgufError> {
        let header = Self::parse_header(data)?;
        let mut cursor = Cursor::new(data);
        cursor.pos = 24; // skip header

        // Parse KV pairs
        let mut metadata = BTreeMap::new();
        let n_kv = header.n_kv as usize;
        for _i in 0..n_kv {
            let key = cursor.read_string()?;
            let vtype = cursor.read_u32()?;

            // For important keys, parse the value; for others, just skip
            let is_important = key.contains("vocab_size")
                || key.contains("embedding_length")
                || key.contains("feed_forward_length")
                || key.contains("block_count")
                || key.contains("attention.head_count")
                || key.contains("attention.head_count_kv")
                || key.contains("context_length")
                || key.contains("rope.freq_base")
                || key.contains("attention.layer_norm_rms_epsilon")
                || key.contains("tokenizer.ggml.tokens")
                || key.contains("tokenizer.ggml.scores")
                || key.contains("tokenizer.ggml.token_type")
                || key.contains("tokenizer.ggml.bos_token_id")
                || key.contains("tokenizer.ggml.eos_token_id")
                || key.contains("general.architecture")
                || key.contains("general.name");

            if is_important {
                let value = cursor.read_value(vtype)?;
                metadata.insert(key, value);
            } else {
                cursor.skip_value(vtype)?;
            }
        }

        // Parse tensor info blocks
        let mut tensors = BTreeMap::new();
        let n_tensors = header.n_tensors as usize;
        for _ in 0..n_tensors {
            let name = cursor.read_string()?;
            let n_dims = cursor.read_u32()? as usize;
            let mut shape = Vec::with_capacity(n_dims);
            for _ in 0..n_dims {
                shape.push(cursor.read_u64()?);
            }
            let dtype_raw = cursor.read_u32()?;
            let dtype = GgmlType::from_u32(dtype_raw)
                .ok_or(GgufError::UnknownType(dtype_raw))?;
            let offset = cursor.read_u64()?;

            tensors.insert(name.clone(), TensorInfo {
                name,
                shape,
                dtype,
                offset,
            });
        }

        // Compute data offset: align cursor.pos to GGUF_DEFAULT_ALIGNMENT
        let alignment = GGUF_DEFAULT_ALIGNMENT;
        let data_offset = (cursor.pos + alignment - 1) & !(alignment - 1);

        Ok(GgufModel {
            header,
            metadata,
            tensors,
            data_offset,
        })
    }

    /// Get a metadata value as u32
    pub fn get_u32(&self, key: &str) -> Option<u32> {
        match self.metadata.get(key)? {
            GgufValue::Uint32(v) => Some(*v),
            GgufValue::Int32(v) => Some(*v as u32),
            _ => None,
        }
    }

    /// Get a metadata value as f32
    pub fn get_f32(&self, key: &str) -> Option<f32> {
        match self.metadata.get(key)? {
            GgufValue::Float32(v) => Some(*v),
            _ => None,
        }
    }

    /// Get vocabulary tokens (array of strings)
    pub fn get_vocab(&self) -> Option<&Vec<String>> {
        match self.metadata.get("tokenizer.ggml.tokens")? {
            GgufValue::ArrayStr(arr) => Some(arr),
            _ => None,
        }
    }

    /// Get vocab scores (array of f32)
    pub fn get_vocab_scores(&self) -> Option<&Vec<f32>> {
        match self.metadata.get("tokenizer.ggml.scores")? {
            GgufValue::ArrayF32(arr) => Some(arr),
            _ => None,
        }
    }

    /// Get BOS token ID
    pub fn bos_token_id(&self) -> u32 {
        self.get_u32("tokenizer.ggml.bos_token_id").unwrap_or(1)
    }

    /// Get EOS token ID
    pub fn eos_token_id(&self) -> u32 {
        self.get_u32("tokenizer.ggml.eos_token_id").unwrap_or(2)
    }

    /// Get the raw data slice for a named tensor.
    /// Returns None if tensor not found or data out of bounds.
    pub fn tensor_data<'a>(&self, name: &str, file_data: &'a [u8]) -> Option<&'a [u8]> {
        let info = self.tensors.get(name)?;
        let start = self.data_offset + info.offset as usize;
        let size = info.data_size() as usize;
        if start + size > file_data.len() {
            return None;
        }
        Some(&file_data[start..start + size])
    }

    /// Get model config from metadata (SmolLM2/Llama architecture)
    pub fn model_config(&self) -> super::inference::ModelConfig {
        let arch_prefix = match self.metadata.get("general.architecture") {
            Some(GgufValue::Str(s)) => alloc::format!("{}.", s),
            _ => String::from("llama."),
        };

        let dim = self.get_u32(&alloc::format!("{}embedding_length", arch_prefix))
            .unwrap_or(576) as usize;
        let hidden_dim = self.get_u32(&alloc::format!("{}feed_forward_length", arch_prefix))
            .unwrap_or(1536) as usize;
        let n_layers = self.get_u32(&alloc::format!("{}block_count", arch_prefix))
            .unwrap_or(30) as usize;
        let n_heads = self.get_u32(&alloc::format!("{}attention.head_count", arch_prefix))
            .unwrap_or(9) as usize;
        let n_kv_heads = self.get_u32(&alloc::format!("{}attention.head_count_kv", arch_prefix))
            .unwrap_or(3) as usize;
        let vocab_size = self.get_vocab()
            .map(|v| v.len())
            .unwrap_or(
                self.get_u32(&alloc::format!("{}vocab_size", arch_prefix))
                    .unwrap_or(49152) as usize
            );
        let max_seq_len = self.get_u32(&alloc::format!("{}context_length", arch_prefix))
            .unwrap_or(2048) as usize;
        let rope_theta = self.get_f32(&alloc::format!("{}rope.freq_base", arch_prefix))
            .unwrap_or(10000.0);
        let norm_eps = self.get_f32(&alloc::format!("{}attention.layer_norm_rms_epsilon", arch_prefix))
            .unwrap_or(1e-5);

        super::inference::ModelConfig {
            dim,
            hidden_dim,
            n_layers,
            n_heads,
            n_kv_heads,
            vocab_size,
            max_seq_len: max_seq_len.min(256), // limit for memory in QEMU
            rope_theta,
            norm_eps,
        }
    }

    /// Get the total number of elements in a tensor
    pub fn tensor_elements(shape: &[u64]) -> u64 {
        shape.iter().product()
    }
}
