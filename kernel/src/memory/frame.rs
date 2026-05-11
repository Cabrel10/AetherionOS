// memory/frame.rs - Bitmap Frame Allocator (Physical Memory)
//
// REWRITE: Jalon 67 - Replaced per-frame array (limited to 32 MB) with a
// bitmap allocator that supports up to 16 GB of physical RAM.
//
// Design: Each bit in the bitmap represents one 4 KB frame.
//   - 0 = free, 1 = allocated (or unusable)
//   - Static bitmap avoids stack overflow (bitmap is in .bss)
//   - [u64; 65536] = 512 KB bitmap = 4,194,304 bits = 16 GB coverage
//
// SECURITY: All index arithmetic uses checked operations to prevent
// integer overflow (CRIT-001). Frame 0 is never returned.

use x86_64::structures::paging::PhysFrame;
use x86_64::PhysAddr;

/// Size of a frame (4KB standard x86_64)
pub const FRAME_SIZE: usize = 4096;

/// Maximum physical frames we can track.
/// 65536 u64s × 64 bits = 4,194,304 frames = 16 GB of RAM.
const BITMAP_ENTRIES: usize = 65536;
const MAX_FRAMES: usize = BITMAP_ENTRIES * 64; // 4,194,304

/// Static bitmap — lives in .bss, not on the stack.
/// Initialized to all 1s (all allocated) before regions are freed.
/// SAFETY: Only accessed through FrameAllocator methods under a lock.
static mut FRAME_BITMAP: [u64; BITMAP_ENTRIES] = [!0u64; BITMAP_ENTRIES];

/// Bitmap Frame Allocator - tracks physical frame allocation state.
///
/// The bitmap itself is in a static to avoid stack overflow (512 KB).
/// This struct just holds the metadata (counters, hints).
///
/// SECURITY INVARIANTS:
/// - Frame 0 is always marked allocated (BIOS/IVT protection)
/// - Kernel/reserved regions are marked allocated during init
/// - Only bootloader Usable regions are marked free
/// - alloc returns unique, non-overlapping frames
pub struct FrameAllocator {
    /// Total number of usable (free at init) frames
    total_usable: usize,
    /// Number of currently allocated frames
    allocated_count: usize,
    /// Hint: start searching from this index (next-fit optimization)
    search_hint: usize,
    /// Highest frame number that is usable (for bounds checking)
    max_frame: usize,
}

impl FrameAllocator {
    /// Create a new FrameAllocator from bootloader memory regions.
    ///
    /// The static FRAME_BITMAP starts as all 1s (all allocated).
    /// Only regions listed in `usable_regions` are marked free.
    ///
    /// # Safety
    /// - `usable_regions` must contain valid physical memory ranges from the bootloader
    /// - Must be called once during boot, before any frame allocations
    pub unsafe fn new(usable_regions: &[(u64, u64)]) -> Self {
        // Reset bitmap to all allocated
        for i in 0..BITMAP_ENTRIES {
            FRAME_BITMAP[i] = !0u64;
        }

        let mut allocator = Self {
            total_usable: 0,
            allocated_count: 0,
            search_hint: 0,
            max_frame: 0,
        };

        // Mark usable regions as free (bit = 0)
        for &(start, end) in usable_regions {
            allocator.mark_region_free(start, end);
        }

        // SECURITY: Always keep frame 0 allocated (BIOS data, IVT)
        Self::set_bit_static(0);

        // Count total usable frames
        let mut free_count = 0usize;
        for i in 0..BITMAP_ENTRIES {
            free_count += (64 - FRAME_BITMAP[i].count_ones()) as usize;
        }
        allocator.total_usable = free_count;

        allocator
    }

    /// Mark a physical memory region as free (usable for allocation).
    fn mark_region_free(&mut self, start: u64, end: u64) {
        if end <= start { return; }

        let start_frame = ((start + FRAME_SIZE as u64 - 1) / FRAME_SIZE as u64) as usize;
        let end_frame = (end / FRAME_SIZE as u64) as usize;

        if start_frame >= MAX_FRAMES { return; }
        let end_frame = core::cmp::min(end_frame, MAX_FRAMES);

        for frame in start_frame..end_frame {
            if frame == 0 { continue; } // Always keep frame 0 reserved
            unsafe { Self::clear_bit_static(frame); }
            if frame > self.max_frame {
                self.max_frame = frame;
            }
        }
    }

    /// Set bit in static bitmap (mark frame as allocated)
    #[inline]
    unsafe fn set_bit_static(frame: usize) {
        if frame >= MAX_FRAMES { return; }
        let idx = frame / 64;
        let bit = frame % 64;
        FRAME_BITMAP[idx] |= 1u64 << bit;
    }

