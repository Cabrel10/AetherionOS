// memory/resource_tag.rs - ACHA Resource Tagging
// Traçabilité totale des allocations mémoire par processus

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
/// 
/// Stocké inline dans le FrameAllocator pour O(1) accès
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceTag {
    /// ID du processus propriétaire (0 = kernel)
    pub process_id: u64,
    /// Timestamp TSC (Time Stamp Counter)
    pub timestamp: u64,
    /// Type d'allocation
    pub allocation_type: AllocationType,
}

impl ResourceTag {
    /// Crée un nouveau tag pour le kernel
    pub fn kernel(alloc_type: AllocationType) -> Self {
        Self {
            process_id: 0,
            timestamp: crate::arch::timer::read_tsc(),
            allocation_type: alloc_type,
        }
    }
    
    /// Crée un nouveau tag pour un processus
    pub fn process(pid: u64, alloc_type: AllocationType) -> Self {
        Self {
            process_id: pid,
            timestamp: crate::arch::timer::read_tsc(),
            allocation_type: alloc_type,
        }
    }
    
    /// Vérifie si l'allocation appartient au kernel
    pub fn is_kernel(&self) -> bool {
        self.process_id == 0
    }
    
    /// Age de l'allocation en cycles TSC
    pub fn age_cycles(&self) -> u64 {
        crate::arch::timer::read_tsc().wrapping_sub(self.timestamp)
    }
}

/// Statistiques de ressources par processus
#[derive(Debug, Clone, Copy)]
pub struct ResourceStats {
    pub process_id: u64,
    pub frames_allocated: usize,
    pub pages_mapped: usize,
    pub heap_bytes: usize,
}

/// Registry global de tags (simplifié pour no_std)
/// 
/// Note: La vraie implémentation utilise le stockage inline
/// dans FrameAllocator pour éviter les allocations dynamiques
pub struct ResourceRegistry;

impl ResourceRegistry {
    /// Log une allocation (stub pour futur développement)
    #[allow(dead_code)]
    pub fn log_allocation(_tag: &ResourceTag, _addr: u64) {
        // TODO: Implémenter un buffer circulaire de logs ACHA
        // Pour l'instant, les logs sont via serial_println! dans le code appelant
    }

    /// Log une désallocation
    #[allow(dead_code)]
    pub fn log_deallocation(_addr: u64, _pid: u64) {
        // TODO: Buffer circulaire
    }
}

/// Macros de debugging ACHA
#[macro_export]
macro_rules! log_resource_alloc {
    ($pid:expr, $type:expr, $addr:expr) => {
        $crate::serial_println!(
            "[ACHA:ALLOC] pid={} type={} addr={:#x} tsc={}",
            $pid,
            $type.as_str(),
            $addr,
            $crate::arch::timer::read_tsc()
        );
    };
}

#[macro_export]
macro_rules! log_resource_free {
    ($pid:expr, $addr:expr) => {
        $crate::serial_println!(
            "[ACHA:FREE] pid={} addr={:#x} tsc={}",
            $pid,
            $addr,
            $crate::arch::timer::read_tsc()
        );
    };
}

/// Tests unitaires
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test_case]
    fn test_resource_tag_creation() {
        let tag = ResourceTag::kernel(AllocationType::Frame);
        assert!(tag.is_kernel());
        assert_eq!(tag.allocation_type, AllocationType::Frame);
        
        let user_tag = ResourceTag::process(42, AllocationType::Heap);
        assert_eq!(user_tag.process_id, 42);
        assert!(!user_tag.is_kernel());
    }
    
    #[test_case]
    fn test_allocation_type_str() {
        assert_eq!(AllocationType::Frame.as_str(), "frame");
        assert_eq!(AllocationType::Heap.as_str(), "heap");
        assert_eq!(AllocationType::Page.as_str(), "page");
        assert_eq!(AllocationType::Stack.as_str(), "stack");
    }
}
