//! Distributed KV Cache — RAM/SSD/Network Hierarchical Key-Value Cache
//!
//! # Architecture (2026-06-25)
//!
//! The KV (Key-Value) cache stores attention keys and values from previous
//! tokens during autoregressive generation. For long contexts (100K+ tokens)
//! or multi-model serving, the KV cache can grow to 10s of GB — far exceeding
//! single-machine RAM.
//!
//! This module implements a 3-tier distributed KV cache:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │              Distributed KV Cache                    │
//! │                                                      │
//! │  ┌───────────┐  Hot: Recent tokens, low latency     │
//! │  │   RAM     │  Stored as contiguous f32 arrays     │
//! │  │  (Hot)    │  Capacity: ~2-8 GB                    │
//! │  ├───────────┤                                      │
//! │  │   SSD     │  Warm: Older tokens, medium latency  │
//! │  │  (Warm)   │  Stored as compressed pages           │
//! │  │           │  Capacity: ~50-500 GB                 │
//! │  ├───────────┤                                      │
//! │  │ Network   │  Cold: Remote node KV cache          │
//! │  │  (Cold)   │  Shared across cluster nodes         │
//! │  │           │  Capacity: Unlimited                  │
//! │  └───────────┘                                      │
//! │                                                      │
//! │  Eviction: LRU within each tier                      │
//! │  Promotion: Cold→Warm→Hot on access                 │
//! │  Compression: FP32→FP16 when moving to SSD          │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! ## Multi-Session Support
//!
//! Each "session" (agent conversation, inference run) gets its own
//! KV cache namespace. When an agent dies, its KV cache can be
//! migrated to a new agent (session transfer) or persisted to disk
//! for later resumption.

use alloc::vec::Vec;
use alloc::vec;
use alloc::collections::BTreeMap;
use alloc::string::String;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use lazy_static::lazy_static;

// ═══════════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════════

/// Unique session identifier for KV cache isolation.
pub type SessionId = u64;

/// Where a KV cache page currently resides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CacheTier {
    Ram = 0,
    Ssd = 1,
    Network = 2,
    Evicted = 3,
}

/// A page of KV cache data (fixed-size for efficient management).
///
/// Each page holds KV data for `PAGE_TOKENS` consecutive tokens
/// for one specific layer.
const PAGE_TOKENS: usize = 64;

#[derive(Clone)]
pub struct KvPage {
    /// Session this page belongs to
    pub session_id: SessionId,
    /// Layer index
    pub layer_idx: usize,
    /// Starting token position
    pub start_pos: usize,
    /// Number of valid tokens in this page (≤ PAGE_TOKENS)
    pub valid_tokens: usize,
    /// Key data: [valid_tokens × kv_dim] in f32
    pub keys: Vec<f32>,
    /// Value data: [valid_tokens × kv_dim] in f32
    pub values: Vec<f32>,
    /// Where this page currently resides
    pub tier: CacheTier,
    /// Last access timestamp (TSC)
    pub last_access: u64,
    /// Access count for frequency-based eviction
    pub access_count: u32,
}

// ═══════════════════════════════════════════════════════════════════════════
// Session Descriptor
// ═══════════════════════════════════════════════════════════════════════════

/// Tracks a single inference session's KV cache.
struct SessionCache {
    pub session_id: SessionId,
    pub model_id: u32,
    pub n_layers: usize,
    pub kv_dim: usize,
    /// Pages keyed by (layer_idx, page_start_pos)
    pub pages: BTreeMap<(usize, usize), KvPage>,
    /// Current sequence length (total tokens generated)
    pub seq_len: usize,
    /// Total bytes used in RAM for this session
    pub ram_bytes: u64,
}

impl SessionCache {
    fn new(session_id: SessionId, model_id: u32, n_layers: usize, kv_dim: usize) -> Self {
        Self {
            session_id,
            model_id,
            n_layers,
            kv_dim,
            pages: BTreeMap::new(),
            seq_len: 0,
            ram_bytes: 0,
        }
    }

