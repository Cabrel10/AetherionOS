//! Unified Persistent Memory — Short-Term / Long-Term / Vector / Episodic
//!
//! # Architecture (2026-06-25)
//!
//! AetherionOS agents need memory that **persists across sessions** and
//! supports both precise recall (key-value) and semantic search (vector).
//! This module implements a 4-tier memory hierarchy:
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │                   Unified Memory API                    │
//! │          store() / recall() / search() / forget()      │
//! ├─────────┬───────────┬─────────────┬────────────────────┤
//! │  L1:    │  L2:      │  L3:        │  L4:               │
//! │ SHORT   │ LONG      │ VECTOR      │ EPISODIC           │
//! │ TERM    │ TERM      │ STORE       │ MEMORY             │
//! │         │           │             │                    │
//! │ Ring    │ BTreeMap  │ f32 vectors │ Action sequences   │
//! │ buffer  │ persistent│ + cosine    │ + crystallization  │
//! │ in RAM  │ to disk   │ similarity  │ into reflexes      │
//! │         │           │             │                    │
//! │ 256 max │ Unlimited │ 4096 max    │ 1024 max           │
//! │ ~10min  │ Forever   │ Forever     │ Decays over time   │
//! │ TTL     │           │             │                    │
//! └─────────┴───────────┴─────────────┴────────────────────┘
//! ```
//!
//! ## L1: Short-Term Memory (STM)
//! - Ring buffer of recent observations/messages per agent
//! - Automatic eviction after TTL or capacity limit
//! - Used for: conversation context, recent tool outputs, working memory
//!
//! ## L2: Long-Term Memory (LTM)
//! - Persistent key-value store backed by ext2/VFS
//! - Survived reboots via `/disk/var/memory.db` serialization
//! - Used for: user preferences, learned facts, configuration
//!
//! ## L3: Vector Store
//! - Dense f32 vectors with cosine similarity search
//! - Used for: RAG retrieval, semantic search, embedding cache
//! - Top-K nearest neighbor search with configurable threshold
//!
//! ## L4: Episodic Memory
//! - Sequences of actions with outcomes (success/failure)
//! - The Reflex Engine (see `ipc::bus::ReflexEngine`) can crystallize
//!   repeated successful sequences into automatic reflexes
//! - Used for: learning from experience, pattern detection

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use lazy_static::lazy_static;

// ═══════════════════════════════════════════════════════════════════════════
// Memory Entry Types
// ═══════════════════════════════════════════════════════════════════════════

/// Unique memory entry identifier.
pub type MemoryId = u64;

/// Memory tier classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MemoryTier {
    ShortTerm = 0,
    LongTerm = 1,
    Vector = 2,
    Episodic = 3,
}

/// A single short-term memory entry.
#[derive(Debug, Clone)]
pub struct ShortTermEntry {
    pub id: MemoryId,
    pub agent_pid: u64,
    pub key: String,
    pub value: String,
    pub created_at: u64,    // TSC timestamp
    pub ttl_ticks: u64,     // Time-to-live in TSC ticks (0 = never expires)
    pub access_count: u32,
}

/// A long-term memory entry (persisted to disk).
#[derive(Debug, Clone)]
pub struct LongTermEntry {
    pub id: MemoryId,
    pub agent_pid: u64,
    pub key: String,
    pub value: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub category: String,   // "fact", "preference", "config", etc.
    pub confidence: f32,    // 0.0 - 1.0 (how certain we are)
}

/// A vector store entry for semantic search.
#[derive(Debug, Clone)]
pub struct VectorEntry {
    pub id: MemoryId,
    pub agent_pid: u64,
    pub embedding: Vec<f32>,  // Dense vector (typically dim=64, 128, 256, or 384)
    pub text: String,          // The text this embedding represents
    pub metadata: String,      // JSON metadata (source, timestamp, etc.)
}

/// An episodic memory entry (action sequence with outcome).
#[derive(Debug, Clone)]
pub struct EpisodicEntry {
    pub id: MemoryId,
    pub agent_pid: u64,
    pub actions: Vec<EpisodicAction>,
    pub outcome: EpisodicOutcome,
    pub created_at: u64,
    pub replay_count: u32,     // How many times this sequence was replayed
    pub crystallized: bool,    // True if converted to a reflex rule
}

/// A single action in an episodic sequence.
#[derive(Debug, Clone)]
pub struct EpisodicAction {
    pub intent_id: u32,
    pub description: String,
    pub timestamp: u64,
}

/// Outcome of an episodic sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EpisodicOutcome {
    Success = 0,
    Failure = 1,
    Partial = 2,
    Unknown = 3,
}

// ═══════════════════════════════════════════════════════════════════════════
// Memory Store Implementations
// ═══════════════════════════════════════════════════════════════════════════

