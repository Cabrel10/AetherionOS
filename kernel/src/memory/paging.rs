// memory/paging.rs - Page Table Manager avec Offset Mapping
// Simplifié: pas de recursive mapping, utilise l'offset physique direct

use super::MemoryError;
use x86_64::structures::paging::{
    PageTable, PageTableFlags, OffsetPageTable,
    Page, PhysFrame, Size4KiB, Translate,
};
use x86_64::{VirtAddr, PhysAddr};
use x86_64::registers::control::Cr3;

/// Gestionnaire de tables de pages avec offset mapping
/// 
/// L'offset mapping permet d'accéder à la mémoire physique via:
/// `phys_addr = virt_addr - physical_memory_offset`
pub struct OffsetPageTableManager {
    /// Mapper de x86_64 crate (offset-based)
    mapper: OffsetPageTable<'static>,
    /// Offset pour accès mémoire physique
    physical_memory_offset: VirtAddr,
}

impl OffsetPageTableManager {
    /// Crée un nouveau PageTableManager avec offset mapping
    /// 
    /// # Safety
    /// - L'offset doit correspondre à l'offset utilisé par le bootloader
    /// - Doit être appelé une seule fois
    pub unsafe fn new(physical_memory_offset: VirtAddr) -> Self {
        // Lire la table P4 actuelle depuis CR3
        let (level_4_table_frame, _) = Cr3::read();
        
        // Calculer l'adresse virtuelle de la P4 via l'offset
        let phys = level_4_table_frame.start_address();
        let virt = physical_memory_offset + phys.as_u64();
        
        // SAFETY: The physical memory offset from bootloader maps all physical
        // memory starting at this virtual address. CR3 contains the physical address
        // of the active P4 table. Adding the offset gives its virtual address.
        // The resulting pointer is valid for the lifetime of the kernel.
        let page_table = &mut *(virt.as_mut_ptr::<PageTable>());
        
        // Créer l'OffsetPageTable
        let mapper = OffsetPageTable::new(page_table, physical_memory_offset);
        
        Self {
            mapper,
            physical_memory_offset,
        }
    }
    
    /// Vérifie si une page est déjà mappée
    fn is_page_mapped(&self, page: Page<Size4KiB>) -> bool {
        use x86_64::structures::paging::mapper::TranslateResult;
        matches!(
            self.mapper.translate(page.start_address()),
            TranslateResult::Mapped { .. }
        )
    }
    
