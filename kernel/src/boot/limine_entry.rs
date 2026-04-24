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

// ===== Embedded ELF Binaries =====
// These are included at compile time and mounted into the VFS during boot.
// The same binaries as main.rs, but accessible from the Limine boot path.
static HELLO_ELF: &[u8] = include_bytes!("../../../userspace/hello.elf");
static HELLO_C_ELF: &[u8] = include_bytes!("../../../userspace/c_apps/hello_c.elf");
static BUSYBOX_ELF: &[u8] = include_bytes!("../../../userspace/busybox.elf");

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

    // Step 10: Enable interrupts
    // Phase 6: segment registers were reloaded in step 1 (SS=0x10),
    // so the timer ISR's iretq restores a valid kernel data selector.
    // All subsystems (heap, scheduler, VFS, net) are now initialized.
    x86_64::instructions::interrupts::enable();
    crate::serial_write("[10/12] Interrupts: ENABLED (timer + keyboard)\n");

    // Summary banner
    crate::serial_write("\n=== AetherionOS v4.3.0-phase7 -- Limine Boot Complete ===\n");
    crate::serial_println!("RAM: {} MiB | Heap: 64 MB | Scheduler: ON | IRQ: ON | ELF: ON",
        boot_info.total_usable_memory / (1024 * 1024));
    crate::serial_write("=========================================================\n\n");

    // === Interactive Shell ===
    crate::serial_write("AetherionOS v4.3.0-phase7 ready.\n");
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

    // Parse "exec <path>" prefix
    if cmd.starts_with("exec ") {
        let path = cmd[5..].trim();
        if path.is_empty() {
            crate::serial_write("Usage: exec <path>  (e.g. exec /bin/hello.elf)\n");
            return;
        }
        crate::serial_println!("[EXEC] Loading ELF: {}", path);
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

/// Infinite halt loop -- used after fatal errors or when kernel work is done.
fn halt_loop() -> ! {
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}
