// elf/mod.rs - Couche 11: Full ELF64 Loader with Per-Process Paging
//
// Features:
//   - ELF64 header and program header parsing
//   - ELF magic verification
//   - PT_LOAD segment mapping into per-process page tables
//   - BSS zero-fill (p_memsz > p_filesz)
//   - Per-process PML4 creation (cloned from kernel PML4)
//   - 8 MiB user stack at virtual address 0x7FFF_FFFF_F000
//   - Ring 3 process creation via IRETQ
//   - load_elf(path) -> Result<Pid, ElfError>
//
// Security:
//   - Address validation: all user mappings below 0x0000_8000_0000_0000
//   - File bounds checking on all segment offsets
//   - Segment overlap detection
//   - Stack guard page (unmapped page below stack)

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

// ===== Constants =====

/// ELF magic bytes
const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

// ===== Linux ABI Auxiliary Vector (AuxV) Constants =====
// Required for musl/glibc binaries to initialize correctly.
// Reference: https://man7.org/linux/man-pages/man3/getauxval.3.html
const AT_NULL: u64     = 0;   // End of AuxV
const AT_PHDR: u64     = 3;   // Program headers address in memory
const AT_PHENT: u64    = 4;   // Size of program header entry
const AT_PHNUM: u64    = 5;   // Number of program headers
const AT_PAGESZ: u64   = 6;   // System page size
const AT_BASE: u64     = 7;   // Interpreter base address (0 for static)
const AT_FLAGS: u64    = 8;   // Flags
const AT_ENTRY: u64    = 9;   // Program entry point
const AT_UID: u64      = 11;  // Real user ID
const AT_EUID: u64     = 12;  // Effective user ID
const AT_GID: u64      = 13;  // Real group ID
const AT_EGID: u64     = 14;  // Effective group ID
const AT_SECURE: u64   = 23;  // Secure mode boolean
const AT_RANDOM: u64   = 25;  // Address of 16 random bytes
const AT_HWCAP: u64    = 16;  // Hardware capabilities (SSE, AVX, etc.)
const AT_HWCAP2: u64   = 26;  // Extended hardware capabilities

/// ELF class: 64-bit
const ELFCLASS64: u8 = 2;
/// ELF data: little-endian
const ELFDATA2LSB: u8 = 1;
/// ELF type: executable
const ET_EXEC: u16 = 2;
/// ELF type: shared object / PIE executable
const ET_DYN: u16 = 3;
/// ELF machine: x86-64
const EM_X86_64: u16 = 62;

/// Program header types
const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const PT_NOTE: u32 = 4;
const PT_PHDR: u32 = 6;
const PT_DYNAMIC: u32 = 2;
const PT_GNU_EH_FRAME: u32 = 0x6474e550;
const PT_GNU_STACK: u32 = 0x6474e551;
const PT_GNU_RELRO: u32 = 0x6474e552;

/// Segment permission flags
const PF_X: u32 = 1; // Execute
const PF_W: u32 = 2; // Write
const PF_R: u32 = 4; // Read

/// Page size
const PAGE_SIZE: u64 = 4096;

/// User stack top virtual address (grows down from here)
/// Stack occupies: 0x7FFF_FFFF_F000 - stack_size to 0x7FFF_FFFF_F000
/// IMPORTANT: The last mapped byte is at 0x7FFF_FFFF_EFFF.
/// Initial RSP must be INSIDE the mapped range and ABI-aligned.
const USER_STACK_TOP: u64 = 0x7FFF_FFFF_F000;

/// ABI-correct initial stack pointer for IRETQ entry:
/// - Must be inside the last mapped page (0x7FFF_FFFF_E000 .. 0x7FFF_FFFF_EFFF)
/// - _start is a #[naked] function entered via IRETQ (not via CALL)
/// - _start convention: RSP % 16 == 0 on IRETQ entry
/// - _start does `call main` which pushes 8 bytes -> main sees RSP % 16 == 8 (correct ABI)
/// - 0x7FFF_FFFF_EFF0 % 16 == 0 => perfect alignment for IRETQ entry
/// - Safe: 16 bytes below unmapped boundary at 0x7FFF_FFFF_F000
const USER_STACK_INITIAL_RSP: u64 = USER_STACK_TOP - 16;
/// User stack size: 8 MiB virtual range reserved.
/// 512 pages (2 MiB) initially mapped — sufficient for SmolLM2 GGUF parsing (272 tensors).
const USER_STACK_PAGES: u64 = 512; // 2 MiB initial mapping

/// Maximum valid user-space address
const USER_ADDR_LIMIT: u64 = 0x0000_8000_0000_0000;

/// ELF frame pool: dedicated frames for ELF loading
/// Increased for sys_fork deep page-table copy (Jalon 25)
/// Jalon 72: Expanded to 1.5M frames (6 GB) to support Mistral 7B model loading
const ELF_FRAME_POOL_SIZE: usize = 1572864; // Up to 6 GiB for LLM models

// ===== ELF64 Header (C-compatible, packed) =====

#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
pub struct Elf64Ehdr {
    pub e_ident: [u8; 16],
    pub e_type: u16,
    pub e_machine: u16,
    pub e_version: u32,
    pub e_entry: u64,
    pub e_phoff: u64,
    pub e_shoff: u64,
    pub e_flags: u32,
    pub e_ehsize: u16,
    pub e_phentsize: u16,
    pub e_phnum: u16,
    pub e_shentsize: u16,
    pub e_shnum: u16,
    pub e_shstrndx: u16,
}

// ===== ELF64 Program Header =====

#[repr(C, packed)]
#[derive(Debug, Copy, Clone)]
pub struct Elf64Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

// ===== Error Type =====

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    /// File too small to contain ELF header
    TooSmall,
    /// Invalid ELF magic number
    BadMagic,
    /// Not a 64-bit ELF
    Not64Bit,
    /// Not little-endian
    NotLittleEndian,
    /// Not an executable ELF
    NotExecutable,
    /// Not x86-64 architecture
    WrongArch,
    /// Invalid program header offset/size
    InvalidPhdr,
    /// Segment offset exceeds file bounds
    InvalidSegment,
    /// Virtual address out of user range
    AddressOutOfRange,
    /// No loadable segments found
    NoLoadSegments,
    /// Out of memory (frames)
    OutOfMemory,
    /// VFS error reading file
    VfsError,
    /// Process creation error
    ProcessError,
}

impl core::fmt::Display for ElfError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Self::TooSmall => write!(f, "File too small for ELF header"),
            Self::BadMagic => write!(f, "Invalid ELF magic"),
            Self::Not64Bit => write!(f, "Not a 64-bit ELF"),
            Self::NotLittleEndian => write!(f, "Not little-endian"),
            Self::NotExecutable => write!(f, "Not an executable"),
            Self::WrongArch => write!(f, "Not x86-64"),
            Self::InvalidPhdr => write!(f, "Invalid program header"),
            Self::InvalidSegment => write!(f, "Invalid segment data"),
            Self::AddressOutOfRange => write!(f, "Address out of user range"),
            Self::NoLoadSegments => write!(f, "No loadable segments"),
            Self::OutOfMemory => write!(f, "Out of memory"),
            Self::VfsError => write!(f, "VFS file read error"),
            Self::ProcessError => write!(f, "Process creation error"),
        }
    }
}

// ===== ELF Frame Pool =====
// A bump allocator with freelist for physical frames used by ELF loading.
// Jalon 94: Added freelist-based recycling to prevent OOM on process exit.

/// Maximum number of freed frames we can track for recycling.
/// When a process exits, its frames are pushed here and reused by the next allocation.
const FREELIST_MAX: usize = 8192; // 32 MiB of recyclable frames

struct ElfFramePool {
    base_frame: u64,    // Physical base address (frame-aligned)
    frames_used: usize,
    max_frames: usize,
    // Freelist: stack of recycled physical frame addresses
    freelist: [u64; FREELIST_MAX],
    freelist_count: usize,
    // Stats
    total_freed: usize,
    total_recycled: usize,
}

static mut ELF_POOL: ElfFramePool = ElfFramePool {
    base_frame: 0,
    frames_used: 0,
    max_frames: 0,
    freelist: [0; FREELIST_MAX],
    freelist_count: 0,
    total_freed: 0,
    total_recycled: 0,
};

static ELF_POOL_INITIALIZED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Initialize the ELF frame pool with a base physical address
/// SAFETY: Must be called once, after physical memory is known
pub unsafe fn init_frame_pool(base_phys: u64, num_frames: usize) {
    ELF_POOL.base_frame = base_phys;
    ELF_POOL.frames_used = 0;
    ELF_POOL.max_frames = num_frames;
    ELF_POOL_INITIALIZED.store(true, Ordering::SeqCst);
    crate::serial_println!(
        "[ELF] Frame pool initialized: base=0x{:X}, frames={}, size={} KB",
        base_phys, num_frames, num_frames * 4
    );
}

/// Allocate a physical frame from the ELF pool.
/// Jalon 94: Checks freelist first (recycled frames), then bumps.
pub unsafe fn alloc_elf_frame() -> Option<u64> {
    if !ELF_POOL_INITIALIZED.load(Ordering::SeqCst) {
        return None;
    }
    // Priority 1: Reuse a freed frame from the freelist
    if ELF_POOL.freelist_count > 0 {
        ELF_POOL.freelist_count -= 1;
        let phys = ELF_POOL.freelist[ELF_POOL.freelist_count];
        ELF_POOL.total_recycled += 1;
        return Some(phys);
    }
    // Priority 2: Bump allocate a new frame
    if ELF_POOL.frames_used >= ELF_POOL.max_frames {
        return None;
    }
    let phys = ELF_POOL.base_frame + (ELF_POOL.frames_used as u64) * PAGE_SIZE;
    ELF_POOL.frames_used += 1;
    Some(phys)
}