/// L1: Short-Term Memory — ring buffer with TTL
struct ShortTermStore {
    entries: Vec<ShortTermEntry>,
    max_size: usize,
}

impl ShortTermStore {
    fn new(max_size: usize) -> Self {
        Self {
            entries: Vec::with_capacity(max_size),
            max_size,
        }
    }

    /// Store a new short-term memory. Evicts oldest if at capacity.
    fn store(&mut self, entry: ShortTermEntry) {
        // Evict expired entries first
        let now = crate::arch::x86_64::timer::read_tsc();
        self.entries.retain(|e| {
            e.ttl_ticks == 0 || (now - e.created_at) < e.ttl_ticks
        });

        // Evict oldest if still at capacity
        if self.entries.len() >= self.max_size {
            self.entries.remove(0);
        }

        self.entries.push(entry);
    }

    /// Recall by key (exact match). Returns most recent match.
    fn recall(&mut self, agent_pid: u64, key: &str) -> Option<&ShortTermEntry> {
        let now = crate::arch::x86_64::timer::read_tsc();
        // Find last matching entry that hasn't expired
        self.entries.iter().rev().find(|e| {
            e.agent_pid == agent_pid
                && e.key == key
                && (e.ttl_ticks == 0 || (now - e.created_at) < e.ttl_ticks)
        })
    }

