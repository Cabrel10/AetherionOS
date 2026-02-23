// ipc/bus.rs - Cognitive Bus Implementation (Lock-free MPMC)
//
// Utilise crossbeam_queue::ArrayQueue pour une file de messages
// lock-free, MPMC (Multi-Producer Multi-Consumer), compatible no_std.
//
// La file est une instance statique globale via lazy_static,
// permettant a n'importe quel composant du kernel de publier
// ou consommer des messages sans mutex.

use super::{IntentMessage, BusError};
use crossbeam_queue::ArrayQueue;
use lazy_static::lazy_static;

/// Capacite maximale du bus (nombre de messages en file)
const BUS_CAPACITY: usize = 100;

lazy_static! {
    /// Instance statique globale du Cognitive Bus
    /// ArrayQueue est lock-free et thread-safe (MPMC)
    static ref COGNITIVE_BUS: ArrayQueue<IntentMessage> = ArrayQueue::new(BUS_CAPACITY);
}

/// Publie un message sur le Cognitive Bus
///
/// # Arguments
/// * `msg` - Le message Intent a publier
///
/// # Returns
/// * `Ok(())` si le message a ete publie avec succes
/// * `Err(BusError::QueueFull)` si la file est pleine
///
/// # Performance
/// O(1) - Lock-free, pas de mutex
pub fn publish(msg: IntentMessage) -> Result<(), BusError> {
    COGNITIVE_BUS.push(msg).map_err(|_| BusError::QueueFull)
}

/// Consomme un message depuis le Cognitive Bus (FIFO)
///
/// # Returns
/// * `Ok(IntentMessage)` avec le prochain message
/// * `Err(BusError::QueueEmpty)` si la file est vide
///
/// # Performance
/// O(1) - Lock-free, pas de mutex
pub fn consume() -> Result<IntentMessage, BusError> {
    COGNITIVE_BUS.pop().ok_or(BusError::QueueEmpty)
}

/// Retourne le nombre de messages actuellement dans le bus
pub fn len() -> usize {
    COGNITIVE_BUS.len()
}

/// Verifie si le bus est vide
pub fn is_empty() -> bool {
    COGNITIVE_BUS.is_empty()
}

/// Retourne la capacite maximale du bus
pub fn capacity() -> usize {
    BUS_CAPACITY
}
