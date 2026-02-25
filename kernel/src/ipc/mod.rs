// ipc/mod.rs - Cognitive Bus (Inter-Process Communication)
// Couche 3: Architecture Intent-Based pour ACHA-OS
//
// Le Cognitive Bus est le systeme nerveux d'AetherionOS.
// Il fournit une file de messages typee "Intent-Based" pour la
// communication entre les differents modules du noyau.
//
// Architecture:
//   - Lock-free MPMC (Multi-Producer Multi-Consumer)
//   - Messages types avec ComponentId et Priority
//   - Zero-copy message passing avec O(1) publish/consume

pub mod bus;

use core::fmt;

// ===== Component Identifiers =====

/// Identifiant des composants du noyau ACHA
/// Chaque module du kernel est identifie par un ComponentId unique
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[allow(clippy::upper_case_acronyms)]
pub enum ComponentId {
    /// Orchestrateur central (destination par defaut)
    Orchestrator = 0,
    /// Hardware Abstraction Layer (Couche 1)
    HAL = 1,
    /// Memory Manager (Couche 2)
    Memory = 2,
    /// Verifier / Security module
    Verifier = 3,
    /// Cerebellum (ML/AI subsystem)
    Cerebellum = 4,
    /// Filesystem (VFS)
    Filesystem = 5,
    /// Network stack
    Network = 6,
    /// Security subsystem
    Security = 7,
    /// Broadcast (tous les composants)
    Broadcast = 0xFF,
}

impl fmt::Display for ComponentId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Orchestrator => write!(f, "Orchestrator"),
            Self::HAL => write!(f, "HAL"),
            Self::Memory => write!(f, "Memory"),
            Self::Verifier => write!(f, "Verifier"),
            Self::Cerebellum => write!(f, "Cerebellum"),
            Self::Filesystem => write!(f, "Filesystem"),
            Self::Network => write!(f, "Network"),
            Self::Security => write!(f, "Security"),
            Self::Broadcast => write!(f, "Broadcast"),
        }
    }
}

// ===== Priority Levels =====

/// Niveaux de priorite des messages
/// L'Orchestrateur peut utiliser la priorite pour trier les messages
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Priority {
    /// Priorite basse - taches de fond
    Low = 0,
    /// Priorite normale - operations courantes
    Normal = 64,
    /// Priorite haute - evenements importants
    High = 128,
    /// Priorite critique - interruptions, paniques
    Critical = 255,
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Low => write!(f, "LOW"),
            Self::Normal => write!(f, "NORMAL"),
            Self::High => write!(f, "HIGH"),
            Self::Critical => write!(f, "CRITICAL"),
        }
    }
}

// ===== Intent Message =====

/// Structure de message Intent-Based pour le Cognitive Bus
///
/// Chaque message represente une "intention" d'un composant vers un autre.
/// Le champ `intent_id` encode l'action demandee (ex: 0x0001 = KeyPress,
/// 0x0010 = AllocRequest, 0x0020 = VerifyIntegrity, etc.)
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct IntentMessage {
    /// Composant emetteur du message
    pub source: ComponentId,
    /// Composant destinataire (Orchestrator = hub central)
    pub destination: ComponentId,
    /// Code de l'action/intention (semantique libre par composant)
    pub intent_id: u32,
    /// Niveau de priorite du message
    pub priority: Priority,
    /// Donnee associee (valeur directe ou pointeur)
    pub payload: u64,
    /// Timestamp TSC pour tracabilite et benchmarks
    pub timestamp: u64,
}

impl IntentMessage {
    /// Cree un nouveau message avec timestamp automatique (TSC)
    pub fn new(
        source: ComponentId,
        destination: ComponentId,
        intent_id: u32,
        priority: Priority,
        payload: u64,
    ) -> Self {
        Self {
            source,
            destination,
            intent_id,
            priority,
            payload,
            timestamp: crate::arch::x86_64::timer::read_tsc(),
        }
    }
}

impl fmt::Display for IntentMessage {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "[{} -> {}] Intent=0x{:04x} Priority={} Payload=0x{:016x}",
            self.source, self.destination, self.intent_id, self.priority, self.payload
        )
    }
}

// ===== Bus Errors =====

/// Erreurs possibles lors des operations sur le bus
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusError {
    /// La file est pleine, impossible de publier
    QueueFull,
    /// La file est vide, rien a consommer
    QueueEmpty,
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::QueueFull => write!(f, "Bus queue is full"),
            Self::QueueEmpty => write!(f, "Bus queue is empty"),
        }
    }
}
