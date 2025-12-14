// Aetherion OS - Heap Allocator Module
// Phase 1.3: Dynamic memory allocation with GlobalAlloc

pub mod bump;
pub mod linked_list;

use bump::BumpAllocator;

/// Global allocator instance
#[global_allocator]
static ALLOCATOR: LockedAllocator = LockedAllocator::new();

/// Locked allocator wrapper for thread-safety
pub struct LockedAllocator {
    inner: spin::Mutex<BumpAllocator>,
}

impl LockedAllocator {
    pub const fn new() -> Self {
        LockedAllocator {
            inner: spin::Mutex::new(BumpAllocator::new()),
        }
    }
}

unsafe impl core::alloc::GlobalAlloc for LockedAllocator {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        self.inner.lock().alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
        self.inner.lock().dealloc(ptr, layout)
    }
}

/// Initialize the heap allocator
/// 
/// # Safety
/// Must be called exactly once at boot, before any heap allocations
pub unsafe fn init_heap(heap_start: usize, heap_size: usize) {
    ALLOCATOR.inner.lock().init(heap_start, heap_size);
}

/// Get heap statistics
pub fn heap_stats() -> HeapStats {
    ALLOCATOR.inner.lock().stats()
}

/// Heap statistics
#[derive(Debug, Clone, Copy)]
pub struct HeapStats {
    pub heap_start: usize,
    pub heap_size: usize,
    pub used: usize,
    pub free: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heap_stats() {
        unsafe {
            init_heap(0x10000, 1024);
        }
        
        let stats = heap_stats();
        assert_eq!(stats.heap_start, 0x10000);
        assert_eq!(stats.heap_size, 1024);
    }
}