    /// Mappe une page virtuelle vers une frame physique
    /// 
    /// # Arguments
    /// * `page` - La page virtuelle à mapper
    /// * `frame` - La frame physique cible
    /// * `flags` - Les flags (PRESENT, WRITABLE, etc.)
    /// * `frame_allocator` - Frame allocator pour créer les tables intermédiaires si nécessaire
    /// 
    /// # Errors
    /// Retourne une erreur si la page est déjà mappée ou si l'allocation de table échoue
    /// 
    /// # Safety
    /// Cette fonction est unsafe car elle modifie les tables de pages actives.
    pub fn map_page(
        &mut self,
        page: Page<Size4KiB>,
        frame: PhysFrame,
        flags: PageTableFlags,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> Result<(), MemoryError> {
        use x86_64::structures::paging::mapper::Mapper;
        
        // SAFETY: page, frame, and flags are validated by caller. The frame comes
        // from FrameAllocator (unique, non-overlapping). frame_allocator may be used
        // to allocate intermediate page table frames (P3/P2/P1). The resulting
        // mapping is flushed from TLB immediately after creation.
        let result = unsafe {
            self.mapper.map_to(page, frame, flags, frame_allocator)
        };
        
        match result {
            Ok(flusher) => {
                flusher.flush();
                Ok(())
            }
            Err(_) => Err(MemoryError::OutOfMemory),
        }
    }
    
    /// Identity map: page virtuelle = adresse physique
    /// Utile pour mapper la mémoire basse (0-4MB)
    pub fn identity_map(
        &mut self,
        frame: PhysFrame,
        flags: PageTableFlags,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> Result<Page<Size4KiB>, MemoryError> {
        let page = Page::containing_address(VirtAddr::new(frame.start_address().as_u64()));
        self.map_page(page, frame, flags, frame_allocator)?;
        Ok(page)
    }
    
    /// Démappe une page virtuelle
    pub fn unmap_page(
        &mut self,
        page: Page<Size4KiB>,
    ) -> Result<PhysFrame, MemoryError> {
        use x86_64::structures::paging::mapper::Mapper;
        
        match self.mapper.unmap(page) {
            Ok((frame, flusher)) => {
                flusher.flush();
                Ok(frame)
            }
            Err(_) => Err(MemoryError::PageNotMapped(page.start_address().as_u64())),
        }
    }
    
    /// Traduit une adresse virtuelle en adresse physique
    pub fn translate(&self, addr: VirtAddr) -> Option<PhysAddr> {
        self.mapper.translate_addr(addr)
    }
    
    /// Traduit une page entière
    pub fn translate_page(&self, page: Page<Size4KiB>) -> Option<PhysFrame<Size4KiB>> {
        self.mapper.translate_addr(page.start_address())
            .map(PhysFrame::containing_address)
    }
    
    /// Mappe une région mémoire avec identity mapping
    /// Utile pour mapper la mémoire kernel
    pub fn identity_map_region(
        &mut self,
        start: PhysAddr,
        end: PhysAddr,
        flags: PageTableFlags,
        frame_allocator: &mut impl x86_64::structures::paging::FrameAllocator<Size4KiB>,
    ) -> Result<(), MemoryError> {
        let start_frame: PhysFrame<Size4KiB> = PhysFrame::containing_address(start);
        let end_addr = end.as_u64().saturating_sub(1);
        let end_frame: PhysFrame<Size4KiB> = PhysFrame::containing_address(PhysAddr::new(end_addr));
        
        // Range manuel
        let mut current_addr = start_frame.start_address().as_u64();
        let end_addr = end_frame.start_address().as_u64();
        
        while current_addr <= end_addr {
            let frame = PhysFrame::containing_address(PhysAddr::new(current_addr));
            self.identity_map(frame, flags, frame_allocator)?;
            current_addr += 4096;
        }
        
        Ok(())
    }
    
    /// Change les flags d'une page existante
    pub fn update_flags(
        &mut self,
        page: Page<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<(), MemoryError> {
        use x86_64::structures::paging::mapper::Mapper;
        
        // SAFETY: The page must be currently mapped (checked by the mapper).
        // Updating flags does not change the physical frame backing, only the
        // permission bits. The TLB is flushed after the update.
        match unsafe { self.mapper.update_flags(page, flags) } {
            Ok(flusher) => {
                flusher.flush();
                Ok(())
            }
            Err(_) => Err(MemoryError::PageNotMapped(page.start_address().as_u64())),
        }
    }
    
    /// Accès à l'offset mémoire physique
    pub fn physical_memory_offset(&self) -> VirtAddr {
        self.physical_memory_offset
    }
    
    /// Convertit adresse physique en virtuelle via offset
    pub fn phys_to_virt(&self, phys: PhysAddr) -> VirtAddr {
        self.physical_memory_offset + phys.as_u64()
    }
}

// Note: NullAllocator supprimé car il empêchait la création de tables intermédiaires
// Le vrai FrameAllocator de frame.rs implémente maintenant x86_64::structures::paging::FrameAllocator
// Ce qui permet à OffsetPageTable::map_to() d'allouer des frames pour les tables P3/P2/P1 si nécessaire

/// Flags de page couramment utilisés
pub mod flags {
    use x86_64::structures::paging::PageTableFlags;
    
    /// Page présente
    pub const PRESENT: PageTableFlags = PageTableFlags::PRESENT;
    /// Page writable
    pub const WRITABLE: PageTableFlags = PageTableFlags::WRITABLE;
    /// Page accessible en user mode
    pub const USER_ACCESSIBLE: PageTableFlags = PageTableFlags::USER_ACCESSIBLE;
    /// Write-through caching
    pub const WRITE_THROUGH: PageTableFlags = PageTableFlags::WRITE_THROUGH;
    /// Disable cache
    pub const NO_CACHE: PageTableFlags = PageTableFlags::NO_CACHE;
    /// Page accessible seulement quand CR0.AC = 0
    pub const ACCESSED: PageTableFlags = PageTableFlags::ACCESSED;
    /// Page modifiée
    pub const DIRTY: PageTableFlags = PageTableFlags::DIRTY;
    /// Huge page (pas pour Size4KiB)
    pub const HUGE_PAGE: PageTableFlags = PageTableFlags::HUGE_PAGE;
    /// Global (TLB pas flush sur context switch)
    pub const GLOBAL: PageTableFlags = PageTableFlags::GLOBAL;
    /// No-execute (NX bit)
    pub const NO_EXECUTE: PageTableFlags = PageTableFlags::NO_EXECUTE;
    
    /// Combinaisons courantes
    pub const KERNEL_CODE: PageTableFlags = PRESENT;
    pub const KERNEL_DATA: PageTableFlags = PRESENT.union(WRITABLE);
    pub const KERNEL_RO: PageTableFlags = PRESENT;
    pub const USER_CODE: PageTableFlags = PRESENT.union(USER_ACCESSIBLE);
    pub const USER_DATA: PageTableFlags = PRESENT.union(WRITABLE).union(USER_ACCESSIBLE);
}

/// Tests unitaires
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::frame::FrameAllocator;
    
    // Note: Les tests de paging nécessitent un environnement de boot complet
    // Ces tests sont des stubs qui vérifient la compilation
    
    #[test_case]
    fn test_paging_module_compiles() {
        assert_eq!(1 + 1, 2);
    }
    
    #[test_case]
    fn test_page_flags() {
        use x86_64::structures::paging::PageTableFlags;
        
        let flags = flags::KERNEL_DATA;
        assert!(flags.contains(PageTableFlags::PRESENT));
        assert!(flags.contains(PageTableFlags::WRITABLE));
    }
}
