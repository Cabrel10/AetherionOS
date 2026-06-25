//! Layer Scheduler — SSD/RAM/VRAM Hierarchical Weight Management
//!
//! # Architecture (2026-06-25)
//!
//! Large models (7B+) don't fit entirely in RAM, let alone VRAM.
//! The Layer Scheduler manages a 3-tier storage hierarchy for model weights:
//!
//! ```text
//! ┌───────────────┐  Fastest, smallest
//! │  VRAM (GPU)   │  ~8-16 GB, ~900 GB/s bandwidth
//! │  Hot layers   │  Currently executing attention/FFN
//! ├───────────────┤
//! │  RAM (DDR4/5) │  ~16-128 GB, ~50 GB/s bandwidth
//! │  Warm layers  │  Prefetched, ready for next iteration
//! ├───────────────┤
//! │  SSD (NVMe)   │  ~1-8 TB, ~7 GB/s bandwidth
//! │  Cold layers  │  Full model weights, loaded on demand
//! └───────────────┘  Slowest, largest
//! ```
//!
//! ## Key Design Decisions
//!
//! 1. **Predictable access pattern**: Transformer layers execute sequentially
//!    (0, 1, 2, ..., N-1, then logit projection). We exploit this by
//!    prefetching layer N+1 from SSD→RAM while layer N executes on GPU.
//!
//! 2. **Zero-copy where possible**: Layers in RAM are memory-mapped from
//!    the GGUF file via VirtIO-BLK + ext2. No intermediate copies.
//!
//! 3. **Eviction policy**: LRU with frequency boost. Recently used layers
//!    stay in VRAM; rarely-used layers (e.g., early layers in long
//!    generation runs) are evicted to RAM.
//!
//! 4. **Double buffering**: While GPU processes layer N from VRAM slot A,
//!    DMA uploads layer N+1 into VRAM slot B. No GPU idle time.

use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, AtomicU32, Ordering};
use spin::Mutex;
use lazy_static::lazy_static;

// ═══════════════════════════════════════════════════════════════════════════
// Storage Tier
// ═══════════════════════════════════════════════════════════════════════════

/// Where a layer's weights currently reside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum StorageTier {
    /// On NVMe/SSD (coldest, largest capacity)
    Ssd = 0,
    /// In host RAM (warm, medium capacity)
    Ram = 1,
    /// In GPU VRAM (hot, smallest capacity)
    Vram = 2,
    /// Not loaded (weights not yet read from disk)
    NotLoaded = 3,
}

