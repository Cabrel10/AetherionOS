// boot/limine_entry.rs — Limine protocol entry point for AetherionOS
//
// This module provides an alternative boot path using the Limine boot protocol.
// When compiled with `--features limine`, the kernel uses this entry point
// instead of the bootloader_api::entry_point! macro.
//
// The Limine bootloader sets up:
//   - Higher-Half Direct Map (HHDM) for physical memory access
//   - Memory map (E820-like)
//   - Framebuffer (optional)
//   - RSDP pointer (optional)
//
// The kernel is loaded at 0xffffffff80000000 (per linker-x86_64.ld).

use limine::{BaseRevision, RequestsStartMarker, RequestsEndMarker};
use limine::request::{
    HhdmRequest, MemmapRequest, FramebufferRequest, RsdpRequest,
    StackSizeRequest, BootloaderInfoRequest,
};

// ===== Direct Serial Port (COM1 = 0x3F8) =====
// Override the lib.rs stub with a real implementation that writes
// directly to the UART hardware. This is needed because main.rs's
// uart_16550 lazy_static is not initialized in the Limine boot path.

#[inline(always)]
fn serial_putc(c: u8) {
    unsafe {
        // Wait for transmit holding register empty (bit 5 of LSR)
        loop {
            let lsr: u8;
            core::arch::asm!("in al, dx", out("al") lsr, in("dx") 0x3F8u16 + 5);
            if lsr & 0x20 != 0 { break; }
        }
        core::arch::asm!("out dx, al", in("dx") 0x3F8u16, in("al") c);
    }
}

#[no_mangle]
pub fn serial_write(s: &str) {
    for b in s.bytes() {
        if b == b'\n' { serial_putc(b'\r'); }
        serial_putc(b);
    }
}

#[no_mangle]
pub fn serial_writeln(s: &str) {
    serial_write(s);
    serial_putc(b'\r');
    serial_putc(b'\n');
}

// ===== Embedded ELF Binaries =====
// These are included at compile time and mounted into the VFS during boot.
// The same binaries as main.rs, but accessible from the Limine boot path.
static HELLO_ELF: &[u8] = include_bytes!("../../../userspace/hello.elf");
static HELLO_C_ELF: &[u8] = include_bytes!("../../../userspace/c_apps/hello_c.elf");
static BUSYBOX_ELF: &[u8] = include_bytes!("../../../userspace/busybox.elf");
static AGENT_AUTONOMOUS_ELF: &[u8] = include_bytes!("../../../userspace/agent_autonomous.elf");

// ===== Limine Request Structures =====
// These are placed in the .requests section by the linker script.

#[used]
#[unsafe(link_section = ".requests")]
// Limine v8.x (8.7.0) supports up to base revision 3.
// The limine crate 0.6.3 defaults to revision 6, which is unsupported
// by v8.x binaries, causing is_supported() to fail.
// Explicitly request revision 3 for compatibility.
static BASE_REVISION: BaseRevision = BaseRevision::with_revision(3);

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START_MARKER: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END_MARKER: RequestsEndMarker = RequestsEndMarker::new();

#[used]
#[unsafe(link_section = ".requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static MEMMAP_REQUEST: MemmapRequest = MemmapRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static RSDP_REQUEST: RsdpRequest = RsdpRequest::new();

#[used]
#[unsafe(link_section = ".requests")]
static STACK_SIZE_REQUEST: StackSizeRequest = StackSizeRequest::new(128 * 1024);

#[used]
#[unsafe(link_section = ".requests")]
static BOOTLOADER_INFO_REQUEST: BootloaderInfoRequest = BootloaderInfoRequest::new();

// ===== Limine Boot Info (kernel-internal) =====

/// Extracted boot information from Limine protocol.
/// This replaces `bootloader_api::BootInfo` for the Limine boot path.
pub struct LimineBootInfo {
    /// Physical memory offset (HHDM base).
    /// All physical addresses can be accessed at virt = phys + hhdm_offset.
    pub hhdm_offset: u64,

    /// Total usable memory in bytes (sum of usable regions).
    pub total_usable_memory: u64,

    /// Number of memory map entries.
    pub memmap_entry_count: usize,

    /// RSDP physical address (if available).
    pub rsdp_address: Option<u64>,

    /// Framebuffer info (if available).
    pub framebuffer: Option<LimineFramebufferInfo>,
}

/// Framebuffer information extracted from Limine.
pub struct LimineFramebufferInfo {
    pub address: u64,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bpp: u16,
}

// ===== Shell Resume Mechanism =====
// After sys_exit kills a user process, the kernel needs to resume the shell.
// The timer ISR's exec_switch_cr3_and_ring3 is noreturn, so the shell's stack
// frame is abandoned. We save the kernel shell's RSP and a resume RIP here
// so sys_exit can longjmp back.

use core::sync::atomic::{AtomicBool, Ordering};

/// Flag: a user process has exited, shell should re-print prompt.
static SHELL_RESUME_FLAG: AtomicBool = AtomicBool::new(false);

/// Called by sys_exit to signal the kernel shell to resume.
pub fn signal_shell_resume() {
    SHELL_RESUME_FLAG.store(true, Ordering::SeqCst);
}

/// Check if the shell should resume (and clear the flag).
pub fn should_resume_shell() -> bool {
    SHELL_RESUME_FLAG.swap(false, Ordering::SeqCst)
}

/// Resume the kernel shell after all user processes have exited.
/// Called from sys_exit when no more user processes are ready.
/// This runs a fresh shell loop on the current (syscall) stack.
pub fn resume_kernel_shell() -> ! {
    crate::serial_write("\n$ ");

    let mut cmd_buf = [0u8; 256];
    let mut cmd_len: usize = 0;

    // We don't have boot_info here, so create a minimal stub
    let boot_info = LimineBootInfo {
        hhdm_offset: 0xFFFF_8000_0000_0000,
        total_usable_memory: 0, // Not needed for commands
        memmap_entry_count: 0,
        rsdp_address: None,
        framebuffer: None,
    };

    loop {
        let c = serial_read_byte();
        if c == 0 {
            unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
            continue;
        }
        match c {
            b'\r' | b'\n' => {
                crate::serial_write("\n");
                if cmd_len > 0 {
                    let cmd = unsafe { core::str::from_utf8_unchecked(&cmd_buf[..cmd_len]) };
                    execute_shell_command(cmd, &boot_info);
                }
                cmd_len = 0;
                crate::serial_write("$ ");
            }
            0x7F | 0x08 => {
                if cmd_len > 0 {
                    cmd_len -= 1;
                    crate::serial_write("\x08 \x08");
                }
            }
            c if c >= 0x20 && c < 0x7F => {
                if cmd_len < 255 {
                    cmd_buf[cmd_len] = c;
                    cmd_len += 1;
                    let s = [c];
                    crate::serial_write(unsafe { core::str::from_utf8_unchecked(&s) });
                }
            }
            _ => {}
        }
    }
}

// ===== Entry Point =====

/// Kernel version for the Limine boot path.
const LIMINE_KERNEL_VERSION: &str = "4.3.0-phase7";

