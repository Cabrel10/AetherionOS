// ipc/bus.rs - Cognitive Bus Implementation (Priority-Aware)
//
// HIGH-002 FIX: Replaced FIFO ArrayQueue with a spin-locked BinaryHeap
// so that Critical messages are always consumed before Normal/Low ones.
//
// The heap is a max-heap keyed on (Priority, timestamp), meaning:
//   1. Higher-priority messages are consumed first.
//   2. Among equal-priority messages, earlier timestamps win (FIFO within level).
//
// Trade-off: O(log n) publish/consume vs O(1) for lock-free ArrayQueue,
// but priority ordering is essential for interrupt-driven orchestration.

use super::{IntentMessage, BusError, ComponentId, Priority};
use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use spin::Mutex;
use lazy_static::lazy_static;

/// Maximum bus capacity (number of messages)
/// J43: increased from 128 to 1024 for large-model IPC traffic
const BUS_CAPACITY: usize = 1024;

/// Priority-aware message wrapper for BinaryHeap ordering.
///
/// Ordering: higher Priority first, then lower timestamp (older first).
#[derive(Debug, Clone, Copy)]
struct PriorityMessage {
    msg: IntentMessage,
}

impl PriorityMessage {
    fn priority_rank(&self) -> u8 {
        self.msg.priority as u8
    }
}

impl PartialEq for PriorityMessage {
    fn eq(&self, other: &Self) -> bool {
        self.priority_rank() == other.priority_rank()
            && self.msg.timestamp == other.msg.timestamp
    }
}

impl Eq for PriorityMessage {}

impl PartialOrd for PriorityMessage {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PriorityMessage {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        // Primary: higher priority first
        self.priority_rank()
            .cmp(&other.priority_rank())
            // Secondary: earlier timestamp first (reverse: smaller = better)
            .then_with(|| other.msg.timestamp.cmp(&self.msg.timestamp))
    }
}

/// The Cognitive Bus state (protected by spin-lock).
struct CognitiveBus {
    /// Backing storage kept sorted via manual sift operations.
    heap: Vec<PriorityMessage>,
}

impl CognitiveBus {
    fn new() -> Self {
        Self {
            heap: Vec::with_capacity(BUS_CAPACITY),
        }
    }

    /// Push a message into the priority queue (O(log n)).
    fn push(&mut self, msg: IntentMessage) -> Result<(), BusError> {
        if self.heap.len() >= BUS_CAPACITY {
            return Err(BusError::QueueFull);
        }
        let pm = PriorityMessage { msg };
        self.heap.push(pm);
        self.sift_up(self.heap.len() - 1);
        Ok(())
    }

    /// Pop the highest-priority message (O(log n)).
    fn pop(&mut self) -> Result<IntentMessage, BusError> {
        if self.heap.is_empty() {
            return Err(BusError::QueueEmpty);
        }
        let last = self.heap.len() - 1;
        self.heap.swap(0, last);
        let pm = self.heap.pop().unwrap(); // safe: checked non-empty
        if !self.heap.is_empty() {
            self.sift_down(0);
        }
        Ok(pm.msg)
    }

    fn len(&self) -> usize {
        self.heap.len()
    }

    fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    // ---- Binary heap helpers ----

    /// Sift up: restore max-heap invariant after insertion.
    ///
    /// FIFO-FIX: Uses `Ord::cmp` (not just `>`) so that equal-priority
    /// messages are ordered by timestamp (older = higher in heap).
    /// This guarantees deterministic FIFO within the same priority level.
    fn sift_up(&mut self, mut idx: usize) {
        while idx > 0 {
            let parent = (idx - 1) / 2;
            // Swap if child is strictly greater (per Ord which encodes FIFO)
            if self.heap[idx].cmp(&self.heap[parent]) == core::cmp::Ordering::Greater {
                self.heap.swap(idx, parent);
                idx = parent;
            } else {
                break;
            }
        }
    }