/// Return a physical frame to the freelist for recycling.
/// SAFETY: The frame must have been allocated from this pool and must not be
/// referenced by any active page table.
pub unsafe fn free_elf_frame(phys: u64) {
    if !ELF_POOL_INITIALIZED.load(Ordering::SeqCst) {
        return;
    }
    // Validate frame belongs to our pool
    let pool_end = ELF_POOL.base_frame + (ELF_POOL.max_frames as u64) * PAGE_SIZE;
    if phys < ELF_POOL.base_frame || phys >= pool_end {
        return; // Not from our pool — don't free
    }
    if phys & 0xFFF != 0 {
        return; // Not page-aligned
    }
    // Push onto freelist if space available
    if ELF_POOL.freelist_count < FREELIST_MAX {
        ELF_POOL.freelist[ELF_POOL.freelist_count] = phys;
        ELF_POOL.freelist_count += 1;
        ELF_POOL.total_freed += 1;
    }
    // If freelist is full, frame is leaked — acceptable for stability
}

/// Get pool usage stats: (used, max, freelist_count, total_freed, total_recycled)
pub fn pool_stats() -> (usize, usize) {
    unsafe { (ELF_POOL.frames_used, ELF_POOL.max_frames) }
}

/// Get detailed pool stats for diagnostics
pub fn pool_stats_detailed() -> (usize, usize, usize, usize, usize) {
    unsafe { (ELF_POOL.frames_used, ELF_POOL.max_frames,
              ELF_POOL.freelist_count, ELF_POOL.total_freed, ELF_POOL.total_recycled) }
}

/// Free all user-space pages and intermediate page tables for a terminated process.
/// Walks the 4-level page table (PML4 → PDPT → PD → PT), freeing:
///   - All leaf page frames (PT entries with PRESENT bit)
///   - All intermediate page table frames allocated from ELF_POOL
///   - The PML4 frame itself
///
/// SAFETY: The PML4 must not be the active CR3. Call only after the process
/// has been fully terminated and no CPU is using this address space.
///
/// We skip PML4 entries that point outside the ELF pool (kernel entries copied
/// verbatim from the kernel PML4) to avoid corrupting kernel page tables.
pub unsafe fn free_user_page_table(pml4_phys: u64) {
    if pml4_phys == 0 || pml4_phys & 0xFFF != 0 {
        return;
    }

    let pool_base = ELF_POOL.base_frame;
    let pool_end = pool_base + (ELF_POOL.max_frames as u64) * PAGE_SIZE;

    // Helper: check if a physical address belongs to ELF_POOL
    let in_pool = |phys: u64| -> bool {
        phys >= pool_base && phys < pool_end && phys & 0xFFF == 0
    };

    let mut freed_count: usize = 0;
    let pml4_virt = phys_to_virt(pml4_phys) as *const u64;

    // Walk all 512 PML4 entries
    for pml4_idx in 0..512usize {
        let pml4_entry = core::ptr::read_volatile(pml4_virt.add(pml4_idx));
        if pml4_entry & 0x01 == 0 { continue; } // Not present

        let pdpt_phys = pml4_entry & !0xFFF;
        if !in_pool(pdpt_phys) { continue; } // Kernel entry — don't touch

        let pdpt_virt = phys_to_virt(pdpt_phys) as *const u64;

        // Walk all 512 PDPT entries
        for pdpt_idx in 0..512usize {
            let pdpt_entry = core::ptr::read_volatile(pdpt_virt.add(pdpt_idx));
            if pdpt_entry & 0x01 == 0 { continue; }
            // Check for 1 GiB huge page (bit 7)
            if pdpt_entry & 0x80 != 0 { continue; } // Don't free huge pages

            let pd_phys = pdpt_entry & !0xFFF;
            if !in_pool(pd_phys) { continue; }

            let pd_virt = phys_to_virt(pd_phys) as *const u64;

            // Walk all 512 PD entries
            for pd_idx in 0..512usize {
                let pd_entry = core::ptr::read_volatile(pd_virt.add(pd_idx));
                if pd_entry & 0x01 == 0 { continue; }
                // Check for 2 MiB huge page (bit 7)
                if pd_entry & 0x80 != 0 { continue; }

                let pt_phys = pd_entry & !0xFFF;
                if !in_pool(pt_phys) { continue; }

                let pt_virt = phys_to_virt(pt_phys) as *const u64;

                // Walk all 512 PT entries — free leaf page frames
                for pt_idx in 0..512usize {
                    let pt_entry = core::ptr::read_volatile(pt_virt.add(pt_idx));
                    if pt_entry & 0x01 == 0 { continue; }

                    let frame_phys = pt_entry & !0xFFF;
                    if in_pool(frame_phys) {
                        free_elf_frame(frame_phys);
                        freed_count += 1;
                    }
                }

                // Free the PT itself
                free_elf_frame(pt_phys);
                freed_count += 1;
            }

            // Free the PD itself
            free_elf_frame(pd_phys);
            freed_count += 1;
        }

        // Free the PDPT itself
        free_elf_frame(pdpt_phys);
        freed_count += 1;
    }

    // Free the PML4 itself
    free_elf_frame(pml4_phys);
    freed_count += 1;

    crate::serial_println!(
        "[GC] Freed {} frames from PML4 0x{:X} (freelist: {}/{})",
        freed_count, pml4_phys, ELF_POOL.freelist_count, FREELIST_MAX
    );
}

/// Allocate a frame for demand paging (called from page fault handler)
/// SAFETY: Must be called with interrupts disabled (inside exception handler)
pub unsafe fn alloc_demand_frame() -> Option<u64> {
    alloc_elf_frame()
}

/// Get the physical memory offset (public for demand paging handler)
pub fn phys_offset() -> u64 {
    PHYS_MEM_OFFSET.load(Ordering::SeqCst)
}

/// Map a user page for demand paging (public wrapper for page fault handler)
/// SAFETY: pml4_phys must be a valid PML4 physical address
pub unsafe fn demand_map_user_page(
    pml4_phys: u64,
    vaddr: u64,
    paddr: u64,
    flags: u64,
) -> Result<(), ElfError> {
    map_user_page(pml4_phys, vaddr, paddr, flags)
}

// ===== ELF Parsing =====

/// Parse and validate an ELF64 header from raw bytes
pub fn parse_header(data: &[u8]) -> Result<Elf64Ehdr, ElfError> {
    if data.len() < core::mem::size_of::<Elf64Ehdr>() {
        return Err(ElfError::TooSmall);
    }

    // Read header by copying bytes (avoids alignment issues with packed struct)
    let hdr: Elf64Ehdr = unsafe {
        core::ptr::read_unaligned(data.as_ptr() as *const Elf64Ehdr)
    };

    // Verify magic
    if hdr.e_ident[0..4] != ELF_MAGIC {
        return Err(ElfError::BadMagic);
    }

    // Verify 64-bit
    if hdr.e_ident[4] != ELFCLASS64 {
        return Err(ElfError::Not64Bit);
    }

    // Verify little-endian
    if hdr.e_ident[5] != ELFDATA2LSB {
        return Err(ElfError::NotLittleEndian);
    }

    // Verify executable type (ET_EXEC or ET_DYN for PIE executables)
    let e_type = hdr.e_type;
    if e_type != ET_EXEC && e_type != ET_DYN {
        return Err(ElfError::NotExecutable);
    }

    // Verify x86-64
    let e_machine = hdr.e_machine;
    if e_machine != EM_X86_64 {
        return Err(ElfError::WrongArch);
    }

    Ok(hdr)
}

/// Parse program headers from ELF data
pub fn parse_program_headers(data: &[u8], hdr: &Elf64Ehdr) -> Result<Vec<Elf64Phdr>, ElfError> {
    let phoff = hdr.e_phoff as usize;
    let phentsize = hdr.e_phentsize as usize;
    let phnum = hdr.e_phnum as usize;

    if phentsize < core::mem::size_of::<Elf64Phdr>() {
        return Err(ElfError::InvalidPhdr);
    }

    let end = phoff + phnum * phentsize;
    if end > data.len() {
        return Err(ElfError::InvalidPhdr);
    }

    let mut phdrs = Vec::with_capacity(phnum);
    for i in 0..phnum {
        let offset = phoff + i * phentsize;
        let phdr: Elf64Phdr = unsafe {
            core::ptr::read_unaligned(data[offset..].as_ptr() as *const Elf64Phdr)
        };
        phdrs.push(phdr);
    }

    Ok(phdrs)
}

/// Convert ELF p_flags to x86-64 page table flags
fn elf_flags_to_page_flags(p_flags: u32) -> u64 {
    // Base: PRESENT + USER_ACCESSIBLE
    let mut flags: u64 = 0x01 | 0x04; // PRESENT | USER_ACCESSIBLE

    if p_flags & PF_W != 0 {
        flags |= 0x02; // WRITABLE
    }

    // NX bit enforcement: if not executable, set NO_EXECUTE
    if p_flags & PF_X == 0 {
        flags |= 1u64 << 63; // NO_EXECUTE
    }

    flags
}

// ===== Per-Process Page Table Creation =====

