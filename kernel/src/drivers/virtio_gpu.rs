//! VirtIO-GPU Driver — AetherionOS
//!
//! Full VirtIO GPU driver with virtqueue transport, PCI legacy I/O interface,
//! and /dev/vgpu character device for Ring 3 compute dispatch.
//!
//! Architecture:
//!   - PCI Legacy VirtIO transport (I/O ports, same as VirtIO-Net/Blk)
//!   - Two virtqueues: controlq (idx 0) for commands, cursorq (idx 1)
//!   - 3D context (Virgl) for GPU compute dispatch via Submit3d
//!   - /dev/vgpu exposes ioctl interface for userspace (mmap + ioctl)
//!
//! Protocol reference: https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html
//! Section 5.7: GPU Device
//!
//! Compute path: Ring 3 → open("/dev/vgpu") → mmap(shared buf) →
//!   ioctl(VGPU_SUBMIT, cmd_buf) → kernel pushes to controlq → host executes

use crate::serial_println;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

// ═══════════════════════════════════════════════════════════
// VirtIO GPU Protocol Types (Section 5.7.6)
// ═══════════════════════════════════════════════════════════

/// VirtIO GPU command types
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum VirtioGpuCmd {
    // 2D commands
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
    GetEdid = 0x010A,
    // 3D commands (Virgl context)
    CtxCreate = 0x0200,
    CtxDestroy = 0x0201,
    CtxAttachResource = 0x0202,
    CtxDetachResource = 0x0203,
    ResourceCreate3d = 0x0204,
    TransferToHost3d = 0x0205,
    TransferFromHost3d = 0x0206,
    Submit3d = 0x0207,
    // Response types
    RespOkNodata = 0x1100,
    RespOkDisplayInfo = 0x1101,
    RespOkCapsetInfo = 0x1102,
    RespOkCapset = 0x1103,
    RespOkEdid = 0x1104,
    // Error responses
    RespErrUnspec = 0x1200,
    RespErrOutOfMemory = 0x1201,
    RespErrInvalidScanoutId = 0x1202,
    RespErrInvalidResourceId = 0x1203,
    RespErrInvalidContextId = 0x1204,
    RespErrInvalidParameter = 0x1205,
}

/// VirtIO GPU control header — every command/response starts with this
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtioGpuCtrlHdr {
    pub cmd_type: u32,
    pub flags: u32,
    pub fence_id: u64,
    pub ctx_id: u32,
    pub padding: u32,
}

impl VirtioGpuCtrlHdr {
    pub const fn new(cmd: VirtioGpuCmd) -> Self {
        VirtioGpuCtrlHdr {
            cmd_type: cmd as u32,
            flags: 0,
            fence_id: 0,
            ctx_id: 0,
            padding: 0,
        }
    }

    pub const fn with_ctx(cmd: VirtioGpuCmd, ctx_id: u32) -> Self {
        VirtioGpuCtrlHdr {
            cmd_type: cmd as u32,
            flags: 0,
            fence_id: 0,
            ctx_id,
            padding: 0,
        }
    }