    /// Get all entries for an agent (most recent first).
    fn recent(&self, agent_pid: u64, limit: usize) -> Vec<&ShortTermEntry> {
        let now = crate::arch::x86_64::timer::read_tsc();
        self.entries
            .iter()
            .rev()
            .filter(|e| {
                e.agent_pid == agent_pid
                    && (e.ttl_ticks == 0 || (now - e.created_at) < e.ttl_ticks)
            })
            .take(limit)
            .collect()
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// L2: Long-Term Memory — persistent key-value store
struct LongTermStore {
    entries: BTreeMap<String, LongTermEntry>,  // keyed by "pid:category:key"
}

impl LongTermStore {
    fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    fn composite_key(agent_pid: u64, category: &str, key: &str) -> String {
        format!("{}:{}:{}", agent_pid, category, key)
    }

    /// Store or update a long-term memory.
    fn store(&mut self, entry: LongTermEntry) {
        let ck = Self::composite_key(entry.agent_pid, &entry.category, &entry.key);
        self.entries.insert(ck, entry);
    }

    /// Recall by exact key.
    fn recall(&self, agent_pid: u64, category: &str, key: &str) -> Option<&LongTermEntry> {
        let ck = Self::composite_key(agent_pid, category, key);
        self.entries.get(&ck)
    }

    /// List all memories for an agent in a category.
    fn list_category(&self, agent_pid: u64, category: &str) -> Vec<&LongTermEntry> {
        let prefix = format!("{}:{}:", agent_pid, category);
        self.entries
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v)
            .collect()
    }

    /// Delete a memory.
    fn forget(&mut self, agent_pid: u64, category: &str, key: &str) -> bool {
        let ck = Self::composite_key(agent_pid, category, key);
        self.entries.remove(&ck).is_some()
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// L3: Vector Store — dense vectors with cosine similarity
struct VectorStore {
    entries: Vec<VectorEntry>,
    max_size: usize,
}

impl VectorStore {
    fn new(max_size: usize) -> Self {
        Self {
            entries: Vec::with_capacity(max_size.min(128)),
            max_size,
        }
    }

    /// Add a vector to the store. Evicts oldest if at capacity.
    fn store(&mut self, entry: VectorEntry) {
        if self.entries.len() >= self.max_size {
            self.entries.remove(0);
        }
        self.entries.push(entry);
    }

    /// Search for top-K nearest neighbors by cosine similarity.
    ///
    /// Returns entries with similarity >= threshold, sorted by similarity descending.
    fn search(
        &self,
        agent_pid: u64,
        query: &[f32],
        top_k: usize,
        threshold: f32,
    ) -> Vec<(f32, &VectorEntry)> {
        let mut results: Vec<(f32, &VectorEntry)> = self
            .entries
            .iter()
            .filter(|e| e.agent_pid == agent_pid || agent_pid == 0)
            .filter_map(|e| {
                let sim = cosine_similarity(query, &e.embedding);
                if sim >= threshold {
                    Some((sim, e))
                } else {
                    None
                }
            })
            .collect();

        // Sort by similarity descending
        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(core::cmp::Ordering::Equal));
        results.truncate(top_k);
        results
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// L4: Episodic Memory — action sequences with outcomes
struct EpisodicStore {
    entries: Vec<EpisodicEntry>,
    max_size: usize,
}

impl EpisodicStore {
    fn new(max_size: usize) -> Self {
        Self {
            entries: Vec::with_capacity(max_size.min(128)),
            max_size,
        }
    }

    /// Record a new episode.
    fn store(&mut self, entry: EpisodicEntry) {
        if self.entries.len() >= self.max_size {
            // Evict oldest non-crystallized entry
            if let Some(pos) = self.entries.iter().position(|e| !e.crystallized) {
                self.entries.remove(pos);
            } else {
                self.entries.remove(0);
            }
        }
        self.entries.push(entry);
    }

    /// Find episodes matching a pattern (by first action's intent_id).
    fn find_pattern(&self, agent_pid: u64, trigger_intent: u32) -> Vec<&EpisodicEntry> {
        self.entries
            .iter()
            .filter(|e| {
                e.agent_pid == agent_pid
                    && !e.actions.is_empty()
                    && e.actions[0].intent_id == trigger_intent
            })
            .collect()
    }

    /// Find repeated successful patterns eligible for crystallization.
    ///
    /// A pattern is eligible if:
    /// - It has been replayed >= `min_replays` times
    /// - Outcome was Success in >= 80% of replays
    /// - It has NOT been crystallized yet
    fn crystallization_candidates(&self, min_replays: u32) -> Vec<&EpisodicEntry> {
        self.entries
            .iter()
            .filter(|e| {
                e.replay_count >= min_replays
                    && e.outcome == EpisodicOutcome::Success
                    && !e.crystallized
            })
            .collect()
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Math Utilities
// ═══════════════════════════════════════════════════════════════════════════

/// Cosine similarity between two vectors.
/// Returns 0.0 if either vector has zero magnitude.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    // 4-way unrolled for performance
    let chunks = n / 4;
    for c in 0..chunks {
        let i = c * 4;
        dot += a[i] * b[i] + a[i + 1] * b[i + 1] + a[i + 2] * b[i + 2] + a[i + 3] * b[i + 3];
        norm_a += a[i] * a[i] + a[i + 1] * a[i + 1] + a[i + 2] * a[i + 2] + a[i + 3] * a[i + 3];
        norm_b += b[i] * b[i] + b[i + 1] * b[i + 1] + b[i + 2] * b[i + 2] + b[i + 3] * b[i + 3];
    }
    for i in (chunks * 4)..n {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let magnitude = (norm_a * norm_b).sqrt();
    if magnitude < 1e-10 {
        0.0
    } else {
        dot / magnitude
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Global Unified Memory
// ═══════════════════════════════════════════════════════════════════════════

static NEXT_MEMORY_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> MemoryId {
    NEXT_MEMORY_ID.fetch_add(1, Ordering::Relaxed)
}

/// The unified memory manager.
struct UnifiedMemory {
    stm: ShortTermStore,
    ltm: LongTermStore,
    vectors: VectorStore,
    episodes: EpisodicStore,
    total_stores: u64,
    total_recalls: u64,
    total_searches: u64,
}

impl UnifiedMemory {
    fn new() -> Self {
        Self {
            stm: ShortTermStore::new(256),
            ltm: LongTermStore::new(),
            vectors: VectorStore::new(4096),
            episodes: EpisodicStore::new(1024),
            total_stores: 0,
            total_recalls: 0,
            total_searches: 0,
        }
    }
}

lazy_static! {
    static ref UNIFIED_MEMORY: Mutex<UnifiedMemory> = Mutex::new(UnifiedMemory::new());
}

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Store a short-term memory.
///
/// TTL is in TSC ticks (0 = no expiration). Typical: 10 min ~ 20_000_000_000 ticks
/// at 2 GHz TSC.
pub fn store_short_term(agent_pid: u64, key: &str, value: &str, ttl_ticks: u64) -> MemoryId {
    let id = next_id();
    let entry = ShortTermEntry {
        id,
        agent_pid,
        key: String::from(key),
        value: String::from(value),
        created_at: crate::arch::x86_64::timer::read_tsc(),
        ttl_ticks,
        access_count: 0,
    };
    let mut mem = UNIFIED_MEMORY.lock();
    mem.stm.store(entry);
    mem.total_stores += 1;
    id
}

/// Store a long-term memory (persisted, survives reboots).
pub fn store_long_term(
    agent_pid: u64,
    category: &str,
    key: &str,
    value: &str,
    confidence: f32,
) -> MemoryId {
    let id = next_id();
    let now = crate::arch::x86_64::timer::read_tsc();
    let entry = LongTermEntry {
        id,
        agent_pid,
        key: String::from(key),
        value: String::from(value),
        created_at: now,
        updated_at: now,
        category: String::from(category),
        confidence,
    };
    let mut mem = UNIFIED_MEMORY.lock();
    mem.ltm.store(entry);
    mem.total_stores += 1;
    id
}

/// Store a vector embedding for semantic search.
pub fn store_vector(
    agent_pid: u64,
    embedding: Vec<f32>,
    text: &str,
    metadata: &str,
) -> MemoryId {
    let id = next_id();
    let entry = VectorEntry {
        id,
        agent_pid,
        embedding,
        text: String::from(text),
        metadata: String::from(metadata),
    };
    let mut mem = UNIFIED_MEMORY.lock();
    mem.vectors.store(entry);
    mem.total_stores += 1;
    id
}

/// Record an episodic memory (action sequence with outcome).
pub fn store_episode(
    agent_pid: u64,
    actions: Vec<EpisodicAction>,
    outcome: EpisodicOutcome,
) -> MemoryId {
    let id = next_id();
    let entry = EpisodicEntry {
        id,
        agent_pid,
        actions,
        outcome,
        created_at: crate::arch::x86_64::timer::read_tsc(),
        replay_count: 1,
        crystallized: false,
    };
    let mut mem = UNIFIED_MEMORY.lock();
    mem.episodes.store(entry);
    mem.total_stores += 1;
    id
}

/// Recall a short-term memory by key.
pub fn recall_short_term(agent_pid: u64, key: &str) -> Option<String> {
    let mut mem = UNIFIED_MEMORY.lock();
    mem.total_recalls += 1;
    mem.stm.recall(agent_pid, key).map(|e| e.value.clone())
}

/// Recall a long-term memory by category and key.
pub fn recall_long_term(agent_pid: u64, category: &str, key: &str) -> Option<String> {
    let mut mem = UNIFIED_MEMORY.lock();
    mem.total_recalls += 1;
    mem.ltm.recall(agent_pid, category, key).map(|e| e.value.clone())
}

/// Semantic search: find top-K nearest vectors.
///
/// Returns Vec of (similarity, text, metadata) sorted by similarity descending.
pub fn vector_search(
    agent_pid: u64,
    query_embedding: &[f32],
    top_k: usize,
    threshold: f32,
) -> Vec<(f32, String, String)> {
    let mut mem = UNIFIED_MEMORY.lock();
    mem.total_searches += 1;
    mem.vectors
        .search(agent_pid, query_embedding, top_k, threshold)
        .iter()
        .map(|(sim, entry)| (*sim, entry.text.clone(), entry.metadata.clone()))
        .collect()
}

/// Find episodic patterns matching a trigger intent.
pub fn find_episodes(agent_pid: u64, trigger_intent: u32) -> Vec<(MemoryId, EpisodicOutcome, u32)> {
    let mem = UNIFIED_MEMORY.lock();
    mem.episodes
        .find_pattern(agent_pid, trigger_intent)
        .iter()
        .map(|e| (e.id, e.outcome, e.replay_count))
        .collect()
}

/// Forget a long-term memory.
pub fn forget_long_term(agent_pid: u64, category: &str, key: &str) -> bool {
    UNIFIED_MEMORY.lock().ltm.forget(agent_pid, category, key)
}

/// Get candidates for reflex crystallization.
pub fn crystallization_candidates(min_replays: u32) -> Vec<MemoryId> {
    UNIFIED_MEMORY.lock()
        .episodes
        .crystallization_candidates(min_replays)
        .iter()
        .map(|e| e.id)
        .collect()
}

/// Mark an episodic entry as crystallized (converted to reflex).
pub fn mark_crystallized(memory_id: MemoryId) -> bool {
    let mut mem = UNIFIED_MEMORY.lock();
    if let Some(entry) = mem.episodes.entries.iter_mut().find(|e| e.id == memory_id) {
        entry.crystallized = true;
        return true;
    }
    false
}

/// Get memory usage metrics.
pub fn memory_metrics() -> MemoryMetrics {
    let mem = UNIFIED_MEMORY.lock();
    MemoryMetrics {
        stm_count: mem.stm.len(),
        ltm_count: mem.ltm.len(),
        vector_count: mem.vectors.len(),
        episode_count: mem.episodes.len(),
        total_stores: mem.total_stores,
        total_recalls: mem.total_recalls,
        total_searches: mem.total_searches,
    }
}

/// Memory metrics for sysinfo/diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct MemoryMetrics {
    pub stm_count: usize,
    pub ltm_count: usize,
    pub vector_count: usize,
    pub episode_count: usize,
    pub total_stores: u64,
    pub total_recalls: u64,
    pub total_searches: u64,
}

/// Initialize the persistent memory subsystem.
pub fn init() {
    // Future: load LTM from disk (/disk/var/memory.db)
    crate::serial_println!(
        "[MEMORY] Unified persistent memory initialized (STM=256, LTM=unlimited, VEC=4096, EPISODE=1024)"
    );
}