/// Physical memory offset (set during kernel boot)
static PHYS_MEM_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Set the physical memory offset (call during boot)
pub fn set_phys_mem_offset(offset: u64) {
    PHYS_MEM_OFFSET.store(offset, Ordering::SeqCst);
}

/// Convert physical address to virtual using the offset mapping
#[inline]
pub fn phys_to_virt(phys: u64) -> u64 {
    phys + phys_offset()
}

/// Public wrapper for lookup_page_frame (diagnostic use)
pub unsafe fn lookup_page_frame_pub(pml4_phys: u64, vaddr: u64) -> Option<u64> {
    lookup_page_frame(pml4_phys, vaddr)
}

/// Create a new PML4 page table for a user process.
///
/// SECURITY DESIGN (Couche 12 - KPTI-lite):
///   - Copies kernel entries 256-511 (upper half) VERBATIM from current PML4.
///     These contain the physical memory offset mapping and kernel heap.
///     They do NOT have USER_ACCESSIBLE bit set → kernel memory is invisible
///     to Ring 3 code. This prevents Meltdown-class attacks.
///   - Copies kernel entry 0 (lower half) for kernel code/data access by the
///     SYSCALL handler. Ring 3 cannot access these pages because the
///     USER_ACCESSIBLE bit is NOT set on the kernel's page table entries.
///   - User segments (PML4[1]+) are created fresh by map_user_page() with
///     proper USER_ACCESSIBLE flags, completely isolated from kernel PML4[0].
///
/// Returns the physical address of the new PML4.
unsafe fn create_user_pml4() -> Result<u64, ElfError> {
    // Allocate a frame for the new PML4
    let new_pml4_phys = alloc_elf_frame().ok_or(ElfError::OutOfMemory)?;

    // Get current PML4 from CR3
    let cr3: u64;
    core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
    let current_pml4_phys = cr3 & !0xFFF;

    let current_pml4_virt = phys_to_virt(current_pml4_phys) as *const u64;
    let new_pml4_virt = phys_to_virt(new_pml4_phys) as *mut u64;

    // Zero the entire new PML4
    core::ptr::write_bytes(new_pml4_virt, 0, 512);

    // Copy ALL kernel PML4 entries INCLUDING PML4[1].
    //
    // PML4[0]: kernel identity mapping (code, GDT, IDT, kernel stacks)
    // PML4[1]: kernel BSS/data + user ELF region (0x8000000000)
    //          Kernel statics (AP GDT, TSS, per-core stacks) live here.
    //          map_user_page() deep-copies PML4[1]'s sub-tables to add
    //          per-process user ELF pages without disturbing kernel entries.
    // PML4[2-135]: various bootloader/kernel mappings
    // PML4[136+]: physical memory offset mapping (bootloader 0.9.x)
    // PML4[256-511]: kernel upper half (physical memory, kernel heap)
    //
    // Ring 3 code cannot access PML4[0], PML4[1] kernel pages, or PML4[256+]
    // because those entries don't have the USER_ACCESSIBLE bit set — KPTI-lite.
    let mut copied = 0usize;
    for i in 0..512usize {
        let entry = core::ptr::read_volatile(current_pml4_virt.add(i));
        if entry & 0x01 != 0 {
            core::ptr::write_volatile(new_pml4_virt.add(i), entry);
            copied += 1;
        }
    }

    // DIAGNOSTIC: Verify kernel mapping is intact in new PML4
    // Kernel .text is at ~0x408560 = PML4[0], PDPT[0], PD[2]
    let new_pml4_e0 = core::ptr::read_volatile(new_pml4_virt.add(0));
    let has_kernel = new_pml4_e0 & 0x01 != 0;
    crate::serial_println!(
        "[ELF] User PML4 created: phys=0x{:X} ({} entries cloned) PML4[0]=0x{:X} kernel={}",
        new_pml4_phys, copied, new_pml4_e0, has_kernel
    );

    Ok(new_pml4_phys)
}

/// Look up whether a virtual address is already mapped in this PML4.
/// Returns the physical frame address if the page is present, None otherwise.
/// Used to detect overlapping ELF segments that share the same page.
unsafe fn lookup_page_frame(pml4_phys: u64, vaddr: u64) -> Option<u64> {
    // Mask for extracting the physical address from a page table entry.
    // Bits 12-51 contain the physical address; bits 0-11 are flags;
    // bits 52-62 are software-available; bit 63 is NX (No Execute).
    const PTE_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

    let indices = [
        ((vaddr >> 39) & 0x1FF) as usize,
        ((vaddr >> 30) & 0x1FF) as usize,
        ((vaddr >> 21) & 0x1FF) as usize,
        ((vaddr >> 12) & 0x1FF) as usize,
    ];

    let mut table_phys = pml4_phys;
    for level in 0..3 {
        let table_virt = phys_to_virt(table_phys) as *const u64;
        let entry = core::ptr::read_volatile(table_virt.add(indices[level]));
        if entry & 0x01 == 0 {
            return None;
        }
        table_phys = entry & PTE_ADDR_MASK;
    }

    let pt_virt = phys_to_virt(table_phys) as *const u64;
    let pte = core::ptr::read_volatile(pt_virt.add(indices[3]));
    if pte & 0x01 != 0 {
        Some(pte & PTE_ADDR_MASK)
    } else {
        None
    }
}

/// Map a single 4K page in the user page tables.
/// Walks PML4 -> PDPT -> PD -> PT, allocating intermediate tables as needed.
///
/// IMPORTANT: When an existing intermediate table entry is found (inherited
/// from the kernel PML4 copy), we deep-copy the table so modifications don't
/// corrupt the kernel's shared page tables. This fixes the multi-ELF loader
/// state leak where user page mappings would overwrite kernel .rodata pages.
unsafe fn map_user_page(
    pml4_phys: u64,
    vaddr: u64,
    paddr: u64,
    flags: u64,
) -> Result<(), ElfError> {
    let indices = [
        ((vaddr >> 39) & 0x1FF) as usize, // PML4 index
        ((vaddr >> 30) & 0x1FF) as usize, // PDPT index
        ((vaddr >> 21) & 0x1FF) as usize, // PD index
        ((vaddr >> 12) & 0x1FF) as usize, // PT index
    ];

    // Track which tables we allocated in THIS load operation.
    // We use the current PML4's own address as a marker: if a table was
    // allocated in this call chain, its parent entry was written by us.
    // To avoid cross-process contamination, we check against pml4_phys.
    //
    // CRITICAL: Need enough slots for all intermediate page tables:
    //   - ELF segments: up to 3 levels (PDPT, PD, PT) per distinct PML4 index
    //   - User stack: 3 more levels at PML4[255]
    //   - Total: typically 6-12 tables per process load
    static mut CURRENT_LOAD_PML4: u64 = 0;
    static mut OWNED_TABLES: [u64; 32] = [0; 32];
    static mut OWNED_COUNT: usize = 0;

    // When PML4 changes (new process), reset owned tables
    if CURRENT_LOAD_PML4 != pml4_phys {
        CURRENT_LOAD_PML4 = pml4_phys;
        OWNED_COUNT = 0;
        for t in OWNED_TABLES.iter_mut() { *t = 0; }
    }

    let mut table_phys = pml4_phys;

    // Walk PML4 -> PDPT -> PD, creating or deep-copying entries as needed
    for level in 0..3 {
        let table_virt = phys_to_virt(table_phys) as *mut u64;
        let entry = core::ptr::read_volatile(table_virt.add(indices[level]));

        if entry & 0x01 == 0 {
            // Entry not present - allocate a new page table
            let new_table = alloc_elf_frame().ok_or(ElfError::OutOfMemory)?;
            core::ptr::write_bytes(phys_to_virt(new_table) as *mut u8, 0, PAGE_SIZE as usize);
            core::ptr::write_volatile(
                table_virt.add(indices[level]),
                new_table | 0x07, // P | W | U
            );
            // Track this table as owned by current load
            if OWNED_COUNT < OWNED_TABLES.len() {
                OWNED_TABLES[OWNED_COUNT] = new_table;
                OWNED_COUNT += 1;
            }
            table_phys = new_table;
        } else {
            let existing_phys = entry & !0xFFF;
            // Check if this table was allocated by THIS load operation.
            // If not, it's either a kernel table or a table from a PREVIOUS
            // process load — both must be deep-copied to prevent cross-process
            // page table corruption (the multi-ELF state leak bug).
            let mut owned = false;
            for i in 0..OWNED_COUNT {
                if OWNED_TABLES[i] == existing_phys {
                    owned = true;
                    break;
                }
            }
            if !owned {
                // NOT owned by this load — deep copy to isolate
                let new_table = alloc_elf_frame().ok_or(ElfError::OutOfMemory)?;
                // Deep copy the ENTIRE table to preserve all mappings
                // (kernel entries AND existing user entries from fork).
                // The new process's user pages will overwrite the old ones
                // as the ELF segments are mapped. Stale entries from the
                // forked parent are harmless since they'll be overwritten or
                // freed when this process exits.
                //
                // PREVIOUS BUG: We were only copying entries outside the ELF pool,
                // which dropped kernel intermediate tables that had been deep-copied
                // during fork (and thus lived in the ELF pool). This caused a triple
                // fault on CR3 switch because kernel .text was unmapped.
                let src_virt = phys_to_virt(existing_phys) as *const u64;
                let dst_virt = phys_to_virt(new_table) as *mut u64;
                core::ptr::copy_nonoverlapping(src_virt, dst_virt, 512);
                let new_entry = new_table | (entry & 0xFFF) | 0x07; // P|W|U
                core::ptr::write_volatile(table_virt.add(indices[level]), new_entry);
                if OWNED_COUNT < OWNED_TABLES.len() {
                    OWNED_TABLES[OWNED_COUNT] = new_table;
                    OWNED_COUNT += 1;
                }
                table_phys = new_table;
            } else {
                // Owned by this load — reuse safely
                table_phys = existing_phys;
            }
        }
    }

    // Write the final PT entry
    let pt_virt = phys_to_virt(table_phys) as *mut u64;
    core::ptr::write_volatile(pt_virt.add(indices[3]), paddr | flags);

    Ok(())
}

