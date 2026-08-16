//! Compute Graph Runtime — Node/Tensor/Operation/Backend Abstraction
//!
//! # Architecture (2026-06-25)
//!
//! The Compute Graph decomposes any model into a DAG (Directed Acyclic Graph)
//! of operations on tensors. This is the universal representation that lets
//! AetherionOS "swallow" ANY model format — GGUF, ONNX, PyTorch, TensorFlow,
//! SafeTensors, TFLite — by converting them to a common compute graph.
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │                  Compute Graph                       │
//! │                                                      │
//! │  Input[0] ──→ Embedding ──→ RMSNorm ──→ MatMul(Q) │
//! │                                            ↓         │
//! │  Input[0] ──→ ... ──→ MatMul(K) → RoPE → Attention │
//! │                                            ↓         │
//! │                              ... → SiLU → MatMul → + │
//! │                                            ↓         │
//! │                              Logits → Softmax → Out  │
//! │                                                      │
//! │  Each node: (Operation, Input Tensors, Output Tensor)│
//! │  Each edge: Tensor reference (zero-copy sharing)     │
//! │  Each op: Dispatched to active ComputeBackend        │
//! └──────────────────────────────────────────────────────┘
//! ```

use alloc::vec::Vec;
use alloc::string::String;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use lazy_static::lazy_static;

// ═══════════════════════════════════════════════════════════════════════════
// Tensor
// ═══════════════════════════════════════════════════════════════════════════

/// Unique tensor identifier within a graph.
pub type TensorId = u32;

/// Data type of tensor elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DType {
    F32 = 0,
    F16 = 1,
    BF16 = 2,
    Q8_0 = 3,
    Q4_0 = 4,
    Q4_1 = 5,
    I8 = 6,
    I32 = 7,
    U8 = 8,
}

impl DType {
    /// Bytes per element (for non-quantized types).
    /// Quantized types return the block size in bytes / elements per block.
    pub fn element_size(&self) -> usize {
        match self {
            DType::F32 | DType::I32 => 4,
            DType::F16 | DType::BF16 => 2,
            DType::I8 | DType::U8 => 1,
            // Quantized: approximate bytes per element
            DType::Q8_0 => 1, // 34 bytes per 32 elements ≈ 1.0625
            DType::Q4_0 => 1, // 18 bytes per 32 elements ≈ 0.5625
            DType::Q4_1 => 1, // 20 bytes per 32 elements
        }
    }
}

/// A tensor in the compute graph.
#[derive(Debug, Clone)]
pub struct Tensor {
    pub id: TensorId,
    pub name: String,
    pub shape: Vec<usize>,    // e.g., [batch, seq_len, dim] or [dim_out, dim_in]
    pub dtype: DType,
    pub data_offset: u64,      // Offset into weight buffer (0 for computed tensors)
    pub data_size_bytes: u64,
    pub is_weight: bool,       // True for model parameters, false for activations
    pub is_computed: bool,     // True if this tensor is an output of an operation
}

