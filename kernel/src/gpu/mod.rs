// gpu/mod.rs - Couche 8: GPU Detection and VRAM Stub
//
// Scans PCI bus for class 0x03 (Display Controller) devices,
// reads BAR0, and initialises a VramAllocator.

pub mod allocator;

use spin::Mutex;
use lazy_static::lazy_static;
use crate::arch::x86_64::pci;
use allocator::VramAllocator;

/// PCI class code for display controllers
const GPU_CLASS: u8 = 0x03;

/// Default VRAM size if BAR sizing is not available (16 MB stub)
const DEFAULT_VRAM_SIZE: usize = 16 * 1024 * 1024;

/// GPU device information
#[derive(Debug, Clone, Copy)]
pub struct GpuDevice {
    pub vendor_id: u16,
    pub device_id: u16,
    pub bar0: u32,
    pub bar0_address: u64,
    pub vram_size: usize,
}

impl core::fmt::Display for GpuDevice {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "GPU [{:04x}:{:04x}] BAR0=0x{:08X} VRAM={}MB",
            self.vendor_id, self.device_id, self.bar0_address, self.vram_size / (1024 * 1024))
    }
}

lazy_static! {
    /// Global VRAM allocator (None if no GPU found)
    static ref VRAM_ALLOC: Mutex<Option<VramAllocator>> = Mutex::new(None);
    /// Detected GPU device info
    static ref GPU_INFO: Mutex<Option<GpuDevice>> = Mutex::new(None);
}

/// Initialize GPU subsystem: scan PCI for class 0x03, read BAR0, create allocator
pub fn init() {
    crate::serial_println!("[GPU] Scanning PCI bus for display controllers (class 0x03)...");

    let devices = pci::scan_for_class(GPU_CLASS);

    if devices.is_empty() {
        // No GPU found on PCI — create a stub with a fake BAR0 for testing
        crate::serial_println!("[GPU] No PCI GPU found; creating stub device");
        let stub = GpuDevice {
            vendor_id: 0x1234,
            device_id: 0x1111,
            bar0: 0xFD00_0000,
            bar0_address: 0xFD00_0000,
            vram_size: DEFAULT_VRAM_SIZE,
        };
        crate::serial_println!("[GPU] Stub: {}", stub);
        let valloc = VramAllocator::new(stub.bar0_address, stub.vram_size);
        *GPU_INFO.lock() = Some(stub);
        *VRAM_ALLOC.lock() = Some(valloc);
        return;
    }

    // Use the first GPU found
    let dev = devices[0];
    let bar0_raw = pci::read_bar(dev.bus, dev.device, dev.function, 0);
    // BAR0: bits 31:4 are the base address (for memory-mapped BARs)
    let bar0_address = (bar0_raw & 0xFFFF_FFF0) as u64;

    let gpu = GpuDevice {
        vendor_id: dev.vendor_id,
        device_id: dev.device_id,
        bar0: bar0_raw,
        bar0_address,
        vram_size: DEFAULT_VRAM_SIZE,
    };

    crate::serial_println!("[GPU] Found: {}", gpu);
    crate::serial_println!("[GPU] BAR0 raw=0x{:08X}, address=0x{:08X}", bar0_raw, bar0_address);

    let valloc = VramAllocator::new(bar0_address, gpu.vram_size);
    *GPU_INFO.lock() = Some(gpu);
    *VRAM_ALLOC.lock() = Some(valloc);

    crate::serial_println!("[GPU] VRAM allocator ready: {}MB at 0x{:08X}",
        gpu.vram_size / (1024 * 1024), bar0_address);
}

/// Allocate VRAM (returns physical address or None)
pub fn vram_alloc(size: usize) -> Option<u64> {
    VRAM_ALLOC.lock().as_mut()?.allocate(size)
}

/// Get GPU device info (if detected)
pub fn device_info() -> Option<GpuDevice> {
    *GPU_INFO.lock()
}

/// Get VRAM allocator metrics
pub fn vram_metrics() -> Option<(u64, usize, usize, u32)> {
    VRAM_ALLOC.lock().as_ref().map(|v| {
        (v.base(), v.used(), v.free(), v.alloc_count())
    })
}