// ===== Full ELF Load Process =====

/// Load result: entry point and stack pointer
pub struct ElfLoadResult {
    pub entry_point: u64,
    pub stack_pointer: u64,
    pub pml4_phys: u64,
    pub segments_loaded: usize,
    pub frames_used: usize,
    /// Jalon 105: True if this ELF binary was detected as a Linux binary
    /// (via EI_OSABI, PT_INTERP, or GNU PT_NOTE)
    pub is_linux_abi: bool,
    /// Virtual address of program headers in memory (for AT_PHDR auxv).
    /// Computed as: first_load_vaddr + e_phoff - first_load_offset.
    pub phdr_vaddr: u64,
    /// Number of program headers (for AT_PHNUM auxv)
    pub phdr_count: u16,
    /// PT_INTERP path (e.g., "/lib/ld-musl-x86_64.so.1")
    pub interp_path: Option<alloc::string::String>,
    /// Whether this is an ET_DYN (PIE) binary
    pub is_pie: bool,
}

/// Result of loading ELF segments into an existing PML4 at a given base address.
/// Used for loading the interpreter (ld.so) into the same address space as the main binary.
pub struct InterpLoadResult {
    /// Interpreter entry point (base + e_entry for ET_DYN)
    pub entry_point: u64,
    /// Base virtual address where the interpreter was loaded
    pub base_vaddr: u64,
    /// Number of segments loaded
    pub segments_loaded: usize,
    /// Virtual address of interpreter's program headers in memory
    pub phdr_vaddr: u64,
    /// Number of interpreter program headers
    pub phdr_count: u16,
}