impl Tensor {
    /// Total number of elements in this tensor.
    pub fn numel(&self) -> usize {
        self.shape.iter().product()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Operation Types
// ═══════════════════════════════════════════════════════════════════════════

/// All supported operations in the compute graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OpType {
    /// Embedding lookup: out[i] = embedding_table[input[i]]
    EmbeddingLookup = 0,
    /// Matrix multiply: out = input × weight
    MatMul = 1,
    /// RMS normalization: out = rmsnorm(input, weight, eps)
    RmsNorm = 2,
    /// Softmax: out = softmax(input)
    Softmax = 3,
    /// SiLU activation: out = x * sigmoid(x)
    SiLU = 4,
    /// RoPE: apply rotary position embedding
    RoPE = 5,
    /// Element-wise multiply: out = a * b
    Mul = 6,
    /// Element-wise add: out = a + b (residual connection)
    Add = 7,
    /// Attention: out = softmax(Q×K^T/√d) × V
    Attention = 8,
    /// Transpose: out = input^T
    Transpose = 9,
    /// Reshape: out = reshape(input, new_shape)
    Reshape = 10,
    /// Concatenate: out = concat(inputs, axis)
    Concat = 11,
    /// Split: split input into N equal parts along axis
    Split = 12,
    /// Greedy argmax sampling: out = argmax(logits)
    ArgMax = 13,
    /// Top-K sampling: out = sample(logits, k, temperature)
    TopKSample = 14,
    /// Copy/identity (used for graph edges that cross backends)
    Copy = 15,
}

// ═══════════════════════════════════════════════════════════════════════════
// Graph Node
// ═══════════════════════════════════════════════════════════════════════════

/// A node in the compute graph representing one operation.
#[derive(Debug, Clone)]
pub struct GraphNode {
    /// Unique node ID
    pub id: u32,
    /// Operation type
    pub op: OpType,
    /// Input tensor IDs (ordered)
    pub inputs: Vec<TensorId>,
    /// Output tensor ID
    pub output: TensorId,
    /// Operation-specific parameters (e.g., eps for RmsNorm, position for RoPE)
    pub params: OpParams,
    /// Which backend should execute this op (None = use default active backend)
    pub preferred_backend: Option<super::BackendType>,
    /// Estimated FLOPs for this operation (for scheduling)
    pub estimated_flops: u64,
}

/// Operation-specific parameters.
#[derive(Debug, Clone)]
pub enum OpParams {
    None,
    RmsNorm { eps: f32 },
    RoPE { position: usize, head_dim: usize },
    Attention { n_heads: usize, n_kv_heads: usize, head_dim: usize, seq_len: usize },
    MatMul { transpose_b: bool },
    TopK { k: usize, temperature: f32 },
    Reshape { new_shape: Vec<usize> },
    Split { n_parts: usize, axis: usize },
}

// ═══════════════════════════════════════════════════════════════════════════
// Compute Graph
// ═══════════════════════════════════════════════════════════════════════════

/// A complete compute graph representing a model's forward pass.
#[derive(Debug)]
pub struct ComputeGraph {
    /// Model identifier
    pub model_id: u32,
    /// Human-readable model name
    pub model_name: String,
    /// All tensors (weights + activations)
    pub tensors: Vec<Tensor>,
    /// All operations in topological order
    pub nodes: Vec<GraphNode>,
    /// Execution order (node indices in the order they should execute)
    pub execution_order: Vec<usize>,
    /// Total estimated FLOPs for one forward pass
    pub total_flops: u64,
}

impl ComputeGraph {
    /// Create a new empty compute graph.
    pub fn new(model_id: u32, model_name: &str) -> Self {
        Self {
            model_id,
            model_name: String::from(model_name),
            tensors: Vec::new(),
            nodes: Vec::new(),
            execution_order: Vec::new(),
            total_flops: 0,
        }
    }

    /// Add a tensor to the graph. Returns its TensorId.
    pub fn add_tensor(
        &mut self,
        name: &str,
        shape: Vec<usize>,
        dtype: DType,
        is_weight: bool,
    ) -> TensorId {
        let id = self.tensors.len() as TensorId;
        let numel: usize = shape.iter().product();
        let size = (numel * dtype.element_size()) as u64;
        self.tensors.push(Tensor {
            id,
            name: String::from(name),
            shape,
            dtype,
            data_offset: 0,
            data_size_bytes: size,
            is_weight,
            is_computed: false,
        });
        id
    }

    /// Add an operation node to the graph.
    pub fn add_node(
        &mut self,
        op: OpType,
        inputs: Vec<TensorId>,
        output: TensorId,
        params: OpParams,
    ) -> u32 {
        let id = self.nodes.len() as u32;

        // Estimate FLOPs based on operation type and tensor shapes
        let estimated_flops = self.estimate_flops(op, &inputs, output);

        self.nodes.push(GraphNode {
            id,
            op,
            inputs,
            output,
            params,
            preferred_backend: None,
            estimated_flops,
        });

        // Mark output tensor as computed
        if (output as usize) < self.tensors.len() {
            self.tensors[output as usize].is_computed = true;
        }

        self.total_flops += estimated_flops;
        id
    }

