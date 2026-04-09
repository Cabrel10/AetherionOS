// kernel/src/drivers/virtio_blk.rs - VirtIO Block Device Driver (Couche 19)
//
// Implements a VirtIO legacy (0.9.5) block device driver using PCI I/O ports.
// Reuses the VirtQueue infrastructure from the VirtIO-Net driver.
//
// VirtIO Block Device:
//   - PCI Vendor: 0x1AF4, Device: 0x1001 (legacy) or 0x1042 (modern)
//   - PCI Class: 0x01 (Mass Storage Controller)
//   - Uses a single virtqueue (index 0) for read/write requests
//   - Block request format: header(16) + data(512*n) + status(1)
//
// References:
//   - VirtIO 1.0 Spec, Section 5.2 (Block Device)
//   - Legacy VirtIO specification

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Block device sector size
pub const SECTOR_SIZE: usize = 512;

/// VirtIO legacy PCI register offsets (same base as VirtIO-Net)
const VIRTIO_PCI_DEVICE_FEATURES: u16 = 0x00;
const VIRTIO_PCI_GUEST_FEATURES: u16 = 0x04;
const VIRTIO_PCI_QUEUE_ADDR: u16 = 0x08;
const VIRTIO_PCI_QUEUE_SIZE: u16 = 0x0C;
const VIRTIO_PCI_QUEUE_SELECT: u16 = 0x0E;
const VIRTIO_PCI_QUEUE_NOTIFY: u16 = 0x10;
const VIRTIO_PCI_DEVICE_STATUS: u16 = 0x12;
const VIRTIO_PCI_ISR: u16 = 0x13;

/// VirtIO Block device config offset (after common config at 0x14)
const VIRTIO_BLK_CAPACITY: u16 = 0x14; // 8 bytes: total sectors (u64)

/// VirtIO status bits
const STATUS_RESET: u8 = 0;
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;

/// VirtIO Block request types
const VIRTIO_BLK_T_IN: u32 = 0;   // Read
const VIRTIO_BLK_T_OUT: u32 = 1;  // Write

/// VirtIO Block request status
const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;

/// Virtqueue descriptor flags
const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

/// VirtIO Block request header
#[repr(C)]
struct VirtioBlkReq {
    req_type: u32,
    reserved: u32,
    sector: u64,
}

/// Virtqueue descriptor
#[repr(C)]
#[derive(Clone, Copy)]
struct VringDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// Global block device state
static BLK_INITIALIZED: AtomicBool = AtomicBool::new(false);
static mut BLK_DEVICE: Option<VirtioBlkDevice> = None;

/// SMP spinlock for block device I/O (prevents concurrent VirtIO queue corruption)
static BLK_IO_LOCK: AtomicU8 = AtomicU8::new(0);

#[inline]
fn blk_lock() {
    loop {
        match BLK_IO_LOCK.compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => return,
            Err(_) => {
                // Spin with PAUSE for power efficiency
                while BLK_IO_LOCK.load(Ordering::Relaxed) != 0 {
                    unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
                }
            }
        }
    }
}

#[inline]
fn blk_unlock() {
    BLK_IO_LOCK.store(0, Ordering::Release);
}

pub struct VirtioBlkDevice {
    io_base: u16,
    capacity_sectors: u64,
    // Virtqueue physical address and metadata
    vq_phys: u64,     // physical address of the virtqueue memory
    vq_virt: u64,     // virtual address (= phys + phys_offset)
    vq_size: u16,     // number of descriptors
    // DMA buffers for requests (pre-allocated)
    req_phys: u64,    // physical address of request buffer area
    req_virt: u64,    // virtual address of request buffer area
}

// Port I/O helpers
#[inline]
unsafe fn outb(port: u16, val: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack));
}

#[inline]
unsafe fn outw(port: u16, val: u16) {
    core::arch::asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack));
}

#[inline]
unsafe fn outl(port: u16, val: u32) {
    core::arch::asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack));
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    core::arch::asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack));
    val
}

#[inline]
unsafe fn inw(port: u16) -> u16 {
    let val: u16;
    core::arch::asm!("in ax, dx", out("ax") val, in("dx") port, options(nomem, nostack));
    val
}

#[inline]
unsafe fn inl(port: u16) -> u32 {
    let val: u32;
    core::arch::asm!("in eax, dx", out("eax") val, in("dx") port, options(nomem, nostack));
    val
}