    /// Sift down: restore max-heap invariant after removal.
    ///
    /// FIFO-FIX: Uses `Ord::cmp` so that among equal-priority messages,
    /// the one with the earlier timestamp is considered "larger" and
    /// stays closer to the root, ensuring FIFO consumption order.
    fn sift_down(&mut self, mut idx: usize) {
        let len = self.heap.len();
        loop {
            let left = 2 * idx + 1;
            let right = 2 * idx + 2;
            let mut largest = idx;

            if left < len && self.heap[left].cmp(&self.heap[largest]) == core::cmp::Ordering::Greater {
                largest = left;
            }
            if right < len && self.heap[right].cmp(&self.heap[largest]) == core::cmp::Ordering::Greater {
                largest = right;
            }
            if largest != idx {
                self.heap.swap(idx, largest);
                idx = largest;
            } else {
                break;
            }
        }
    }
}

// ═══════════════════════════════════════════════════
// Reflex Engine (Jalon 119)
// ═══════════════════════════════════════════════════
//
// The Reflex Engine provides fast, O(1) routing for known intent patterns.
// When an intent matches a registered reflex rule, it is directly routed
// to the target component (e.g., Window Manager) without waking the full
// AI pipeline (LLM agent). This is analogous to biological reflexes:
// simple stimulus → immediate response, bypassing the "brain".
//
// Rules are stored in a BTreeMap<u32, ReflexAction> keyed by intent_id.
// On publish(), the engine checks if the intent has a matching reflex.
// If so, it generates a new routed message and publishes it immediately.

/// Action to take when a reflex matches.
#[derive(Debug, Clone, Copy)]
pub struct ReflexAction {
    /// Target component to route to
    pub target: ComponentId,
    /// Intent to generate (can differ from trigger intent)
    pub emit_intent: u32,
    /// Priority of the emitted message
    pub priority: Priority,
    /// Whether to also keep the original message in the bus
    pub pass_through: bool,
}

/// The Reflex Routing Table
struct ReflexEngine {
    rules: BTreeMap<u32, ReflexAction>,
    /// Total number of reflexes fired since boot
    fire_count: u64,
}

impl ReflexEngine {
    fn new() -> Self {
        Self {
            rules: BTreeMap::new(),
            fire_count: 0,
        }
    }

    /// Register a reflex rule: when `trigger_intent` is published,
    /// automatically route to `action.target` with `action.emit_intent`.
    fn register(&mut self, trigger_intent: u32, action: ReflexAction) {
        self.rules.insert(trigger_intent, action);
    }

    /// Check if an intent has a matching reflex. If so, return the action.
    fn check(&self, intent_id: u32) -> Option<&ReflexAction> {
        self.rules.get(&intent_id)
    }

    /// Increment the fire counter
    fn fired(&mut self) {
        self.fire_count += 1;
    }

    /// Get number of registered rules
    fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Get total fire count
    fn total_fires(&self) -> u64 {
        self.fire_count
    }
}

lazy_static! {
    /// Global Cognitive Bus instance (spin-lock protected).
    static ref COGNITIVE_BUS: Mutex<CognitiveBus> = Mutex::new(CognitiveBus::new());
    /// Global Reflex Engine (Jalon 119)
    static ref REFLEX_ENGINE: Mutex<ReflexEngine> = Mutex::new(ReflexEngine::new());
}

/// Publish a message to the Cognitive Bus (priority-aware).
///
/// Messages are ordered by priority: Critical > High > Normal > Low.
/// Within the same priority level, earlier messages are consumed first.
///
/// Jalon 119: Before inserting, the Reflex Engine checks if this intent
/// matches a registered reflex rule. If so, a routed message is auto-
/// generated and published alongside the original (or instead of it,
/// depending on `pass_through`).
///
/// # Returns
/// * `Ok(())` if the message was published successfully
/// * `Err(BusError::QueueFull)` if the bus is at capacity
///
/// # Performance
/// O(log n) with spin-lock (n = current message count)
pub fn publish(msg: IntentMessage) -> Result<(), BusError> {
    // Jalon 119: Check Reflex Engine
    let reflex_action = {
        let engine = REFLEX_ENGINE.lock();
        engine.check(msg.intent_id).copied()
    };

    if let Some(action) = reflex_action {
        // Fire the reflex: generate a routed message
        let routed = IntentMessage {
            source: msg.source,
            destination: action.target,
            intent_id: action.emit_intent,
            priority: action.priority,
            payload: msg.payload,
            timestamp: msg.timestamp,
            session_id: msg.session_id,
            correlation_id: msg.correlation_id,
        };
        COGNITIVE_BUS.lock().push(routed)?;
        REFLEX_ENGINE.lock().fired();

        if !action.pass_through {
            // Reflex consumed the intent — don't keep original
            return Ok(());
        }
    }

    COGNITIVE_BUS.lock().push(msg)
}