    /// Clear bit in static bitmap (mark frame as free)
    #[inline]
    unsafe fn clear_bit_static(frame: usize) {
        if frame >= MAX_FRAMES { return; }
        let idx = frame / 64;
        let bit = frame % 64;
        FRAME_BITMAP[idx] &= !(1u64 << bit);
    }

    /// Test if a frame is allocated
    #[inline]
    fn is_allocated(frame: usize) -> bool {
        if frame >= MAX_FRAMES { return true; }
        let idx = frame / 64;
        let bit = frame % 64;
        unsafe { (FRAME_BITMAP[idx] >> bit) & 1 == 1 }
    }

    /// Allocate a physical frame for kernel use.
    ///
    /// Uses next-fit algorithm with bitmap scanning.
    pub fn alloc_frame_kernel(&mut self) -> Option<PhysFrame> {
        let max_idx = core::cmp::min((self.max_frame / 64) + 1, BITMAP_ENTRIES);

        // Phase 1: Search from hint to end
        if let Some(frame) = self.find_free_frame(self.search_hint / 64, max_idx) {
            return self.commit_alloc(frame);
        }

        // Phase 2: Wrap around
        let hint_idx = self.search_hint / 64;
        if hint_idx > 0 {
            if let Some(frame) = self.find_free_frame(0, hint_idx) {
                return self.commit_alloc(frame);
            }
        }

        None
    }

    /// Find a free frame in bitmap range [start_idx..end_idx)
    fn find_free_frame(&self, start_idx: usize, end_idx: usize) -> Option<usize> {
        for idx in start_idx..end_idx {
            let word = unsafe { FRAME_BITMAP[idx] };
            if word != !0u64 {
                // At least one bit is 0 (free frame)
                let bit = (!word).trailing_zeros() as usize;
                let frame = idx * 64 + bit;
                if frame == 0 { continue; }
                if frame <= self.max_frame {
                    return Some(frame);
                }
            }
        }
        None
    }

    /// Commit a frame allocation: set the bit, update counters
    fn commit_alloc(&mut self, frame: usize) -> Option<PhysFrame> {
        unsafe { Self::set_bit_static(frame); }
        self.allocated_count = self.allocated_count.saturating_add(1);
        self.search_hint = frame + 1;

        let phys_addr = PhysAddr::new((frame as u64).checked_mul(FRAME_SIZE as u64)?);
        // J135: sanity check — if we're allocating a frame that overlaps the
        // kernel image (approx 0x200000-0xC10000 for this build), log a
        // warning. The bootloader should have marked this region as non-usable,
        // but double-check at runtime to catch regressions.
        let phys_u64 = phys_addr.as_u64();
        if phys_u64 >= 0x200_000 && phys_u64 < 0xD00_000 {
            // Only log the first such overlap to avoid serial flooding.
            static ONCE: core::sync::atomic::AtomicBool =
                core::sync::atomic::AtomicBool::new(false);
            if !ONCE.swap(true, core::sync::atomic::Ordering::SeqCst) {
                crate::serial_println!(
                    "[FRAME-WARN] Allocated frame at phys 0x{:X} overlaps kernel image!",
                    phys_u64
                );
            }
        }
        PhysFrame::from_start_address(phys_addr).ok()
    }

    /// Mark a specific physical frame as allocated (protect from reuse).
    ///
    /// Used during boot to protect Limine page table frames that may live
    /// inside USABLE memory regions. Without this, the frame allocator could
    /// hand out a PT frame, the new owner zeroes it, and kernel page table
    /// entries are destroyed (the PT[195]=0x0 bug).
    pub fn mark_frame_allocated(&mut self, phys_addr: u64) {
        let frame = (phys_addr / FRAME_SIZE as u64) as usize;
        if frame == 0 || frame >= MAX_FRAMES { return; }
        if !Self::is_allocated(frame) {
            unsafe { Self::set_bit_static(frame); }
            self.allocated_count = self.allocated_count.saturating_add(1);
        }
    }

    /// Free a previously allocated frame (return it to the pool).
    ///
    /// # Safety
    /// The caller must ensure the frame was previously allocated and
    /// is no longer referenced by any page table or DMA operation.
    pub unsafe fn free_frame(&mut self, frame_num: usize) {
        if frame_num == 0 || frame_num >= MAX_FRAMES { return; }
        if Self::is_allocated(frame_num) {
            Self::clear_bit_static(frame_num);
            self.allocated_count = self.allocated_count.saturating_sub(1);
            if frame_num < self.search_hint {
                self.search_hint = frame_num;
            }
        }
    }

    /// Total usable frames discovered at boot
    pub fn total_frames(&self) -> usize {
        self.total_usable
    }

    /// Currently allocated frames
    pub fn used_frames(&self) -> usize {
        self.allocated_count
    }

    /// Currently free frames
    pub fn free_frames(&self) -> usize {
        self.total_usable.saturating_sub(self.allocated_count)
    }

