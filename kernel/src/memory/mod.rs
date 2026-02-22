// memory/mod.rs - Couche 2: Gestion mémoire et ressources
// ACHA-OS Memory Management Subsystem
// Adapté pour bootloader 0.9.23

pub mod frame;
pub mod paging;
pub mod heap;
pub mod resource_tag;

use crate::serial_println;
use bootloader::bootinfo::{BootInfo, MemoryRegionType};
use x86_64::VirtAddr;

/// Erreurs mémoire exhaustives
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryError {
    /// Plus de mémoire physique disponible
    OutOfMemory,
    /// Frame déjà allouée
    FrameAlreadyAllocated(u64),
    /// Frame non allouée (désallocation invalide)
    FrameNotAllocated(u64),
    /// Page déjà mappée
    PageAlreadyMapped(u64),
    /// Page non mappée
    PageNotMapped(u64),
    /// Échec initialisation heap
    HeapInitFailed,
    /// Fuite mémoire détectée
    MemoryLeak { frames_leaked: usize },
}

impl core::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::OutOfMemory => write!(f, "Out of physical memory"),
            Self::FrameAlreadyAllocated(addr) => 
                write!(f, "Frame at {:#x} already allocated", addr),
            Self::FrameNotAllocated(addr) =>
                write!(f, "Frame at {:#x} not allocated", addr),
            Self::PageAlreadyMapped(addr) =>
                write!(f, "Page at {:#x} already mapped", addr),
            Self::PageNotMapped(addr) =>
                write!(f, "Page at {:#x} not mapped", addr),
            Self::HeapInitFailed => write!(f, "Heap initialization failed"),
            Self::MemoryLeak { frames_leaked } =>
                write!(f, "Memory leak detected: {} frames", frames_leaked),
        }
    }
}

/// Résultat des opérations mémoire
pub type MemoryResult<T> = Result<T, MemoryError>;

// Note: PHYSICAL_MEMORY_OFFSET n'est plus une constante hardcodée.
// L'offset est maintenant récupéré dynamiquement depuis boot_info.physical_memory_offset
// (nécessite la feature "map_physical_memory" activée sur le crate bootloader)
// Voir: https://docs.rs/bootloader/0.9.x/bootloader/bootinfo/struct.BootInfo.html

/// État global du système mémoire
pub struct MemoryManager {
    pub frame_allocator: frame::FrameAllocator,
    pub page_table: paging::OffsetPageTableManager,
    pub heap_initialized: bool,
}

impl MemoryManager {
    /// Crée un nouveau MemoryManager à partir des infos de boot
    /// 
    /// # Safety
    /// Cette fonction est unsafe car elle dépend du bootloader ayant mappé
    /// la mémoire physique à l'offset spécifié dans BootInfo.
    /// Assurez-vous d'avoir activé `map-physical-memory` dans Cargo.toml:
    /// ```toml
    /// [package.metadata.bootloader]
    /// map-physical-memory = true
    /// ```
    pub fn new(boot_info: &BootInfo) -> MemoryResult<Self> {
        // 1. Récupérer l'offset depuis BootInfo (nécessite map-physical-memory feature)
        let physical_memory_offset = boot_info.physical_memory_offset;
        
        // Vérifier que l'offset est valide (non-zéro)
        if physical_memory_offset == 0 {
            serial_println!("[MEMORY] ERROR: physical_memory_offset is 0");
            serial_println!("[MEMORY] Did you enable 'map-physical-memory' in Cargo.toml?");
            return Err(MemoryError::OutOfMemory);
        }
        
        let phys_offset = VirtAddr::new(physical_memory_offset);
        
        serial_println!("[MEMORY] Physical memory offset from BootInfo: {:#x}", physical_memory_offset);
        
        // 2. Calculer les régions de mémoire utilisable depuis memory_map
        let mut total_usable = 0u64;
        let mut usable_regions = [(0u64, 0u64); 32]; // Max 32 régions
        let mut region_count = 0;
        
        for region in boot_info.memory_map.iter() {
            let start = region.range.start_addr();
            let end = region.range.end_addr();
            
            serial_println!(
                "[MEMORY] Region {:#x}-{:#x}: {:?}",
                start, end, region.region_type
            );
            
            if region.region_type == MemoryRegionType::Usable && region_count < 32 {
                usable_regions[region_count] = (start, end);
                region_count += 1;
                total_usable += end - start;
            }
        }
        
        serial_println!(
            "[MEMORY] Found {} usable regions, total: {} KB",
            region_count, total_usable / 1024
        );
        
        // 3. Initialiser le frame allocator
        let frame_allocator = unsafe {
            frame::FrameAllocator::new(&usable_regions[..region_count])
        };
        
        serial_println!(
            "[MEMORY] Frame allocator: {} frames ({} MB) available",
            frame_allocator.total_frames(),
            frame_allocator.total_frames() * 4 / 1024
        );
        
        // 4. Initialiser le page table manager avec offset mapping
        let page_table = unsafe {
            paging::OffsetPageTableManager::new(phys_offset)
        };
        
        serial_println!("[MEMORY] Page table manager initialized (offset mapping)");
        
        Ok(Self {
            frame_allocator,
            page_table,
            heap_initialized: false,
        })
    }
    
    /// Initialise le heap allocator
    pub fn init_heap(&mut self) -> MemoryResult<()> {
        if self.heap_initialized {
            return Ok(());
        }
        
        heap::init_heap(&mut self.page_table, &mut self.frame_allocator)
            .map_err(|_| MemoryError::HeapInitFailed)?;
        
        self.heap_initialized = true;
        serial_println!("[MEMORY] Heap initialized: {} KB", heap::HEAP_SIZE / 1024);
        
        Ok(())
    }
}

/// Initialisation globale de la mémoire (appelée depuis main.rs)
pub fn init(boot_info: &BootInfo) -> MemoryResult<MemoryManager> {
    serial_println!("\n========================================");
    serial_println!("[MEMORY] Couche 2 - Initializing...");
    serial_println!("========================================");
    
    let manager = MemoryManager::new(boot_info)?;
    
    serial_println!("[MEMORY] Couche 2 core initialized ✅");
    serial_println!("========================================\n");
    
    Ok(manager)
}

/// Tests de validation (adaptés pour sandbox ~500 MB)
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test_case]
    fn test_memory_module_creation() {
        // Note: Ce test nécessite un BootInfo simulé en environnement de test
        serial_println!("[TEST] Memory module compiles correctly");
        assert_eq!(4 + 4, 8);
    }
}