/// Consume the highest-priority message from the Cognitive Bus.
///
/// Returns the message with the highest priority. Among messages with
/// equal priority, the oldest (earliest timestamp) is returned first.
///
/// # Returns
/// * `Ok(IntentMessage)` with the highest-priority message
/// * `Err(BusError::QueueEmpty)` if the bus is empty
///
/// # Performance
/// O(log n) with spin-lock
pub fn consume() -> Result<IntentMessage, BusError> {
    COGNITIVE_BUS.lock().pop()
}

/// Intent-Based Routing: Consume only messages matching a specific intent ID.
/// All other messages are left untouched in the bus.
///
/// Level 8 (ACHA §3.7.1): This is the foundation of the Pub/Sub model.
/// Each agent subscribes to its own intent(s) and never steals messages
/// destined for other agents. This fixes the shared-bus race condition
/// where the Terminal would accidentally consume MCP's 0x9002 messages.
///
/// # Returns
/// * `Ok(IntentMessage)` with the highest-priority message matching `target_intent`
/// * `Err(BusError::QueueEmpty)` if no matching message exists
///
/// # Performance
/// O(n) scan + O(log n) re-heapify. Acceptable for bus sizes ≤ 1024.
pub fn consume_intent(target_intent: u32) -> Result<IntentMessage, BusError> {
    let mut bus = COGNITIVE_BUS.lock();
    let mut found_idx = None;
    for (i, pm) in bus.heap.iter().enumerate() {
        if pm.msg.intent_id == target_intent {
            found_idx = Some(i);
            break;
        }
    }
    if let Some(idx) = found_idx {
        let last = bus.heap.len() - 1;
        bus.heap.swap(idx, last);
        let pm = bus.heap.pop().unwrap();
        if !bus.heap.is_empty() && idx < bus.heap.len() {
            bus.sift_down(idx);
            bus.sift_up(idx);
        }
        return Ok(pm.msg);
    }
    Err(BusError::QueueEmpty)
}

/// Returns the number of messages currently in the bus
pub fn len() -> usize {
    COGNITIVE_BUS.lock().len()
}

/// Check if the bus is empty
pub fn is_empty() -> bool {
    COGNITIVE_BUS.lock().is_empty()
}

/// Returns the maximum capacity of the bus
pub fn capacity() -> usize {
    BUS_CAPACITY
}

// ═══════════════════════════════════════════════════
// Reflex Engine Public API (Jalon 119)
// ═══════════════════════════════════════════════════

/// Register a reflex rule: when `trigger_intent` is seen on the bus,
/// automatically route a message to the specified target component.
///
/// This allows the WM (or any fast-path agent) to receive certain intents
/// directly without the LLM agent having to process and re-route them.
///
/// # Example
/// ```
/// // Route INTENT_GET_UI_TREE (0xB119) directly to the WM (Worker component)
/// register_reflex(0xB119, ReflexAction {
///     target: ComponentId::Worker,
///     emit_intent: 0xB119,
///     priority: Priority::High,
///     pass_through: false,
/// });
/// ```
pub fn register_reflex(trigger_intent: u32, action: ReflexAction) {
    REFLEX_ENGINE.lock().register(trigger_intent, action);
}

/// Get the number of registered reflex rules.
pub fn reflex_rule_count() -> usize {
    REFLEX_ENGINE.lock().rule_count()
}

/// Get the total number of reflexes fired since boot.
pub fn reflex_fire_count() -> u64 {
    REFLEX_ENGINE.lock().total_fires()
}
