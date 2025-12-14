// Aetherion OS - Linked List Allocator
// Phase 1.3: Free-list heap allocator with deallocation support

use core::alloc::{GlobalAlloc, Layout};
use core::mem;
use core::ptr;

/// Minimum block size (must hold ListNode)
const MIN_BLOCK_SIZE: usize = mem::size_of::<ListNode>();

/// Node in the free list
struct ListNode {
    size: usize,
    next: Option<&'static mut ListNode>,
}

impl ListNode {
    const fn new(size: usize) -> Self {
        ListNode { size, next: None }
    }

    fn start_addr(&self) -> usize {
        self as *const Self as usize
    }

    fn end_addr(&self) -> usize {
        self.start_addr() + self.size
    }
}

/// Linked List Allocator
/// 
/// Maintains a linked list of free blocks
/// Allocations find a suitable free block (first-fit)
/// Deallocations return blocks to the free list
/// 
/// Pros:
/// - Supports deallocation
/// - Can reuse freed memory
/// - Reasonable performance
/// 
/// Cons:
/// - External fragmentation
/// - Slower than bump allocator
/// - Requires minimum allocation size
pub struct LinkedListAllocator {
    head: ListNode,
}

impl LinkedListAllocator {
    /// Create a new empty allocator
    pub const fn new() -> Self {
        LinkedListAllocator {
            head: ListNode::new(0),
        }
    }

    /// Initialize allocator with a heap region
    /// 
    /// # Safety
    /// The heap region must be valid and unused
    pub unsafe fn init(&mut self, heap_start: usize, heap_size: usize) {
        self.add_free_region(heap_start, heap_size);
    }

    /// Add a free region to the free list
    unsafe fn add_free_region(&mut self, addr: usize, size: usize) {
        // Ensure region is large enough
        assert!(size >= MIN_BLOCK_SIZE);
        assert!(addr % mem::align_of::<ListNode>() == 0);

        // Create new node
        let mut node = ListNode::new(size);
        node.next = self.head.next.take();

        // Write node to start of region
        let node_ptr = addr as *mut ListNode;
        node_ptr.write(node);

        // Update head
        self.head.next = Some(&mut *node_ptr);
    }

    /// Find a suitable free region (first-fit strategy)
    fn find_region(&mut self, size: usize, align: usize) -> Option<(&'static mut ListNode, usize)> {
        let mut current = &mut self.head;

        while let Some(ref mut region) = current.next {
            if let Ok(alloc_start) = Self::alloc_from_region(&region, size, align) {
                // Region is suitable
                let next = region.next.take();
                let ret = Some((current.next.take().unwrap(), alloc_start));
                current.next = next;
                return ret;
            } else {
                // Try next region
                current = current.next.as_mut().unwrap();
            }
        }

        None
    }

    /// Try to allocate from a specific region
    fn alloc_from_region(region: &ListNode, size: usize, align: usize) -> Result<usize, ()> {
        let alloc_start = align_up(region.start_addr(), align);
        let alloc_end = alloc_start.checked_add(size).ok_or(())?;

        if alloc_end > region.end_addr() {
            return Err(());
        }

        let excess_size = region.end_addr() - alloc_end;
        if excess_size > 0 && excess_size < MIN_BLOCK_SIZE {
            // Remaining space too small for a free block
            return Err(());
        }

        Ok(alloc_start)
    }

    /// Allocate memory
    pub unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let size = layout.size().max(MIN_BLOCK_SIZE);
        let size = align_up(size, mem::align_of::<ListNode>());

        if let Some((region, alloc_start)) = self.find_region(size, layout.align()) {
            let alloc_end = alloc_start.checked_add(size).expect("overflow");
            let excess_size = region.end_addr() - alloc_end;

            if excess_size > 0 {
                // Put remaining space back in free list
                self.add_free_region(alloc_end, excess_size);
            }

            alloc_start as *mut u8
        } else {
            ptr::null_mut()
        }
    }

    /// Deallocate memory
    pub unsafe fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        let size = layout.size().max(MIN_BLOCK_SIZE);
        let size = align_up(size, mem::align_of::<ListNode>());

        self.add_free_region(ptr as usize, size);
    }
}

/// Align address up to alignment
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linked_list_allocator() {
        let mut allocator = LinkedListAllocator::new();
        
        // Create heap region
        let mut heap = [0u8; 1024];
        let heap_start = heap.as_mut_ptr() as usize;
        
        unsafe {
            allocator.init(heap_start, 1024);
        }
    }

    #[test]
    fn test_alloc_dealloc() {
        let mut allocator = LinkedListAllocator::new();
        let mut heap = [0u8; 1024];
        
        unsafe {
            allocator.init(heap.as_mut_ptr() as usize, 1024);

            // Allocate
            let layout = Layout::from_size_align(64, 8).unwrap();
            let ptr1 = allocator.alloc(layout);
            assert!(!ptr1.is_null());

            // Deallocate
            allocator.dealloc(ptr1, layout);

            // Allocate again (should reuse freed block)
            let ptr2 = allocator.alloc(layout);
            assert!(!ptr2.is_null());
        }
    }

    #[test]
    fn test_multiple_allocs() {
        let mut allocator = LinkedListAllocator::new();
        let mut heap = [0u8; 4096];
        
        unsafe {
            allocator.init(heap.as_mut_ptr() as usize, 4096);

            let layout = Layout::from_size_align(16, 8).unwrap();
            
            // Allocate 10 blocks
            let mut ptrs = Vec::new();
            for _ in 0..10 {
                let ptr = allocator.alloc(layout);
                assert!(!ptr.is_null());
                ptrs.push(ptr);
            }

            // Deallocate all
            for ptr in ptrs {
                allocator.dealloc(ptr, layout);
            }
        }
    }
}