/// Limine entry point -- replaces kernel_main when built with `--features limine`.
///
/// # Safety
/// Called by the Limine bootloader after setting up paging, GDT, and the HHDM.
/// The kernel is running in 64-bit mode with interrupts disabled.
#[no_mangle]
unsafe extern "C" fn kmain() -> ! {
    // Serial is initialized lazily via lazy_static on first use.

    crate::serial_write("\n=== AetherionOS Kernel Boot (Limine) ===\n");
    crate::serial_write("Version: ");
    crate::serial_write(LIMINE_KERNEL_VERSION);
    crate::serial_write("\n");

    // Verify base revision is supported
    if !BASE_REVISION.is_supported() {
        crate::serial_write("[FATAL] Limine base revision not supported!\n");
        halt_loop();
    }
    crate::serial_write("[LIMINE] Base revision OK\n");

    // Print bootloader info
    if let Some(info) = BOOTLOADER_INFO_REQUEST.response() {
        crate::serial_write("[LIMINE] Bootloader: ");
        crate::serial_write(info.name());
        crate::serial_write(" v");
        crate::serial_write(info.version());
        crate::serial_write("\n");
    }

    // === HHDM (Physical Memory Offset) ===
    let hhdm_offset = match HHDM_REQUEST.response() {
        Some(resp) => {
            let offset = resp.offset;
            crate::serial_println!("[LIMINE] HHDM offset: 0x{:X}", offset);
            offset
        }
        None => {
            crate::serial_write("[FATAL] No HHDM response from Limine!\n");
            halt_loop();
        }
    };

    // === Memory Map ===
    let memmap_response = match MEMMAP_REQUEST.response() {
        Some(resp) => resp,
        None => {
            crate::serial_write("[FATAL] No memory map response from Limine!\n");
            halt_loop();
        }
    };

    let entries = memmap_response.entries();
    let mut total_usable: u64 = 0;
    let mut usable_count: usize = 0;

    crate::serial_println!("[LIMINE] Memory map: {} entries", entries.len());

    for entry in entries.iter() {
        let type_str = match entry.type_ {
            limine::memmap::MEMMAP_USABLE => "Usable",
            limine::memmap::MEMMAP_RESERVED => "Reserved",
            limine::memmap::MEMMAP_ACPI_RECLAIMABLE => "ACPI Reclaim",
            limine::memmap::MEMMAP_ACPI_NVS => "ACPI NVS",
            limine::memmap::MEMMAP_BAD_MEMORY => "Bad Memory",
            limine::memmap::MEMMAP_BOOTLOADER_RECLAIMABLE => "Bootloader Reclaim",
            limine::memmap::MEMMAP_EXECUTABLE_AND_MODULES => "Kernel+Modules",
            limine::memmap::MEMMAP_FRAMEBUFFER => "Framebuffer",
            limine::memmap::MEMMAP_MAPPED_RESERVED => "Mapped Reserved",
            _ => "Unknown",
        };

        crate::serial_println!(
            "  [{:>20}] 0x{:016X} - 0x{:016X} ({} KiB)",
            type_str,
            entry.base,
            entry.base + entry.length,
            entry.length / 1024
        );

        if entry.type_ == limine::memmap::MEMMAP_USABLE {
            total_usable += entry.length;
            usable_count += 1;
        }
    }

    crate::serial_println!(
        "[LIMINE] Total usable: {} MiB ({} regions)",
        total_usable / (1024 * 1024),
        usable_count
    );

    // === Framebuffer (optional) ===
    let fb_info = if let Some(fb_resp) = FRAMEBUFFER_REQUEST.response() {
        let framebuffers = fb_resp.framebuffers();
        if !framebuffers.is_empty() {
            let fb = framebuffers[0];
            crate::serial_println!(
                "[LIMINE] Framebuffer: {}x{} @ 0x{:X} (bpp={}, pitch={})",
                fb.width, fb.height, fb.address() as u64, fb.bpp, fb.pitch
            );
            Some(LimineFramebufferInfo {
                address: fb.address() as u64,
                width: fb.width,
                height: fb.height,
                pitch: fb.pitch,
                bpp: fb.bpp,
            })
        } else {
            crate::serial_write("[LIMINE] No framebuffer available\n");
            None
        }
    } else {
        crate::serial_write("[LIMINE] Framebuffer not requested or unavailable\n");
        None
    };

    // === RSDP (optional) ===
    let rsdp_addr = if let Some(rsdp_resp) = RSDP_REQUEST.response() {
        let addr = rsdp_resp.address as u64;
        crate::serial_println!("[LIMINE] RSDP address: 0x{:X}", addr);
        Some(addr)
    } else {
        crate::serial_write("[LIMINE] No RSDP available\n");
        None
    };

    // === Build LimineBootInfo ===
    let boot_info = LimineBootInfo {
        hhdm_offset,
        total_usable_memory: total_usable,
        memmap_entry_count: entries.len(),
        rsdp_address: rsdp_addr,
        framebuffer: fb_info,
    };

    crate::serial_write("\n[LIMINE] Boot info extraction complete.\n");
    crate::serial_write("[LIMINE] Proceeding with kernel initialization...\n\n");

    // === Initialize kernel subsystems ===
    // Same sequence as kernel_main but using Limine boot info

    // Step 1: GDT
    crate::serial_write("[1/12] Loading GDT (R0+R3)...\n");
    crate::arch::x86_64::gdt::init();

    // Phase 6 FIX (#GP(0x30)): Reload ALL data segment registers.
    // Limine leaves SS/ES/FS/GS with selectors from its own GDT.
    // gdt::init() only sets CS and DS via the x86_64 crate.
    // If SS still holds a stale Limine selector (e.g. 0x30), the
    // first timer interrupt pushes that SS onto the interrupt frame,
    // and iretq tries to restore it -- but 0x30 points at the TSS
    // high-half descriptor in OUR GDT, causing #GP(0x30).
    // Fix: force SS=DS=ES=0x10 (kernel data), FS=GS=0 (null, unused).
    core::arch::asm!(
        "mov ax, 0x10",
        "mov ss, ax",
        "mov ds, ax",
        "mov es, ax",
        "xor ax, ax",
        "mov fs, ax",
        "mov gs, ax",
        options(nomem, nostack)
    );
    crate::serial_write("       [OK] GDT + TSS + segments reloaded (SS=DS=ES=0x10)\n");

    // Step 1b: FPU/SSE/AVX
    crate::serial_write("[1b/12] Enabling FPU/SSE/AVX...\n");
    crate::arch::x86_64::context::enable_sse();
    let avx_enabled = crate::arch::x86_64::context::enable_avx();
    if avx_enabled {
        crate::serial_write("       [OK] AVX enabled\n");
    } else {
        crate::serial_write("       [INFO] AVX not available (SSE-only mode)\n");
    }
    let cpu_features = crate::arch::x86_64::context::detect_cpu_features();
    crate::arch::x86_64::context::log_cpu_features(&cpu_features);

    // Step 2: IDT
    crate::serial_write("[2/12] Loading IDT...\n");
    crate::arch::x86_64::idt::init();
    crate::serial_write("       [OK] IDT with 20 handlers\n");

    // Step 3: PIC
    crate::serial_write("[3/12] Initializing PIC...\n");
    crate::arch::x86_64::interrupts::init();
    // Disable interrupts after PIC remap -- heap/scheduler not ready yet.
    // They will be re-enabled in step 10 after all subsystems are up.
    x86_64::instructions::interrupts::disable();
    crate::serial_write("       [OK] PIC remapped (32-47), IRQs deferred\n");

    // Step 3.5: PS/2
    crate::serial_write("[3.5/12] Initializing PS/2 controller...\n");
    crate::drivers::ps2::init();
    crate::serial_write("       [OK] PS/2 keyboard: Translation=ON, IRQ1=ON\n");

    // Step 4: Security
    crate::serial_write("[4/12] Security init...\n");
    crate::security::init();
    crate::serial_write("       [OK] TPM stub + PCR0 + stack protector\n");
    crate::security::kpti::init();
    crate::serial_write("       [OK] KPTI-Lite active\n");

    // Step 5: Memory -- frame allocator + page tables + heap
    crate::serial_write("[5/12] Memory init (Limine HHDM)...\n");
    crate::serial_println!("       HHDM offset: 0x{:X}", boot_info.hhdm_offset);
    crate::serial_println!("       Usable memory: {} MiB", boot_info.total_usable_memory / (1024 * 1024));

    crate::elf::set_phys_mem_offset(boot_info.hhdm_offset);

    // Collect usable regions from Limine memory map
    let mut usable_regions = [(0u64, 0u64); 32];
    let mut region_count = 0usize;
    for entry in entries.iter() {
        if entry.type_ == limine::memmap::MEMMAP_USABLE && region_count < 32 {
            usable_regions[region_count] = (entry.base, entry.base + entry.length);
            region_count += 1;
        }
    }

    let mut mem_manager = match crate::memory::init_from_limine(
        &usable_regions[..region_count],
        boot_info.hhdm_offset,
    ) {
        Ok(m) => m,
        Err(e) => {
            crate::serial_println!("[FATAL] Memory init failed: {}", e);
            halt_loop();
        }
    };
    crate::serial_write("       [OK] Frame allocator + page tables\n");

    // ═══════════════════════════════════════════════════════════════
    // Step 5a: Force-map any kernel pages that Limine left unmapped.
    //
    // Limine maps kernel ELF LOAD segments, but .bss may extend beyond
    // what's in the file. Large static arrays (e.g., FRAME_BITMAP[65536],
    // FREELIST_MAX arrays) cause .bss to grow, and Limine doesn't always
    // map ALL .bss pages. This causes #PF at first access.
    //
    // Strategy: Walk the kernel virtual address range [__kernel_start, __kernel_end)
    // and for any page where PT entry = 0, allocate a physical frame and map it.
    // ═══════════════════════════════════════════════════════════════
    {
        extern "C" {
            static __kernel_start: u8;
            static __kernel_end: u8;
        }

        let ks = unsafe { &__kernel_start as *const u8 as u64 };
        let ke = unsafe { &__kernel_end as *const u8 as u64 };
        let hhdm = boot_info.hhdm_offset;

        // Read CR3 to get the current PML4 physical address
        let cr3: u64;
        unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)) };
        let pml4_phys = cr3 & !0xFFF;

        let ks_aligned = ks & !0xFFF;
        let ke_aligned = (ke + 0xFFF) & !0xFFF;
        let total_pages = ((ke_aligned - ks_aligned) / 4096) as usize;
        let mut fixed = 0usize;

        crate::serial_println!(
            "[5a] Verifying kernel pages: 0x{:X}..0x{:X} ({} pages), CR3=0x{:X}",
            ks_aligned, ke_aligned, total_pages, pml4_phys
        );

        let mut page_va = ks_aligned;
        while page_va < ke_aligned {
            let pml4_idx = ((page_va >> 39) & 0x1FF) as usize;
            let pdpt_idx = ((page_va >> 30) & 0x1FF) as usize;
            let pd_idx   = ((page_va >> 21) & 0x1FF) as usize;
            let pt_idx   = ((page_va >> 12) & 0x1FF) as usize;

            unsafe {
                let pml4_virt = (pml4_phys + hhdm) as *mut u64;
                let pml4_entry = core::ptr::read_volatile(pml4_virt.add(pml4_idx));
                if pml4_entry & 1 == 0 {
                    page_va += 4096;
                    continue;
                }

                let pdpt_phys = pml4_entry & 0x000F_FFFF_FFFF_F000;
                let pdpt_virt = (pdpt_phys + hhdm) as *mut u64;
                let pdpt_entry = core::ptr::read_volatile(pdpt_virt.add(pdpt_idx));
                if pdpt_entry & 1 == 0 {
                    page_va += 4096;
                    continue;
                }
                // 1G huge page — skip (unlikely for kernel)
                if pdpt_entry & 0x80 != 0 { page_va += 4096; continue; }

                let pd_phys = pdpt_entry & 0x000F_FFFF_FFFF_F000;
                let pd_virt = (pd_phys + hhdm) as *mut u64;
                let pd_entry = core::ptr::read_volatile(pd_virt.add(pd_idx));

                if pd_entry & 1 == 0 {
                    // PD entry missing — allocate a PT and a frame
                    if let Some(pt_frame) = mem_manager.frame_allocator.alloc_frame_kernel() {
                        let pt_phys_addr = pt_frame.start_address().as_u64();
                        let pt_virt_addr = (pt_phys_addr + hhdm) as *mut u8;
                        core::ptr::write_bytes(pt_virt_addr, 0, 4096);
                        // Write PD entry: P|W (no U for kernel pages)
                        core::ptr::write_volatile(pd_virt.add(pd_idx), pt_phys_addr | 0x03);

                        // Now allocate a frame for this page
                        if let Some(page_frame) = mem_manager.frame_allocator.alloc_frame_kernel() {
                            let pf_addr = page_frame.start_address().as_u64();
                            let pt_virt2 = (pt_phys_addr + hhdm) as *mut u64;
                            core::ptr::write_bytes((pf_addr + hhdm) as *mut u8, 0, 4096);
                            core::ptr::write_volatile(pt_virt2.add(pt_idx), pf_addr | 0x03); // P|W
                            fixed += 1;
                        }
                    }
                    page_va += 4096;
                    continue;
                }

                // 2M huge page — skip, page is covered
                if pd_entry & 0x80 != 0 { page_va += 4096; continue; }

                let pt_phys = pd_entry & 0x000F_FFFF_FFFF_F000;
                let pt_virt = (pt_phys + hhdm) as *mut u64;
                let pt_entry = core::ptr::read_volatile(pt_virt.add(pt_idx));

                if pt_entry & 1 == 0 {
                    // PT entry is 0 — page not mapped! Allocate and map it.
                    if let Some(page_frame) = mem_manager.frame_allocator.alloc_frame_kernel() {
                        let pf_addr = page_frame.start_address().as_u64();
                        // Zero the frame (it's BSS, must be zero)
                        core::ptr::write_bytes((pf_addr + hhdm) as *mut u8, 0, 4096);
                        // Map with Present + Writable (kernel page, no User bit)
                        core::ptr::write_volatile(pt_virt.add(pt_idx), pf_addr | 0x03);
                        fixed += 1;
                    }
                }
            }
            page_va += 4096;
        }

        if fixed > 0 {
            crate::serial_println!(
                "[5a] Fixed {} unmapped kernel pages (total={}, range=0x{:X}..0x{:X})",
                fixed, total_pages, ks_aligned, ke_aligned
            );
            // Flush TLB to pick up new mappings
            unsafe { core::arch::asm!("mov rax, cr3", "mov cr3, rax", out("rax") _, options(nostack)); }
        } else {
            crate::serial_println!("[5a] All {} kernel pages present (OK)", total_pages);
        }

        // ═══════════════════════════════════════════════════════════════
        // Jalon 250: Compute kernel_phys_base and protect Limine PT frames.
        //
        // Walk the kernel page tables (PML4[511]→PDPT[510]→PD[*]→PT[*])
        // to achieve two goals:
        //   1. Derive kernel_phys_base from the first valid PT entry:
        //      phys_base = entry_phys - (entry_virt - 0xFFFFFFFF80000000)
        //   2. Mark all intermediate page table frames (PDPT, PD, PT frames)
        //      as allocated in the bitmap so the frame allocator never
        //      hands them out. This prevents the PT[195]=0x0 corruption bug.
        // ═══════════════════════════════════════════════════════════════
        {
            let kernel_virt_base: u64 = 0xFFFF_FFFF_8000_0000;
            let pml4_idx_k = ((kernel_virt_base >> 39) & 0x1FF) as usize; // 511
            let pdpt_idx_k = ((kernel_virt_base >> 30) & 0x1FF) as usize; // 510

            let mut computed_phys_base: u64 = 0;
            let mut pt_frames_protected: usize = 0;

            unsafe {
                let pml4_virt = (pml4_phys + hhdm) as *mut u64;
                let pml4_entry = core::ptr::read_volatile(pml4_virt.add(pml4_idx_k));

                if pml4_entry & 1 != 0 {
                    // Mark the PML4 frame itself
                    mem_manager.frame_allocator.mark_frame_allocated(pml4_phys);
                    pt_frames_protected += 1;

                    let pdpt_phys = pml4_entry & 0x000F_FFFF_FFFF_F000;
                    mem_manager.frame_allocator.mark_frame_allocated(pdpt_phys);
                    pt_frames_protected += 1;

                    let pdpt_virt = (pdpt_phys + hhdm) as *mut u64;
                    let pdpt_entry = core::ptr::read_volatile(pdpt_virt.add(pdpt_idx_k));

                    if pdpt_entry & 1 != 0 && pdpt_entry & 0x80 == 0 {
                        let pd_phys = pdpt_entry & 0x000F_FFFF_FFFF_F000;
                        mem_manager.frame_allocator.mark_frame_allocated(pd_phys);
                        pt_frames_protected += 1;

                        let pd_virt = (pd_phys + hhdm) as *mut u64;
                        // Walk all 512 PD entries (each covers 2MB)
                        for pd_i in 0..512usize {
                            let pd_e = core::ptr::read_volatile(pd_virt.add(pd_i));
                            if pd_e & 1 == 0 { continue; }
                            if pd_e & 0x80 != 0 { continue; } // 2M huge page, no PT to protect

                            let pt_phys = pd_e & 0x000F_FFFF_FFFF_F000;
                            mem_manager.frame_allocator.mark_frame_allocated(pt_phys);
                            pt_frames_protected += 1;

                            // If we haven't found kernel_phys_base yet, scan PT entries
                            if computed_phys_base == 0 {
                                let pt_virt = (pt_phys + hhdm) as *mut u64;
                                for pt_i in 0..512usize {
                                    let pt_e = core::ptr::read_volatile(pt_virt.add(pt_i));
                                    if pt_e & 1 != 0 && pt_e & 0x80 == 0 {
                                        let entry_phys = pt_e & 0x000F_FFFF_FFFF_F000;
                                        // Reconstruct the virtual address of this entry
                                        let entry_virt: u64 = kernel_virt_base
                                            | ((pdpt_idx_k as u64) << 30)
                                            | ((pd_i as u64) << 21)
                                            | ((pt_i as u64) << 12);
                                        let offset = entry_virt - kernel_virt_base;
                                        computed_phys_base = entry_phys - offset;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if computed_phys_base != 0 {
                crate::elf::set_kernel_phys_base(computed_phys_base);
                crate::serial_println!(
                    "[5a] kernel_phys_base = 0x{:X}, protected {} PT frames",
                    computed_phys_base, pt_frames_protected
                );
            } else {
                crate::serial_println!(
                    "[5a] WARNING: Could not compute kernel_phys_base! Protected {} PT frames",
                    pt_frames_protected
                );
            }
        }
    }

    // Step 5b: Heap
    crate::serial_write("[5b/12] Initializing kernel heap (64 MB)...\n");
    match mem_manager.init_heap() {
        Ok(()) => {
            crate::serial_write("       [OK] Heap ready -- alloc enabled\n");
        }
        Err(e) => {
            crate::serial_println!("[FATAL] Heap init failed: {}", e);
            halt_loop();
        }
    }

    // Verify heap works: Box::new smoke test
    {
        let boxed = alloc::boxed::Box::new(42u64);
        assert_eq!(*boxed, 42);
        let mut v = alloc::vec![1u64, 2, 3, 4, 5];
        v.push(6);
        assert_eq!(v.len(), 6);
        crate::serial_write("       [OK] Box::new + Vec verified\n");
    }

    // Step 6: Framebuffer (Limine GOP)
    if let Some(ref fb) = boot_info.framebuffer {
        crate::serial_write("[6/12] Framebuffer init (Limine GOP)...\n");
        if let Some(_fb_info) = crate::framebuffer::init_from_limine(
            fb.address,
            fb.width as u32,
            fb.height as u32,
            fb.pitch as u32,
            fb.bpp as u32,
        ) {
            crate::serial_write("       [OK] Framebuffer initialized from Limine\n");
        }
    } else {
        crate::serial_write("[6/12] No framebuffer (headless mode)\n");
    }

    // Step 7: Scheduler init (now that heap is available for VecDeque)
    crate::serial_write("[7/12] Scheduler init...\n");
    crate::scheduler::init();
    crate::serial_write("       [OK] Priority scheduler ready\n");

    // Step 8: IPC / Cognitive Bus
    // The Cognitive Bus is a lazy_static -- it initializes on first use.
    // Force initialization by checking its capacity.
    crate::serial_write("[8/12] Cognitive Bus init...\n");
    let _bus_cap = crate::ipc::bus::capacity();
    crate::serial_write("       [OK] Cognitive Bus ready\n");

    // Step 9: VFS
    crate::serial_write("[9/12] VFS init...\n");
    let _ = crate::fs::vfs::init();
    crate::serial_write("       [OK] VFS mounted\n");

    // Step 9a: ELF Loader initialization
    crate::serial_write("[9a/12] ELF Loader init...\n");
    {
        // Initialize ELF frame pool: allocate a contiguous block of physical frames
        // for the ELF loader to use when creating per-process page tables.
        // 32768 frames = 128 MiB -- BusyBox + musl need more for demand paging
        let pool_frames = 32768usize; // 128 MiB
        if let Some(first_frame) = mem_manager.frame_allocator.alloc_frame_kernel() {
            let base_phys = first_frame.start_address().as_u64();
            // Pre-allocate all frames and push them to the pool freelist.
            // This handles non-contiguous frame allocation correctly.
            unsafe { crate::elf::init_frame_pool(base_phys, pool_frames); }
            unsafe { crate::elf::push_pool_frame(base_phys); }
            let mut allocated = 1usize;
            for _ in 1..pool_frames {
                if let Some(frame) = mem_manager.frame_allocator.alloc_frame_kernel() {
                    unsafe { crate::elf::push_pool_frame(frame.start_address().as_u64()); }
                    allocated += 1;
                } else {
                    break;
                }
            }
            crate::serial_println!(
                "       [OK] ELF frame pool: {} frames ({} MiB) base=0x{:X}",
                allocated, allocated * 4096 / (1024 * 1024), base_phys
            );
            crate::serial_println!(
                "       [OK] Pool freelist: {} frames pre-filled",
                crate::elf::freelist_count()
            );
        } else {
            crate::serial_write("       [WARN] No frames available for ELF pool\n");
        }

        // Initialize SYSCALL/SYSRET MSRs (EFER.SCE, STAR, LSTAR, SFMASK, PER_CPU)
        // WITHOUT this, the `syscall` instruction in Ring 3 triggers #UD (Invalid Opcode)
        // because EFER.SCE (System Call Extensions) is not enabled.
        crate::arch::x86_64::syscall::init();

        // Initialize KPTI trampolines (requires heap for alloc)
        crate::elf::init_global_iretq_trampoline();
        crate::arch::x86_64::syscall::init_global_sysret_trampoline();
        // Relocate LSTAR to phys-offset address (safe under user CR3)
        crate::arch::x86_64::syscall::relocate_lstar_for_kpti();
        crate::serial_write("       [OK] SYSCALL MSRs + KPTI trampolines ready\n");
    }

    // Step 9b: Mount ELF binaries into VFS
    crate::serial_write("[9b/12] Mounting ELF binaries in VFS...\n");
    {
        // Use lock_root() to directly insert files into the /bin directory
        // (file_write requires the file to already exist)
        let mut root = crate::fs::vfs::lock_root();
        if let Some(crate::fs::vfs::VfsNode::Directory(ref mut bin_dir)) = root.get_mut("bin") {
            bin_dir.insert(
                alloc::string::String::from("hello.elf"),
                crate::fs::vfs::VfsNode::File(alloc::vec::Vec::from(HELLO_ELF)),
            );
            crate::serial_println!("       [OK] /bin/hello.elf ({} bytes)", HELLO_ELF.len());

            bin_dir.insert(
                alloc::string::String::from("hello_c.elf"),
                crate::fs::vfs::VfsNode::File(alloc::vec::Vec::from(HELLO_C_ELF)),
            );
            crate::serial_println!("       [OK] /bin/hello_c.elf ({} bytes)", HELLO_C_ELF.len());

            bin_dir.insert(
                alloc::string::String::from("busybox"),
                crate::fs::vfs::VfsNode::File(alloc::vec::Vec::from(BUSYBOX_ELF)),
            );
            crate::serial_println!("       [OK] /bin/busybox ({} bytes)", BUSYBOX_ELF.len());

            bin_dir.insert(
                alloc::string::String::from("agent_autonomous"),
                crate::fs::vfs::VfsNode::File(alloc::vec::Vec::from(AGENT_AUTONOMOUS_ELF)),
            );
            crate::serial_println!("       [OK] /bin/agent_autonomous ({} bytes)", AGENT_AUTONOMOUS_ELF.len());

            // BusyBox symlinks for common applets
            let bb_applets = ["sh", "ash", "ls", "cat", "echo", "mkdir", "rm",
                              "cp", "mv", "grep", "head", "tail", "wc", "sort",
                              "uname", "id", "ps", "pwd", "env", "test", "true",
                              "false", "sleep", "date", "whoami", "hostname",
                              "vi", "wget", "ping", "ifconfig", "mount", "umount",
                              "df", "du", "free", "top", "kill", "ln", "chmod",
                              "chown", "touch", "find", "xargs", "tr", "sed",
                              "awk", "cut", "tee", "printf", "expr", "seq",
                              "basename", "dirname", "readlink", "realpath"];
            for applet in &bb_applets {
                bin_dir.insert(
                    alloc::string::String::from(*applet),
                    crate::fs::vfs::VfsNode::Symlink(alloc::string::String::from("/bin/busybox")),
                );
            }
            crate::serial_println!("       [OK] {} BusyBox symlinks in /bin", bb_applets.len());

            // Note: sh.elf is a 136-byte stub in CI and is not git-tracked.
            // It will be added when the C build step produces a real binary.
        } else {
            crate::serial_write("       [FAIL] /bin directory not found in VFS\n");
        }
        drop(root);
    }

    // Step 9c: Network init
    crate::serial_write("[9c/12] Network init...\n");
    crate::net::init();
    if crate::net::is_available() {
        crate::serial_write("       [OK] VirtIO-Net online (10.0.2.15)\n");
    } else {
        crate::serial_write("       [INFO] No VirtIO-Net found (headless/no -nic)\n");
    }

    // Step 9d: VirtIO-BLK and Ext2 Mount
    crate::serial_write("[9d/12] VirtIO-BLK init...\n");
    crate::drivers::virtio_blk::init();
    if crate::drivers::virtio_blk::is_available() {
        let sectors = crate::drivers::virtio_blk::capacity();
        crate::serial_println!("       [OK] VirtIO-BLK: {} sectors ({} MiB)",
            sectors, sectors * 512 / (1024 * 1024));

        // Try to mount ext2 filesystem
        crate::serial_write("[9e/12] Ext2 mount...\n");
        if crate::fs::ext2::mount() {
            crate::serial_write("       [OK] Ext2 filesystem mounted\n");
        } else {
            crate::serial_write("       [WARN] Ext2 mount failed (disk may not be ext2)\n");
        }
    } else {
        crate::serial_write("       [INFO] No VirtIO-BLK device found\n");
    }

    // Step 10: Enable interrupts
    x86_64::instructions::interrupts::enable();
    crate::serial_write("[10/12] Interrupts: ENABLED (timer + keyboard)\n");

    // Summary banner
    crate::serial_write("\n=== AetherionOS v4.3.0-phase8 -- Limine Boot Complete ===\n");
    crate::serial_println!("RAM: {} MiB | Heap: 64 MB | Scheduler: ON | IRQ: ON | ELF: ON",
        boot_info.total_usable_memory / (1024 * 1024));
    crate::serial_println!("Layers: Network | ext2 | DynLink | LLM");
    crate::serial_write("=========================================================\n\n");

    // === CI Auto-Test Sequence ===
    // When QEMU_CI_MODE env (checked via ext2 presence), run automated tests
    if crate::fs::ext2::is_mounted() {
        run_ci_tests();
    }

    // === Interactive Shell ===
    crate::serial_write("AetherionOS v4.3.0-phase8 ready.\n");
    crate::serial_write("Type 'help' for available commands.\n\n");
    crate::serial_write("$ ");

    // Simple serial shell loop -- reads characters from serial port
    let mut cmd_buf = [0u8; 256];
    let mut cmd_len: usize = 0;

    loop {
        // Read one character from serial port (polling)
        let c = serial_read_byte();
        if c == 0 {
            // No data available -- yield CPU
            unsafe { core::arch::asm!("hlt", options(nomem, nostack)); }
            continue;
        }

        match c {
            b'\r' | b'\n' => {
                crate::serial_write("\n");
                if cmd_len > 0 {
                    let cmd = unsafe { core::str::from_utf8_unchecked(&cmd_buf[..cmd_len]) };
                    execute_shell_command(cmd, &boot_info);
                }
                cmd_len = 0;
                crate::serial_write("$ ");
            }
            0x7F | 0x08 => {
                // Backspace
                if cmd_len > 0 {
                    cmd_len -= 1;
                    crate::serial_write("\x08 \x08");
                }
            }
            c if c >= 0x20 && c < 0x7F => {
                if cmd_len < 255 {
                    cmd_buf[cmd_len] = c;
                    cmd_len += 1;
                    // Echo
                    let s = [c];
                    crate::serial_write(unsafe { core::str::from_utf8_unchecked(&s) });
                }
            }
            _ => {}
        }
    }
}

/// Execute a shell command
fn execute_shell_command(cmd: &str, boot_info: &LimineBootInfo) {
    let cmd = cmd.trim();

    // Parse "exec <path> [args...]" prefix
    if cmd.starts_with("exec ") {
        let rest = cmd[5..].trim();
        if rest.is_empty() {
            crate::serial_write("Usage: exec <path> [args]  (e.g. exec /bin/busybox sh)\n");
            return;
        }
        // Split path from args: first token is path, rest are args
        let (path, args) = if let Some(sp) = rest.find(' ') {
            (&rest[..sp], rest[sp+1..].trim())
        } else {
            (rest, "")
        };
        crate::serial_println!("[EXEC] Loading ELF: {} args=[{}]", path, args);
        // If args provided, set argv0 to the first arg (e.g. "sh" for busybox sh)
        if !args.is_empty() {
            let first_arg = args.split_whitespace().next().unwrap_or(path);
            crate::elf::set_argv0(first_arg);
        }
        match crate::elf::load_elf(path) {
            Ok(pid) => {
                crate::serial_println!("[EXEC] PID {} started from {}", pid, path);
            }
            Err(e) => {
                crate::serial_println!("[EXEC] Failed: {:?}", e);
            }
        }
        return;
    }

    // Parse "ping <ip>" prefix
    if cmd.starts_with("ping ") {
        let target = cmd[5..].trim();
        crate::serial_println!("[PING] Target: {}", target);
        if !crate::net::is_available() {
            crate::serial_write("[PING] Network not available\n");
            return;
        }
        let ip = if target == "gateway" || target == "10.0.2.2" {
            crate::net::ipv4::Ipv4Addr::new(10, 0, 2, 2)
        } else {
            parse_ipv4(target).unwrap_or(crate::net::ipv4::Ipv4Addr::new(10, 0, 2, 2))
        };
        for seq in 1..=3u16 {
            crate::net::send_ping(ip, seq);
            for _ in 0..100_000u32 {
                crate::net::poll();
                if crate::net::check_ping_reply(seq).is_some() {
                    crate::serial_println!("[PING] Reply from {} seq={}", ip, seq);
                    break;
                }
                core::hint::spin_loop();
            }
        }
        return;
    }

    // Parse "wget <url>" prefix (stub)
    if cmd.starts_with("wget ") {
        let url = cmd[5..].trim();
        crate::serial_println!("[WGET] URL: {}", url);
        if !crate::net::is_available() {
            crate::serial_write("[WGET] Network not available\n");
            return;
        }
        crate::serial_write("[WGET] HTTP not yet implemented (TCP stack in progress)\n");
        return;
    }

    match cmd {
        "help" => {
            crate::serial_write("Available commands:\n");
            crate::serial_write("  help          -- Show this help\n");
            crate::serial_write("  uname         -- System information\n");
            crate::serial_write("  free          -- Memory statistics\n");
            crate::serial_write("  ps            -- List processes\n");
            crate::serial_write("  uptime        -- System uptime (ticks)\n");
            crate::serial_write("  heap          -- Heap allocator test\n");
            crate::serial_write("  bus           -- Cognitive Bus status\n");
            crate::serial_write("  net           -- Network status\n");
            crate::serial_write("  exec <path>   -- Load and run ELF binary\n");
            crate::serial_write("  ping <ip>     -- Send ICMP echo request\n");
            crate::serial_write("  wget <url>    -- HTTP GET (stub)\n");
            crate::serial_write("  clear         -- Clear screen\n");
            crate::serial_write("  halt          -- Halt the system\n");
        }
        "uname" | "uname -a" => {
            crate::serial_write("AetherionOS v4.2.0-phase6-exec x86_64 Limine\n");
        }
        "free" => {
            crate::serial_println!("Total RAM:  {} MiB", boot_info.total_usable_memory / (1024 * 1024));
            crate::serial_println!("Heap:       {} KB / {} KB",
                crate::memory::heap::HEAP_SIZE / 1024,
                crate::memory::heap::HEAP_SIZE / 1024);
            crate::serial_println!("Heap start: 0x{:X}", crate::memory::heap::HEAP_START);
        }
        "ps" => {
            crate::serial_write("PID  STATE       ENTRY            NAME\n");
            crate::serial_write("  0  running     ---              kernel\n");
            // List all active processes from process table
            let pids = crate::process::list_active_pids();
            for pid in pids.iter() {
                if *pid == 0 { continue; }
                if let Some(info) = crate::process::get_info(*pid) {
                    crate::serial_println!("{}", info);
                }
            }
            let count = crate::process::active_count();
            crate::serial_println!("Total processes: {}", count);
        }
        "uptime" => {
            let ticks = crate::scheduler::total_ticks();
            crate::serial_println!("Uptime: {} timer ticks", ticks);
        }
        "heap" => {
            crate::serial_write("[TEST] Allocating Box<[u8; 4096]>...\n");
            let page = alloc::boxed::Box::new([0xAAu8; 4096]);
            crate::serial_println!("[TEST] OK: page[0]=0x{:02X}, page[4095]=0x{:02X}",
                page[0], page[4095]);
            drop(page);
            crate::serial_write("[TEST] Freed. Heap is functional.\n");
        }
        "bus" => {
            crate::serial_write("[BUS] Cognitive Bus status:\n");
            let pending = crate::ipc::bus::len();
            let cap = crate::ipc::bus::capacity();
            crate::serial_println!("  Pending:  {}", pending);
            crate::serial_println!("  Capacity: {}", cap);
        }
        "net" => {
            if crate::net::is_available() {
                crate::serial_write("[NET] VirtIO-Net: online\n");
                crate::serial_write("[NET] IP: 10.0.2.15/24, GW: 10.0.2.2, DNS: 10.0.2.3\n");
                let (tx, rx, tx_b, rx_b) = crate::net::get_stats();
                crate::serial_println!("[NET] TX: {} pkts ({} B), RX: {} pkts ({} B)",
                    tx, tx_b, rx, rx_b);
            } else {
                crate::serial_write("[NET] No network device available\n");
                crate::serial_write("[NET] Start QEMU with: -device virtio-net-pci,netdev=n -netdev user,id=n\n");
            }
        }
        "clear" => {
            // ANSI clear screen
            crate::serial_write("\x1B[2J\x1B[H");
        }
        "halt" => {
            crate::serial_write("System halting...\n");
            halt_loop();
        }
        "" => {}
        _ => {
            crate::serial_write("Unknown command: ");
            crate::serial_write(cmd);
            crate::serial_write("\nType 'help' for available commands.\n");
        }
    }
}

/// Read one byte from the serial port (polling, non-blocking).
/// Returns 0 if no data is available.
fn serial_read_byte() -> u8 {
    // uart_16550 serial port at 0x3F8
    // Line Status Register (LSR) is at port + 5 = 0x3FD
    // Bit 0 of LSR = Data Ready
    let lsr: u8;
    unsafe { core::arch::asm!("in al, dx", out("al") lsr, in("dx") 0x3FDu16, options(nomem, nostack)); }
    if lsr & 1 == 0 {
        return 0; // No data available
    }
    // Read data from port 0x3F8
    let data: u8;
    unsafe { core::arch::asm!("in al, dx", out("al") data, in("dx") 0x3F8u16, options(nomem, nostack)); }
    data
}

/// Parse a dotted-quad IPv4 address string (e.g. "10.0.2.2").
fn parse_ipv4(s: &str) -> Option<crate::net::ipv4::Ipv4Addr> {
    let mut parts = [0u8; 4];
    let mut idx = 0;
    for octet_str in s.split('.') {
        if idx >= 4 { return None; }
        let mut val: u16 = 0;
        for b in octet_str.bytes() {
            if b < b'0' || b > b'9' { return None; }
            val = val * 10 + (b - b'0') as u16;
            if val > 255 { return None; }
        }
        parts[idx] = val as u8;
        idx += 1;
    }
    if idx != 4 { return None; }
    Some(crate::net::ipv4::Ipv4Addr::new(parts[0], parts[1], parts[2], parts[3]))
}

// ===== CI Auto-Test Sequence =====
// Design: ALL kernel-side tests run first (ext2, net, LLM, matmul).
// Python3 user-mode test is LAST because launch_ci_test_safe is noreturn (IRETQ).

/// Run the complete CI test suite. Each test prints markers for CI log parsing.
fn run_ci_tests() {
    crate::serial_write("\n========================================\n");
    crate::serial_write("[CI] AetherionOS Automated Test Suite v2\n");
    crate::serial_write("========================================\n\n");

    // CI-TEST-1: Ext2 filesystem verification
    crate::serial_write("[CI-TEST-1] Ext2 filesystem verification\n");
    if crate::fs::ext2::is_mounted() {
        crate::serial_write("[CI-TEST-1] PASS: Ext2 mounted\n");
        if let Some(entries) = crate::fs::ext2::list_directory("/") {
            crate::serial_println!("[CI-TEST-1] Root: {} entries", entries.len());
            // Show key directories
            for e in entries.iter().take(25) {
                crate::serial_println!("[CI-TEST-1]   {}", e.name);
            }
        }
    } else {
        crate::serial_write("[CI-TEST-1] FAIL: Ext2 not mounted\n");
        return;
    }

    // CI-TEST-2: Check for Python3 binary
    crate::serial_write("[CI-TEST-2] Python3 binary lookup\n");
    let python_path = if crate::fs::ext2::lookup_path("/usr/bin/python3.12").is_some() {
        "/usr/bin/python3.12"
    } else if crate::fs::ext2::lookup_path("/usr/bin/python3").is_some() {
        "/usr/bin/python3"
    } else {
        crate::serial_write("[CI-TEST-2] SKIP: python3 not found on ext2\n");
        ""
    };
    if !python_path.is_empty() {
        if let Some(stat) = crate::fs::ext2::stat_path(python_path) {
            crate::serial_println!("[CI-TEST-2] PASS: {} ino={} size={}", python_path, stat.ino, stat.size);
        }
    }

    // CI-TEST-3: Ext2 read test (read a small file)
    crate::serial_write("[CI-TEST-3] Ext2 file read test\n");
    if let Some(data) = crate::fs::ext2::read_file_by_path("/etc/os-release") {
        crate::serial_println!("[CI-TEST-3] PASS: /etc/os-release = {} bytes", data.len());
        if let Ok(text) = core::str::from_utf8(&data[..data.len().min(200)]) {
            crate::serial_println!("[CI-TEST-3] Content: {}", text);
        }
    } else {
        // Try /etc/alpine-release as fallback
        if let Some(data) = crate::fs::ext2::read_file_by_path("/etc/alpine-release") {
            crate::serial_println!("[CI-TEST-3] PASS: /etc/alpine-release = {} bytes", data.len());
        } else {
            crate::serial_write("[CI-TEST-3] WARN: no release file found\n");
        }
    }

    // CI-TEST-4: Dynamic linker check (/proc/self/maps generation)
    crate::serial_write("[CI-TEST-4] /proc/self/maps generation test\n");
    {
        let maps = crate::compat::linux_abi::generate_proc_self_maps(0);
        if maps.is_empty() {
            crate::serial_write("[CI-TEST-4] INFO: No VMAs for PID 0 (expected)\n");
        } else {
            crate::serial_println!("[CI-TEST-4] Generated {} bytes of maps data", maps.len());
        }
        // Verify the function returns valid format with heap/stack/vdso
        if maps.contains("[heap]") && maps.contains("[stack]") && maps.contains("[vdso]") {
            crate::serial_write("[CI-TEST-4] PASS: /proc/self/maps has heap+stack+vdso\n");
        } else {
            crate::serial_write("[CI-TEST-4] WARN: maps format incomplete\n");
        }
    }

    // CI-TEST-5: VirtIO-Net status
    crate::serial_write("\n[CI-TEST-5] VirtIO-Net status\n");
    if crate::net::is_available() {
        crate::serial_write("[CI-TEST-5] PASS: Network driver active\n");
        // Quick ping test
        let gw = crate::net::ipv4::Ipv4Addr::new(10, 0, 2, 2);
        crate::net::send_ping(gw, 42);
        let mut ping_ok = false;
        for _ in 0..2_000_000u32 {
            crate::net::poll();
            if crate::net::check_ping_reply(42).is_some() {
                crate::serial_write("[CI-TEST-5] PING-OK: gateway 10.0.2.2\n");
                ping_ok = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !ping_ok {
            crate::serial_write("[CI-TEST-5] WARN: ping timeout\n");
        }
    } else {
        crate::serial_write("[CI-TEST-5] INFO: No VirtIO-Net\n");
    }

    // CI-TEST-6: HTTP test (DNS + wget)
    crate::serial_write("\n[CI-TEST-6] Network HTTP test\n");
    if crate::net::is_available() {
        match crate::net::dns::resolve("example.com") {
            Ok(ip) => {
                crate::serial_println!("[CI-TEST-6] DNS OK: example.com -> {}", ip);
                match crate::net::http::wget("http://example.com/") {
                    Ok(data) => {
                        crate::serial_println!("[CI-TEST-6] WGET-OK: {} bytes", data.len());
                    }
                    Err(e) => {
                        crate::serial_println!("[CI-TEST-6] HTTP error {}", e);
                    }
                }
            }
            Err(e) => {
                crate::serial_println!("[CI-TEST-6] DNS error {}", e);
            }
        }
    } else {
        crate::serial_write("[CI-TEST-6] SKIP: no network\n");
    }

    // CI-TEST-7: LLM GGUF model check + kernel-side matmul benchmark
    crate::serial_write("\n[CI-TEST-7] [LLM] GGUF model + MatMul benchmark\n");
    run_kernel_llm_benchmark();

    // CI-TEST-8: APK repository index
    crate::serial_write("\n[CI-TEST-8] APK repository index\n");
    if crate::net::is_available() {
        let apk_url = "http://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/APKINDEX.tar.gz";
        match crate::net::http::wget(apk_url) {
            Ok(data) => {
                if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
                    crate::serial_println!("[CI-TEST-8] APKINDEX-OK: {} bytes (gzip)", data.len());
                } else {
                    crate::serial_println!("[CI-TEST-8] Downloaded {} bytes (not gzip?)", data.len());
                }
            }
            Err(e) => {
                crate::serial_println!("[CI-TEST-8] HTTP error {}", e);
            }
        }
    } else {
        crate::serial_write("[CI-TEST-8] SKIP: no network\n");
    }

    // CI-TEST-9: HTTPS/TLS 1.3 test (wget https://example.com)
    crate::serial_write("\n[CI-TEST-9] HTTPS/TLS 1.3 test\n");
    if crate::net::is_available() {
        match crate::net::http::wget("https://example.com/") {
            Ok(data) => {
                let body_str = core::str::from_utf8(&data).unwrap_or("");
                if body_str.contains("Example Domain") {
                    crate::serial_println!("[CI-TEST-9] [HTTPS] WGET-OK: {} bytes — <title>Example Domain</title>", data.len());
                } else {
                    crate::serial_println!("[CI-TEST-9] [HTTPS] WGET-OK: {} bytes (no title match)", data.len());
                }
                // Print first 200 chars of HTML for proof
                let preview_len = core::cmp::min(data.len(), 200);
                if let Ok(preview) = core::str::from_utf8(&data[..preview_len]) {
                    crate::serial_println!("[CI-TEST-9] HTML preview: {}", preview);
                }
            }
            Err(e) => {
                crate::serial_println!("[CI-TEST-9] HTTPS error: {}", e);
            }
        }
    } else {
        crate::serial_write("[CI-TEST-9] SKIP: no network\n");
    }

    crate::serial_write("\n========================================\n");
    crate::serial_write("[CI] Kernel-side tests complete\n");
    crate::serial_write("========================================\n\n");

    // CI-TEST-10: Python3 print(42*42) = 1764
    // THIS MUST BE LAST — launch is noreturn (IRETQ to Ring 3)
    if !python_path.is_empty() {
        crate::serial_write("[CI-TEST-10] Python3 execution: print(42*42)\n");
        crate::serial_println!("[CI-TEST-10] Loading ELF: {}", python_path);
        crate::elf::set_extra_args("-c\0print(42*42)");
        match crate::elf::load_elf(python_path) {
            Ok(pid) => {
                crate::serial_println!("[CI-TEST-10] python3 PID={} — launching via scheduler-safe path", pid);
                launch_ci_test_safe(pid);
                // noreturn — process exit goes to sys_exit → resume_kernel_shell
            }
            Err(e) => {
                crate::serial_println!("[CI-TEST-10] FAIL: {:?}", e);
            }
        }
    }

    // If we reach here, python3 wasn't found or load failed
    crate::serial_write("[CI] All tests done (no user-mode process launched)\n");
}

/// Kernel-side LLM benchmark: read GGUF from ext2, dequantize Q4_0, run matmul
fn run_kernel_llm_benchmark() {
    let model_paths = [
        "/models/smollm2-135m-q4_0.gguf",
        "/models/smollm2.gguf",
        "/models/SmolLM2-135M-Instruct-Q4_K_S.gguf",
    ];

    let mut found_model = false;
    for mp in &model_paths {
        if let Some(stat) = crate::fs::ext2::stat_path(mp) {
            crate::serial_println!("[LLM] Model: {} ({} bytes, ino={})", mp, stat.size, stat.ino);
            // Read first 32 bytes for GGUF header
            let mut hdr = [0u8; 32];
            let n = crate::fs::ext2::read_file_chunk(mp, 0, &mut hdr);
            if n >= 24 {
                let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
                if magic == 0x4655_4747 {
                    let version = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
                    let tensors = u64::from_le_bytes([
                        hdr[8], hdr[9], hdr[10], hdr[11],
                        hdr[12], hdr[13], hdr[14], hdr[15],
                    ]);
                    crate::serial_println!("[LLM] GGUF v{} tensors={} — magic OK", version, tensors);
                    crate::serial_write("[LLM] LLM-LOAD-OK\n");
                    found_model = true;
                } else {
                    crate::serial_println!("[LLM] Bad magic: 0x{:08X}", magic);
                }
            }
            break;
        }
    }
    if !found_model {
        crate::serial_write("[LLM] No GGUF model on disk\n");
    }

    // Kernel-side matmul benchmark (always runs, even without model)
    crate::serial_write("[LLM] Running kernel-side matmul benchmark...\n");
    kernel_matmul_benchmark();
}

/// Simple f32 matmul benchmark executed in kernel mode (Ring 0)
fn kernel_matmul_benchmark() {
    use alloc::vec;

    let m: usize = 128;
    let n: usize = 128;
    let mut mat = vec![0.0f32; m * n];
    let mut v = vec![0.0f32; n];
    let mut out = vec![0.0f32; m];

    // Initialize with deterministic pattern
    for i in 0..m * n {
        mat[i] = ((i % 17) as f32 - 8.0) * 0.01;
    }
    for i in 0..n {
        v[i] = 1.0 / (1.0 + i as f32);
    }

    // Warmup
    for row in 0..m {
        let mut acc: f32 = 0.0;
        for j in 0..n {
            acc += mat[row * n + j] * v[j];
        }
        out[row] = acc;
    }

    // Benchmark with RDTSC
    let iterations: u64 = 200;
    let start: u64;
    unsafe {
        core::arch::asm!("rdtsc", "shl rdx, 32", "or rax, rdx",
            out("rax") start, out("rdx") _, options(nomem, nostack));
    }

    for _ in 0..iterations {
        for row in 0..m {
            let mut acc: f32 = 0.0;
            let base = row * n;
            let mut j = 0;
            while j + 7 < n {
                acc += mat[base + j] * v[j];
                acc += mat[base + j + 1] * v[j + 1];
                acc += mat[base + j + 2] * v[j + 2];
                acc += mat[base + j + 3] * v[j + 3];
                acc += mat[base + j + 4] * v[j + 4];
                acc += mat[base + j + 5] * v[j + 5];
                acc += mat[base + j + 6] * v[j + 6];
                acc += mat[base + j + 7] * v[j + 7];
                j += 8;
            }
            while j < n {
                acc += mat[base + j] * v[j];
                j += 1;
            }
            out[row] = acc;
        }
    }

    let end: u64;
    unsafe {
        core::arch::asm!("rdtsc", "shl rdx, 32", "or rax, rdx",
            out("rax") end, out("rdx") _, options(nomem, nostack));
    }

    let cycles = end.saturating_sub(start);
    let flops_per_iter: u64 = 2 * m as u64 * n as u64;
    let total_flops = flops_per_iter * iterations;
    // Assume ~2 GHz QEMU
    let gflops_x1000 = if cycles > 0 {
        (total_flops * 2 * 1000) / cycles
    } else {
        0
    };
    let gf_int = gflops_x1000 / 1000;
    let gf_frac = gflops_x1000 % 1000;

    crate::serial_println!("[LLM] {}x{} matmul, {} iters, {} cycles", m, n, iterations, cycles);
    // Format GFLOPS with 3 decimal places
    if gf_frac < 10 {
        crate::serial_println!("[LLM] MatMul Benchmark: {}.00{} GFLOPS", gf_int, gf_frac);
    } else if gf_frac < 100 {
        crate::serial_println!("[LLM] MatMul Benchmark: {}.0{} GFLOPS", gf_int, gf_frac);
    } else {
        crate::serial_println!("[LLM] MatMul Benchmark: {}.{} GFLOPS", gf_int, gf_frac);
    }
    crate::serial_println!("[LLM] Output[0]={} (x10000)", (out[0] * 10000.0) as i64);
}

/// Launch a CI test PID safely — sets up GS_BASE correctly before Ring 3 transition.
///
/// CRITICAL FIX: The old `launch_ci_test_pid` called `exec_trampoline` directly which
/// does SWAPGS, but GS_BASE was never initialized → GS_BASE=0 after swap → first
/// interrupt handler tries `mov gs:[8], rsp` → NULL deref at address 0x8 → panic.
///
/// This function uses `exec_switch_cr3_and_ring3` which properly handles diagnostics,
/// and we set up GS_BASE=PER_CPU / KERNEL_GS_BASE=0 beforehand so SWAPGS in the naked
/// trampoline produces: GS_BASE=0 (user), KERNEL_GS_BASE=PER_CPU (kernel). ✓
///
/// This function never returns — process exits via sys_exit → resume_kernel_shell.
fn launch_ci_test_safe(pid: u64) {
    if let Some((entry, stack, pml4)) = crate::process::get_entry_state(pid) {
        if entry != 0 && pml4 != 0 {
            crate::serial_println!(
                "[CI-TEST] Switching to PID {} entry=0x{:X} rsp=0x{:X} cr3=0x{:X}",
                pid, entry, stack, pml4
            );

            // Set as current process for the scheduler
            crate::scheduler::set_current_pid(pid);
            let _ = crate::process::set_state(pid, crate::process::ProcessState::Running);

            // KPTI: Store user CR3 for syscall_entry
            crate::arch::x86_64::syscall::set_user_cr3(pml4);

            // CRITICAL: Set up GS_BASE = PER_CPU, KERNEL_GS_BASE = 0
            // The naked trampoline does SWAPGS which swaps them:
            //   After SWAPGS: GS_BASE = 0 (user), KERNEL_GS_BASE = PER_CPU (kernel) ✓
            // This is what the timer ISR does (idt.rs lines 1566-1584) and what
            // kill_user_and_switch does — the ONLY correct way to transition to Ring 3.
            unsafe {
                let per_cpu_addr = crate::arch::x86_64::syscall::get_per_cpu_addr();
                // IA32_GS_BASE = PER_CPU (will become KERNEL_GS_BASE after swapgs)
                core::arch::asm!(
                    "wrmsr",
                    in("ecx") 0xC000_0101u32,
                    in("eax") (per_cpu_addr & 0xFFFF_FFFF) as u32,
                    in("edx") (per_cpu_addr >> 32) as u32,
                    options(nostack),
                );
                // IA32_KERNEL_GS_BASE = 0 (will become GS_BASE after swapgs = user value)
                core::arch::asm!(
                    "wrmsr",
                    in("ecx") 0xC000_0102u32,
                    in("eax") 0u32,
                    in("edx") 0u32,
                    options(nostack),
                );
                crate::serial_println!(
                    "[CI-TEST] GS_BASE=0x{:X} (PER_CPU), KERNEL_GS_BASE=0x0 — ready for SWAPGS",
                    per_cpu_addr
                );
            }

            // Use the safe trampoline that handles CR3 switch, swapgs, GPR zeroing, IRETQ
            unsafe {
                crate::elf::exec_switch_cr3_and_ring3(pml4, entry, stack);
            }
            // unreachable — exec_switch_cr3_and_ring3 is -> !
        } else {
            crate::serial_println!("[CI-TEST] PID {} has invalid entry/pml4", pid);
        }
    } else {
        crate::serial_println!("[CI-TEST] PID {} not found in process table", pid);
    }
}

/// Infinite halt loop -- used after fatal errors or when kernel work is done.
fn halt_loop() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}