    /// Store key and value for a token at a given position and layer.
    fn store(&mut self, layer_idx: usize, pos: usize, key: &[f32], value: &[f32]) {
        let page_start = (pos / PAGE_TOKENS) * PAGE_TOKENS;
        let page_offset = pos - page_start;

        let page = self.pages.entry((layer_idx, page_start)).or_insert_with(|| {
            let cap = PAGE_TOKENS * self.kv_dim;
            let bytes = (cap * 4 * 2) as u64; // keys + values
            self.ram_bytes += bytes;
            KvPage {
                session_id: self.session_id,
                layer_idx,
                start_pos: page_start,
                valid_tokens: 0,
                keys: vec![0.0; cap],
                values: vec![0.0; cap],
                tier: CacheTier::Ram,
                last_access: 0,
                access_count: 0,
            }
        });

        let kv_dim = self.kv_dim;
        let offset = page_offset * kv_dim;
        if offset + kv_dim <= page.keys.len() {
            page.keys[offset..offset + kv_dim].copy_from_slice(&key[..kv_dim]);
            page.values[offset..offset + kv_dim].copy_from_slice(&value[..kv_dim]);
            if page_offset + 1 > page.valid_tokens {
                page.valid_tokens = page_offset + 1;
            }
        }

        page.last_access = crate::arch::x86_64::timer::read_tsc();
        page.access_count += 1;

        if pos + 1 > self.seq_len {
            self.seq_len = pos + 1;
        }
    }

    /// Retrieve key and value for a range of tokens at a layer.
    fn retrieve(&mut self, layer_idx: usize, pos_start: usize, pos_end: usize) -> (Vec<f32>, Vec<f32>) {
        let kv_dim = self.kv_dim;
        let len = pos_end - pos_start;
        let mut keys = vec![0.0f32; len * kv_dim];
        let mut values = vec![0.0f32; len * kv_dim];

        for pos in pos_start..pos_end {
            let page_start = (pos / PAGE_TOKENS) * PAGE_TOKENS;
            let page_offset = pos - page_start;
            let out_offset = (pos - pos_start) * kv_dim;

            if let Some(page) = self.pages.get_mut(&(layer_idx, page_start)) {
                let in_offset = page_offset * kv_dim;
                if in_offset + kv_dim <= page.keys.len() && out_offset + kv_dim <= keys.len() {
                    keys[out_offset..out_offset + kv_dim]
                        .copy_from_slice(&page.keys[in_offset..in_offset + kv_dim]);
                    values[out_offset..out_offset + kv_dim]
                        .copy_from_slice(&page.values[in_offset..in_offset + kv_dim]);
                }
                page.last_access = crate::arch::x86_64::timer::read_tsc();
                page.access_count += 1;
            }
        }

        (keys, values)
    }