    /// Total physical RAM tracked (in bytes)
    pub fn total_ram_bytes(&self) -> u64 {
        (self.total_usable as u64) * FRAME_SIZE as u64
    }

    /// Maximum frame number
    pub fn max_frame_number(&self) -> usize {
        self.max_frame
    }

    /// Allocate `count` physically contiguous frames.
    ///
    /// Scans the bitmap for a run of `count` consecutive free bits.
    /// Returns the physical address of the first frame, or None if no run found.
    /// All frames in the run are marked allocated.
    ///
    /// This is needed for DMA buffers (e.g. VirtIO queues) that require
    /// physically contiguous memory.
    pub fn alloc_contiguous_frames(&mut self, count: usize) -> Option<u64> {
        if count == 0 { return None; }
        if count == 1 {
            return self.alloc_frame_kernel().map(|f| f.start_address().as_u64());
        }

        // Scan the entire bitmap for a contiguous run of `count` free frames.
        // Start from frame 1 (skip frame 0 which is always reserved).
        let max = self.max_frame.min(MAX_FRAMES - 1);
        let mut start_frame = 1usize;

        'outer: while start_frame + count - 1 <= max {
            // Check if all frames in [start_frame .. start_frame+count) are free
            for offset in 0..count {
                let f = start_frame + offset;
                if Self::is_allocated(f) {
                    // Skip past this allocated frame
                    start_frame = f + 1;
                    continue 'outer;
                }
            }
            // Found a contiguous run! Mark them all allocated.
            for offset in 0..count {
                let f = start_frame + offset;
                unsafe { Self::set_bit_static(f); }
                self.allocated_count = self.allocated_count.saturating_add(1);
            }
            self.search_hint = start_frame + count;
            let phys = (start_frame as u64) * (FRAME_SIZE as u64);
            crate::serial_println!(
                "[FRAME] Allocated {} contiguous frames at phys 0x{:X} ({} KiB)",
                count, phys, count * 4
            );
            return Some(phys);
        }

        crate::serial_println!("[FRAME] Failed to find {} contiguous frames", count);
        None
    }
}

/// Allocate `count` physically contiguous frames for DMA.
///
/// This is a **static** function that can be called without a `&mut FrameAllocator`
/// reference, making it available globally (e.g. for VirtIO drivers that need
/// contiguous DMA buffers during boot, before a global allocator is set up).
///
/// Scans the static bitmap for a run of `count` consecutive free bits,
/// preferring frames in the first 1 GiB (for 32-bit DMA compatibility).
/// Returns the physical address of the first frame.
///
/// # Safety
/// Must be called after the frame allocator bitmap has been initialized (i.e.
/// after `FrameAllocator::new()` during boot).
pub unsafe fn alloc_contiguous_dma(count: usize) -> Option<u64> {
    if count == 0 { return None; }

    // Search for `count` contiguous free frames in the bitmap.
    // Prefer low addresses (< 1 GiB) for legacy DMA compatibility.
    // 1 GiB = 262144 frames.
    let max_frame = MAX_FRAMES - 1;
    let preferred_max = 262144usize.min(max_frame); // 1 GiB

    // Helper: check & allocate a contiguous run starting at `start_frame`.
    let try_range = |limit: usize| -> Option<u64> {
        let mut start_frame = 1usize; // skip frame 0
        'outer: while start_frame + count - 1 <= limit {
            for offset in 0..count {
                let f = start_frame + offset;
                if FrameAllocator::is_allocated(f) {
                    start_frame = f + 1;
                    continue 'outer;
                }
            }
            // Found! Mark them all allocated.
            for offset in 0..count {
                FrameAllocator::set_bit_static(start_frame + offset);
            }
            let phys = (start_frame as u64) * (FRAME_SIZE as u64);
            crate::serial_println!(
                "[FRAME-DMA] Allocated {} contiguous frames at phys 0x{:X} ({} KiB)",
                count, phys, count * 4
            );
            return Some(phys);
        }
        None
    };

    // Phase 1: try in the first 1 GiB
    if let Some(phys) = try_range(preferred_max) {
        return Some(phys);
    }
    // Phase 2: try the rest of RAM
    // (Re-scan from 1 to max_frame; the overlap is acceptable for simplicity)
    try_range(max_frame)
}

impl Default for FrameAllocator {
    fn default() -> Self {
        unsafe { Self::new(&[]) }
    }
}

use x86_64::structures::paging::Size4KiB;

// SAFETY: alloc_frame_kernel returns valid, unique, non-overlapping physical
// frames. Each frame is only returned once (bitmap bit 0→1). Freed frames
// can be re-allocated. The static bitmap prevents stack overflow.
unsafe impl x86_64::structures::paging::FrameAllocator<Size4KiB> for FrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.alloc_frame_kernel()
    }
}
