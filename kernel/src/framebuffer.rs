// framebuffer.rs - Jalon 21: VBE Framebuffer for GUI
//
// Uses the Bochs VGA Extensions (BGA) available in QEMU with -vga std.
// Switches to a linear framebuffer mode with 32-bit color depth.
//
// Bochs VBE Dispi ports:
//   Index port: 0x01CE
//   Data port:  0x01CF
//
// Framebuffer physical address: detected via VBE register or defaults to 0xFD000000

use core::sync::atomic::{AtomicU64, AtomicU32, Ordering};

// ===== Bochs VBE Dispi Register Indices =====
const VBE_DISPI_INDEX_ID: u16 = 0;
const VBE_DISPI_INDEX_XRES: u16 = 1;
const VBE_DISPI_INDEX_YRES: u16 = 2;
const VBE_DISPI_INDEX_BPP: u16 = 3;
const VBE_DISPI_INDEX_ENABLE: u16 = 4;
const VBE_DISPI_INDEX_BANK: u16 = 5;
const VBE_DISPI_INDEX_VIRT_WIDTH: u16 = 6;
const VBE_DISPI_INDEX_VIRT_HEIGHT: u16 = 7;
const VBE_DISPI_INDEX_X_OFFSET: u16 = 8;
const VBE_DISPI_INDEX_Y_OFFSET: u16 = 9;
const VBE_DISPI_INDEX_VIDEO_MEMORY_64K: u16 = 10;

// VBE Dispi I/O Ports
const VBE_DISPI_IOPORT_INDEX: u16 = 0x01CE;
const VBE_DISPI_IOPORT_DATA: u16 = 0x01CF;

// VBE Enable flags
const VBE_DISPI_DISABLED: u16 = 0x00;
const VBE_DISPI_ENABLED: u16 = 0x01;
const VBE_DISPI_LFB_ENABLED: u16 = 0x40;

// Default framebuffer physical address for QEMU -vga std
// This is the PCI BAR0 of the VGA adapter
const DEFAULT_FB_PHYS: u64 = 0xFD000000;

// ===== Global Framebuffer Info =====

static FB_PHYS_ADDR: AtomicU64 = AtomicU64::new(0);
static FB_WIDTH: AtomicU32 = AtomicU32::new(0);
static FB_HEIGHT: AtomicU32 = AtomicU32::new(0);
static FB_STRIDE: AtomicU32 = AtomicU32::new(0);
static FB_SIZE: AtomicU64 = AtomicU64::new(0);

/// Framebuffer information for syscall responses
#[derive(Debug, Clone, Copy)]
pub struct FramebufferInfo {
    pub phys_addr: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,  // bytes per row
    pub bpp: u32,     // bits per pixel
    pub size: u64,    // total size in bytes
}

// ===== Port I/O helpers =====

#[inline]
unsafe fn outw(port: u16, value: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack));
}

#[inline]
unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    core::arch::asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack));
    value
}

/// Write a VBE Dispi register
unsafe fn vbe_write(index: u16, value: u16) {
    outw(VBE_DISPI_IOPORT_INDEX, index);
    outw(VBE_DISPI_IOPORT_DATA, value);
}

/// Read a VBE Dispi register
unsafe fn vbe_read(index: u16) -> u16 {
    outw(VBE_DISPI_IOPORT_INDEX, index);
    inw(VBE_DISPI_IOPORT_DATA)
}

/// Detect the Bochs VGA adapter by reading the VBE Dispi ID register
fn detect_bga() -> bool {
    unsafe {
        let id = vbe_read(VBE_DISPI_INDEX_ID);
        // BGA IDs: 0xB0C0..0xB0C5
        id >= 0xB0C0 && id <= 0xB0C5
    }
}