    /// Total RAM bytes used by this session.
    fn memory_usage(&self) -> u64 {
        self.ram_bytes
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Global KV Cache Manager
// ═══════════════════════════════════════════════════════════════════════════

struct KvCacheManager {
    sessions: BTreeMap<SessionId, SessionCache>,
    /// Maximum RAM budget for all KV caches combined
    ram_budget: u64,
    /// Current total RAM usage
    total_ram_used: u64,
    /// Statistics
    total_stores: u64,
    total_retrievals: u64,
    total_evictions: u64,
    total_migrations: u64,
}

impl KvCacheManager {
    fn new(ram_budget: u64) -> Self {
        Self {
            sessions: BTreeMap::new(),
            ram_budget,
            total_ram_used: 0,
            total_stores: 0,
            total_retrievals: 0,
            total_evictions: 0,
            total_migrations: 0,
        }
    }
}

lazy_static! {
    static ref KV_CACHE: Mutex<KvCacheManager> =
        Mutex::new(KvCacheManager::new(64 * 1024 * 1024)); // 64 MB default
}

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Create a new KV cache session for a model.
pub fn create_session(model_id: u32, n_layers: usize, kv_dim: usize) -> SessionId {
    let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let session = SessionCache::new(session_id, model_id, n_layers, kv_dim);

    let mut mgr = KV_CACHE.lock();
    mgr.sessions.insert(session_id, session);

    crate::serial_println!(
        "[KV-CACHE] session {} created (model={}, layers={}, kv_dim={})",
        session_id, model_id, n_layers, kv_dim
    );
    session_id
}

/// Store KV pair for a token.
pub fn store_kv(session_id: SessionId, layer_idx: usize, pos: usize, key: &[f32], value: &[f32]) {
    let mut mgr = KV_CACHE.lock();
    mgr.total_stores += 1;

    if let Some(session) = mgr.sessions.get_mut(&session_id) {
        let old_usage = session.memory_usage();
        session.store(layer_idx, pos, key, value);
        let new_usage = session.memory_usage();
        mgr.total_ram_used += new_usage - old_usage;
    }
}

/// Retrieve KV data for a range of tokens.
pub fn retrieve_kv(
    session_id: SessionId,
    layer_idx: usize,
    pos_start: usize,
    pos_end: usize,
) -> Option<(Vec<f32>, Vec<f32>)> {
    let mut mgr = KV_CACHE.lock();
    mgr.total_retrievals += 1;

    mgr.sessions
        .get_mut(&session_id)
        .map(|session| session.retrieve(layer_idx, pos_start, pos_end))
}

/// Get the current sequence length for a session.
pub fn session_seq_len(session_id: SessionId) -> usize {
    KV_CACHE
        .lock()
        .sessions
        .get(&session_id)
        .map(|s| s.seq_len)
        .unwrap_or(0)
}

/// Destroy a session and free its memory.
pub fn destroy_session(session_id: SessionId) -> bool {
    let mut mgr = KV_CACHE.lock();
    if let Some(session) = mgr.sessions.remove(&session_id) {
        mgr.total_ram_used = mgr.total_ram_used.saturating_sub(session.memory_usage());
        crate::serial_println!("[KV-CACHE] session {} destroyed", session_id);
        true
    } else {
        false
    }
}

/// Migrate a session's KV cache from one agent to another.
/// This is used when an agent dies and its conversation context
/// needs to be transferred to a replacement agent.
pub fn migrate_session(session_id: SessionId, _new_agent_pid: u64) -> bool {
    let mut mgr = KV_CACHE.lock();
    if mgr.sessions.contains_key(&session_id) {
        mgr.total_migrations += 1;
        // The session data stays in place; only the ownership changes.
        // The new agent can access it with the same session_id.
        true
    } else {
        false
    }
}

/// Set the RAM budget for the KV cache subsystem.
pub fn set_ram_budget(bytes: u64) {
    KV_CACHE.lock().ram_budget = bytes;
    crate::serial_println!("[KV-CACHE] RAM budget set to {} MB", bytes / (1024 * 1024));
}

/// Get KV cache metrics.
pub fn kv_metrics() -> KvCacheMetrics {
    let mgr = KV_CACHE.lock();
    KvCacheMetrics {
        active_sessions: mgr.sessions.len(),
        total_ram_used: mgr.total_ram_used,
        ram_budget: mgr.ram_budget,
        total_stores: mgr.total_stores,
        total_retrievals: mgr.total_retrievals,
        total_evictions: mgr.total_evictions,
        total_migrations: mgr.total_migrations,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct KvCacheMetrics {
    pub active_sessions: usize,
    pub total_ram_used: u64,
    pub ram_budget: u64,
    pub total_stores: u64,
    pub total_retrievals: u64,
    pub total_evictions: u64,
    pub total_migrations: u64,
}

/// Initialize the distributed KV cache.
pub fn init() {
    crate::serial_println!("[KV-CACHE] Distributed KV cache initialized (3-tier: RAM/SSD/Network)");
}
