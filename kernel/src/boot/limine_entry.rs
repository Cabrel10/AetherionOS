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

// ===== Limine Request Structures =====
// These are placed in the .requests section by the linker script.

#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

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

// ===== Entry Point =====

/// Kernel version for the Limine boot path.
const LIMINE_KERNEL_VERSION: &str = "3.1.0-j148-limine";

/// Limine entry point — replaces kernel_main when built with `--features limine`.
///
/// # Safety
/// Called by the Limine bootloader after setting up paging, GDT, and the HHDM.
/// The kernel is running in 64-bit mode with interrupts disabled.
#[no_mangle]
unsafe extern "C" fn kmain() -> ! {
    // Serial is initialized lazily via lazy_static on first use.

    crate::serial_write("\n╔══════════════════════════════════════════════════╗\n");
    crate::serial_write("║  AetherionOS Kernel Boot Sequence (Limine)       ║\n");
    crate::serial_write("║  Version: ");
    crate::serial_write(LIMINE_KERNEL_VERSION);
    crate::serial_write("\n");
    crate::serial_write("║  Boot Protocol: Limine v7+                       ║\n");
    crate::serial_write("╚══════════════════════════════════════════════════╝\n\n");

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
    crate::serial_write("       [OK] GDT + TSS + Ring 3 selectors\n");

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
    crate::serial_write("       [OK] PIC remapped (32-47)\n");

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

    // Step 5: Memory (adapted for Limine)
    crate::serial_write("[5/12] Memory init (Limine HHDM)...\n");
    crate::serial_println!("       HHDM offset: 0x{:X}", boot_info.hhdm_offset);
    crate::serial_println!("       Usable memory: {} MiB", boot_info.total_usable_memory / (1024 * 1024));

    // NOTE: Memory initialization with Limine requires adapting memory::init()
    // to accept the HHDM offset and memory map directly instead of BootInfo.
    // This is a TODO for the full Limine integration.
    // For now, we set the physical memory offset for the ELF loader.
    crate::elf::set_phys_mem_offset(boot_info.hhdm_offset);
    crate::serial_write("       [OK] Physical memory offset configured for ELF loader\n");

    crate::serial_write("\n[LIMINE] ════════════════════════════════════════════\n");
    crate::serial_write("[LIMINE] Kernel initialization complete (Limine path).\n");
    crate::serial_println!("[LIMINE] HHDM: 0x{:X}, RAM: {} MiB",
        boot_info.hhdm_offset,
        boot_info.total_usable_memory / (1024 * 1024));
    crate::serial_write("[LIMINE] ════════════════════════════════════════════\n");

    // TODO: Full memory manager init, heap, VFS, scheduler, shell
    // These require adapting memory::init() for Limine's memory map format.

    crate::serial_write("\n[LIMINE] Entering halt loop (full init TODO).\n");
    crate::serial_write("$ ");

    halt_loop();
}

/// Infinite halt loop — used after fatal errors or when kernel work is done.
fn halt_loop() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}
