// memory/frame.rs - Frame Allocator (Physical Memory)
// Simple next-fit allocator tracking usable physical frames
//
// SECURITY: All index arithmetic uses checked operations to prevent
// integer overflow (CRIT-001). In release mode, unchecked += would
// silently wrap, potentially allowing allocation of frame 0 (BIOS/kernel).

use x86_64::structures::paging::PhysFrame;
use x86_64::PhysAddr;

/// Taille d'une frame (4KB standard x86_64)
pub const FRAME_SIZE: usize = 4096;

/// Maximum number of usable frames we track
/// Each entry stores the physical frame number
const MAX_USABLE_FRAMES: usize = 8192;

/// Frame Allocator - tracks usable physical frames
///
/// SECURITY INVARIANTS:
/// - next_alloc <= total_frames (enforced by checked_add)
/// - total_frames <= MAX_USABLE_FRAMES (enforced by add_region)
/// - Frame 0 is never returned (skip if present)
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
    ///
    /// # Safety
    /// - `regions` must contain valid physical memory ranges from bootloader
    /// - Each region (start, end) must be page-aligned or will be rounded
    /// - Must be called once during boot, before any frame allocations
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
    /// Skips frame 0 to prevent BIOS/kernel corruption (CRIT-001 hardening)
    fn add_region(&mut self, start: u64, end: u64) {
        // SECURITY: Validate region bounds
        if end <= start {
            return;
        }
        
        let start_frame = (start + FRAME_SIZE as u64 - 1) / FRAME_SIZE as u64;
        let end_frame = end / FRAME_SIZE as u64;
        
        let mut frame = start_frame;
        while frame < end_frame && self.total_frames < MAX_USABLE_FRAMES {
            // SECURITY: Skip frame 0 (BIOS data area, IVT)
            if frame == 0 {
                frame = 1;
                continue;
            }
            self.usable_frames[self.total_frames] = frame;
            // SECURITY: checked increment prevents overflow (CRIT-001)
            self.total_frames = match self.total_frames.checked_add(1) {
                Some(v) => v,
                None => break, // saturate, don't wrap
            };
            frame = match frame.checked_add(1) {
                Some(v) => v,
                None => break,
            };
        }
    }
    
    /// Allocate a physical frame for kernel use
    ///
    /// Returns None if no frames available.
    /// Uses checked arithmetic to prevent integer overflow (CRIT-001).
    pub fn alloc_frame_kernel(&mut self) -> Option<PhysFrame> {
        if self.next_alloc >= self.total_frames {
            return None;
        }
        
        let frame_num = self.usable_frames[self.next_alloc];
        
        // SECURITY: checked_add prevents wraparound to 0 (CRIT-001)
        self.next_alloc = self.next_alloc.checked_add(1)?;
        
        // SECURITY: validate frame_num is non-zero
        if frame_num == 0 {
            crate::serial_write("[FRAME] WARNING: Skipping frame 0 allocation\n");
            return self.alloc_frame_kernel(); // recurse to next frame
        }
        
        let phys_addr = PhysAddr::new(frame_num.checked_mul(FRAME_SIZE as u64)?);
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
        // SAFETY: Empty region list produces a valid but empty allocator
        unsafe { Self::new(&[]) }
    }
}

// Implement x86_64 FrameAllocator trait for page table mapping
use x86_64::structures::paging::Size4KiB;

// SAFETY: alloc_frame_kernel returns valid, unique, non-overlapping physical
// frames from the bootloader's usable memory regions. Each frame is only
// returned once (next_alloc monotonically increases). The frames are suitable
// for use as page table entries by the x86_64 crate's mapper.
unsafe impl x86_64::structures::paging::FrameAllocator<Size4KiB> for FrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.alloc_frame_kernel()
    }
}
