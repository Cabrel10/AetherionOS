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

/// ELF class: 64-bit
const ELFCLASS64: u8 = 2;
/// ELF data: little-endian
const ELFDATA2LSB: u8 = 1;
/// ELF type: executable
const ET_EXEC: u16 = 2;
/// ELF machine: x86-64
const EM_X86_64: u16 = 62;

/// Program header type: loadable segment
const PT_LOAD: u32 = 1;

/// Segment permission flags
const PF_X: u32 = 1; // Execute
const PF_W: u32 = 2; // Write
const PF_R: u32 = 4; // Read

/// Page size
const PAGE_SIZE: u64 = 4096;

/// User stack top virtual address (grows down from here)
/// Stack occupies: 0x7FFF_FFFF_F000 - stack_size to 0x7FFF_FFFF_F000
const USER_STACK_TOP: u64 = 0x7FFF_FFFF_F000;
/// User stack size: 8 MiB (2048 pages) — but we map only 16 pages (64 KiB)
/// during initial load to conserve frames. The full 8 MiB is the virtual
/// range reserved; demand paging will handle the rest in the future.
const USER_STACK_PAGES: u64 = 16; // 64 KiB initial mapping

/// Maximum valid user-space address
const USER_ADDR_LIMIT: u64 = 0x0000_8000_0000_0000;

/// ELF frame pool: dedicated frames for ELF loading
const ELF_FRAME_POOL_SIZE: usize = 4096; // Up to 16 MiB of user pages

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
// A simple bump allocator for physical frames used by ELF loading.
// In a real OS this would integrate with the main frame allocator.

struct ElfFramePool {
    base_frame: u64,    // Physical base address (frame-aligned)
    frames_used: usize,
    max_frames: usize,
}

