// memory/resource_tag.rs - ACHA Resource Tagging
// Traçabilité des allocations mémoire par processus

/// Type d'allocation pour classification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationType {
    /// Frame physique (4KB)
    Frame,
    /// Page virtuelle mappée
    Page,
    /// Allocation heap (Box, Vec)
    Heap,
    /// Stack
    Stack,
}

impl AllocationType {
    /// Description textuelle pour logs
    pub fn as_str(&self) -> &'static str {
        match self {
            AllocationType::Frame => "frame",
            AllocationType::Page => "page",
            AllocationType::Heap => "heap",
            AllocationType::Stack => "stack",
        }
    }
}

/// Tag de ressource ACHA - Métadonnées d'allocation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceTag {
    /// ID du processus propriétaire (0 = kernel)
    pub process_id: u64,
    /// Timestamp TSC
    pub timestamp: u64,
    /// Type d'allocation
    pub allocation_type: AllocationType,
}

impl ResourceTag {
    /// Crée un nouveau tag pour le kernel
    pub fn kernel(alloc_type: AllocationType) -> Self {
        Self {
            process_id: 0,
            timestamp: 0, // Will be set by caller if needed
            allocation_type: alloc_type,
        }
    }
    
    /// Vérifie si l'allocation appartient au kernel
    pub fn is_kernel(&self) -> bool {
        self.process_id == 0
    }
}
