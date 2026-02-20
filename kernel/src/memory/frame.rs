// memory/frame.rs - Frame Allocator (Physical Memory)
// Bitmap-based avec inline metadata pour ACHA Resource Tagging

use super::MemoryError;
use super::resource_tag::{ResourceTag, AllocationType};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use x86_64::structures::paging::PhysFrame;
use x86_64::PhysAddr;

/// Taille d'une frame (4KB standard x86_64)
pub const FRAME_SIZE: usize = 4096;

/// Nombre maximum de frames supporté (64K frames = 256 MB pour tests sandbox)
/// Limite adaptée pour environnement sandbox (~537 MB disponible)
pub const MAX_FRAMES: usize = 131072; // 128K frames = 512 MB max

/// Nombre d'entrées dans le bitmap (64 bits par entrée)
const BITMAP_ENTRIES: usize = MAX_FRAMES / 64;

/// Frame Allocator - Gestion mémoire physique
/// 
/// Utilise un bitmap atomique pour thread-safety sans lock global
/// Les métadonnées ACHA sont stockées inline pour O(1) accès
pub struct FrameAllocator {
    /// Bitmap d'allocation: 1 = occupé, 0 = libre
    bitmap: [AtomicU64; BITMAP_ENTRIES],
    /// Index du prochain frame libre probable (hint pour performance)
    next_free_hint: AtomicUsize,
    /// Nombre total de frames gérés
    total_frames: usize,
    /// Nombre de frames utilisés
    used_frames: AtomicUsize,
    /// Métadonnées ACHA inline (par frame)
    /// Stocke ResourceTag directement pour éviter heap allocation
    metadata: [Option<ResourceTag>; MAX_FRAMES],
}

impl FrameAllocator {
    /// Crée un nouveau FrameAllocator à partir des régions de mémoire
    /// 
    /// # Safety
    /// Doit être appelé une seule fois avant toute allocation
    pub unsafe fn new(regions: &[(u64, u64)]) -> Self {
        let mut allocator = Self {
            bitmap: core::array::from_fn(|_| AtomicU64::new(0)),
            next_free_hint: AtomicUsize::new(0),
            total_frames: 0,
            used_frames: AtomicUsize::new(0),
            metadata: [None; MAX_FRAMES],
        };
        
        // Marquer les frames des régions utilisables
        for (start, end) in regions.iter() {
            allocator.add_region(*start, *end);
        }
        
        allocator
    }
    
    /// Ajoute une région de mémoire utilisable
    fn add_region(&mut self, start: u64, end: u64) {
        let start_frame = (start as usize + FRAME_SIZE - 1) / FRAME_SIZE;
        let end_frame = (end as usize) / FRAME_SIZE;
        
        // Limiter à MAX_FRAMES
        let start_frame = start_frame.min(MAX_FRAMES);
        let end_frame = end_frame.min(MAX_FRAMES);
        
        if end_frame > start_frame {
            self.total_frames += end_frame - start_frame;
        }
    }
    
    /// Alloue un frame physique
    /// 
    /// # ACHA Integration
    /// Si pid != 0, tagge l'allocation avec l'ID du processus
    pub fn alloc_frame(&mut self, pid: u64) -> Option<PhysFrame> {
        // Trouver le premier bit libre dans le bitmap
        let frame_idx = self.find_free_frame()?;
        
        // Marquer comme alloué
        self.set_frame_allocated(frame_idx, true);
        
        // Incrémenter compteur
        self.used_frames.fetch_add(1, Ordering::Relaxed);
        
        // Tag ACHA si demandé
        if pid != 0 {
            self.metadata[frame_idx] = Some(ResourceTag {
                process_id: pid,
                timestamp: crate::arch::timer::read_tsc(),
                allocation_type: AllocationType::Frame,
            });
        }
        
        // Mettre à jour hint pour prochaine allocation
        self.next_free_hint.store(frame_idx + 1, Ordering::Relaxed);
        
        // Créer le PhysFrame
        let phys_addr = PhysAddr::new((frame_idx * FRAME_SIZE) as u64);
        PhysFrame::from_start_address(phys_addr).ok()
    }
    
    /// Alloue un frame pour le kernel (pid = 0, pas de tag)
    pub fn alloc_frame_kernel(&mut self) -> Option<PhysFrame> {
        self.alloc_frame(0)
    }
    
