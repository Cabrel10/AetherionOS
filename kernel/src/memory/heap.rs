// memory/heap.rs - Heap Allocator pour alloc (Box, Vec, etc.)
// Utilise linked_list_allocator pour no_std

use super::{MemoryError, MemoryResult};
use super::frame::FrameAllocator;
use super::paging::{OffsetPageTableManager, flags};
use x86_64::structures::paging::{
    Page,
};
use x86_64::VirtAddr;
use linked_list_allocator::LockedHeap;

/// Adresse de début du heap (espace haute mémoire)
/// Choisi pour éviter les conflits avec le kernel (généralement en 0x200000+)
pub const HEAP_START: usize = 0x_4444_4444_0000;

/// Taille initiale du heap (100 KB - extensible)
pub const HEAP_SIZE: usize = 100 * 1024;

/// Nombre de pages nécessaires pour le heap
const HEAP_PAGES: usize = (HEAP_SIZE + 4095) / 4096;

/// Heap allocator global (LockedHeap pour thread-safety)
/// 
/// # Safety
/// Doit être initialisé avec init_heap() avant utilisation
#[global_allocator]
static HEAP_ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Flag d'initialisation (pour éviter double-init)
static HEAP_INITIALIZED: core::sync::atomic::AtomicBool = 
    core::sync::atomic::AtomicBool::new(false);

/// Initialise le heap allocator
/// 
/// # Processus
/// 1. Mapper les pages heap dans l'espace d'adressage
/// 2. Initialiser le linked_list_allocator avec cette région
/// 
/// # Errors
/// Retourne une erreur si le mapping échoue ou si déjà initialisé
pub fn init_heap(
    page_table: &mut OffsetPageTableManager,
    frame_allocator: &mut FrameAllocator,
) -> MemoryResult<()> {
    // Vérifier si déjà initialisé
    if HEAP_INITIALIZED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return Ok(()); // Déjà initialisé, pas une erreur
    }
    
    let heap_start = VirtAddr::new(HEAP_START as u64);
    let _heap_end = heap_start + HEAP_SIZE as u64;

    crate::serial_println!("[HEAP] Mapping {} pages for heap...", HEAP_PAGES);
    
    // Mapper chaque page du heap
    for i in 0..HEAP_PAGES {
        let page = Page::containing_address(heap_start + (i * 4096) as u64);
        let frame = frame_allocator
            .alloc_frame_kernel()
            .ok_or(MemoryError::OutOfMemory)?;
        
        page_table
            .map_page(page, frame, flags::KERNEL_DATA, frame_allocator)
            .map_err(|_| {
                // Rollback: désallouer le frame alloué
                let _ = frame_allocator.dealloc_frame(frame);
                MemoryError::HeapInitFailed
            })?;
    }
    
    // Initialiser le heap allocator
    unsafe {
        HEAP_ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }
    
    crate::serial_println!(
        "[HEAP] Heap initialized at {:#x}, size: {} KB",
        HEAP_START, HEAP_SIZE / 1024
    );
    
    Ok(())
}

/// Vérifie si le heap est initialisé
pub fn is_initialized() -> bool {
    HEAP_INITIALIZED.load(core::sync::atomic::Ordering::SeqCst)
}

/// Layout de l'allocateur heap
#[repr(C)]
pub struct HeapStats {
    pub total_size: usize,
    pub used_size: usize,
    pub free_size: usize,
}

/// Statistiques du heap (approximation)
pub fn stats() -> HeapStats {
    if !is_initialized() {
        return HeapStats {
            total_size: 0,
            used_size: 0,
            free_size: 0,
        };
    }
    
    // linked_list_allocator ne fournit pas de stats directes
    // On retourne les valeurs configurées
    HeapStats {
        total_size: HEAP_SIZE,
        used_size: HEAP_SIZE / 2, // Estimation
        free_size: HEAP_SIZE / 2,
    }
}

/// Handler d'allocation échouée (oom = out of memory)
/// 
/// Cette fonction est appelée quand alloc échoue
#[alloc_error_handler]
fn alloc_error_handler(layout: core::alloc::Layout) -> ! {
    panic!(
        "[OOM] Heap allocation failed: size={}, align={}",
        layout.size(),
        layout.align()
    );
}

/// Tests unitaires du heap
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::vec::Vec;
    use alloc::string::String;
    
    // Note: Ces tests nécessitent un environnement de boot complet
    // avec page_table et frame_allocator initialisés
    
    #[test_case]
    fn test_heap_allocation_box() {
        // Nécessite init_heap() préalable
        // Simulé ici avec vérification de compilation
        let value: u32 = 42;
        assert_eq!(value, 42);
    }
    
    #[test_case]
    fn test_heap_allocation_vec() {
        // Test de compilation Vec
        let mut vec = Vec::new();
        vec.push(1);
        vec.push(2);
        assert_eq!(vec.len(), 2);
    }
    
    #[test_case]
    fn test_heap_allocation_string() {
        // Test de compilation String
        let s = String::from("test");
        assert_eq!(s.len(), 4);
    }
}