/// Load an ELF binary into a new per-process address space
///
/// Steps:
/// 1. Parse and validate ELF header
/// 2. Parse program headers
/// 3. Create per-process PML4 (clone kernel upper half)
/// 4. Map PT_LOAD segments with proper permissions
/// 5. Zero BSS regions (p_memsz > p_filesz)
/// 6. Map 8 MiB user stack at USER_STACK_TOP
/// 7. Return load result
pub fn load_elf_binary(elf_data: &[u8]) -> Result<ElfLoadResult, ElfError> {
    let frames_before = unsafe { ELF_POOL.frames_used };

    // Step 1: Parse header
    let hdr = parse_header(elf_data)?;
    let entry = hdr.e_entry;
    let phnum = hdr.e_phnum;
    let e_phoff = hdr.e_phoff;
    let e_type = hdr.e_type;
    let is_pie = e_type == ET_DYN;

    // Jalon 105: Detect Linux ABI for Linuxulator compatibility
    let is_linux_abi = crate::compat::linux_abi::detect_linux_elf(elf_data);

    crate::serial_println!(
        "[ELF] Header OK: entry=0x{:X}, phnum={}, type={}, abi={}",
        entry, phnum, if is_pie { "DYN/PIE" } else { "EXEC" },
        if is_linux_abi { "Linux" } else { "AetherionOS" }
    );

    // Step 2: Parse program headers
    let phdrs = parse_program_headers(elf_data, &hdr)?;

    // Step 2b: Detect PT_INTERP (dynamic linker path)
    let mut interp_path: Option<alloc::string::String> = None;
    for phdr in phdrs.iter() {
        if phdr.p_type == PT_INTERP {
            let offset = phdr.p_offset as usize;
            let size = phdr.p_filesz as usize;
            if offset + size <= elf_data.len() && size > 0 {
                // Read the interpreter path (NUL-terminated string)
                let path_bytes = &elf_data[offset..offset + size];
                // Strip trailing NUL
                let end = path_bytes.iter().position(|&b| b == 0).unwrap_or(size);
                if let Ok(path_str) = core::str::from_utf8(&path_bytes[..end]) {
                    interp_path = Some(alloc::string::String::from(path_str));
                    crate::serial_println!(
                        "[ELF] PT_INTERP detected: '{}'", path_str
                    );
                }
            }
        }
    }

    // Step 2c: For PIE (ET_DYN) binaries, compute base address offset.
    // PIE binaries have segments starting at vaddr 0; we load them at a fixed base
    // to avoid conflicts with the interpreter (which we load at a higher address).
    let pie_base: u64 = if is_pie {
        // Load PIE main binary at 0x0040_0000 (typical Linux default for PIE ASLR off)
        0x0040_0000u64
    } else {
        0 // ET_EXEC: use the vaddr from the ELF as-is
    };

    // Step 3: Create per-process PML4
    let pml4_phys = unsafe { create_user_pml4()? };

    // Step 4: Map PT_LOAD segments
    let mut segments_loaded = 0usize;

    // Track the first PT_LOAD segment's vaddr and file offset to compute AT_PHDR.
    // AT_PHDR = first_load_vaddr + e_phoff - first_load_offset
    let mut first_load_vaddr: u64 = 0;
    let mut first_load_offset: u64 = 0;
    let mut found_first_load = false;

    // Track already-mapped pages to handle overlapping segments (e.g., .rodata + .got
    // sharing the same 4K page). Max 256 unique pages per ELF (~1 MiB of mapped code/data).
    let mut mapped_pages: [(u64, u64); 256] = [(0, 0); 256];
    let mut mapped_page_count: usize = 0;

    for (i, phdr) in phdrs.iter().enumerate() {
        let p_type = phdr.p_type;
        if p_type != PT_LOAD {
            // Silently skip known Linux ELF program header types
            match p_type {
                PT_NOTE | PT_GNU_EH_FRAME | PT_GNU_STACK | PT_GNU_RELRO => {
                    // Expected Linux headers — no warning needed
                }
                _ => {
                    crate::serial_println!(
                        "[ELF] Skipping segment {}: type=0x{:X} (non-PT_LOAD)", i, p_type
                    );
                }
            }
            continue;
        }

        let raw_vaddr = phdr.p_vaddr;
        let vaddr = raw_vaddr + pie_base; // Apply PIE base offset for ET_DYN
        let memsz = phdr.p_memsz;
        let filesz = phdr.p_filesz;
        let offset = phdr.p_offset;
        let p_flags = phdr.p_flags;

        // Validate segment
        if offset + filesz > elf_data.len() as u64 {
            crate::serial_println!(
                "[ELF] ERROR: Segment {} offset+filesz exceeds file bounds",
                i
            );
            return Err(ElfError::InvalidSegment);
        }

        if vaddr >= USER_ADDR_LIMIT || vaddr + memsz > USER_ADDR_LIMIT {
            crate::serial_println!(
                "[ELF] ERROR: Segment {} vaddr 0x{:X} out of user range",
                i, vaddr
            );
            return Err(ElfError::AddressOutOfRange);
        }

        // Track first PT_LOAD segment for AT_PHDR computation
        if !found_first_load {
            first_load_vaddr = vaddr;
            first_load_offset = offset;
            found_first_load = true;
        }

        let page_flags = elf_flags_to_page_flags(p_flags);

        // Calculate page range
        let page_start = vaddr & !0xFFF;
        let page_end = (vaddr + memsz + 0xFFF) & !0xFFF;
        let num_pages = ((page_end - page_start) / PAGE_SIZE) as usize;

        crate::serial_println!(
            "[ELF] Loading segment {}: vaddr=0x{:X}, memsz=0x{:X}, filesz=0x{:X}, pages={}",
            i, vaddr, memsz, filesz, num_pages
        );

        // Map each page
        for page_idx in 0..num_pages {
            let page_vaddr = page_start + (page_idx as u64) * PAGE_SIZE;

            // FIX: Check if this page was already mapped by a previous segment.
            // Uses a simple O(n) scan of already-mapped pages instead of walking
            // potentially-unstable page tables during ELF load.
            let mut existing_frame: Option<u64> = None;
            for k in 0..mapped_page_count {
                if mapped_pages[k].0 == page_vaddr {
                    existing_frame = Some(mapped_pages[k].1);
                    break;
                }
            }

            let frame_phys = if let Some(f) = existing_frame {
                // Reuse existing frame — do NOT zero, previous segment data preserved
                f
            } else {
                // New page — allocate and zero
                let new_frame = unsafe { alloc_elf_frame().ok_or(ElfError::OutOfMemory)? };
                unsafe {
                    core::ptr::write_bytes(
                        phys_to_virt(new_frame) as *mut u8,
                        0,
                        PAGE_SIZE as usize,
                    );
                }
                // Record this mapping
                if mapped_page_count < mapped_pages.len() {
                    mapped_pages[mapped_page_count] = (page_vaddr, new_frame);
                    mapped_page_count += 1;
                }
                new_frame
            };

            // Jalon 109: Copy file data using precise segment/page intersection math.
            // This correctly handles segments that start mid-page (e.g. .got sharing
            // a physical page with .rodata) by computing exact byte ranges.
            {
                let seg_start = vaddr;
                let seg_end   = vaddr + filesz;  // end of file-backed data

                let page_start_va = page_vaddr;
                let page_end_va   = page_vaddr + PAGE_SIZE;

                let copy_start = core::cmp::max(seg_start, page_start_va);
                let copy_end   = core::cmp::min(seg_end,   page_end_va);

                if copy_start < copy_end {
                    let copy_len    = (copy_end - copy_start) as usize;
                    let file_off    = offset + (copy_start - seg_start);
                    let dest_offset = (copy_start - page_start_va) as usize;

                    if copy_len > 0 && (file_off as usize + copy_len) <= elf_data.len() {
                        unsafe {
                            let dst = (phys_to_virt(frame_phys) as *mut u8).add(dest_offset);
                            let src = elf_data.as_ptr().add(file_off as usize);
                            core::ptr::copy_nonoverlapping(src, dst, copy_len);
                        }
                    }
                }
            }
            // Pages beyond filesz are already zeroed (BSS)

            // Map the page in the user page table
            // (for reused frames, this updates flags; for new frames, this creates the mapping)
            unsafe {
                map_user_page(pml4_phys, page_vaddr, frame_phys, page_flags)?;
            }
        }

        segments_loaded += 1;
    }

    if segments_loaded == 0 {
        return Err(ElfError::NoLoadSegments);
    }

    // Step 6: Map user stack (8 MiB)
    let stack_bottom = USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE;
    crate::serial_println!(
        "[ELF] Mapping user stack: 0x{:X} - 0x{:X} ({} pages, {} KiB)",
        stack_bottom,
        USER_STACK_TOP,
        USER_STACK_PAGES,
        USER_STACK_PAGES * 4
    );

    // Stack flags: PRESENT | WRITABLE | USER_ACCESSIBLE | NO_EXECUTE
    let stack_flags: u64 = 0x01 | 0x02 | 0x04 | (1u64 << 63);

    // Capture the physical frame of the top stack page for AuxV injection.
    // This avoids the dangerous lookup_page_frame() call that caused #GP
    // by walking partially-built page tables.
    let mut top_stack_frame_phys: u64 = 0;

    for page_idx in 0..USER_STACK_PAGES {
        let page_vaddr = stack_bottom + page_idx * PAGE_SIZE;
        let frame_phys = unsafe { alloc_elf_frame().ok_or(ElfError::OutOfMemory)? };

        // Save the frame for the top-most stack page (USER_STACK_TOP - PAGE_SIZE)
        if page_vaddr == USER_STACK_TOP - PAGE_SIZE {
            top_stack_frame_phys = frame_phys;
        }

        // Zero the stack frame
        unsafe {
            core::ptr::write_bytes(
                phys_to_virt(frame_phys) as *mut u8,
                0,
                PAGE_SIZE as usize,
            );
        }

        unsafe {
            map_user_page(pml4_phys, page_vaddr, frame_phys, stack_flags)?;
        }
    }

    let frames_after = unsafe { ELF_POOL.frames_used };

    // ═══════════════════════════════════════════════════════════════
    // Step 7: Linux ABI Stack Layout — Inject AuxV, argv, envp
    // This allows unmodified Linux static binaries (musl/glibc) to boot.
    // Layout (growing DOWN from USER_STACK_TOP):
    //   [16 random bytes]        <- AT_RANDOM points here
    //   [program name string]    <- argv[0] points here
    //   [AuxV entries]           <- pairs of (type, value), ends with AT_NULL
    //   [NULL]                   <- end of envp
    //   [NULL]                   <- end of argv
    //   [argv[0] ptr]            <- pointer to program name
    //   [argc = 1]               <- top of stack (RSP points here)
    // ═══════════════════════════════════════════════════════════════
    let linux_rsp = unsafe {
        // Use the physical frame saved during stack allocation (Step 6).
        // This avoids the dangerous lookup_page_frame() which walks
        // partially-built user page tables and causes #GP kernel panics.
        let frame_phys = top_stack_frame_phys;
        if frame_phys == 0 {
            crate::serial_println!("[ELF] WARNING: Cannot find stack top page for AuxV injection");
            let computed_phdr_vaddr = if found_first_load { first_load_vaddr + e_phoff - first_load_offset } else { 0 };
            return Ok(ElfLoadResult {
                entry_point: entry + pie_base,
                stack_pointer: USER_STACK_INITIAL_RSP,
                pml4_phys,
                segments_loaded,
                frames_used: frames_after - frames_before,
                is_linux_abi,
                phdr_vaddr: computed_phdr_vaddr,
                phdr_count: phnum,
                interp_path: interp_path.clone(),
                is_pie,
            });
        }

        let page_base = phys_to_virt(frame_phys) as *mut u8;
        let top_stack_page = USER_STACK_TOP - PAGE_SIZE; // vaddr 0x7FFF_FFFF_E000
        // The page maps vaddr [0x7FFF_FFFF_E000 .. 0x7FFF_FFFF_EFFF]
        // We'll write data starting from the END of this page, growing down.

        // --- Write 16 random bytes at offset 0xF00 in page (vaddr 0x7FFF_FFFF_EF00) ---
        let random_offset: usize = 0xF00;
        let random_vaddr: u64 = top_stack_page + random_offset as u64;
        {
            // Use RDTSC as entropy source for pseudo-random bytes
            let tsc: u64;
            core::arch::asm!("rdtsc", "shl rdx, 32", "or rax, rdx",
                             out("rax") tsc, out("rdx") _, options(nomem, nostack));
            let random_ptr = page_base.add(random_offset);
            let mut seed = tsc;
            for i in 0..16 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                core::ptr::write_volatile(random_ptr.add(i), (seed >> 33) as u8);
            }
        }

        // --- Write program name "aetherion" at offset 0xF10 ---
        let progname_offset: usize = 0xF10;
        let progname_vaddr: u64 = top_stack_page + progname_offset as u64;
        {
            let name = b"aetherion\0";
            let dst = page_base.add(progname_offset);
            for (i, &b) in name.iter().enumerate() {
                core::ptr::write_volatile(dst.add(i), b);
            }
        }

        // --- Build the stack frame growing DOWN from offset 0xEF0 ---
        // We use offset 0xE00 as the base for our stack data (plenty of room)
        // Each entry is 8 bytes (u64). We build bottom-up then set RSP.

        // Jalon 133: Compute correct AT_PHDR from first PT_LOAD segment.
        // AT_PHDR = first_load_vaddr + e_phoff - first_load_offset
        // This gives the virtual address where program headers are mapped in memory.
        let computed_phdr_vaddr = if found_first_load {
            first_load_vaddr + e_phoff - first_load_offset
        } else {
            entry & !0xFFF  // fallback
        };

        // Compute AuxV entries
        let auxv: [(u64, u64); 14] = [
            (AT_PAGESZ,  4096),
            (AT_PHDR,    computed_phdr_vaddr),   // Correct phdr address in memory
            (AT_PHENT,   56),                    // sizeof(Elf64_Phdr)
            (AT_PHNUM,   phdrs.len() as u64),
            (AT_ENTRY,   entry),
            (AT_BASE,    0),                     // No interpreter (static binary)
            (AT_FLAGS,   0),
            (AT_UID,     0),
            (AT_EUID,    0),
            (AT_GID,     0),
            (AT_EGID,    0),
            (AT_SECURE,  0),
            (AT_RANDOM,  random_vaddr),
            (AT_HWCAP,   0x078bfbff),            // SSE, SSE2, AVX, AVX2, FMA
        ];

        // Layout (addresses grow DOWN):
        // [argc]           <- RSP
        // [argv[0] ptr]
        // [NULL]           <- end of argv
        // [NULL]           <- end of envp
        // [auxv[0].type]   [auxv[0].value]
        // ...
        // [AT_NULL]        [0]

        // Total u64 entries: 1 (argc) + 1 (argv ptr) + 1 (null) + 1 (null) + 14*2 (auxv) + 2 (AT_NULL) = 34
        let total_entries: usize = 1 + 1 + 1 + 1 + auxv.len() * 2 + 2;
        let stack_data_size = total_entries * 8;

        // Place RSP at a 16-byte aligned address well within the page
        // Start from offset 0xEF0 and go backwards
        let rsp_offset = 0xEF0 - stack_data_size;
        let rsp_offset_aligned = rsp_offset & !0xF; // 16-byte align
        let rsp_vaddr = top_stack_page + rsp_offset_aligned as u64;

        let mut pos = rsp_offset_aligned;
        let write_u64 = |off: usize, val: u64| {
            let ptr = page_base.add(off) as *mut u64;
            core::ptr::write_volatile(ptr, val);
        };

        // argc = 1
        write_u64(pos, 1); pos += 8;
        // argv[0] = pointer to program name
        write_u64(pos, progname_vaddr); pos += 8;
        // argv terminator (NULL)
        write_u64(pos, 0); pos += 8;
        // envp terminator (NULL)
        write_u64(pos, 0); pos += 8;
        // AuxV entries
        for &(atype, aval) in auxv.iter() {
            write_u64(pos, atype); pos += 8;
            write_u64(pos, aval);  pos += 8;
        }
        // AT_NULL terminator
        write_u64(pos, AT_NULL); pos += 8;
        write_u64(pos, 0);

        crate::serial_println!(
            "[ELF] Linux ABI: AuxV injected, RSP=0x{:X}, argc=1, AT_PHDR=0x{:X}, AT_RANDOM=0x{:X}",
            rsp_vaddr, computed_phdr_vaddr, random_vaddr
        );

        rsp_vaddr
    };

    // Apply PIE base to entry point
    let final_entry = entry + pie_base;

    crate::serial_println!(
        "[ELF] Load complete: entry=0x{:X}, stack_rsp=0x{:X}, segments={}, frames={}, pie_base=0x{:X}",
        final_entry,
        linux_rsp,
        segments_loaded,
        frames_after - frames_before,
        pie_base
    );

    let final_phdr_vaddr = if found_first_load { first_load_vaddr + e_phoff - first_load_offset } else { 0 };

    if interp_path.is_some() {
        crate::serial_println!(
            "[ELF] Dynamic binary: interp='{}', AT_PHDR=0x{:X}, AT_ENTRY=0x{:X}",
            interp_path.as_ref().unwrap(), final_phdr_vaddr, final_entry
        );
    }

    Ok(ElfLoadResult {
        entry_point: final_entry,
        stack_pointer: linux_rsp,
        pml4_phys,
        segments_loaded,
        frames_used: frames_after - frames_before,
        is_linux_abi,
        phdr_vaddr: final_phdr_vaddr,
        phdr_count: phnum,
        interp_path,
        is_pie,
    })
}