    /// Désalloue un frame physique
    /// 
    /// # Errors
    /// Retourne Err si le frame n'était pas alloué
    pub fn dealloc_frame(&mut self, frame: PhysFrame) -> Result<(), MemoryError> {
        let frame_idx = (frame.start_address().as_u64() as usize) / FRAME_SIZE;
        
        if frame_idx >= MAX_FRAMES {
            return Err(MemoryError::FrameNotAllocated(frame.start_address().as_u64()));
        }
        
        // Vérifier qu'il était alloué
        if !self.is_frame_allocated(frame_idx) {
            return Err(MemoryError::FrameNotAllocated(frame.start_address().as_u64()));
        }
        
        // Marquer comme libre
        self.set_frame_allocated(frame_idx, false);
        
        // Décrémenter compteur
        self.used_frames.fetch_sub(1, Ordering::Relaxed);
        
        // Nettoyer métadonnées ACHA
        self.metadata[frame_idx] = None;
        
        // Mettre à jour hint si c'est plus tôt
        let current_hint = self.next_free_hint.load(Ordering::Relaxed);
        if frame_idx < current_hint {
            self.next_free_hint.store(frame_idx, Ordering::Relaxed);
        }
        
        Ok(())
    }
    
    /// Trouve le premier frame libre (First-Fit)
    fn find_free_frame(&self) -> Option<usize> {
        let start_hint = self.next_free_hint.load(Ordering::Relaxed);
        
        // Chercher à partir du hint
        for entry_idx in (start_hint / 64)..BITMAP_ENTRIES {
            let bitmap_val = self.bitmap[entry_idx].load(Ordering::Relaxed);
            
            if bitmap_val != u64::MAX {
                // Il y a au moins un bit libre
                let bit_idx = (!bitmap_val).trailing_zeros() as usize;
                if bit_idx < 64 {
                    let frame_idx = entry_idx * 64 + bit_idx;
                    if frame_idx < self.total_frames {
                        return Some(frame_idx);
                    }
                }
            }
        }
        
        // Chercher depuis le début si pas trouvé
        for entry_idx in 0..(start_hint / 64) {
            let bitmap_val = self.bitmap[entry_idx].load(Ordering::Relaxed);
            
            if bitmap_val != u64::MAX {
                let bit_idx = (!bitmap_val).trailing_zeros() as usize;
                if bit_idx < 64 {
                    let frame_idx = entry_idx * 64 + bit_idx;
                    if frame_idx < self.total_frames {
                        return Some(frame_idx);
                    }
                }
            }
        }
        
        None
    }
    
    /// Vérifie si un frame est alloué
    fn is_frame_allocated(&self, frame_idx: usize) -> bool {
        let entry_idx = frame_idx / 64;
        let bit_idx = frame_idx % 64;
        
        let bitmap_val = self.bitmap[entry_idx].load(Ordering::Relaxed);
        (bitmap_val >> bit_idx) & 1 == 1
    }
    
    /// Définit l'état d'allocation d'un frame
    fn set_frame_allocated(&self, frame_idx: usize, allocated: bool) {
        let entry_idx = frame_idx / 64;
        let bit_idx = frame_idx % 64;
        let mask = 1u64 << bit_idx;
        
        loop {
            let current = self.bitmap[entry_idx].load(Ordering::Relaxed);
            let new_val = if allocated {
                current | mask
            } else {
                current & !mask
            };
            
            match self.bitmap[entry_idx].compare_exchange_weak(
                current,
                new_val,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(_) => continue, // Retry
            }
        }
    }
    
    /// Nombre total de frames gérés
    pub fn total_frames(&self) -> usize {
        self.total_frames
    }
    
    /// Nombre de frames utilisés
    pub fn used_frames(&self) -> usize {
        self.used_frames.load(Ordering::Relaxed)
    }
    
    /// Nombre de frames libres
    pub fn free_frames(&self) -> usize {
        self.total_frames.saturating_sub(self.used_frames())
    }
    
    /// Taux d'utilisation (0-100)
    pub fn utilization_percent(&self) -> u8 {
        if self.total_frames == 0 {
            return 0;
        }
        ((self.used_frames() * 100) / self.total_frames) as u8
    }
    
    /// Audit ACHA: compte les allocations d'un processus
    pub fn audit_process_allocations(&self, pid: u64) -> usize {
        let mut count = 0;
        for i in 0..self.total_frames.min(MAX_FRAMES) {
            if let Some(ref tag) = self.metadata[i] {
                if tag.process_id == pid {
                    count += 1;
                }
            }
        }
        count
    }
    
    /// Récupère le tag d'un frame (pour debugging)
    pub fn get_frame_tag(&self, frame: PhysFrame) -> Option<&ResourceTag> {
        let frame_idx = (frame.start_address().as_u64() as usize) / FRAME_SIZE;
        if frame_idx < MAX_FRAMES {
            self.metadata[frame_idx].as_ref()
        } else {
            None
        }
    }
}

