// memory/frame.rs - Frame Allocator (Physical Memory)
// Simple next-fit allocator tracking usable physical frames

use super::MemoryError;
use x86_64::structures::paging::PhysFrame;
use x86_64::PhysAddr;

/// Taille d'une frame (4KB standard x86_64)
pub const FRAME_SIZE: usize = 4096;

/// Maximum number of usable frames we track
/// Each entry stores the physical frame number
const MAX_USABLE_FRAMES: usize = 8192;

/// Frame Allocator - tracks usable physical frames
pub struct FrameAllocator {
    /// Array of usable frame physical addresses (base address / 4096)
    usable_frames: [u64; MAX_USABLE_FRAMES],
    /// Total number of usable frames discovered
    total_frames: usize,
    /// Next frame to allocate (index into usable_frames)
    next_alloc: usize,
}

impl FrameAllocator {
    /// Create a new FrameAllocator from bootloader memory regions
    pub unsafe fn new(regions: &[(u64, u64)]) -> Self {
        let mut allocator = Self {
            usable_frames: [0u64; MAX_USABLE_FRAMES],
            total_frames: 0,
            next_alloc: 0,
        };
        
        for (start, end) in regions.iter() {
            allocator.add_region(*start, *end);
        }
        
        allocator
    }
    
    /// Add a usable memory region
    fn add_region(&mut self, start: u64, end: u64) {
        let start_frame = (start + FRAME_SIZE as u64 - 1) / FRAME_SIZE as u64;
        let end_frame = end / FRAME_SIZE as u64;
        
        let mut frame = start_frame;
        while frame < end_frame && self.total_frames < MAX_USABLE_FRAMES {
            self.usable_frames[self.total_frames] = frame;
            self.total_frames += 1;
            frame += 1;
        }
    }
    
    /// Allocate a physical frame for kernel use
    pub fn alloc_frame_kernel(&mut self) -> Option<PhysFrame> {
        if self.next_alloc >= self.total_frames {
            return None;
        }
        
        let frame_num = self.usable_frames[self.next_alloc];
        self.next_alloc += 1;
        
        let phys_addr = PhysAddr::new(frame_num * FRAME_SIZE as u64);
        PhysFrame::from_start_address(phys_addr).ok()
    }
    
    /// Total usable frames
    pub fn total_frames(&self) -> usize {
        self.total_frames
    }
    
    /// Used frames
    pub fn used_frames(&self) -> usize {
        self.next_alloc
    }
    
    /// Free frames
    pub fn free_frames(&self) -> usize {
        self.total_frames.saturating_sub(self.next_alloc)
    }
}

impl Default for FrameAllocator {
    fn default() -> Self {
        unsafe { Self::new(&[]) }
    }
}

// Implement x86_64 FrameAllocator trait for page table mapping
use x86_64::structures::paging::Size4KiB;

unsafe impl x86_64::structures::paging::FrameAllocator<Size4KiB> for FrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.alloc_frame_kernel()
    }
}