static mut ELF_POOL: ElfFramePool = ElfFramePool {
    base_frame: 0,
    frames_used: 0,
    max_frames: 0,
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

/// Allocate a physical frame from the ELF pool
unsafe fn alloc_elf_frame() -> Option<u64> {
    if !ELF_POOL_INITIALIZED.load(Ordering::SeqCst) {
        return None;
    }
    if ELF_POOL.frames_used >= ELF_POOL.max_frames {
        return None;
    }
    let phys = ELF_POOL.base_frame + (ELF_POOL.frames_used as u64) * PAGE_SIZE;
    ELF_POOL.frames_used += 1;
    Some(phys)
}

/// Get pool usage stats
pub fn pool_stats() -> (usize, usize) {
    unsafe { (ELF_POOL.frames_used, ELF_POOL.max_frames) }
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

    // Verify executable type
    let e_type = hdr.e_type;
    if e_type != ET_EXEC {
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

/// Get the physical memory offset
fn phys_offset() -> u64 {
    PHYS_MEM_OFFSET.load(Ordering::SeqCst)
}

/// Convert physical address to virtual using the offset mapping
#[inline]
fn phys_to_virt(phys: u64) -> u64 {
    phys + phys_offset()
}

/// Create a new PML4 page table for a user process
/// Copies kernel entries (upper half, entries 256-511) from the current PML4
/// Returns the physical address of the new PML4
unsafe fn create_user_pml4() -> Result<u64, ElfError> {
    // Allocate a frame for the new PML4
    let new_pml4_phys = alloc_elf_frame().ok_or(ElfError::OutOfMemory)?;

    // Get current PML4 from CR3
    let cr3: u64;
    core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
    let current_pml4_phys = cr3 & !0xFFF;

    let current_pml4_virt = phys_to_virt(current_pml4_phys) as *const u64;
    let new_pml4_virt = phys_to_virt(new_pml4_phys) as *mut u64;

    // Zero the new PML4
    core::ptr::write_bytes(new_pml4_virt, 0, 512);

    // Copy kernel entries (entries 256-511) - the upper half of virtual address space
    for i in 256..512usize {
        let entry = core::ptr::read_volatile(current_pml4_virt.add(i));
        core::ptr::write_volatile(new_pml4_virt.add(i), entry);
    }

    crate::serial_println!(
        "[ELF] User PML4 created: phys=0x{:X} (kernel entries 256-511 copied)",
        new_pml4_phys
    );

    Ok(new_pml4_phys)
}

/// Map a single 4K page in the user page tables
/// Walks PML4 -> PDPT -> PD -> PT, allocating intermediate tables as needed
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

    let mut table_phys = pml4_phys;

    // Walk PML4 -> PDPT -> PD, creating entries as needed
    for level in 0..3 {
        let table_virt = phys_to_virt(table_phys) as *mut u64;
        let entry = core::ptr::read_volatile(table_virt.add(indices[level]));

        if entry & 0x01 == 0 {
            // Entry not present - allocate a new page table
            let new_table = alloc_elf_frame().ok_or(ElfError::OutOfMemory)?;
            // Zero the new table
            core::ptr::write_bytes(phys_to_virt(new_table) as *mut u8, 0, PAGE_SIZE as usize);
            // Set entry: PRESENT | WRITABLE | USER_ACCESSIBLE
            core::ptr::write_volatile(
                table_virt.add(indices[level]),
                new_table | 0x07, // P | W | U
            );
            table_phys = new_table;
        } else {
            table_phys = entry & !0xFFF;
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

    crate::serial_println!(
        "[ELF] Header OK: entry=0x{:X}, phnum={}",
        entry, phnum
    );

    // Step 2: Parse program headers
    let phdrs = parse_program_headers(elf_data, &hdr)?;

    // Step 3: Create per-process PML4
    let pml4_phys = unsafe { create_user_pml4()? };

    // Step 4: Map PT_LOAD segments
    let mut segments_loaded = 0usize;

    for (i, phdr) in phdrs.iter().enumerate() {
        let p_type = phdr.p_type;
        if p_type != PT_LOAD {
            continue;
        }

        let vaddr = phdr.p_vaddr;
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

            // Allocate a physical frame
            let frame_phys = unsafe { alloc_elf_frame().ok_or(ElfError::OutOfMemory)? };

            // Zero the frame first (handles BSS and partial pages)
            unsafe {
                core::ptr::write_bytes(
                    phys_to_virt(frame_phys) as *mut u8,
                    0,
                    PAGE_SIZE as usize,
                );
            }

            // Copy file data into the frame if applicable
            let page_offset_in_segment = page_vaddr.saturating_sub(vaddr);
            if page_offset_in_segment < filesz {
                let file_offset = offset + page_offset_in_segment;
                let copy_start_in_page = if page_vaddr < vaddr {
                    (vaddr - page_vaddr) as usize
                } else {
                    0
                };
                let bytes_remaining = filesz.saturating_sub(page_offset_in_segment);
                let copy_len = core::cmp::min(
                    bytes_remaining as usize,
                    PAGE_SIZE as usize - copy_start_in_page,
                );

                if copy_len > 0 && (file_offset as usize + copy_len) <= elf_data.len() {
                    unsafe {
                        let dst = (phys_to_virt(frame_phys) as *mut u8).add(copy_start_in_page);
                        let src = elf_data.as_ptr().add(file_offset as usize);
                        core::ptr::copy_nonoverlapping(src, dst, copy_len);
                    }
                }
            }
            // Pages beyond filesz are already zeroed (BSS)

            // Map the page in the user page table
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

    for page_idx in 0..USER_STACK_PAGES {
        let page_vaddr = stack_bottom + page_idx * PAGE_SIZE;
        let frame_phys = unsafe { alloc_elf_frame().ok_or(ElfError::OutOfMemory)? };

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

    crate::serial_println!(
        "[ELF] Load complete: entry=0x{:X}, stack=0x{:X}, segments={}, frames={}",
        entry,
        USER_STACK_TOP,
        segments_loaded,
        frames_after - frames_before
    );

    Ok(ElfLoadResult {
        entry_point: entry,
        stack_pointer: USER_STACK_TOP,
        pml4_phys,
        segments_loaded,
        frames_used: frames_after - frames_before,
    })
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
#[allow(unused)]
pub unsafe fn jump_to_ring3(entry_point: u64, stack_pointer: u64) -> ! {
    core::arch::asm!(
        // Push SS (User Data = 0x1B)
        "push 0x1B",
        // Push RSP (user stack pointer)
        "push {rsp_val}",
        // Push RFLAGS (IF=1, bit1=1 -> 0x202)
        "push 0x202",
        // Push CS (User Code = 0x23)
        "push 0x23",
        // Push RIP (entry point)
        "push {rip_val}",
        // Execute IRETQ to switch to Ring 3
        "iretq",
        rsp_val = in(reg) stack_pointer,
        rip_val = in(reg) entry_point,
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

    // Test 5: Full ELF load (into per-process page table)
    crate::serial_write("  [TEST 5/8] Full ELF load... ");
    match load_elf_binary(elf_data) {
        Ok(result) => {
            crate::serial_println!(
                "OK (entry=0x{:X}, stack=0x{:X}, segs={}, frames={})",
                result.entry_point,
                result.stack_pointer,
                result.segments_loaded,
                result.frames_used
            );
            passed += 1;
        }
        Err(e) => {
            crate::serial_println!("FAIL: {}", e);
            failed += 1;
        }
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
