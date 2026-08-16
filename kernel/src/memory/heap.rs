// memory/heap.rs - Heap Allocator pour alloc (Box, Vec, etc.)
// Utilise linked_list_allocator pour no_std
//
// SECURITY: alloc_error_handler uses direct serial write (no allocation)
// to prevent recursive panic when heap is exhausted (CRIT-003).

use super::{MemoryError, MemoryResult};
use super::frame::FrameAllocator;
use super::paging::{OffsetPageTableManager, flags};
use x86_64::structures::paging::Page;
use x86_64::VirtAddr;
use linked_list_allocator::LockedHeap;

/// Adresse de debut du heap
pub const HEAP_START: usize = 0x_4444_4444_0000;

/// Taille du heap: 256 MB — needed for zero-copy Q8_0 LLM inference (30-layer SmolLM2 = ~142 MB).
/// Mapping optimization: pages are mapped in batches with deferred TLB flush
/// to avoid the slow per-page flush that previously took >3 min.
pub const HEAP_SIZE: usize = 256 * 1024 * 1024;

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

    {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "[HEAP] Mapping {} pages ({} MB)...", HEAP_PAGES, HEAP_SIZE / 1024 / 1024);
        crate::serial_write(&s);
    }
    
    // Map each heap page with DEFERRED TLB flush for speed.
    // Instead of flushing TLB per-page (slow: 65536 invlpg instructions),
    // we skip individual flushes and do one global TLB flush at the end.
    {
        use x86_64::structures::paging::mapper::Mapper;
        for i in 0..HEAP_PAGES {
            let page = Page::containing_address(heap_start + (i * 4096) as u64);
            
            let frame = frame_allocator
                .alloc_frame_kernel()
                .ok_or(MemoryError::OutOfMemory)?;
            
            // SAFETY: page and frame are valid, non-overlapping. We deliberately
            // skip the per-page TLB flush (forget the flusher) and do a bulk flush below.
            let result = unsafe {
                page_table.mapper.map_to(page, frame, flags::KERNEL_DATA, frame_allocator)
            };
            match result {
                Ok(flusher) => {
                    // DELIBERATELY skip flusher.flush() — we'll do a bulk flush below
                    flusher.ignore();
                }
                Err(_) => return Err(MemoryError::HeapInitFailed),
            }
            
            // Progress report every 16384 pages (~64 MB)
            if (i + 1) % 16384 == 0 {
                use core::fmt::Write;
                let mut s = arrayvec::ArrayString::<64>::new();
                let _ = writeln!(s, "[HEAP] {}MB mapped...", (i + 1) * 4 / 1024);
                crate::serial_write(&s);
            }
        }
        
        // SAFETY: Global TLB flush — reload CR3 to flush all TLB entries.
        // This is safe because all page table modifications are complete.
        unsafe {
            core::arch::asm!(
                "mov rax, cr3",
                "mov cr3, rax",
                out("rax") _,
                options(nostack, nomem)
            );
        }
        crate::serial_write("[HEAP] TLB flushed (bulk)\n");
    }
    
    crate::serial_write("[HEAP] Pages mapped, initializing allocator...\n");
    
    // SAFETY: HEAP_START points to freshly mapped, zeroed memory.
    // HEAP_SIZE bytes are available. LockedHeap is safe for concurrent use.
    // This is called exactly once (guarded by HEAP_INITIALIZED atomic).
    unsafe {
        HEAP_ALLOCATOR.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }
    
    {
        use core::fmt::Write;
        let mut s = arrayvec::ArrayString::<128>::new();
        let _ = writeln!(s, "[HEAP] Ready: {} KB at {:#x}", HEAP_SIZE / 1024, HEAP_START);
        crate::serial_write(&s);
    }
    
    Ok(())
}

/// Verifie si le heap est initialise
pub fn is_initialized() -> bool {
    HEAP_INITIALIZED.load(core::sync::atomic::Ordering::SeqCst)
}

/// Write a u64 to serial without any allocation (for OOM handler)
fn serial_write_u64(mut val: u64) {
    if val == 0 {
        crate::serial_write("0");
        return;
    }
    let mut buf = [0u8; 20]; // max u64 = 20 digits
    let mut pos = 20;
    while val > 0 && pos > 0 {
        pos -= 1;
        buf[pos] = b'0' + (val % 10) as u8;
        val /= 10;
    }
    // SAFETY: buf[pos..20] contains only ASCII digits (0x30..0x39)
    let s = unsafe { core::str::from_utf8_unchecked(&buf[pos..20]) };
    crate::serial_write(s);
}

/// Handler d'allocation echouee
///
/// SECURITY (CRIT-003): This handler MUST NOT allocate memory.
/// Using panic!() with format args would attempt allocation for the
/// formatted string, causing recursive OOM -> stack overflow -> triple fault.
/// Instead we write directly to serial port and halt the CPU.
#[alloc_error_handler]
fn alloc_error_handler(layout: core::alloc::Layout) -> ! {
    // CRITICAL: No allocation allowed here! Direct serial writes only.
    crate::serial_write("\n[FATAL] Heap allocation failed! size=");
    serial_write_u64(layout.size() as u64);
    crate::serial_write(", align=");
    serial_write_u64(layout.align() as u64);
    crate::serial_write("\n[FATAL] System halted - out of memory.\n");
    
    // Halt CPU without panic (no allocation risk)
    loop {
        // SAFETY: HLT instruction is safe in ring 0, it simply waits
        // for the next interrupt. Since we're in an unrecoverable state,
        // this prevents CPU spin while we wait for external reset.
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
    }
}
