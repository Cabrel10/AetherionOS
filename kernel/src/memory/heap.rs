// memory/heap.rs - Heap Allocator pour alloc (Box, Vec, etc.)
// Utilise linked_list_allocator pour no_std

use super::{MemoryError, MemoryResult};
use super::frame::FrameAllocator;
use super::paging::{OffsetPageTableManager, flags};
use x86_64::structures::paging::Page;
use x86_64::VirtAddr;
use linked_list_allocator::LockedHeap;

/// Adresse de debut du heap
pub const HEAP_START: usize = 0x_4444_4444_0000;

/// Taille initiale du heap (100 KB)
pub const HEAP_SIZE: usize = 100 * 1024;

/// Nombre de pages necessaires pour le heap
const HEAP_PAGES: usize = (HEAP_SIZE + 4095) / 4096;

/// Heap allocator global
#[global_allocator]
static HEAP_ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Flag d'initialisation
static HEAP_INITIALIZED: core::sync::atomic::AtomicBool = 
    core::sync::atomic::AtomicBool::new(false);

/// Initialise le heap allocator
pub fn init_heap(
    page_table: &mut OffsetPageTableManager,
    frame_allocator: &mut FrameAllocator,
) -> MemoryResult<()> {
    if HEAP_INITIALIZED.swap(true, core::sync::atomic::Ordering::SeqCst) {
        return Ok(());
    }
    
    let heap_start = VirtAddr::new(HEAP_START as u64);

    crate::serial_write("[HEAP] Mapping pages...\n");
    
    // Map each heap page
    for i in 0..HEAP_PAGES {
        let page = Page::containing_address(heap_start + (i * 4096) as u64);
        
        let frame = frame_allocator
            .alloc_frame_kernel()
            .ok_or(MemoryError::OutOfMemory)?;
        
        page_table
            .map_page(page, frame, flags::KERNEL_DATA, frame_allocator)
            .map_err(|_| MemoryError::HeapInitFailed)?;
    }
    
    crate::serial_write("[HEAP] Pages mapped, initializing allocator...\n");
    
    // Initialize the heap allocator
    unsafe {
        HEAP_ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }
    
    {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = write!(s, "[HEAP] Ready: {} KB at {:#x}\n", HEAP_SIZE / 1024, HEAP_START);
        crate::serial_write(&s);
    }
    
    Ok(())
}

/// Verifie si le heap est initialise
pub fn is_initialized() -> bool {
    HEAP_INITIALIZED.load(core::sync::atomic::Ordering::SeqCst)
}

/// Handler d'allocation echouee
#[alloc_error_handler]
fn alloc_error_handler(layout: core::alloc::Layout) -> ! {
    panic!(
        "[OOM] Heap allocation failed: size={}, align={}",
        layout.size(),
        layout.align()
    );
}