impl Default for FrameAllocator {
    fn default() -> Self {
        unsafe { Self::new(&[]) }
    }
}

/// Tests unitaires (adaptés pour sandbox)
#[cfg(test)]
mod tests {
    use super::*;
    
    /// Région de test simulée (128 MB)
    fn test_region() -> [(u64, u64); 1] {
        [(0x100000, 0x8100000)] // 128 MB starting at 1MB
    }
    
    #[test_case]
    fn test_allocator_creation() {
        let regions = test_region();
        let allocator = unsafe { FrameAllocator::new(&regions) };
        
        // 128 MB / 4KB = 32768 frames
        assert!(allocator.total_frames() > 0);
        assert_eq!(allocator.used_frames(), 0);
        assert_eq!(allocator.utilization_percent(), 0);
    }
    
    #[test_case]
    fn test_alloc_dealloc_single() {
        let regions = test_region();
        let mut allocator = unsafe { FrameAllocator::new(&regions) };
        
        // Allouer un frame
        let frame = allocator.alloc_frame_kernel().expect("Allocation should succeed");
        assert_eq!(allocator.used_frames(), 1);
        
        // Désallouer
        allocator.dealloc_frame(frame).expect("Deallocation should succeed");
        assert_eq!(allocator.used_frames(), 0);
    }
    
    #[test_case]
    fn test_alloc_many_frames() {
        let regions = test_region();
        let mut allocator = unsafe { FrameAllocator::new(&regions) };
        
        // Allouer 1000 frames (test sandbox-friendly)
        const NUM_FRAMES: usize = 1000;
        let mut frames = [None; NUM_FRAMES];
        
        for i in 0..NUM_FRAMES {
            frames[i] = allocator.alloc_frame_kernel();
            assert!(frames[i].is_some(), "Frame {} should allocate", i);
        }
        
        assert_eq!(allocator.used_frames(), NUM_FRAMES);
        
        // Désallouer tous
        for frame in frames.iter().flatten() {
            allocator.dealloc_frame(*frame).unwrap();
        }
        
        assert_eq!(allocator.used_frames(), 0);
    }
    
    #[test_case]
    fn test_acha_resource_tagging() {
        let regions = test_region();
        let mut allocator = unsafe { FrameAllocator::new(&regions) };
        
        // Allouer avec tag (pid = 42)
        let frame = allocator.alloc_frame(42).expect("Allocation should succeed");
        
        // Vérifier le tag
        let tag = allocator.get_frame_tag(frame).expect("Tag should exist");
        assert_eq!(tag.process_id, 42);
        assert!(matches!(tag.allocation_type, AllocationType::Frame));
        
        // Audit
        let count = allocator.audit_process_allocations(42);
        assert_eq!(count, 1);
        
        // Désallouer
        allocator.dealloc_frame(frame).unwrap();
        
        // Vérifier audit après désallocation
        let count_after = allocator.audit_process_allocations(42);
        assert_eq!(count_after, 0);
    }
    
    #[test_case]
    fn test_fragmentation_resistance() {
        let regions = test_region();
        let mut allocator = unsafe { FrameAllocator::new(&regions) };
        
        // Allouer 1000 frames
        const NUM_FRAMES: usize = 1000;
        let mut frames = [None; NUM_FRAMES];
        
        for i in 0..NUM_FRAMES {
            frames[i] = allocator.alloc_frame_kernel();
        }
        
        // Désallouer 1 frame sur 2 (créer fragmentation)
        for (i, frame_opt) in frames.iter_mut().enumerate() {
            if i % 2 == 0 {
                if let Some(frame) = frame_opt.take() {
                    allocator.dealloc_frame(frame).unwrap();
                }
            }
        }
        
        // Réallouer 500 frames (devrait réutiliser les trous)
        let mut new_frames = 0;
        for _ in 0..500 {
            if allocator.alloc_frame_kernel().is_some() {
                new_frames += 1;
            }
        }
        
        assert_eq!(new_frames, 500, "Should reallocate into fragmented holes");
    }
    
    #[test_case]
    fn test_dealloc_not_allocated_fails() {
        let regions = test_region();
        let mut allocator = unsafe { FrameAllocator::new(&regions) };
        
        // Créer un frame fictif non alloué
        let fake_frame = PhysFrame::from_start_address(PhysAddr::new(0x100000)).unwrap();
        
        // La désallocation devrait échouer
        let result = allocator.dealloc_frame(fake_frame);
        assert!(result.is_err());
    }
}