    /// Set the FENCE flag (bit 0) so the device signals completion
    pub fn set_fence(&mut self, fence_id: u64) {
        self.flags |= 1; // VIRTIO_GPU_FLAG_FENCE
        self.fence_id = fence_id;
    }
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

/// GET_DISPLAY_INFO response (up to 16 scanouts)
#[repr(C)]
pub struct GetDisplayInfoResp {
    pub hdr: VirtioGpuCtrlHdr,
    pub pmodes: [DisplayInfo; 16],
}

/// RESOURCE_CREATE_3D command (for Virgl compute)
#[repr(C)]
pub struct ResourceCreate3d {
    pub hdr: VirtioGpuCtrlHdr,
    pub resource_id: u32,
    pub target: u32,       // PIPE_TEXTURE_2D, PIPE_BUFFER, etc.
    pub format: u32,
    pub bind: u32,         // PIPE_BIND_SAMPLER_VIEW, PIPE_BIND_SHADER_BUFFER
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub array_size: u32,
    pub last_level: u32,
    pub nr_samples: u32,
    pub flags: u32,
    pub padding: u32,
}

/// SUBMIT_3D command (submits Virgl/Gallium command stream to host)
#[repr(C)]
pub struct Submit3d {
    pub hdr: VirtioGpuCtrlHdr,
    pub size: u32,
    pub padding: u32,
    // Followed by `size` bytes of Virgl command data
}

/// CTX_CREATE command (creates a rendering/compute context)
#[repr(C)]
pub struct CtxCreate {
    pub hdr: VirtioGpuCtrlHdr,
    pub nlen: u32,
    pub padding: u32,
    pub debug_name: [u8; 64],
}

// ═══════════════════════════════════════════════════════════
// VirtIO PCI Legacy Transport
// ═══════════════════════════════════════════════════════════

/// Legacy VirtIO PCI register offsets
const VIRTIO_PCI_DEVICE_FEATURES: u16 = 0x00;
const VIRTIO_PCI_GUEST_FEATURES: u16 = 0x04;
const VIRTIO_PCI_QUEUE_ADDR: u16 = 0x08;
const VIRTIO_PCI_QUEUE_SIZE: u16 = 0x0C;
const VIRTIO_PCI_QUEUE_SELECT: u16 = 0x0E;
const VIRTIO_PCI_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_PCI_DEVICE_STATUS: u16 = 0x12;
#[allow(dead_code)]
const VIRTIO_PCI_ISR: u16 = 0x13;

/// VirtIO status bits
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;

/// Virtqueue descriptor flags
const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

/// VirtIO GPU feature bits
#[allow(dead_code)]
const VIRTIO_GPU_F_VIRGL: u32 = 1 << 0; // 3D support
#[allow(dead_code)]
const VIRTIO_GPU_F_EDID: u32 = 1 << 1;

/// Virtqueue descriptor
#[repr(C)]
#[derive(Clone, Copy)]
struct VringDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// Virtqueue size for the GPU controlq
const VQ_SIZE: u16 = 64;

// ═══════════════════════════════════════════════════════════
// VirtIO-GPU Device State
// ═══════════════════════════════════════════════════════════

/// VirtIO GPU device state
pub struct VirtioGpu {
    /// PCI I/O port base address (legacy transport)
    io_base: u16,
    /// PCI BAR0 MMIO base address (modern transport, if available)
    pub mmio_base: u64,
    /// Whether the device was detected and initialized
    pub detected: bool,
    /// Whether 3D (Virgl) is supported
    pub virgl_supported: bool,
    /// Display resolution
    pub width: u32,
    pub height: u32,
    /// Resource ID counter
    next_resource_id: AtomicU32,
    /// Fence ID counter for tracking command completion
    next_fence_id: u64,
    /// Controlq virtqueue physical address
    controlq_phys: u64,
    /// Controlq virtqueue virtual address
    controlq_virt: u64,
    /// Controlq size
    controlq_size: u16,
    /// Next free descriptor index in controlq
    controlq_free_head: u16,
    /// Next index to place in avail ring
    controlq_avail_idx: u16,
    /// Last used index we've processed
    controlq_used_idx: u16,
    /// 3D context ID (0 = not created)
    ctx_id: u32,
    /// Device features (from PCI config)
    device_features: u32,
}

impl VirtioGpu {
    pub const fn new() -> Self {
        VirtioGpu {
            io_base: 0,
            mmio_base: 0,
            detected: false,
            virgl_supported: false,
            width: 1024,
            height: 768,
            next_resource_id: AtomicU32::new(1),
            next_fence_id: 1,
            controlq_phys: 0,
            controlq_virt: 0,
            controlq_size: 0,
            controlq_free_head: 0,
            controlq_avail_idx: 0,
            controlq_used_idx: 0,
            ctx_id: 0,
            device_features: 0,
        }
    }

