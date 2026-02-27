// gpu/allocator.rs - Couche 8: VRAM Bump Allocator
//
// Simple bump allocator for GPU VRAM. Allocates contiguous blocks from
// a base address with a fixed capacity. No deallocation (bump-only).

/// A simple bump allocator for VRAM regions
#[derive(Debug, Clone, Copy)]
pub struct VramAllocator {
    /// Base physical address of VRAM (from BAR0)
    base: u64,
    /// Total VRAM size in bytes
    capacity: usize,
    /// Current allocation offset
    offset: usize,
    /// Number of allocations made
    alloc_count: u32,
}

impl VramAllocator {
    /// Create a new VRAM allocator with the given base address and capacity
    pub const fn new(base: u64, capacity: usize) -> Self {
        VramAllocator {
            base,
            capacity,
            offset: 0,
            alloc_count: 0,
        }
    }

    /// Allocate `size` bytes from VRAM. Returns the physical address or None.
    pub fn allocate(&mut self, size: usize) -> Option<u64> {
        // Align to 4KB page boundary
        let aligned_size = (size + 0xFFF) & !0xFFF;
        if self.offset + aligned_size > self.capacity {
            return None;
        }
        let addr = self.base + self.offset as u64;
        self.offset += aligned_size;
        self.alloc_count += 1;
        Some(addr)
    }

    /// Reset the allocator (free all)
    pub fn reset(&mut self) {
        self.offset = 0;
        self.alloc_count = 0;
    }

    /// Get the base address
    pub fn base(&self) -> u64 { self.base }

    /// Get the total capacity
    pub fn capacity(&self) -> usize { self.capacity }

    /// Get the used bytes
    pub fn used(&self) -> usize { self.offset }

    /// Get the remaining free bytes
    pub fn free(&self) -> usize { self.capacity - self.offset }

    /// Get the number of allocations
    pub fn alloc_count(&self) -> u32 { self.alloc_count }
}

impl core::fmt::Display for VramAllocator {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "VRAM[base=0x{:08X}, cap={}KB, used={}KB, allocs={}]",
            self.base, self.capacity / 1024, self.offset / 1024, self.alloc_count)
    }
}
