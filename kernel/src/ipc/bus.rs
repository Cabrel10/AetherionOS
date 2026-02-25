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

use super::{IntentMessage, BusError};
use alloc::vec::Vec;
use spin::Mutex;
use lazy_static::lazy_static;

/// Maximum bus capacity (number of messages)
const BUS_CAPACITY: usize = 128;

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

    fn sift_up(&mut self, mut idx: usize) {
        while idx > 0 {
            let parent = (idx - 1) / 2;
            if self.heap[idx] > self.heap[parent] {
                self.heap.swap(idx, parent);
                idx = parent;
            } else {
                break;
            }
        }
    }

    fn sift_down(&mut self, mut idx: usize) {
        let len = self.heap.len();
        loop {
            let left = 2 * idx + 1;
            let right = 2 * idx + 2;
            let mut largest = idx;

            if left < len && self.heap[left] > self.heap[largest] {
                largest = left;
            }
            if right < len && self.heap[right] > self.heap[largest] {
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

lazy_static! {
    /// Global Cognitive Bus instance (spin-lock protected).
    static ref COGNITIVE_BUS: Mutex<CognitiveBus> = Mutex::new(CognitiveBus::new());
}

/// Publish a message to the Cognitive Bus (priority-aware).
///
/// Messages are ordered by priority: Critical > High > Normal > Low.
/// Within the same priority level, earlier messages are consumed first.
///
/// # Returns
/// * `Ok(())` if the message was published successfully
/// * `Err(BusError::QueueFull)` if the bus is at capacity
///
/// # Performance
/// O(log n) with spin-lock (n = current message count)
pub fn publish(msg: IntentMessage) -> Result<(), BusError> {
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