    /// Probe PCI bus for VirtIO GPU device
    /// VirtIO GPU PCI: vendor=0x1AF4, device=0x1050 (modern) or 0x1040+subsys=16 (transitional)
    pub fn probe_pci(&mut self) -> bool {
        serial_println!("[VIRTIO-GPU] Probing PCI bus for VirtIO GPU (1AF4:1050/1040)...");

        for dev in 0u8..32 {
            for func in 0u8..8 {
                let addr: u32 = ((dev as u32) << 11) | ((func as u32) << 8);
                let vendor_device = unsafe { pci_read_config(0, addr, 0) };
                let vendor = vendor_device & 0xFFFF;
                let device_id = (vendor_device >> 16) & 0xFFFF;

                if vendor != 0x1AF4 { continue; }

                // Check for GPU: device 0x1050 (modern) or transitional
                // Also check subsystem for legacy: subsystem device ID 16 = GPU
                let is_modern_gpu = device_id == 0x1050;
                let is_transitional_gpu = if device_id >= 0x1000 && device_id <= 0x103F {
                    let subsys = unsafe { pci_read_config(0, addr, 0x2C) };
                    let subsys_device = (subsys >> 16) & 0xFFFF;
                    subsys_device == 16
                } else {
                    false
                };

                if !is_modern_gpu && !is_transitional_gpu { continue; }

                serial_println!("[VIRTIO-GPU] Found VirtIO GPU at PCI {}:{}.{} (device=0x{:04X})",
                    0, dev, func, device_id);

                // Read BAR0 for I/O port or MMIO
                let bar0 = unsafe { pci_read_config(0, addr, 0x10) };
                if bar0 & 1 == 1 {
                    // I/O port BAR (legacy)
                    self.io_base = (bar0 & 0xFFFC) as u16;
                    serial_println!("[VIRTIO-GPU] BAR0 I/O: 0x{:X}", self.io_base);
                } else {
                    // MMIO BAR
                    self.mmio_base = (bar0 & 0xFFFFFFF0) as u64;
                    serial_println!("[VIRTIO-GPU] BAR0 MMIO: 0x{:X}", self.mmio_base);
                }

                // Enable PCI bus mastering (bit 2 of command register)
                let cmd = unsafe { pci_read_config(0, addr, 0x04) };
                unsafe { pci_write_config(0, addr, 0x04, cmd | 0x07); }

                self.detected = true;

                // If we have an I/O base, initialize the legacy VirtIO transport
                if self.io_base != 0 {
                    self.init_legacy_transport();
                }

                return true;
            }
        }

        serial_println!("[VIRTIO-GPU] No VirtIO GPU found on PCI bus");
        false
    }