/// Initialize the framebuffer with the given resolution and 32-bit color
pub fn init(width: u32, height: u32) -> Option<FramebufferInfo> {
    crate::serial_println!("[FB] Detecting Bochs VGA adapter...");
    
    if !detect_bga() {
        crate::serial_println!("[FB] Bochs VGA adapter NOT detected");
        return None;
    }

    let bga_id = unsafe { vbe_read(VBE_DISPI_INDEX_ID) };
    crate::serial_println!("[FB] Bochs VGA adapter detected (ID=0x{:04X})", bga_id);

    // Get total video memory
    let video_mem_64k = unsafe { vbe_read(VBE_DISPI_INDEX_VIDEO_MEMORY_64K) } as u64;
    let video_mem_bytes = video_mem_64k * 64 * 1024;
    crate::serial_println!("[FB] Video memory: {} KB ({} x 64KB blocks)", video_mem_bytes / 1024, video_mem_64k);

    // Set the resolution
    let w = width as u16;
    let h = height as u16;
    let bpp: u16 = 32; // 32-bit color (BGRA)

    // Check if requested resolution fits in video memory
    let needed = (width as u64) * (height as u64) * 4;
    if needed > video_mem_bytes {
        crate::serial_println!("[FB] Resolution {}x{} too large for {} KB VRAM", width, height, video_mem_bytes / 1024);
        return None;
    }

    unsafe {
        // Disable VBE first
        vbe_write(VBE_DISPI_INDEX_ENABLE, VBE_DISPI_DISABLED);

        // Set resolution and color depth
        vbe_write(VBE_DISPI_INDEX_XRES, w);
        vbe_write(VBE_DISPI_INDEX_YRES, h);
        vbe_write(VBE_DISPI_INDEX_BPP, bpp);

        // Enable VBE with LFB
        vbe_write(VBE_DISPI_INDEX_ENABLE, VBE_DISPI_ENABLED | VBE_DISPI_LFB_ENABLED);
    }

    // Verify settings
    let actual_w = unsafe { vbe_read(VBE_DISPI_INDEX_XRES) } as u32;
    let actual_h = unsafe { vbe_read(VBE_DISPI_INDEX_YRES) } as u32;
    let actual_bpp = unsafe { vbe_read(VBE_DISPI_INDEX_BPP) } as u32;

    crate::serial_println!("[FB] Mode set: {}x{}x{} bpp", actual_w, actual_h, actual_bpp);

    let stride = actual_w * (actual_bpp / 8);
    let size = (stride as u64) * (actual_h as u64);
    let phys_addr = DEFAULT_FB_PHYS;

    // Store in global atomics
    FB_PHYS_ADDR.store(phys_addr, Ordering::SeqCst);
    FB_WIDTH.store(actual_w, Ordering::SeqCst);
    FB_HEIGHT.store(actual_h, Ordering::SeqCst);
    FB_STRIDE.store(stride, Ordering::SeqCst);
    FB_SIZE.store(size, Ordering::SeqCst);

    crate::serial_println!(
        "[FB] Framebuffer: phys=0x{:X}, {}x{}, stride={}, size={} KB",
        phys_addr, actual_w, actual_h, stride, size / 1024
    );

    Some(FramebufferInfo {
        phys_addr,
        width: actual_w,
        height: actual_h,
        stride,
        bpp: actual_bpp,
        size,
    })
}

/// Initialize framebuffer from Limine boot protocol GOP data.
/// Called by limine_entry.rs when booting via Limine.
/// Unlike init() which programs Bochs VBE registers, this simply records
/// the framebuffer info that Limine already set up via UEFI GOP or BIOS VBE.
pub fn init_from_limine(phys_addr: u64, width: u32, height: u32, pitch: u32, bpp: u32) -> Option<FramebufferInfo> {
    crate::serial_println!(
        "[FB] Limine GOP framebuffer: phys=0x{:X}, {}x{}, pitch={}, bpp={}",
        phys_addr, width, height, pitch, bpp
    );

    let stride = pitch; // Limine gives us pitch in bytes
    let size = (stride as u64) * (height as u64);

    FB_PHYS_ADDR.store(phys_addr, Ordering::SeqCst);
    FB_WIDTH.store(width, Ordering::SeqCst);
    FB_HEIGHT.store(height, Ordering::SeqCst);
    FB_STRIDE.store(stride, Ordering::SeqCst);
    FB_SIZE.store(size, Ordering::SeqCst);

    crate::serial_println!(
        "[FB] Limine framebuffer ready: {}x{}x{}, {} KB",
        width, height, bpp, size / 1024
    );

    Some(FramebufferInfo {
        phys_addr,
        width,
        height,
        stride,
        bpp,
        size,
    })
}