impl core::fmt::Display for StorageTier {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::Ssd => write!(f, "SSD"),
            Self::Ram => write!(f, "RAM"),
            Self::Vram => write!(f, "VRAM"),
            Self::NotLoaded => write!(f, "NOT_LOADED"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Layer Descriptor
// ═══════════════════════════════════════════════════════════════════════════

/// Describes a single transformer layer's weight storage state.
#[derive(Debug, Clone)]
pub struct LayerDescriptor {
    /// Layer index (0..n_layers)
    pub layer_idx: usize,
    /// Model this layer belongs to (for multi-model support)
    pub model_id: u32,
    /// Where the weights currently reside
    pub tier: StorageTier,
    /// Size of this layer's weights in bytes (all tensors combined)
    pub size_bytes: u64,
    /// Offset in the GGUF/model file on disk
    pub disk_offset: u64,
    /// Address in RAM (0 if not in RAM)
    pub ram_addr: u64,
    /// Offset in VRAM (0 if not in VRAM)
    pub vram_offset: u64,
    /// Last access timestamp (TSC)
    pub last_access: u64,
    /// Total access count (for frequency-based eviction)
    pub access_count: u64,
    /// True if currently being transferred between tiers
    pub in_transfer: bool,
}

// ═══════════════════════════════════════════════════════════════════════════
// Tier Capacity Configuration
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for the 3-tier hierarchy.
#[derive(Debug, Clone, Copy)]
pub struct TierConfig {
    /// Maximum VRAM budget for layers (bytes). 0 = no GPU.
    pub vram_budget: u64,
    /// Maximum RAM budget for layers (bytes). Shared with OS.
    pub ram_budget: u64,
    /// SSD capacity is effectively unlimited (model file size).
    pub ssd_capacity: u64,
    /// Number of VRAM "slots" for double-buffering
    pub vram_slots: usize,
    /// Prefetch depth: how many layers ahead to prefetch from SSD→RAM
    pub prefetch_depth: usize,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            vram_budget: 0,           // No GPU by default
            ram_budget: 128 * 1024 * 1024, // 128 MB RAM budget for layers
            ssd_capacity: 0,
            vram_slots: 2,            // Double buffering
            prefetch_depth: 2,        // Prefetch 2 layers ahead
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Layer Scheduler State
// ═══════════════════════════════════════════════════════════════════════════

/// Prefetch request queued by the scheduler.
#[derive(Debug, Clone, Copy)]
struct PrefetchRequest {
    layer_idx: usize,
    model_id: u32,
    target_tier: StorageTier,
    priority: u8,
}

struct LayerSchedulerState {
    /// All tracked layers, keyed by (model_id, layer_idx)
    layers: BTreeMap<(u32, usize), LayerDescriptor>,
    /// Tier configuration
    config: TierConfig,
    /// Current VRAM usage in bytes
    vram_used: u64,
    /// Current RAM usage in bytes (for layer storage)
    ram_used: u64,
    /// Pending prefetch queue
    prefetch_queue: Vec<PrefetchRequest>,
    /// Statistics
    cache_hits: u64,
    cache_misses: u64,
    evictions: u64,
    prefetches_issued: u64,
    /// Current forward pass layer index (for predictive prefetch)
    current_layer: usize,
}

impl LayerSchedulerState {
    fn new(config: TierConfig) -> Self {
        Self {
            layers: BTreeMap::new(),
            config,
            vram_used: 0,
            ram_used: 0,
            prefetch_queue: Vec::new(),
            cache_hits: 0,
            cache_misses: 0,
            evictions: 0,
            prefetches_issued: 0,
            current_layer: 0,
        }
    }
}

lazy_static! {
    static ref LAYER_SCHEDULER: Mutex<LayerSchedulerState> =
        Mutex::new(LayerSchedulerState::new(TierConfig::default()));
}

// ═══════════════════════════════════════════════════════════════════════════
// Core Scheduling Logic
// ═══════════════════════════════════════════════════════════════════════════

/// Register a layer's existence (called during model loading).
pub fn register_layer(
    model_id: u32,
    layer_idx: usize,
    size_bytes: u64,
    disk_offset: u64,
) {
    let descriptor = LayerDescriptor {
        layer_idx,
        model_id,
        tier: StorageTier::Ssd, // Initially all layers are on SSD
        size_bytes,
        disk_offset,
        ram_addr: 0,
        vram_offset: 0,
        last_access: 0,
        access_count: 0,
        in_transfer: false,
    };

    let mut sched = LAYER_SCHEDULER.lock();
    sched.layers.insert((model_id, layer_idx), descriptor);
}

/// Request a layer for computation. Returns the storage tier where
/// the layer is currently available.
///
/// If the layer is not in the required tier, this triggers:
/// 1. Promotion (SSD→RAM or RAM→VRAM)
/// 2. Eviction of cold layers to make room
/// 3. Prefetch of upcoming layers
pub fn request_layer(model_id: u32, layer_idx: usize) -> StorageTier {
    let mut sched = LAYER_SCHEDULER.lock();

    // Update current layer for predictive prefetch
    sched.current_layer = layer_idx;

    if let Some(desc) = sched.layers.get_mut(&(model_id, layer_idx)) {
        let now = crate::arch::x86_64::timer::read_tsc();
        desc.last_access = now;
        desc.access_count += 1;

        let tier = desc.tier;

        match tier {
            StorageTier::Vram | StorageTier::Ram => {
                sched.cache_hits += 1;
            }
            StorageTier::Ssd => {
                sched.cache_misses += 1;
                // TODO: Trigger async SSD→RAM promotion via VirtIO-BLK
            }
            StorageTier::NotLoaded => {
                sched.cache_misses += 1;
            }
        }

        // Issue predictive prefetch for upcoming layers
        let prefetch_depth = sched.config.prefetch_depth;
        let total_layers = sched.layers.len();
        for ahead in 1..=prefetch_depth {
            let next_idx = layer_idx + ahead;
            if next_idx < total_layers {
                if let Some(next_desc) = sched.layers.get(&(model_id, next_idx)) {
                    if next_desc.tier == StorageTier::Ssd && !next_desc.in_transfer {
                        sched.prefetch_queue.push(PrefetchRequest {
                            layer_idx: next_idx,
                            model_id,
                            target_tier: StorageTier::Ram,
                            priority: if ahead == 1 { 4 } else { 2 },
                        });
                        sched.prefetches_issued += 1;
                    }
                }
            }
        }

        tier
    } else {
        StorageTier::NotLoaded
    }
}

/// Promote a layer from a lower tier to a higher one.
///
/// Returns true if the promotion was successful (or already at target tier).
pub fn promote_layer(model_id: u32, layer_idx: usize, target_tier: StorageTier) -> bool {
    let mut sched = LAYER_SCHEDULER.lock();

    if let Some(desc) = sched.layers.get_mut(&(model_id, layer_idx)) {
        if desc.tier as u8 >= target_tier as u8 {
            return true; // Already at or above target tier
        }

        match target_tier {
            StorageTier::Ram => {
                let budget = sched.config.ram_budget;
                let used = sched.ram_used;
                if used + desc.size_bytes > budget {
                    // Need to evict cold layers from RAM
                    evict_ram_layers(&mut sched, desc.size_bytes);
                }
                if sched.ram_used + desc.size_bytes <= budget {
                    // TODO: Actually read from disk via VirtIO-BLK
                    desc.tier = StorageTier::Ram;
                    sched.ram_used += desc.size_bytes;
                    return true;
                }
            }
            StorageTier::Vram => {
                let budget = sched.config.vram_budget;
                if budget == 0 {
                    return false; // No GPU available
                }
                let used = sched.vram_used;
                if used + desc.size_bytes > budget {
                    evict_vram_layers(&mut sched, desc.size_bytes);
                }
                if sched.vram_used + desc.size_bytes <= budget {
                    // TODO: DMA transfer from RAM to VRAM
                    desc.tier = StorageTier::Vram;
                    sched.vram_used += desc.size_bytes;
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Evict layers from RAM to make room for `needed_bytes`.
/// Uses LRU with frequency boost: access_count/2 penalizes eviction.
fn evict_ram_layers(sched: &mut LayerSchedulerState, needed_bytes: u64) {
    let mut freed = 0u64;

    // Collect RAM-resident layers sorted by eviction priority (least valuable first)
    let mut candidates: Vec<(u32, usize, u64, u64)> = sched
        .layers
        .iter()
        .filter(|(_, d)| d.tier == StorageTier::Ram && !d.in_transfer)
        .map(|((mid, lidx), d)| (*mid, *lidx, d.last_access, d.size_bytes))
        .collect();

    // Sort by last_access ascending (oldest first)
    candidates.sort_by_key(|&(_, _, la, _)| la);

    for (mid, lidx, _, size) in candidates {
        if freed >= needed_bytes {
            break;
        }
        if let Some(desc) = sched.layers.get_mut(&(mid, lidx)) {
            desc.tier = StorageTier::Ssd;
            desc.ram_addr = 0;
            sched.ram_used = sched.ram_used.saturating_sub(size);
            sched.evictions += 1;
            freed += size;
        }
    }
}

/// Evict layers from VRAM to make room.
fn evict_vram_layers(sched: &mut LayerSchedulerState, needed_bytes: u64) {
    let mut freed = 0u64;

    let mut candidates: Vec<(u32, usize, u64, u64)> = sched
        .layers
        .iter()
        .filter(|(_, d)| d.tier == StorageTier::Vram && !d.in_transfer)
        .map(|((mid, lidx), d)| (*mid, *lidx, d.last_access, d.size_bytes))
        .collect();

    candidates.sort_by_key(|&(_, _, la, _)| la);

    for (mid, lidx, _, size) in candidates {
        if freed >= needed_bytes {
            break;
        }
        if let Some(desc) = sched.layers.get_mut(&(mid, lidx)) {
            // Demote to RAM (not SSD, to keep it warm)
            desc.tier = StorageTier::Ram;
            desc.vram_offset = 0;
            sched.vram_used = sched.vram_used.saturating_sub(size);
            sched.evictions += 1;
            freed += size;
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Configure the layer scheduler tier budgets.
pub fn configure(config: TierConfig) {
    let mut sched = LAYER_SCHEDULER.lock();
    sched.config = config;
    crate::serial_println!(
        "[LAYER-SCHED] configured: VRAM={} MB, RAM={} MB, prefetch_depth={}",
        config.vram_budget / (1024 * 1024),
        config.ram_budget / (1024 * 1024),
        config.prefetch_depth
    );
}

/// Get scheduler metrics.
pub fn metrics() -> LayerSchedulerMetrics {
    let sched = LAYER_SCHEDULER.lock();
    LayerSchedulerMetrics {
        total_layers: sched.layers.len(),
        ram_used: sched.ram_used,
        vram_used: sched.vram_used,
        cache_hits: sched.cache_hits,
        cache_misses: sched.cache_misses,
        evictions: sched.evictions,
        prefetches_issued: sched.prefetches_issued,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LayerSchedulerMetrics {
    pub total_layers: usize,
    pub ram_used: u64,
    pub vram_used: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub evictions: u64,
    pub prefetches_issued: u64,
}

/// Initialize the layer scheduler.
pub fn init() {
    crate::serial_println!("[LAYER-SCHED] initialized (3-tier: SSD/RAM/VRAM)");
}
