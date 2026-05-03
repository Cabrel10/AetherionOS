// kernel/src/llm/gguf.rs — GGUF v3 Model Format Parser
//
// GGUF (GGML Universal Format) is the standard format for quantized LLMs.
// This parser extracts model metadata and tensor locations from raw bytes,
// enabling direct inference from mmap'd model files.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// GGUF file magic number: "GGUF" in little-endian
const GGUF_MAGIC: u32 = 0x46554747;

/// GGUF file header (24 bytes)
#[derive(Debug, Clone)]
pub struct GgufHeader {
    pub magic: u32,
    pub version: u32,
    pub n_tensors: u64,
    pub n_kv: u64,
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

    /// Size in bytes per element for this type
    pub fn element_size(&self) -> usize {
        match self {
            Self::F32 => 4,
            Self::F16 | Self::BF16 => 2,
            _ => 1, // quantized types use block-based sizing
        }
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

/// Parsed GGUF file
#[derive(Debug)]
pub struct GgufModel {
    pub header: GgufHeader,
    pub tensors: BTreeMap<String, TensorInfo>,
    pub data_offset: usize,
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
}

impl GgufModel {
    /// Parse GGUF header from raw bytes.
    /// Returns the header and validates the magic number.
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

    /// Get the total number of elements in a tensor
    pub fn tensor_elements(shape: &[u64]) -> u64 {
        shape.iter().product()
    }
}