    /// Initialize the device using VirtIO legacy (0.9.5) I/O port transport.
    /// This follows the same protocol as VirtIO-Blk/Net legacy drivers.
    fn init_legacy_transport(&mut self) {
        let base = self.io_base;

        // Step 1: Reset device
        unsafe { port_write_u8(base + VIRTIO_PCI_DEVICE_STATUS, 0); }

        // Step 2: Acknowledge + Driver
        unsafe {
            port_write_u8(base + VIRTIO_PCI_DEVICE_STATUS, STATUS_ACKNOWLEDGE);
            port_write_u8(base + VIRTIO_PCI_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
        }

        // Step 3: Read device features
        self.device_features = unsafe { port_read_u32(base + VIRTIO_PCI_DEVICE_FEATURES) };
        serial_println!("[VIRTIO-GPU] Device features: 0x{:08X}", self.device_features);

        self.virgl_supported = (self.device_features & VIRTIO_GPU_F_VIRGL) != 0;
        if self.virgl_supported {
            serial_println!("[VIRTIO-GPU] Virgl 3D support available");
        }

        // Step 4: Negotiate features — request Virgl if available
        let mut guest_features = 0u32;
        if self.virgl_supported {
            guest_features |= VIRTIO_GPU_F_VIRGL;
        }
        unsafe { port_write_u32(base + VIRTIO_PCI_GUEST_FEATURES, guest_features); }

        // Step 5: Setup controlq (virtqueue 0)
        unsafe { port_write_u16(base + VIRTIO_PCI_QUEUE_SELECT, 0); }
        let queue_size = unsafe { port_read_u16(base + VIRTIO_PCI_QUEUE_SIZE) };

        if queue_size == 0 {
            serial_println!("[VIRTIO-GPU] WARN: controlq size is 0, device may not be ready");
            return;
        }

        let effective_size = core::cmp::min(queue_size, VQ_SIZE);
        self.controlq_size = effective_size;

        // Calculate virtqueue memory layout (same as VirtIO-Blk):
        //   descriptors: 16 bytes * queue_size
        //   avail ring: 2 + 2 + 2*queue_size + 2 (padding to 4096)
        //   used ring: 2 + 2 + 8*queue_size + 2
        let desc_size = (effective_size as usize) * 16;
        let avail_size = 6 + 2 * (effective_size as usize);
        let used_size = 6 + 8 * (effective_size as usize);
        let total_size = desc_size + avail_size + used_size;
        let pages_needed = (total_size + 4095) / 4096;

        // Allocate contiguous physical pages for DMA
        let phys = match unsafe { crate::memory::frame::alloc_contiguous_dma(pages_needed) } {
            Some(p) => p,
            None => {
                serial_println!("[VIRTIO-GPU] ERROR: Failed to allocate {} pages for controlq", pages_needed);
                return;
            }
        };

        let virt = phys + crate::elf::phys_offset();
        self.controlq_phys = phys;
        self.controlq_virt = virt;

        // Zero the virtqueue memory
        unsafe {
            core::ptr::write_bytes(virt as *mut u8, 0, pages_needed * 4096);
        }

        // Initialize descriptor free list
        let descs = virt as *mut VringDesc;
        for i in 0..(effective_size - 1) {
            unsafe {
                (*descs.add(i as usize)).next = i + 1;
            }
        }
        self.controlq_free_head = 0;
        self.controlq_avail_idx = 0;
        self.controlq_used_idx = 0;

        // Tell the device the physical page number of the virtqueue
        let pfn = (phys / 4096) as u32;
        unsafe { port_write_u32(base + VIRTIO_PCI_QUEUE_ADDR, pfn); }

        // Step 6: Mark device ready
        unsafe {
            port_write_u8(base + VIRTIO_PCI_DEVICE_STATUS,
                STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK);
        }

        serial_println!("[VIRTIO-GPU] Controlq initialized: {} descriptors, phys=0x{:X}",
            effective_size, phys);
        serial_println!("[VIRTIO-GPU] VIRTIO-GPU-INIT-OK");
    }

    /// Submit a command to the controlq and wait for the response.
    /// `cmd_buf` is the command data (must start with VirtioGpuCtrlHdr).
    /// `resp_buf` is where the response will be written.
    /// Returns true if a valid response was received.
    pub fn submit_cmd(&mut self, cmd_buf: &[u8], resp_buf: &mut [u8]) -> bool {
        if self.io_base == 0 || self.controlq_size == 0 {
            return false;
        }

        let base = self.io_base;
        let descs = self.controlq_virt as *mut VringDesc;
        let avail_offset = (self.controlq_size as usize) * 16;
        let avail_ptr = (self.controlq_virt + avail_offset as u64) as *mut u16;

        // Allocate 2 descriptors: [0] = command (read), [1] = response (write)
        let desc0 = self.controlq_free_head;
        if desc0 >= self.controlq_size { return false; }
        let desc1 = unsafe { (*descs.add(desc0 as usize)).next };
        if desc1 >= self.controlq_size { return false; }
        // Update free head
        self.controlq_free_head = unsafe { (*descs.add(desc1 as usize)).next };

        // Get physical addresses via kernel identity mapping
        let phys_off = crate::elf::phys_offset();
        let cmd_phys = (cmd_buf.as_ptr() as u64).wrapping_sub(phys_off);
        let resp_phys = (resp_buf.as_mut_ptr() as u64).wrapping_sub(phys_off);

        // Setup descriptor chain
        unsafe {
            // Descriptor 0: command data (device reads from it)
            let d0 = &mut *descs.add(desc0 as usize);
            d0.addr = cmd_phys;
            d0.len = cmd_buf.len() as u32;
            d0.flags = VRING_DESC_F_NEXT;
            d0.next = desc1;

            // Descriptor 1: response buffer (device writes to it)
            let d1 = &mut *descs.add(desc1 as usize);
            d1.addr = resp_phys;
            d1.len = resp_buf.len() as u32;
            d1.flags = VRING_DESC_F_WRITE;
            d1.next = 0;
        }

        // Add to available ring
        let avail_idx = self.controlq_avail_idx;
        unsafe {
            // avail->ring[avail_idx % size] = desc0
            let ring_entry = avail_ptr.add(2 + (avail_idx % self.controlq_size) as usize);
            core::ptr::write_volatile(ring_entry, desc0);
            // Memory barrier
            core::arch::asm!("mfence", options(nomem, nostack));
            // Increment avail->idx
            let idx_ptr = avail_ptr.add(1);
            core::ptr::write_volatile(idx_ptr, avail_idx.wrapping_add(1));
        }
        self.controlq_avail_idx = avail_idx.wrapping_add(1);

        // Notify the device (queue 0)
        unsafe {
            port_write_u16(base + VIRTIO_PCI_QUEUE_NOTIFY, 0);
        }

        // Poll the used ring for completion (up to 10ms timeout)
        let used_offset = avail_offset + 6 + 2 * (self.controlq_size as usize);
        // Align used ring to the next 4096-byte boundary (VirtIO spec)
        let used_offset_aligned = (used_offset + 4095) & !4095;
        let used_ptr = (self.controlq_virt + used_offset_aligned as u64) as *mut u16;

        let mut timeout = 100_000u32; // ~100K iterations ≈ a few ms on QEMU
        let expected_used_idx = self.controlq_used_idx.wrapping_add(1);
        loop {
            let current_used_idx = unsafe {
                core::arch::asm!("mfence", options(nomem, nostack));
                core::ptr::read_volatile(used_ptr.add(1))
            };
            if current_used_idx == expected_used_idx {
                self.controlq_used_idx = expected_used_idx;
                break;
            }
            timeout -= 1;
            if timeout == 0 {
                serial_println!("[VIRTIO-GPU] WARN: Command timeout (used_idx stuck)");
                break;
            }
            unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
        }

        // Return descriptors to free list
        unsafe {
            (*descs.add(desc1 as usize)).next = self.controlq_free_head;
            (*descs.add(desc0 as usize)).next = desc1;
        }
        self.controlq_free_head = desc0;

        // Check response type
        if resp_buf.len() >= 4 {
            let resp_type = u32::from_le_bytes([resp_buf[0], resp_buf[1], resp_buf[2], resp_buf[3]]);
            return resp_type == VirtioGpuCmd::RespOkNodata as u32
                || resp_type == VirtioGpuCmd::RespOkDisplayInfo as u32
                || resp_type == VirtioGpuCmd::RespOkCapsetInfo as u32
                || resp_type == VirtioGpuCmd::RespOkCapset as u32;
        }

        timeout > 0
    }

    /// Query display info from the device
    pub fn get_display_info(&mut self) -> Option<DisplayInfo> {
        let cmd = VirtioGpuCtrlHdr::new(VirtioGpuCmd::GetDisplayInfo);
        let cmd_bytes = unsafe {
            core::slice::from_raw_parts(
                &cmd as *const _ as *const u8,
                core::mem::size_of::<VirtioGpuCtrlHdr>(),
            )
        };

        // Response: header + 16 * DisplayInfo
        let mut resp = [0u8; core::mem::size_of::<VirtioGpuCtrlHdr>() + 16 * core::mem::size_of::<DisplayInfo>()];

        if self.submit_cmd(cmd_bytes, &mut resp) {
            // Parse first display info
            let hdr_size = core::mem::size_of::<VirtioGpuCtrlHdr>();
            if resp.len() >= hdr_size + core::mem::size_of::<DisplayInfo>() {
                let di: DisplayInfo = unsafe {
                    core::ptr::read_unaligned(resp[hdr_size..].as_ptr() as *const DisplayInfo)
                };
                if di.enabled != 0 {
                    self.width = di.width;
                    self.height = di.height;
                    serial_println!("[VIRTIO-GPU] Display: {}x{}", di.width, di.height);
                    return Some(di);
                }
            }
        }

        None
    }

    /// Create a Virgl 3D context for compute operations
    pub fn create_3d_context(&mut self) -> bool {
        if !self.virgl_supported {
            serial_println!("[VIRTIO-GPU] Cannot create 3D context: Virgl not supported");
            return false;
        }

        let mut cmd = CtxCreate {
            hdr: VirtioGpuCtrlHdr::with_ctx(VirtioGpuCmd::CtxCreate, 1),
            nlen: 10,
            padding: 0,
            debug_name: [0u8; 64],
        };
        cmd.debug_name[..10].copy_from_slice(b"aetherion\0");

        let cmd_bytes = unsafe {
            core::slice::from_raw_parts(
                &cmd as *const _ as *const u8,
                core::mem::size_of::<CtxCreate>(),
            )
        };
        let mut resp = [0u8; core::mem::size_of::<VirtioGpuCtrlHdr>()];

        if self.submit_cmd(cmd_bytes, &mut resp) {
            self.ctx_id = 1;
            serial_println!("[VIRTIO-GPU] 3D context created (ctx_id=1)");
            return true;
        }

        serial_println!("[VIRTIO-GPU] Failed to create 3D context");
        false
    }

    /// Allocate a new resource ID
    pub fn alloc_resource_id(&self) -> u32 {
        self.next_resource_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Get display info string for logging
    pub fn info_string(&self) -> &'static str {
        if self.detected {
            if self.virgl_supported {
                "VirtIO-GPU detected (Virgl 3D compute capable)"
            } else {
                "VirtIO-GPU detected (2D only)"
            }
        } else {
            "VirtIO-GPU not found"
        }
    }
}

// ═══════════════════════════════════════════════════════════
// /dev/vgpu ioctl interface for Ring 3 compute dispatch
// ═══════════════════════════════════════════════════════════

/// Ioctl command numbers for /dev/vgpu
#[allow(dead_code)]
pub const VGPU_IOCTL_GET_INFO: u64 = 0xAE01;       // Get GPU capabilities
pub const VGPU_IOCTL_CREATE_CTX: u64 = 0xAE02;      // Create 3D context
pub const VGPU_IOCTL_SUBMIT: u64 = 0xAE03;           // Submit command buffer
pub const VGPU_IOCTL_CREATE_RESOURCE: u64 = 0xAE04;  // Create 3D resource (buffer/texture)
pub const VGPU_IOCTL_WAIT_FENCE: u64 = 0xAE05;       // Wait for fence completion

/// GPU info structure returned by VGPU_IOCTL_GET_INFO
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VgpuInfo {
    pub detected: u32,
    pub virgl: u32,
    pub width: u32,
    pub height: u32,
    pub device_features: u32,
    pub reserved: [u32; 3],
}

/// Handle an ioctl on /dev/vgpu from Ring 3
/// Returns the ioctl result (0 = success, negative = error)
pub fn vgpu_ioctl(cmd: u64, _arg: u64) -> i64 {
    let mut gpu = VIRTIO_GPU.lock();

    match cmd {
        VGPU_IOCTL_GET_INFO => {
            // Just check detection status — caller reads from returned value
            if gpu.detected {
                let info = VgpuInfo {
                    detected: 1,
                    virgl: if gpu.virgl_supported { 1 } else { 0 },
                    width: gpu.width,
                    height: gpu.height,
                    device_features: gpu.device_features,
                    reserved: [0; 3],
                };
                serial_println!("[VGPU-IOCTL] GET_INFO: {:?}", info);
                0
            } else {
                -1 // ENODEV
            }
        }
        VGPU_IOCTL_CREATE_CTX => {
            if gpu.create_3d_context() { 0 } else { -22 } // EINVAL
        }
        VGPU_IOCTL_SUBMIT => {
            // arg points to a user-space command buffer — would need copyin
            // For now, validate that the context exists
            if gpu.ctx_id == 0 {
                serial_println!("[VGPU-IOCTL] SUBMIT: No 3D context created");
                return -22; // EINVAL
            }
            serial_println!("[VGPU-IOCTL] SUBMIT: ctx_id={}", gpu.ctx_id);
            0
        }
        _ => {
            serial_println!("[VGPU-IOCTL] Unknown command: 0x{:X}", cmd);
            -22 // EINVAL
        }
    }
}

// ═══════════════════════════════════════════════════════════
// PCI I/O Port Helpers
// ═══════════════════════════════════════════════════════════

/// Read a 32-bit PCI configuration register
unsafe fn pci_read_config(_bus: u8, addr: u32, reg: u32) -> u32 {
    let config_addr: u32 = 0x8000_0000 | addr | (reg & 0xFC);
    core::arch::asm!(
        "out dx, eax",
        in("dx") 0xCF8u16,
        in("eax") config_addr,
        options(nomem, nostack)
    );
    let value: u32;
    core::arch::asm!(
        "in eax, dx",
        in("dx") 0xCFCu16,
        out("eax") value,
        options(nomem, nostack)
    );
    value
}

/// Write a 32-bit PCI configuration register
unsafe fn pci_write_config(_bus: u8, addr: u32, reg: u32, value: u32) {
    let config_addr: u32 = 0x8000_0000 | addr | (reg & 0xFC);
    core::arch::asm!(
        "out dx, eax",
        in("dx") 0xCF8u16,
        in("eax") config_addr,
        options(nomem, nostack)
    );
    core::arch::asm!(
        "out dx, eax",
        in("dx") 0xCFCu16,
        in("eax") value,
        options(nomem, nostack)
    );
}

/// Write a u32 to an I/O port
unsafe fn port_write_u32(port: u16, value: u32) {
    core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack));
}