/// Register /dev/fb0 and /sys/class/backlight in the VFS.
/// This makes the framebuffer accessible to userspace programs (TinyX, etc.).
pub fn register_vfs_nodes() {
    let info = match get_info() {
        Some(i) => i,
        None => {
            crate::serial_println!("[FB] No framebuffer to register in VFS");
            return;
        }
    };

    // Create /dev/fb0 — a pseudo-file that exposes framebuffer metadata
    // Real mmap is handled by map_fb_for_user() via a syscall
    let fb_info_str = alloc::format!(
        "phys_addr=0x{:X}\nwidth={}\nheight={}\nstride={}\nbpp={}\nsize={}\n",
        info.phys_addr, info.width, info.height, info.stride, info.bpp, info.size
    );
    let _ = crate::fs::vfs::file_write("/dev/fb0", fb_info_str.as_bytes());

    // Create /sys/class/backlight/panel0/
    let _ = crate::fs::vfs::mkdir("/sys/class");
    let _ = crate::fs::vfs::mkdir("/sys/class/backlight");
    let _ = crate::fs::vfs::mkdir("/sys/class/backlight/panel0");
    let _ = crate::fs::vfs::file_write("/sys/class/backlight/panel0/brightness", b"100");
    let _ = crate::fs::vfs::file_write("/sys/class/backlight/panel0/max_brightness", b"255");
    let _ = crate::fs::vfs::file_write("/sys/class/backlight/panel0/actual_brightness", b"100");
    let _ = crate::fs::vfs::file_write("/sys/class/backlight/panel0/type", b"raw");

    crate::serial_println!(
        "[FB] VFS: /dev/fb0 registered ({}x{}), /sys/class/backlight/panel0 created",
        info.width, info.height
    );
}

/// Get current framebuffer info (None if not initialized)
pub fn get_info() -> Option<FramebufferInfo> {
    let phys = FB_PHYS_ADDR.load(Ordering::SeqCst);
    if phys == 0 {
        return None;
    }
    Some(FramebufferInfo {
        phys_addr: phys,
        width: FB_WIDTH.load(Ordering::SeqCst),
        height: FB_HEIGHT.load(Ordering::SeqCst),
        stride: FB_STRIDE.load(Ordering::SeqCst),
        bpp: 32,
        size: FB_SIZE.load(Ordering::SeqCst),
    })
}

/// Map the framebuffer into a user process's address space.
/// Returns the virtual address where the framebuffer is mapped.
pub fn map_fb_for_user(pml4_phys: u64) -> Option<u64> {
    let info = get_info()?;
    
    // Map at a fixed user virtual address: 0x0000_5000_0000_0000
    const FB_USER_VADDR: u64 = 0x0000_5000_0000_0000;
    
    let num_pages = ((info.size + 4095) / 4096) as usize;
    
    crate::serial_println!(
        "[FB] Mapping {} pages of FB (phys=0x{:X}) at user vaddr=0x{:X}",
        num_pages, info.phys_addr, FB_USER_VADDR
    );
    
    for i in 0..num_pages {
        let vaddr = FB_USER_VADDR + (i as u64) * 4096;
        let paddr = info.phys_addr + (i as u64) * 4096;
        
        // Map with USER | WRITABLE | PRESENT | Write-Through | Cache-Disable
        // NX removed: framebuffer is MMIO, NX bit can cause issues on some hardware
        // Write-Through (bit 3) helps with MMIO framebuffer coherence
        let flags: u64 = 0x01 | 0x02 | 0x04 | 0x08; // PRESENT | WRITABLE | USER_ACCESSIBLE | PWT
        unsafe {
            if crate::elf::demand_map_user_page(pml4_phys, vaddr, paddr, flags).is_err() {
                crate::serial_println!("[FB] Failed to map page at 0x{:X}", vaddr);
                return None;
            }
        }
    }
    
    // Flush TLB
    unsafe {
        let cr3: u64;
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
        core::arch::asm!("mov cr3, {}", in(reg) cr3, options(nostack));
    }
    
    crate::serial_println!("[FB] Framebuffer mapped at 0x{:X} ({} pages)", FB_USER_VADDR, num_pages);
    Some(FB_USER_VADDR)
}