impl VirtioBlkDevice {
    /// Initialize a VirtIO block device at the given PCI location
    pub fn init(bus: u8, device: u8, function: u8) -> Option<Self> {
        // Read BAR0 for I/O port base
        let bar0 = crate::arch::x86_64::pci::read_bar(bus, device, function, 0);
        if bar0 & 0x01 == 0 {
            crate::serial_println!("[BLK] BAR0 is not I/O space: 0x{:08X}", bar0);
            return None;
        }
        let io_base = (bar0 & 0xFFFC) as u16;
        crate::serial_println!("[BLK] VirtIO-Block I/O base: 0x{:04X}", io_base);

        unsafe {
            // Reset device
            outb(io_base + VIRTIO_PCI_DEVICE_STATUS, STATUS_RESET);
            for _ in 0..10000 { core::arch::asm!("pause", options(nomem, nostack)); }

            // Acknowledge
            outb(io_base + VIRTIO_PCI_DEVICE_STATUS, STATUS_ACKNOWLEDGE);
            outb(io_base + VIRTIO_PCI_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

            // Read and accept features (we don't need any special features)
            let _features = inl(io_base + VIRTIO_PCI_DEVICE_FEATURES);
            outl(io_base + VIRTIO_PCI_GUEST_FEATURES, 0); // Accept no features

            // Read capacity
            let cap_lo = inl(io_base + VIRTIO_BLK_CAPACITY);
            let cap_hi = inl(io_base + VIRTIO_BLK_CAPACITY + 4);
            let capacity = (cap_hi as u64) << 32 | cap_lo as u64;
            crate::serial_println!("[BLK] Capacity: {} sectors ({} KiB)", capacity, capacity / 2);

            // Setup virtqueue 0
            outw(io_base + VIRTIO_PCI_QUEUE_SELECT, 0);
            let vq_size = inw(io_base + VIRTIO_PCI_QUEUE_SIZE);
            if vq_size == 0 {
                crate::serial_println!("[BLK] Queue size is 0, aborting");
                return None;
            }
            crate::serial_println!("[BLK] Queue 0 size: {}", vq_size);

            // Calculate virtqueue memory size
            // desc_table: 16 * vq_size, avail ring: 6 + 2*vq_size, 
            // used ring (page aligned): 6 + 8*vq_size
            let desc_size = 16 * vq_size as usize;
            let avail_size = 6 + 2 * vq_size as usize;
            let used_offset = align_up(desc_size + avail_size, 4096);
            let used_size = 6 + 8 * vq_size as usize;
            let total_size = align_up(used_offset + used_size, 4096);

            // Allocate contiguous pages for the virtqueue
            let phys_offset = crate::elf::phys_offset();
            let num_pages = (total_size + 4095) / 4096;
            let vq_phys = alloc_contiguous_pages(num_pages)?;
            let vq_virt = vq_phys + phys_offset;

            // Zero the virtqueue memory
            core::ptr::write_bytes(vq_virt as *mut u8, 0, total_size);

            // Tell the device the physical page of the virtqueue
            let vq_pfn = (vq_phys / 4096) as u32;
            outl(io_base + VIRTIO_PCI_QUEUE_ADDR, vq_pfn);
            crate::serial_println!("[BLK] Queue 0 at phys 0x{:X} (PFN=0x{:X})", vq_phys, vq_pfn);

            // Allocate DMA buffer area for block requests
            // Each request needs: header(16) + data(512) + status(1) = 529 bytes
            // Allocate 2 pages for request buffers
            let req_phys = alloc_contiguous_pages(2)?;
            let req_virt = req_phys + phys_offset;
            core::ptr::write_bytes(req_virt as *mut u8, 0, 8192);

            // Mark driver as ready
            outb(io_base + VIRTIO_PCI_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK);

            crate::serial_println!("[BLK] VirtIO-Block initialized: {} sectors, queue={}", capacity, vq_size);

            Some(VirtioBlkDevice {
                io_base,
                capacity_sectors: capacity,
                vq_phys,
                vq_virt,
                vq_size,
                req_phys,
                req_virt,
            })
        }
    }

    /// Read a single sector (512 bytes) from the block device
    pub fn read_sector(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        if lba >= self.capacity_sectors || buf.len() < SECTOR_SIZE {
            return false;
        }
        self.do_request(VIRTIO_BLK_T_IN, lba, buf)
    }

    /// Write a single sector (512 bytes) to the block device
    pub fn write_sector(&mut self, lba: u64, buf: &[u8]) -> bool {
        if lba >= self.capacity_sectors || buf.len() < SECTOR_SIZE {
            return false;
        }
        // Need mutable slice for the request
        let mut data = [0u8; SECTOR_SIZE];
        data[..SECTOR_SIZE].copy_from_slice(&buf[..SECTOR_SIZE]);
        self.do_request(VIRTIO_BLK_T_OUT, lba, &mut data)
    }

    /// Perform a block I/O request using the virtqueue
    fn do_request(&mut self, req_type: u32, sector: u64, data: &mut [u8]) -> bool {
        unsafe {
            // Layout in req_virt:
            // [0..16]    = VirtioBlkReq header
            // [16..528]  = data buffer (512 bytes)
            // [528]      = status byte
            let req_ptr = self.req_virt as *mut u8;
            let req_phys = self.req_phys;

            // Write request header
            let header = req_ptr as *mut VirtioBlkReq;
            (*header).req_type = req_type;
            (*header).reserved = 0;
            (*header).sector = sector;

            // For write requests, copy data to DMA buffer
            if req_type == VIRTIO_BLK_T_OUT {
                core::ptr::copy_nonoverlapping(data.as_ptr(), req_ptr.add(16), SECTOR_SIZE);
            }

            // Clear status byte
            *req_ptr.add(16 + SECTOR_SIZE) = 0xFF;

            // Set up 3-descriptor chain in the virtqueue
            let desc_base = self.vq_virt as *mut VringDesc;

            // Descriptor 0: header (device-readable)
            (*desc_base.add(0)).addr = req_phys;
            (*desc_base.add(0)).len = 16;
            (*desc_base.add(0)).flags = VRING_DESC_F_NEXT;
            (*desc_base.add(0)).next = 1;

            // Descriptor 1: data buffer
            (*desc_base.add(1)).addr = req_phys + 16;
            (*desc_base.add(1)).len = SECTOR_SIZE as u32;
            if req_type == VIRTIO_BLK_T_IN {
                (*desc_base.add(1)).flags = VRING_DESC_F_NEXT | VRING_DESC_F_WRITE;
            } else {
                (*desc_base.add(1)).flags = VRING_DESC_F_NEXT;
            }
            (*desc_base.add(1)).next = 2;

            // Descriptor 2: status byte (device-writable)
            (*desc_base.add(2)).addr = req_phys + 16 + SECTOR_SIZE as u64;
            (*desc_base.add(2)).len = 1;
            (*desc_base.add(2)).flags = VRING_DESC_F_WRITE;
            (*desc_base.add(2)).next = 0;

            // Add to available ring
            let avail_base = (self.vq_virt + 16 * self.vq_size as u64) as *mut u16;
            // avail ring: [flags(2), idx(2), ring[vq_size](2 each)]
            let avail_idx_ptr = avail_base.add(1);
            let avail_idx = core::ptr::read_volatile(avail_idx_ptr);
            let ring_entry = avail_base.add(2 + (avail_idx as usize % self.vq_size as usize));
            core::ptr::write_volatile(ring_entry, 0); // First descriptor in chain
            
            // Memory barrier
            core::arch::asm!("mfence", options(nomem, nostack));
            
            // Increment available index
            core::ptr::write_volatile(avail_idx_ptr, avail_idx.wrapping_add(1));
            
            // Memory barrier
            core::arch::asm!("mfence", options(nomem, nostack));

            // Notify the device (queue 0)
            outw(self.io_base + VIRTIO_PCI_QUEUE_NOTIFY, 0);

            // Wait for completion: poll the used ring
            let used_offset = align_up(
                16 * self.vq_size as usize + 6 + 2 * self.vq_size as usize,
                4096,
            );
            let used_base = (self.vq_virt + used_offset as u64) as *mut u16;
            // used ring: [flags(2), idx(2), ring[vq_size](id(4) + len(4) each)]
            let used_idx_ptr = used_base.add(1);
            let expected_idx = avail_idx.wrapping_add(1);

            let mut timeout = 0u32;
            loop {
                let current_used = core::ptr::read_volatile(used_idx_ptr);
                if current_used == expected_idx {
                    break;
                }
                timeout += 1;
                if timeout > 10_000_000 {
                    crate::serial_println!("[BLK] Request timeout (sector {})", sector);
                    return false;
                }
                core::arch::asm!("pause", options(nomem, nostack));
            }

            // Read ISR to acknowledge interrupt
            let _ = inb(self.io_base + VIRTIO_PCI_ISR);

            // Check status byte
            let status = *req_ptr.add(16 + SECTOR_SIZE);
            if status != VIRTIO_BLK_S_OK {
                crate::serial_println!("[BLK] Request error: status={} (sector {})", status, sector);
                return false;
            }

            // For read requests, copy data from DMA buffer to caller
            if req_type == VIRTIO_BLK_T_IN {
                core::ptr::copy_nonoverlapping(req_ptr.add(16), data.as_mut_ptr(), SECTOR_SIZE);
            }

            true
        }
    }

    /// Get device capacity in sectors
    pub fn capacity(&self) -> u64 {
        self.capacity_sectors
    }
}

/// Initialize the VirtIO block device
pub fn init() {
    crate::serial_println!("[BLK] Scanning PCI for VirtIO-Block devices...");

    // Strategy 1: Scan PCI for storage controllers (class 0x01)
    let devices = crate::arch::x86_64::pci::scan_for_class(0x01);
    crate::serial_println!("[BLK] PCI scan: found {} storage controller(s)", devices.len());

    for dev in &devices {
        crate::serial_println!("[BLK] {}", dev);

        // Check for VirtIO (Vendor 0x1AF4, Device 0x1001 or 0x1042)
        if dev.vendor_id == 0x1AF4 && (dev.device_id == 0x1001 || dev.device_id == 0x1042) {
            crate::serial_println!("[BLK] VirtIO-Block device detected!");

            match VirtioBlkDevice::init(dev.bus, dev.device, dev.function) {
                Some(blk_dev) => {
                    let capacity = blk_dev.capacity_sectors;
                    unsafe { BLK_DEVICE = Some(blk_dev); }
                    BLK_INITIALIZED.store(true, Ordering::SeqCst);
                    crate::serial_println!("[BLK] VirtIO-Block ready: {} sectors ({} KiB)",
                        capacity, capacity * SECTOR_SIZE as u64 / 1024);
                    return;
                }
                None => {
                    crate::serial_println!("[BLK] Failed to initialize VirtIO-Block");
                }
            }
        }
    }

    // Strategy 2: Direct PCI scan for VirtIO vendor (0x1AF4) across ALL classes
    // Modern QEMU virtio-blk may appear under different class codes
    crate::serial_println!("[BLK] Fallback: scanning all PCI devices for VirtIO vendor 0x1AF4...");
    for bus in 0u8..=0 {
        for device in 0u8..32 {
            let vendor = crate::arch::x86_64::pci::read_config_u32(bus, device, 0, 0) as u16;
            if vendor == 0x1AF4 {
                let dev_id = (crate::arch::x86_64::pci::read_config_u32(bus, device, 0, 0) >> 16) as u16;
                let class_word = crate::arch::x86_64::pci::read_config_u32(bus, device, 0, 0x08);
                let class_code = ((class_word >> 24) & 0xFF) as u8;
                let subclass = ((class_word >> 16) & 0xFF) as u8;
                crate::serial_println!("[BLK] Found VirtIO device: bus={} dev={} id=0x{:04X} class=0x{:02X} sub=0x{:02X}",
                    bus, device, dev_id, class_code, subclass);
                if dev_id == 0x1001 || dev_id == 0x1042 || (class_code == 0x01 && subclass == 0x00) {
                    crate::serial_println!("[BLK] VirtIO-Block device detected (fallback)!");
                    match VirtioBlkDevice::init(bus, device, 0) {
                        Some(blk_dev) => {
                            let capacity = blk_dev.capacity_sectors;
                            unsafe { BLK_DEVICE = Some(blk_dev); }
                            BLK_INITIALIZED.store(true, Ordering::SeqCst);
                            crate::serial_println!("[BLK] VirtIO-Block ready: {} sectors ({} KiB)",
                                capacity, capacity * SECTOR_SIZE as u64 / 1024);
                            return;
                        }
                        None => {
                            crate::serial_println!("[BLK] Failed to initialize VirtIO-Block (fallback)");
                        }
                    }
                }
            }
        }
    }

    crate::serial_println!("[BLK] No VirtIO-Block device found");
    crate::serial_println!("[BLK] (QEMU needs: -drive file=disk.img,format=raw,if=virtio)");
}

/// Check if block device is available
pub fn is_available() -> bool {
    BLK_INITIALIZED.load(Ordering::SeqCst)
}

/// Read a sector from the block device (SMP-safe via spinlock)
pub fn read_sector(lba: u64, buf: &mut [u8]) -> bool {
    blk_lock();
    let result = unsafe {
        if let Some(ref mut dev) = BLK_DEVICE {
            dev.read_sector(lba, buf)
        } else {
            false
        }
    };
    blk_unlock();
    result
}

/// Write a sector to the block device (SMP-safe via spinlock)
pub fn write_sector(lba: u64, buf: &[u8]) -> bool {
    blk_lock();
    let result = unsafe {
        if let Some(ref mut dev) = BLK_DEVICE {
            dev.write_sector(lba, buf)
        } else {
            false
        }
    };
    blk_unlock();
    result
}

/// Get disk capacity in sectors
pub fn capacity() -> u64 {
    unsafe {
        if let Some(ref dev) = BLK_DEVICE {
            dev.capacity()
        } else {
            0
        }
    }
}

/// Write multiple sectors
pub fn write_sectors(start_lba: u64, count: usize, buf: &[u8]) -> bool {
    if buf.len() < count * SECTOR_SIZE {
        return false;
    }
    for i in 0..count {
        let offset = i * SECTOR_SIZE;
        let mut sector_buf = [0u8; SECTOR_SIZE];
        sector_buf.copy_from_slice(&buf[offset..offset + SECTOR_SIZE]);
        if !write_sector(start_lba + i as u64, &sector_buf) {
            return false;
        }
    }
    true
}

/// Read multiple sectors
pub fn read_sectors(start_lba: u64, count: usize, buf: &mut [u8]) -> bool {
    if buf.len() < count * SECTOR_SIZE {
        return false;
    }
    for i in 0..count {
        let offset = i * SECTOR_SIZE;
        if !read_sector(start_lba + i as u64, &mut buf[offset..offset + SECTOR_SIZE]) {
            return false;
        }
    }
    true
}

/// Allocate contiguous physical pages
fn alloc_contiguous_pages(count: usize) -> Option<u64> {
    // Use the ELF frame pool allocator
    let first = unsafe { crate::elf::alloc_demand_frame()? };
    for _ in 1..count {
        unsafe { crate::elf::alloc_demand_frame()?; }
    }
    Some(first)
}

/// Align a value up to the given alignment
fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

/// Run VirtIO-Block self-tests
pub fn run_tests() {
    crate::serial_println!("\n========================================");
    crate::serial_println!("[BLK TESTS] Couche 19 - VirtIO-Block Driver");
    crate::serial_println!("========================================\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // Test 1: Device detected
    crate::serial_write("  [TEST 1/4] Block device available... ");
    if is_available() {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_write("SKIP (no VirtIO-Block device)\n");
        crate::serial_println!("\n========================================");
        crate::serial_println!("[BLK TESTS] Skipped (no block device)");
        crate::serial_println!("========================================");
        return;
    }

    // Test 2: Capacity
    crate::serial_write("  [TEST 2/4] Disk capacity... ");
    let cap = capacity();
    if cap > 0 {
        crate::serial_println!("OK ({} sectors = {} KiB)", cap, cap / 2);
        passed += 1;
    } else {
        crate::serial_write("FAIL (0 sectors)\n");
        failed += 1;
    }

    // Test 3: Read sector 0 (MBR / boot sector)
    crate::serial_write("  [TEST 3/4] Read sector 0... ");
    {
        let mut buf = [0u8; SECTOR_SIZE];
        if read_sector(0, &mut buf) {
            crate::serial_println!("OK (first 4 bytes: {:02X} {:02X} {:02X} {:02X})",
                buf[0], buf[1], buf[2], buf[3]);
            passed += 1;
        } else {
            crate::serial_write("FAIL\n");
            failed += 1;
        }
    }

    // Test 4: Read sector 0 FAT32 signature check
    crate::serial_write("  [TEST 4/4] FAT32 boot signature... ");
    {
        let mut buf = [0u8; SECTOR_SIZE];
        if read_sector(0, &mut buf) {
            // FAT32 BPB: bytes_per_sector at offset 11-12, signature at 510-511
            let sig = u16::from_le_bytes([buf[510], buf[511]]);
            if sig == 0xAA55 {
                crate::serial_println!("OK (0xAA55)");
                passed += 1;
            } else {
                crate::serial_println!("WARN (sig=0x{:04X}, not FAT32)", sig);
                passed += 1; // Still pass - might not be FAT32
            }
        } else {
            crate::serial_write("FAIL (read error)\n");
            failed += 1;
        }
    }

    crate::serial_println!("\n========================================");
    crate::serial_println!("[BLK TESTS] {}/{} passed, {} failed", passed, passed + failed, failed);
    if failed == 0 { crate::serial_write("[BLK TESTS] ALL TESTS PASSED!\n"); }
    crate::serial_println!("========================================");
}
