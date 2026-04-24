//! VirtIO-GPU Driver Skeleton — AetherionOS
//!
//! Provides basic 2D acceleration via the VirtIO GPU protocol.
//! When running in QEMU with `-device virtio-gpu-pci`, this driver
//! can create 2D resources, transfer pixel data, and display framebuffers.
//!
//! Protocol reference: https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html
//!
//! Capabilities:
//!   - Resource creation (RESOURCE_CREATE_2D)
//!   - Pixel data transfer (TRANSFER_TO_HOST_2D)
//!   - Scanout assignment (SET_SCANOUT)
//!   - Display info query (GET_DISPLAY_INFO)
//!
//! Future: 3D via Virgl (SUBMIT_3D commands for OpenGL ES)

use crate::serial_println;

/// VirtIO GPU command types
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum VirtioGpuCmd {
    GetDisplayInfo = 0x0100,
    ResourceCreate2d = 0x0101,
    ResourceUnref = 0x0102,
    SetScanout = 0x0103,
    ResourceFlush = 0x0104,
    TransferToHost2d = 0x0105,
    ResourceAttachBacking = 0x0106,
    ResourceDetachBacking = 0x0107,
    GetCapsetInfo = 0x0108,
    GetCapset = 0x0109,
    // 3D commands (Virgl)
    CtxCreate = 0x0200,
    CtxDestroy = 0x0201,
    CtxAttachResource = 0x0202,
    CtxDetachResource = 0x0203,
    ResourceCreate3d = 0x0204,
    TransferToHost3d = 0x0205,
    TransferFromHost3d = 0x0206,
    Submit3d = 0x0207,
}

/// VirtIO GPU pixel formats
#[repr(u32)]
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum VirtioGpuFormat {
    B8G8R8A8Unorm = 1,
    B8G8R8X8Unorm = 2,
    A8R8G8B8Unorm = 3,
    X8R8G8B8Unorm = 4,
    R8G8B8A8Unorm = 67,
    X8B8G8R8Unorm = 68,
    A8B8G8R8Unorm = 121,
    R8G8B8X8Unorm = 134,
}

/// Display info for a single scanout
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DisplayInfo {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub enabled: u32,
    pub flags: u32,
}

/// VirtIO GPU device state
pub struct VirtioGpu {
    /// PCI BAR0 MMIO base address
    pub mmio_base: u64,
    /// Whether the device was detected
    pub detected: bool,
    /// Display resolution
    pub width: u32,
    pub height: u32,
    /// Resource ID counter
    next_resource_id: u32,
}

impl VirtioGpu {
    pub const fn new() -> Self {
        VirtioGpu {
            mmio_base: 0,
            detected: false,
            width: 1024,
            height: 768,
            next_resource_id: 1,
        }
    }

    /// Probe PCI bus for VirtIO GPU device (vendor=0x1AF4, device=0x1050)
    pub fn probe_pci(&mut self) -> bool {
        serial_println!("[VIRTIO-GPU] Probing PCI bus for VirtIO GPU (1AF4:1050)...");
        
        // Scan PCI bus 0, devices 0-31, function 0
        for dev in 0u8..32 {
            let addr: u32 = (dev as u32) << 11;
            let vendor_device = unsafe { pci_read_config(0, addr, 0) };
            let vendor = vendor_device & 0xFFFF;
            let device = (vendor_device >> 16) & 0xFFFF;
            
            if vendor == 0x1AF4 && (device == 0x1050 || device == 0x1040) {
                serial_println!("[VIRTIO-GPU] Found VirtIO GPU at PCI {}:0.0", dev);
                
                // Read BAR0
                let bar0 = unsafe { pci_read_config(0, addr, 0x10) };
                if bar0 & 1 == 0 {
                    // Memory-mapped BAR
                    self.mmio_base = (bar0 & 0xFFFFFFF0) as u64;
                    serial_println!("[VIRTIO-GPU] BAR0 MMIO: 0x{:X}", self.mmio_base);
                }
                
                self.detected = true;
                return true;
            }
        }
        
        serial_println!("[VIRTIO-GPU] No VirtIO GPU found on PCI bus");
        false
    }

    /// Allocate a new resource ID
    pub fn alloc_resource_id(&mut self) -> u32 {
        let id = self.next_resource_id;
        self.next_resource_id += 1;
        id
    }

    /// Get display info string for logging
    pub fn info_string(&self) -> &'static str {
        if self.detected {
            "VirtIO-GPU detected"
        } else {
            "VirtIO-GPU not found (using Limine GOP framebuffer)"
        }
    }
}

/// Read a 32-bit PCI configuration register
/// bus: PCI bus number, addr: device/function << 11, reg: register offset
unsafe fn pci_read_config(_bus: u8, addr: u32, reg: u32) -> u32 {
    let config_addr: u32 = 0x8000_0000 | addr | (reg & 0xFC);
    // Write to PCI config address port (0xCF8)
    core::arch::asm!(
        "out dx, eax",
        in("dx") 0xCF8u16,
        in("eax") config_addr,
        options(nomem, nostack)
    );
    // Read from PCI config data port (0xCFC)
    let value: u32;
    core::arch::asm!(
        "in eax, dx",
        in("dx") 0xCFCu16,
        out("eax") value,
        options(nomem, nostack)
    );
    value
}

/// Global VirtIO GPU instance (behind spinlock for safe access)
static VIRTIO_GPU: spin::Mutex<VirtioGpu> = spin::Mutex::new(VirtioGpu::new());

/// Initialize VirtIO GPU (called from kernel boot)
pub fn init() {
    let mut gpu = VIRTIO_GPU.lock();
    gpu.probe_pci();
    serial_println!("[VIRTIO-GPU] {}", gpu.info_string());
}

/// Check if VirtIO GPU is available
pub fn is_available() -> bool {
    VIRTIO_GPU.lock().detected
}