    /// Build the execution order via topological sort.
    pub fn build_execution_order(&mut self) {
        // Simple topological sort: nodes are already in dependency order
        // because we add them sequentially during graph construction.
        self.execution_order = (0..self.nodes.len()).collect();
    }

    /// Estimate FLOPs for an operation based on input/output tensor shapes.
    fn estimate_flops(&self, op: OpType, inputs: &[TensorId], _output: TensorId) -> u64 {
        match op {
            OpType::MatMul => {
                // C = A × B: FLOPs = 2 × M × N × K
                if inputs.len() >= 2 {
                    let a = &self.tensors[inputs[0] as usize];
                    let b = &self.tensors[inputs[1] as usize];
                    let m = *a.shape.first().unwrap_or(&1) as u64;
                    let k = *a.shape.last().unwrap_or(&1) as u64;
                    let n = *b.shape.first().unwrap_or(&1) as u64;
                    2 * m * n * k
                } else {
                    0
                }
            }
            OpType::Attention => {
                // Approximate: 4 × seq_len × dim² (Q, K, V projections + output)
                if let Some(a) = inputs.first() {
                    let dim = *self.tensors[*a as usize].shape.last().unwrap_or(&1) as u64;
                    4 * dim * dim
                } else {
                    0
                }
            }
            OpType::Softmax | OpType::SiLU | OpType::RmsNorm | OpType::RoPE => {
                // Element-wise: FLOPs ≈ 5 × numel
                if let Some(a) = inputs.first() {
                    5 * self.tensors[*a as usize].numel() as u64
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    /// Get total weight memory footprint.
    pub fn weight_memory_bytes(&self) -> u64 {
        self.tensors
            .iter()
            .filter(|t| t.is_weight)
            .map(|t| t.data_size_bytes)
            .sum()
    }

    /// Get total activation memory footprint.
    pub fn activation_memory_bytes(&self) -> u64 {
        self.tensors
            .iter()
            .filter(|t| !t.is_weight)
            .map(|t| t.data_size_bytes)
            .sum()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Graph Execution Engine
// ═══════════════════════════════════════════════════════════════════════════

static GRAPHS_EXECUTED: AtomicU64 = AtomicU64::new(0);
static NODES_EXECUTED: AtomicU64 = AtomicU64::new(0);

/// Execute a compute graph.
///
/// This is the main entry point for model inference. The engine walks
/// the graph in execution order and dispatches each operation to the
/// active compute backend.
///
/// NOTE: This is the graph-level orchestrator. Actual tensor data
/// management and backend dispatch are handled by the respective
/// subsystems (layer_scheduler for weights, ComputeBackend for ops).
pub fn execute_graph(graph: &ComputeGraph) -> u64 {
    let start = crate::arch::x86_64::timer::read_tsc();

    for &node_idx in &graph.execution_order {
        let node = &graph.nodes[node_idx];

        // Dispatch to the appropriate backend
        // In the future, each node can target a different backend
        // (e.g., attention on GPU, normalization on CPU)
        let _backend = super::active_backend();

        // TODO: Actual dispatch based on OpType
        // For now, the inference engine still uses its own direct calls.
        // The graph runtime will replace those once fully integrated.

        NODES_EXECUTED.fetch_add(1, Ordering::Relaxed);
    }

    GRAPHS_EXECUTED.fetch_add(1, Ordering::Relaxed);
    let elapsed = crate::arch::x86_64::timer::read_tsc() - start;
    elapsed
}

/// Get graph execution metrics.
pub fn graph_metrics() -> GraphMetrics {
    GraphMetrics {
        graphs_executed: GRAPHS_EXECUTED.load(Ordering::Relaxed),
        nodes_executed: NODES_EXECUTED.load(Ordering::Relaxed),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GraphMetrics {
    pub graphs_executed: u64,
    pub nodes_executed: u64,
}

/// Initialize the compute graph runtime.
pub fn init() {
    crate::serial_println!("[GRAPH] Compute graph runtime initialized");
}