// ═══════════════════════════════════════════════════════════════
// Jalon 134: Load interpreter (ld.so) into existing PML4
// ═══════════════════════════════════════════════════════════════

/// Load an ET_DYN ELF (interpreter/ld.so) into an existing process PML4
/// at a given base virtual address.
///
/// This is called after load_elf_binary() has loaded the main executable.
/// The interpreter is mapped at `base_vaddr` (e.g., 0x7FC0_0000_0000) to
/// avoid conflicts with the main binary's address range.
///
/// Returns the interpreter's adjusted entry point and metadata.
pub fn load_interp_into_pml4(
    interp_data: &[u8],
    pml4_phys: u64,
    base_vaddr: u64,
) -> Result<InterpLoadResult, ElfError> {
    // Parse interpreter ELF header
    let hdr = parse_header(interp_data)?;
    let e_entry = hdr.e_entry;
    let e_phoff = hdr.e_phoff;
    let phnum = hdr.e_phnum;

    crate::serial_println!(
        "[INTERP] Loading interpreter: e_entry=0x{:X}, phnum={}, base=0x{:X}",
        e_entry, phnum, base_vaddr
    );

    let phdrs = parse_program_headers(interp_data, &hdr)?;

    let mut segments_loaded = 0usize;
    let mut first_load_vaddr: u64 = 0;
    let mut first_load_offset: u64 = 0;
    let mut found_first_load = false;

    let mut mapped_pages: [(u64, u64); 512] = [(0, 0); 512];
    let mut mapped_page_count: usize = 0;

    for (i, phdr) in phdrs.iter().enumerate() {
        if phdr.p_type != PT_LOAD {
            continue;
        }

        let raw_vaddr = phdr.p_vaddr;
        let vaddr = raw_vaddr + base_vaddr; // Relocate to base
        let memsz = phdr.p_memsz;
        let filesz = phdr.p_filesz;
        let offset = phdr.p_offset;
        let p_flags = phdr.p_flags;

        if offset + filesz > interp_data.len() as u64 {
            crate::serial_println!("[INTERP] ERROR: Segment {} exceeds file bounds", i);
            return Err(ElfError::InvalidSegment);
        }

        if vaddr >= USER_ADDR_LIMIT || vaddr + memsz > USER_ADDR_LIMIT {
            crate::serial_println!("[INTERP] ERROR: Segment {} vaddr 0x{:X} out of range", i, vaddr);
            return Err(ElfError::AddressOutOfRange);
        }

        if !found_first_load {
            first_load_vaddr = vaddr;
            first_load_offset = offset;
            found_first_load = true;
        }

        let page_flags = elf_flags_to_page_flags(p_flags);
        let page_start = vaddr & !0xFFF;
        let page_end = (vaddr + memsz + 0xFFF) & !0xFFF;
        let num_pages = ((page_end - page_start) / PAGE_SIZE) as usize;

        crate::serial_println!(
            "[INTERP] Segment {}: vaddr=0x{:X}, memsz=0x{:X}, filesz=0x{:X}, pages={}",
            i, vaddr, memsz, filesz, num_pages
        );

        for page_idx in 0..num_pages {
            let page_vaddr = page_start + (page_idx as u64) * PAGE_SIZE;

            // Check if page already mapped by a previous segment
            let mut existing_frame: Option<u64> = None;
            for k in 0..mapped_page_count {
                if mapped_pages[k].0 == page_vaddr {
                    existing_frame = Some(mapped_pages[k].1);
                    break;
                }
            }

            let frame_phys = if let Some(f) = existing_frame {
                f
            } else {
                let new_frame = unsafe { alloc_elf_frame().ok_or(ElfError::OutOfMemory)? };
                unsafe {
                    core::ptr::write_bytes(
                        phys_to_virt(new_frame) as *mut u8,
                        0,
                        PAGE_SIZE as usize,
                    );
                }
                if mapped_page_count < mapped_pages.len() {
                    mapped_pages[mapped_page_count] = (page_vaddr, new_frame);
                    mapped_page_count += 1;
                }
                new_frame
            };

            // Copy file data
            {
                let seg_start = vaddr;
                let seg_end = vaddr + filesz;
                let page_start_va = page_vaddr;
                let page_end_va = page_vaddr + PAGE_SIZE;
                let copy_start = core::cmp::max(seg_start, page_start_va);
                let copy_end = core::cmp::min(seg_end, page_end_va);

                if copy_start < copy_end {
                    let copy_len = (copy_end - copy_start) as usize;
                    let file_off = offset + (copy_start - seg_start);
                    let dest_offset = (copy_start - page_start_va) as usize;

                    if copy_len > 0 && (file_off as usize + copy_len) <= interp_data.len() {
                        unsafe {
                            let dst = (phys_to_virt(frame_phys) as *mut u8).add(dest_offset);
                            let src = interp_data.as_ptr().add(file_off as usize);
                            core::ptr::copy_nonoverlapping(src, dst, copy_len);
                        }
                    }
                }
            }

            unsafe {
                map_user_page(pml4_phys, page_vaddr, frame_phys, page_flags)?;
            }
        }

        segments_loaded += 1;
    }

    let interp_entry = e_entry + base_vaddr;
    let interp_phdr_vaddr = if found_first_load {
        first_load_vaddr + e_phoff - first_load_offset
    } else {
        0
    };

    crate::serial_println!(
        "[INTERP] Loaded: entry=0x{:X}, base=0x{:X}, segments={}, phdr=0x{:X}",
        interp_entry, base_vaddr, segments_loaded, interp_phdr_vaddr
    );

    Ok(InterpLoadResult {
        entry_point: interp_entry,
        base_vaddr,
        segments_loaded,
        phdr_vaddr: interp_phdr_vaddr,
        phdr_count: phnum,
    })
}

// ═══════════════════════════════════════════════════════════════
// Jalon 127: Build System V ABI Stack with real argv/envp
// ═══════════════════════════════════════════════════════════════

