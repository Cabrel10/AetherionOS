// memory/mod.rs - Couche 2: Gestion mémoire et ressources
// ACHA-OS Memory Management Subsystem
// Adapted for bootloader_api 0.11

pub mod frame;
pub mod paging;
pub mod heap;
pub mod resource_tag;

use bootloader_api::info::{BootInfo, MemoryRegionKind, Optional};
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
        // 1. Récupérer l'offset depuis BootInfo (bootloader_api 0.11: Optional<u64>)
        let physical_memory_offset = match boot_info.physical_memory_offset {
            Optional::Some(off) => off,
            Optional::None => {
                crate::serial_write("[MEMORY] ERROR: physical_memory_offset not provided by bootloader\n");
                return Err(MemoryError::OutOfMemory);
            }
        };
        
        if physical_memory_offset == 0 {
            crate::serial_write("[MEMORY] ERROR: physical_memory_offset is 0\n");
            return Err(MemoryError::OutOfMemory);
        }
        
        let phys_offset = VirtAddr::new(physical_memory_offset);
        
        // Log physical_memory_offset
        {
            use core::fmt::Write;
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = writeln!(s, "[MEMORY] Physical memory offset: {:#x}", physical_memory_offset);
            crate::serial_write(&s);
        }
        
        // 2. Calculer les régions de mémoire utilisable
        // bootloader_api 0.11: memory_regions (not memory_map),
        //   region.start/end (not range.start_addr()/end_addr()),
        //   MemoryRegionKind::Usable (not MemoryRegionType::Usable)
        let mut total_usable = 0u64;
        let mut usable_regions = [(0u64, 0u64); 32];
        let mut region_count = 0;
        
        // J135: Dump ALL memory regions for diagnostic
        crate::serial_write("[MEMORY] Boot memory map (all regions):\n");
        for region in boot_info.memory_regions.iter() {
            let start = region.start;
            let end = region.end;
            let kind = region.kind;
            {
                use core::fmt::Write;
                let mut s = arrayvec::ArrayString::<128>::new();
                let _ = writeln!(s, "[MEMORY]   {:#x} - {:#x} ({:>4} KB) {:?}",
                    start, end, (end - start) / 1024, kind);
                crate::serial_write(&s);
            }

            if region.kind == MemoryRegionKind::Usable && region_count < 32 {
                // J135 CRITICAL FIX: Exclude kernel physical region
                const KERNEL_RESERVED_START: u64 = 0x200000;
                const KERNEL_RESERVED_END:   u64 = 0x1000000; // 16 MiB safety margin
                if end <= KERNEL_RESERVED_START || start >= KERNEL_RESERVED_END {
                    usable_regions[region_count] = (start, end);
                    region_count += 1;
                    total_usable += end - start;
                } else {
                    if start < KERNEL_RESERVED_START && region_count < 32 {
                        let hi = KERNEL_RESERVED_START;
                        usable_regions[region_count] = (start, hi);
                        region_count += 1;
                        total_usable += hi - start;
                    }
                    if end > KERNEL_RESERVED_END && region_count < 32 {
                        let lo = KERNEL_RESERVED_END;
                        usable_regions[region_count] = (lo, end);
                        region_count += 1;
                        total_usable += end - lo;
                    }
                    {
                        use core::fmt::Write;
                        let mut s = arrayvec::ArrayString::<160>::new();
                        let _ = writeln!(s, "[MEMORY] J135: kernel carve-out {:#x}-{:#x} removed from usable region",
                            core::cmp::max(start, KERNEL_RESERVED_START),
                            core::cmp::min(end, KERNEL_RESERVED_END));
                        crate::serial_write(&s);
                    }
                }
            }
        }
        
        {
            use core::fmt::Write;
            let mut s = arrayvec::ArrayString::<128>::new();
            let _ = writeln!(s, "[MEMORY] Found {} usable regions, total: {} KB",
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
            let _ = writeln!(s, "[MEMORY] Frame allocator: {} frames ({} MB)",
                frame_allocator.total_frames(),
                (frame_allocator.total_frames() * 4) / 1024);
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
            let _ = writeln!(s, "[HEAP] Initialized: {} KB at {:#x}",
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

/// Initialise la mémoire à partir de la carte mémoire Limine.
///
/// Prend les régions usable directement (déjà extraites dans limine_entry.rs)
/// et le HHDM offset. Crée le frame allocator, page table manager, et heap.
///
/// # Safety
/// - `usable_regions` doit contenir des plages physiques valides
/// - `hhdm_offset` doit correspondre au HHDM configuré par Limine
/// - Doit être appelé une seule fois pendant le boot
pub fn init_from_limine(
    usable_regions: &[(u64, u64)],
    hhdm_offset: u64,
) -> MemoryResult<MemoryManager> {
    crate::serial_write("\n========================================\n");
    crate::serial_write("[MEMORY] Limine path - Initializing...\n");
    crate::serial_write("========================================\n");

    let phys_offset = VirtAddr::new(hhdm_offset);

    {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "[MEMORY] HHDM offset: {:#x}", hhdm_offset);
        crate::serial_write(&s);
    }

    // Log usable regions
    let mut total_usable = 0u64;
    for &(start, end) in usable_regions {
        total_usable += end - start;
    }
    {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "[MEMORY] {} usable regions, total: {} MB",
            usable_regions.len(), total_usable / (1024 * 1024));
        crate::serial_write(&s);
    }

    // Frame allocator from Limine usable regions
    let frame_allocator = unsafe {
        frame::FrameAllocator::new(usable_regions)
    };

    {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "[MEMORY] Frame allocator: {} frames ({} MB)",
            frame_allocator.total_frames(),
            (frame_allocator.total_frames() * 4) / 1024);
        crate::serial_write(&s);
    }

    // Page table manager using Limine's HHDM
    let page_table = unsafe {
        paging::OffsetPageTableManager::new(phys_offset)
    };
    crate::serial_write("[MEMORY] Page table manager initialized (Limine CR3)\n");

    crate::serial_write("[MEMORY] Limine path core initialized\n");
    crate::serial_write("========================================\n\n");

    Ok(MemoryManager {
        frame_allocator,
        page_table,
        heap_initialized: false,
    })
}
