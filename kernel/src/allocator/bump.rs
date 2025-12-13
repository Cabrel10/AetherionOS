// Aetherion OS - Bump Allocator
// Phase 1.3: Simple bump-pointer heap allocator

use core::alloc::{GlobalAlloc, Layout};
use core::ptr;
use super::HeapStats;

/// Bump Allocator - simplest allocator (no deallocation)
/// 
/// Allocations increment a pointer (the "bump")
/// Deallocation is a no-op (memory freed only when allocator reset)
/// 
/// Pros:
/// - Extremely fast allocation (O(1))
/// - Simple implementation
/// - No fragmentation
/// 
/// Cons:
/// - No deallocation (memory leak until reset)
/// - Wastes memory if objects are freed
/// 
/// Use case: Early boot, short-lived allocations
pub struct BumpAllocator {
    heap_start: usize,
    heap_end: usize,
    next: usize,
    allocations: usize,
}

impl BumpAllocator {
    /// Create a new uninitialized bump allocator
    pub const fn new() -> Self {
        BumpAllocator {
            heap_start: 0,
            heap_end: 0,
            next: 0,
            allocations: 0,
        }
    }

    /// Initialize the allocator with a heap region
    /// 
    /// # Safety
    /// The heap region [heap_start, heap_start + heap_size) must be valid
    /// and not used by anything else
    pub unsafe fn init(&mut self, heap_start: usize, heap_size: usize) {
        self.heap_start = heap_start;
        self.heap_end = heap_start + heap_size;
        self.next = heap_start;
        self.allocations = 0;
    }

    /// Allocate memory with given layout
    pub unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        // Align the next pointer
        let alloc_start = align_up(self.next, layout.align());
        let alloc_end = alloc_start.checked_add(layout.size())?;

        if alloc_end > self.heap_end {
            // Out of memory
            ptr::null_mut()
        } else {
            self.next = alloc_end;
            self.allocations += 1;
            alloc_start as *mut u8
        }
    }

    /// Deallocate memory (no-op for bump allocator)
    pub unsafe fn dealloc(&mut self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator doesn't support deallocation
        // Memory is freed when allocator is reset
    }

    /// Get heap statistics
    pub fn stats(&self) -> HeapStats {
        let used = self.next.saturating_sub(self.heap_start);
        let free = self.heap_end.saturating_sub(self.next);

        HeapStats {
            heap_start: self.heap_start,
            heap_size: self.heap_end - self.heap_start,
            used,
            free,
        }
    }

    /// Reset the allocator (free all memory)
    pub unsafe fn reset(&mut self) {
        self.next = self.heap_start;
        self.allocations = 0;
    }

    /// Get number of allocations
    pub fn allocation_count(&self) -> usize {
        self.allocations
    }
}

/// Align address up to nearest multiple of align
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 4), 0);
        assert_eq!(align_up(1, 4), 4);
        assert_eq!(align_up(4, 4), 4);
        assert_eq!(align_up(5, 4), 8);
        assert_eq!(align_up(0x1234, 4096), 0x2000);
    }

    #[test]
    fn test_bump_allocator() {
        let mut allocator = BumpAllocator::new();
        
        unsafe {
            allocator.init(0x10000, 4096);
        }

        let stats = allocator.stats();
        assert_eq!(stats.heap_start, 0x10000);
        assert_eq!(stats.heap_size, 4096);
        assert_eq!(stats.used, 0);
        assert_eq!(stats.free, 4096);
    }

    #[test]
    fn test_allocation() {
        let mut allocator = BumpAllocator::new();
        
        unsafe {
            allocator.init(0x10000, 4096);

            let layout1 = Layout::from_size_align(16, 8).unwrap();
            let ptr1 = allocator.alloc(layout1);
            assert!(!ptr1.is_null());
            assert_eq!(ptr1 as usize, 0x10000);

            let layout2 = Layout::from_size_align(32, 8).unwrap();
            let ptr2 = allocator.alloc(layout2);
            assert!(!ptr2.is_null());
            assert_eq!(ptr2 as usize, 0x10010); // After 16 bytes

            assert_eq!(allocator.allocation_count(), 2);
        }
    }

    #[test]
    fn test_out_of_memory() {
        let mut allocator = BumpAllocator::new();
        
        unsafe {
            allocator.init(0x10000, 64); // Small heap

            // Allocate entire heap
            let layout = Layout::from_size_align(64, 1).unwrap();
            let ptr1 = allocator.alloc(layout);
            assert!(!ptr1.is_null());

            // Next allocation should fail
            let ptr2 = allocator.alloc(layout);
            assert!(ptr2.is_null());
        }
    }

    #[test]
    fn test_reset() {
        let mut allocator = BumpAllocator::new();
        
        unsafe {
            allocator.init(0x10000, 4096);

            let layout = Layout::from_size_align(16, 8).unwrap();
            allocator.alloc(layout);
            allocator.alloc(layout);

            assert_eq!(allocator.allocation_count(), 2);

            allocator.reset();
            assert_eq!(allocator.allocation_count(), 0);
            
            let stats = allocator.stats();
            assert_eq!(stats.used, 0);
        }
    }
}