/// Build a proper System V x86_64 ABI stack for execve.
///
/// Writes strings, pointers, auxv, argc onto the top stack page.
/// Returns the final RSP value (16-byte aligned, pointing to argc).
///
/// Stack layout (growing DOWN from page top):
///   [string area: argv[0]\0, argv[1]\0, ..., envp[0]\0, ...]
///   [16 random bytes]
///   [padding to 16-byte align]
///   [AT_NULL, 0]
///   [auxv entries ...]
///   [NULL]                <- end of envp
///   [envp[N-1] ptr ... envp[0] ptr]
///   [NULL]                <- end of argv
///   [argv[N-1] ptr ... argv[0] ptr]
///   [argc]                <- RSP points here
///
/// SAFETY: pml4_phys must be a valid PML4 with the top stack page mapped.
///
/// Parameters:
///   - main_entry: the main binary's entry point (for AT_ENTRY)
///   - interp_base: base address where the interpreter was loaded (for AT_BASE), 0 if static
///   - phdr_vaddr: virtual address of the MAIN binary's program headers (for AT_PHDR)
///   - phdr_count: number of MAIN binary's program headers (for AT_PHNUM)
pub unsafe fn build_sysv_stack(
    pml4_phys: u64,
    argv: &[alloc::string::String],
    envp: &[alloc::string::String],
    main_entry: u64,
    interp_base: u64,
    phdr_vaddr: u64,
    phdr_count: u16,
) -> Option<u64> {
    // Find the physical frame backing the top stack page
    let top_stack_page_vaddr = USER_STACK_TOP - PAGE_SIZE; // 0x7FFF_FFFF_E000
    let frame_phys = lookup_page_frame(pml4_phys, top_stack_page_vaddr)?;
    let virt_addr = phys_to_virt(frame_phys);
    crate::serial_println!(
        "[ELF] build_sysv_stack: pml4=0x{:X}, stack_page=0x{:X}, frame=0x{:X}, virt=0x{:X}",
        pml4_phys, top_stack_page_vaddr, frame_phys, virt_addr
    );
    let page_base = virt_addr as *mut u8;

    // We have 4096 bytes in this page. Layout:
    // Offsets 0xC00..0xFFF: string area + random bytes (1024 bytes for strings)
    // Offsets 0x400..0xBFF: pointer/auxv area (2048 bytes = 256 u64s)

    // --- Phase 1: Write strings starting at offset 0xC00 ---
    let mut str_offset: usize = 0xC00;
    let mut argv_vaddrs: alloc::vec::Vec<u64> = alloc::vec::Vec::new();
    let mut envp_vaddrs: alloc::vec::Vec<u64> = alloc::vec::Vec::new();

    for arg in argv.iter() {
        let bytes = arg.as_bytes();
        let len = core::cmp::min(bytes.len(), 255); // Limit each arg to 255 bytes
        if str_offset + len + 1 > 0xFF0 { break; } // Leave room for random bytes
        let vaddr = top_stack_page_vaddr + str_offset as u64;
        for i in 0..len {
            core::ptr::write_volatile(page_base.add(str_offset + i), bytes[i]);
        }
        core::ptr::write_volatile(page_base.add(str_offset + len), 0u8); // NUL
        argv_vaddrs.push(vaddr);
        str_offset += len + 1;
    }

    for env in envp.iter() {
        let bytes = env.as_bytes();
        let len = core::cmp::min(bytes.len(), 255);
        if str_offset + len + 1 > 0xFF0 { break; }
        let vaddr = top_stack_page_vaddr + str_offset as u64;
        for i in 0..len {
            core::ptr::write_volatile(page_base.add(str_offset + i), bytes[i]);
        }
        core::ptr::write_volatile(page_base.add(str_offset + len), 0u8);
        envp_vaddrs.push(vaddr);
        str_offset += len + 1;
    }

    // --- Phase 2: Write 16 random bytes at offset 0xFF0 ---
    let random_vaddr = top_stack_page_vaddr + 0xFF0;
    {
        let tsc: u64;
        core::arch::asm!("rdtsc", "shl rdx, 32", "or rax, rdx",
                         out("rax") tsc, out("rdx") _, options(nomem, nostack));
        let random_ptr = page_base.add(0xFF0);
        let mut seed = tsc;
        for i in 0..16 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            core::ptr::write_volatile(random_ptr.add(i), (seed >> 33) as u8);
        }
    }

    // --- Phase 3: Build pointer/auxv area growing DOWN from offset 0xBF8 ---
    // Jalon 134: Use correct AT_PHDR (main binary's phdr), AT_ENTRY (main entry),
    // and AT_BASE (interpreter base, 0 for static binaries).
    let actual_phdr = if phdr_vaddr != 0 { phdr_vaddr } else { main_entry & !0xFFF };
    let actual_phnum = if phdr_count > 0 { phdr_count as u64 } else { 4 };
    let auxv: [(u64, u64); 14] = [
        (AT_PAGESZ,  4096),
        (AT_PHDR,    actual_phdr),              // Main binary's program headers in memory
        (AT_PHENT,   56),
        (AT_PHNUM,   actual_phnum),             // Main binary's program header count
        (AT_ENTRY,   main_entry),               // Main binary's entry point (ld.so uses this)
        (AT_BASE,    interp_base),              // Interpreter base address (0 for static)
        (AT_FLAGS,   0),
        (AT_UID,     0),
        (AT_EUID,    0),
        (AT_GID,     0),
        (AT_EGID,    0),
        (AT_SECURE,  0),
        (AT_RANDOM,  random_vaddr),
        (AT_HWCAP,   0x078bfbff),
    ];

    // Total u64 entries:
    //   1 (argc) + argv_vaddrs.len() (argv ptrs) + 1 (NULL)
    //   + envp_vaddrs.len() (envp ptrs) + 1 (NULL)
    //   + auxv.len()*2 (type/value pairs) + 2 (AT_NULL pair)
    let total_entries = 1 + argv_vaddrs.len() + 1 + envp_vaddrs.len() + 1 + auxv.len() * 2 + 2;
    let stack_data_size = total_entries * 8;

    // RSP must be 16-byte aligned, well within the page
    let rsp_offset = (0xBF8 - stack_data_size) & !0xF;
    if rsp_offset < 0x100 {
        // Too many args, not enough stack space
        crate::serial_println!("[ELF] WARNING: argv/envp too large for stack page");
        return None;
    }

    let rsp_vaddr = top_stack_page_vaddr + rsp_offset as u64;

    let write_u64 = |off: usize, val: u64| {
        let ptr = page_base.add(off) as *mut u64;
        core::ptr::write_volatile(ptr, val);
    };

    let mut pos = rsp_offset;

    // argc
    write_u64(pos, argv_vaddrs.len() as u64); pos += 8;

    // argv[0], argv[1], ...
    for &vaddr in argv_vaddrs.iter() {
        write_u64(pos, vaddr); pos += 8;
    }
    // argv terminator (NULL)
    write_u64(pos, 0); pos += 8;

    // envp[0], envp[1], ...
    for &vaddr in envp_vaddrs.iter() {
        write_u64(pos, vaddr); pos += 8;
    }
    // envp terminator (NULL)
    write_u64(pos, 0); pos += 8;

    // AuxV entries
    for &(atype, aval) in auxv.iter() {
        write_u64(pos, atype); pos += 8;
        write_u64(pos, aval);  pos += 8;
    }
    // AT_NULL terminator
    write_u64(pos, AT_NULL); pos += 8;
    write_u64(pos, 0);

    crate::serial_println!(
        "[ELF] Jalon 127: System V stack built: argc={}, envc={}, RSP=0x{:X}, AT_PHDR=0x{:X}, AT_PHNUM={}",
        argv_vaddrs.len(), envp_vaddrs.len(), rsp_vaddr, actual_phdr, actual_phnum
    );

    Some(rsp_vaddr)
}

// ===== Load ELF from VFS path =====

/// Load an ELF binary from the VFS and create a Ring 3 process
///
/// This is the main entry point called by the shell's `exec` command.
/// Returns the PID of the newly created process.
pub fn load_elf(path: &str) -> Result<u64, ElfError> {
    crate::serial_println!("[ELF] load_elf(\"{}\")", path);

    // Step 1: Read file from VFS
    let elf_data = crate::fs::vfs::file_read(path).map_err(|e| {
        crate::serial_println!("[ELF] VFS error reading '{}': {}", path, e);
        ElfError::VfsError
    })?;

    crate::serial_println!("[ELF] Read {} bytes from VFS", elf_data.len());

    // Step 2: Load ELF binary
    let result = load_elf_binary(&elf_data)?;

    // Step 3: Create a process with Ring 3 context
    // GDT selectors for Ring 3:
    //   CS = 0x23 (User Code, RPL=3)
    //   SS = 0x1B (User Data, RPL=3)
    //   RFLAGS = 0x202 (IF=1, reserved bit 1)
    //   RIP = entry point
    //   RSP = stack top

    let pid = crate::process::spawn_kernel_thread(path)
        .map_err(|_| ElfError::ProcessError)?;
    crate::process::set_pml4_phys(pid, result.pml4_phys).map_err(|_| ElfError::ProcessError)?;

    crate::serial_println!(
        "[ELF] Process created: PID={}, entry=0x{:X}, stack=0x{:X}",
        pid, result.entry_point, result.stack_pointer
    );

    // Register with scheduler
    crate::scheduler::enqueue_process(pid);

    // Log the IRETQ frame that would be used for Ring 3 transition
    crate::serial_println!("[ELF] Ring 3 IRETQ frame:");
    crate::serial_println!("  RIP    = 0x{:X}", result.entry_point);
    crate::serial_println!("  CS     = 0x23 (User Code, RPL=3)");
    crate::serial_println!("  RFLAGS = 0x202 (IF=1)");
    crate::serial_println!("  RSP    = 0x{:X}", result.stack_pointer);
    crate::serial_println!("  SS     = 0x1B (User Data, RPL=3)");
    crate::serial_println!(
        "[ELF] PML4 = 0x{:X}, ready for CR3 switch + IRETQ",
        result.pml4_phys
    );

    Ok(pid)
}

// ===== Ring 3 Jump (IRETQ) =====

/// Jump to Ring 3 user mode via IRETQ
///
/// This sets up the IRETQ stack frame and executes it.
/// The CPU will switch to user mode (Ring 3) with:
///   - CS = 0x23 (User Code Segment, RPL=3)
///   - SS = 0x1B (User Data Segment, RPL=3)
///   - RIP = entry_point
///   - RSP = stack_pointer
///   - RFLAGS = 0x202 (IF=1)
///
/// SAFETY: The page tables must be loaded (CR3) before calling this.
/// The entry_point and stack_pointer must be mapped in the user address space.
/// Trampoline for execve: switches CR3 and jumps to Ring 3.
///
/// This function MUST be called via its physical-offset mapping address
/// (0xFFFF800000000000 + phys_addr) because the new PML4 may not have
/// kernel .text mapped at its identity-mapped address (PML4[0] may be
/// overwritten by user ELF segments like BusyBox at 0x400000).
///
/// Register convention (all passed in):
///   r8  = new PML4 physical address (for CR3)
///   r9  = user RSP (stack pointer for iretq)
///   r10 = user RIP (entry point for iretq)
///
/// This function never returns.
#[naked]
pub unsafe extern "C" fn exec_trampoline() -> ! {
    core::arch::asm!(
        // The caller has already:
        //   - Disabled interrupts (cli)
        //   - Relocated RSP to the physical-offset mapping
        // Register convention:
        //   r8  = new PML4 physical address
        //   r9  = user RSP
        //   r10 = user RIP

        // Switch to new address space
        "mov cr3, r8",

        // Swap GS: kernel GS -> user GS
        "swapgs",

        // Zero all GPRs to prevent kernel state leaks
        "xor rax, rax",
        "xor rbx, rbx",
        "xor rcx, rcx",
        "xor rdx, rdx",
        "xor rsi, rsi",
        "xor rdi, rdi",
        "xor rbp, rbp",
        "xor r8, r8",
        "xor r11, r11",
        "xor r12, r12",
        "xor r13, r13",
        "xor r14, r14",
        "xor r15, r15",
        // Build IRETQ frame
        "push 0x1B",     // SS  = User Data (RPL=3)
        "push r9",       // RSP = user stack
        "push 0x202",    // RFLAGS (IF=1)
        "push 0x23",     // CS  = User Code (RPL=3)
        "push r10",      // RIP = entry point
        "iretq",
        options(noreturn),
    );
}

