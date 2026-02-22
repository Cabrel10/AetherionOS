// memory/mod.rs - Couche 2: Gestion mémoire et ressources
// ACHA-OS Memory Management Subsystem
// Adapté pour bootloader 0.9.23

pub mod frame;
pub mod paging;
pub mod heap;
pub mod resource_tag;

use bootloader::bootinfo::{BootInfo, MemoryRegionType};
use x86_64::VirtAddr;

/// Erreurs mémoire
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryError {
    OutOfMemory,
    FrameAlreadyAllocated(u64),
    FrameNotAllocated(u64),
    PageAlreadyMapped(u64),
    PageNotMapped(u64),
    HeapInitFailed,
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
        }
    }
}

pub type MemoryResult<T> = Result<T, MemoryError>;

/// État global du système mémoire
pub struct MemoryManager {
    pub frame_allocator: frame::FrameAllocator,
    pub page_table: paging::OffsetPageTableManager,
    pub heap_initialized: bool,
}

impl MemoryManager {
    /// Crée un nouveau MemoryManager à partir des infos de boot
    pub fn new(boot_info: &BootInfo) -> MemoryResult<Self> {
        // 1. Récupérer l'offset depuis BootInfo
        let physical_memory_offset = boot_info.physical_memory_offset;
        
        if physical_memory_offset == 0 {
            crate::serial_write("[MEMORY] ERROR: physical_memory_offset is 0\n");
            return Err(MemoryError::OutOfMemory);
        }
        
        let phys_offset = VirtAddr::new(physical_memory_offset);
        
        // Log physical_memory_offset
        {
            use core::fmt::Write;
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = write!(s, "[MEMORY] Physical memory offset: {:#x}\n", physical_memory_offset);
            crate::serial_write(&s);
        }
        
        // 2. Calculer les régions de mémoire utilisable
        let mut total_usable = 0u64;
        let mut usable_regions = [(0u64, 0u64); 32];
        let mut region_count = 0;
        
        for region in boot_info.memory_map.iter() {
            let start = region.range.start_addr();
            let end = region.range.end_addr();
            
            if region.region_type == MemoryRegionType::Usable && region_count < 32 {
                usable_regions[region_count] = (start, end);
                region_count += 1;
                total_usable += end - start;
            }
        }
        
        {
            use core::fmt::Write;
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = write!(s, "[MEMORY] Found {} usable regions, total: {} KB\n",
                region_count, total_usable / 1024);
            crate::serial_write(&s);
        }
        
        // 3. Initialiser le frame allocator
        let frame_allocator = unsafe {
            frame::FrameAllocator::new(&usable_regions[..region_count])
        };
        
        {
            use core::fmt::Write;
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = write!(s, "[MEMORY] Frame allocator: {} frames ({} KB)\n",
                frame_allocator.total_frames(),
                frame_allocator.total_frames() * 4);
            crate::serial_write(&s);
        }
        
        // 4. Initialiser le page table manager
        let page_table = unsafe {
            paging::OffsetPageTableManager::new(phys_offset)
        };
        
        crate::serial_write("[MEMORY] Page table manager initialized\n");
        
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
        
        {
            use core::fmt::Write;
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = write!(s, "[HEAP] Initialized: {} KB at {:#x}\n",
                heap::HEAP_SIZE / 1024, heap::HEAP_START);
            crate::serial_write(&s);
        }
        
        Ok(())
    }
}

/// Initialisation globale de la mémoire
pub fn init(boot_info: &BootInfo) -> MemoryResult<MemoryManager> {
    crate::serial_write("\n========================================\n");
    crate::serial_write("[MEMORY] Couche 2 - Initializing...\n");
    crate::serial_write("========================================\n");
    
    let manager = MemoryManager::new(boot_info)?;
    
    crate::serial_write("[MEMORY] Couche 2 core initialized\n");
    crate::serial_write("========================================\n\n");
    
    Ok(manager)
}