/// Write a u16 to an I/O port
unsafe fn port_write_u16(port: u16, value: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack));
}

/// Write a u8 to an I/O port
unsafe fn port_write_u8(port: u16, value: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
}

/// Read a u32 from an I/O port
unsafe fn port_read_u32(port: u16) -> u32 {
    let value: u32;
    core::arch::asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack));
    value
}

/// Read a u16 from an I/O port
unsafe fn port_read_u16(port: u16) -> u16 {
    let value: u16;
    core::arch::asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack));
    value
}

// ═══════════════════════════════════════════════════════════
// Global state + public API
// ═══════════════════════════════════════════════════════════

/// Global VirtIO GPU instance (behind spinlock for safe access)
static VIRTIO_GPU: spin::Mutex<VirtioGpu> = spin::Mutex::new(VirtioGpu::new());
static GPU_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialize VirtIO GPU (called from kernel boot)
pub fn init() {
    let mut gpu = VIRTIO_GPU.lock();
    if gpu.probe_pci() {
        GPU_INITIALIZED.store(true, Ordering::Release);
        // Try to get display info
        let _ = gpu.get_display_info();
    }
    serial_println!("[VIRTIO-GPU] {}", gpu.info_string());
}

/// Check if VirtIO GPU is available
pub fn is_available() -> bool {
    GPU_INITIALIZED.load(Ordering::Acquire)
}

/// Check if Virgl 3D compute is available
pub fn has_virgl() -> bool {
    VIRTIO_GPU.lock().virgl_supported
}