/// Execute a CR3 switch + jump to Ring 3 safely, even when the new PML4
/// has user ELF segments that overlap kernel .text (e.g., BusyBox at 0x400000).
///
/// Strategy: We call exec_trampoline via its physical-offset mapping address.
/// PML4[256] (physical memory offset at 0xFFFF800000000000) is present in BOTH
/// the old and new PML4, so the trampoline code remains accessible during and
/// after the CR3 switch.
pub unsafe fn exec_switch_cr3_and_ring3(
    new_pml4_phys: u64,
    user_entry: u64,
    user_rsp: u64,
) -> ! {
    // Compute the physical address of exec_trampoline
    let trampoline_virt = exec_trampoline as *const () as u64;
    // Convert identity-mapped VA to physical address
    // Kernel is identity-mapped, so phys = virt for low addresses
    let trampoline_phys = trampoline_virt; // identity mapping: phys == virt
    // Compute the high-half virtual address via physical memory offset
    let phys_off = phys_offset();
    let trampoline_high = trampoline_phys + phys_off;

    // Load arguments into the registers expected by the trampoline:
    //   r8  = new CR3
    //   r9  = user RSP
    //   r10 = user RIP
    // Then jump to the trampoline at its high-address mapping.
    core::arch::asm!(
        "cli",
        "add rsp, {phys_off}",
        "jmp {trampoline}",
        phys_off = in(reg) phys_off,
        trampoline = in(reg) trampoline_high,
        in("r8") new_pml4_phys,
        in("r9") user_rsp,
        in("r10") user_entry,
        options(noreturn),
    );
}

#[allow(unused)]
pub unsafe fn jump_to_ring3(entry_point: u64, stack_pointer: u64) -> ! {
    // Jalon 133: Direct UART trace (no lock, no formatting) to confirm we reach here
    core::arch::asm!(
        "2: mov dx, 0x3FD", "in al, dx", "test al, 0x20", "jz 2b",
        "mov dx, 0x3F8", "mov al, 0x4A", "out dx, al",  // 'J'
        out("dx") _, out("al") _, options(nomem, nostack, preserves_flags)
    );
    // Jalon 109c+133: Clear all GPRs to prevent leaking kernel state to user mode.
    // Also prevents stale register values from being misinterpreted as syscall args.
    let f_rsp = core::ptr::read_volatile(&stack_pointer);
    let f_rip = core::ptr::read_volatile(&entry_point);
    core::arch::asm!(
        // Zero all general-purpose registers that user code might read
        "xor rax, rax",
        "xor rbx, rbx",
        "xor rcx, rcx",
        "xor rdx, rdx",
        "xor rsi, rsi",
        "xor rdi, rdi",
        "xor rbp, rbp",
        "xor r8, r8",
        "xor r11, r11",
        "xor r12, r12",
        "xor r13, r13",
        "xor r14, r14",
        "xor r15, r15",
        // Push SS (User Data = 0x1B)
        "push 0x1B",
        // Push RSP (user stack pointer, guaranteed in r9)
        "push r9",
        // Push RFLAGS (IF=1, bit1=1 -> 0x202)
        "push 0x202",
        // Push CS (User Code = 0x23)
        "push 0x23",
        // Push RIP (entry point, guaranteed in r10)
        "push r10",
        // Execute IRETQ to switch to Ring 3
        "iretq",
        in("r9") f_rsp,
        in("r10") f_rip,
        options(noreturn),
    );
}

// ===== Self-Test Suite =====

/// Run ELF loader tests using embedded hello.elf
pub fn run_tests(elf_data: &[u8]) {
    crate::serial_write("\n========================================\n");
    crate::serial_write("[ELF TESTS] Couche 11 - ELF Loader\n");
    crate::serial_write("========================================\n\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // Test 1: ELF magic verification
    crate::serial_write("  [TEST 1/8] ELF magic... ");
    if elf_data.len() >= 4 && elf_data[0..4] == ELF_MAGIC {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_write("FAIL\n");
        failed += 1;
    }

    // Test 2: Parse header
    crate::serial_write("  [TEST 2/8] Parse ELF64 header... ");
    match parse_header(elf_data) {
        Ok(hdr) => {
            let entry = hdr.e_entry;
            let phnum = hdr.e_phnum;
            crate::serial_println!("OK (entry=0x{:X}, phnum={})", entry, phnum);
            passed += 1;
        }
        Err(e) => {
            crate::serial_println!("FAIL: {}", e);
            failed += 1;
        }
    }

    // Test 3: Parse program headers
    crate::serial_write("  [TEST 3/8] Parse program headers... ");
    let hdr = parse_header(elf_data);
    if let Ok(ref h) = hdr {
        match parse_program_headers(elf_data, h) {
            Ok(phdrs) => {
                crate::serial_println!("OK ({} headers)", phdrs.len());
                passed += 1;
            }
            Err(e) => {
                crate::serial_println!("FAIL: {}", e);
                failed += 1;
            }
        }
    } else {
        crate::serial_write("SKIP (header parse failed)\n");
        failed += 1;
    }

    // Test 4: PT_LOAD segments found
    crate::serial_write("  [TEST 4/8] PT_LOAD segments... ");
    if let Ok(ref h) = hdr {
        if let Ok(phdrs) = parse_program_headers(elf_data, h) {
            let load_count = phdrs.iter().filter(|p| p.p_type == PT_LOAD).count();
            if load_count > 0 {
                crate::serial_println!("OK ({} loadable)", load_count);
                passed += 1;
            } else {
                crate::serial_write("FAIL (no PT_LOAD)\n");
                failed += 1;
            }
        } else {
            crate::serial_write("SKIP\n");
            failed += 1;
        }
    } else {
        crate::serial_write("SKIP\n");
        failed += 1;
    }

    // Test 5: Validate ELF load parameters (non-destructive: parse only)
    // NOTE: We do NOT call load_elf_binary() here because it modifies the
    // shared kernel page table subtree at PML4[16]. A second load_elf_binary
    // (the actual launch) would then find stale user page mappings from this
    // test, causing deterministic page faults. The real load happens in main().
    crate::serial_write("  [TEST 5/8] ELF load validation (parse)... ");
    if let Ok(ref h) = hdr {
        if let Ok(phdrs) = parse_program_headers(elf_data, h) {
            let load_segs: usize = phdrs.iter().filter(|p| p.p_type == PT_LOAD).count();
            let entry = h.e_entry;
            let valid = load_segs > 0
                && entry < USER_ADDR_LIMIT
                && ELF_POOL_INITIALIZED.load(Ordering::SeqCst);
            if valid {
                crate::serial_println!(
                    "OK (entry=0x{:X}, {} segments, pool ready)",
                    entry, load_segs
                );
                passed += 1;
            } else {
                crate::serial_write("FAIL (validation)\n");
                failed += 1;
            }
        } else {
            crate::serial_write("SKIP (phdr parse)\n");
            failed += 1;
        }
    } else {
        crate::serial_write("SKIP (header)\n");
        failed += 1;
    }

    // Test 6: Invalid ELF rejected (bad magic in full-size buffer)
    crate::serial_write("  [TEST 6/8] Invalid ELF rejected... ");
    {
        let mut bad_elf = [0u8; 64]; // sizeof(Elf64Ehdr) = 64
        bad_elf[0] = 0xFF; // wrong magic
        match parse_header(&bad_elf) {
            Err(ElfError::BadMagic) => {
                crate::serial_write("OK (BadMagic)\n");
                passed += 1;
            }
            other => {
                crate::serial_println!("FAIL (got {:?})", other);
                failed += 1;
            }
        }
    }

    // Test 7: Too-small data rejected
    crate::serial_write("  [TEST 7/8] Too-small data rejected... ");
    match parse_header(&[0x7F, b'E']) {
        Err(ElfError::TooSmall) => {
            crate::serial_write("OK (TooSmall)\n");
            passed += 1;
        }
        other => {
            crate::serial_println!("FAIL (got {:?})", other);
            failed += 1;
        }
    }

    // Test 8: User stack address check
    crate::serial_write("  [TEST 8/8] Stack address range... ");
    {
        let stack_bottom = USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE;
        if stack_bottom < USER_ADDR_LIMIT && USER_STACK_TOP < USER_ADDR_LIMIT {
            crate::serial_println!(
                "OK (0x{:X} - 0x{:X})",
                stack_bottom,
                USER_STACK_TOP
            );
            passed += 1;
        } else {
            crate::serial_write("FAIL (out of range)\n");
            failed += 1;
        }
    }

    crate::serial_write("\n========================================\n");
    crate::serial_println!(
        "[ELF TESTS] {}/{} passed, {} failed",
        passed,
        passed + failed,
        failed
    );
    if failed == 0 {
        crate::serial_write("[ELF TESTS] ALL TESTS PASSED!\n");
    }
    crate::serial_write("========================================\n");
}
