// kernel/src/net/virtio_net.rs - VirtIO-Net Driver (Couche 17)
//
// VirtIO Network Device Specification v1.0
// PCI Vendor: 0x1AF4, Legacy Device ID: 0x1000, Modern: 0x1041
//
// Uses legacy (transitional) VirtIO PCI interface with I/O port BAR.
//
// VirtIO registers (offset from BAR0 I/O base):
//   0x00   Device Features (32-bit, R)
//   0x04   Guest Features (32-bit, W)
//   0x08   Queue Address (32-bit, W) - physical page number
//   0x0C   Queue Size (16-bit, R)
//   0x0E   Queue Select (16-bit, W)
//   0x10   Queue Notify (16-bit, W)
//   0x12   Device Status (8-bit, RW)
//   0x13   ISR Status (8-bit, R)
//   0x14+  Device-specific: MAC address (6 bytes), Status (2 bytes)
//
// VirtQueues:
//   Queue 0: Receive (RX)
//   Queue 1: Transmit (TX)
//
// Each VirtQueue entry:
//   Descriptor Table: (addr, len, flags, next) * queue_size
//   Available Ring: (flags, idx, ring[queue_size])
//   Used Ring: (flags, idx, ring[queue_size] of (id, len))

use core::arch::asm;
use alloc::vec::Vec;
use alloc::vec;
use super::ethernet::MacAddress;

// ===== VirtIO Status Bits =====
const VIRTIO_STATUS_RESET: u8 = 0;
const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
const VIRTIO_STATUS_DRIVER: u8 = 2;
const VIRTIO_STATUS_FEATURES_OK: u8 = 8;
const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
const VIRTIO_STATUS_FAILED: u8 = 128;

// ===== VirtIO Feature Bits =====
const VIRTIO_NET_F_MAC: u32 = 1 << 5;
const VIRTIO_NET_F_STATUS: u32 = 1 << 16;

// ===== VirtQueue Descriptor Flags =====
const VRING_DESC_F_NEXT: u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

// ===== VirtIO Net Header =====
/// Every packet sent/received via VirtIO-Net is prefixed with this header
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VirtioNetHeader {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
    // In mergeable buffers mode: num_buffers: u16
}

impl VirtioNetHeader {
    pub const LEN: usize = 10;

    pub fn empty() -> Self {
        VirtioNetHeader {
            flags: 0,
            gso_type: 0,
            hdr_len: 0,
            gso_size: 0,
            csum_start: 0,
            csum_offset: 0,
        }
    }
}

// ===== VirtQueue Descriptor =====
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct VringDesc {
    addr: u64,   // Physical address
    len: u32,    // Length
    flags: u16,  // VRING_DESC_F_*
    next: u16,   // Next descriptor index
}

// ===== Port I/O Helpers =====
#[inline]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack));
    val
}

#[inline]
unsafe fn inw(port: u16) -> u16 {
    let val: u16;
    asm!("in ax, dx", out("ax") val, in("dx") port, options(nomem, nostack));
    val
}

#[inline]
unsafe fn inl(port: u16) -> u32 {
    let val: u32;
    asm!("in eax, dx", out("eax") val, in("dx") port, options(nomem, nostack));
    val
}

#[inline]
unsafe fn outb(port: u16, val: u8) {
    asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack));
}

#[inline]
unsafe fn outw(port: u16, val: u16) {
    asm!("out dx, ax", in("dx") port, in("ax") val, options(nomem, nostack));
}

#[inline]
unsafe fn outl(port: u16, val: u32) {
    asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack));
}

// ===== VirtQueue =====
const QUEUE_SIZE: u16 = 256;

/// Memory layout for one VirtQueue
/// Allocated as a contiguous physical region
struct VirtQueue {
    /// I/O base of the VirtIO device
    io_base: u16,
    /// Queue index (0=RX, 1=TX)
    queue_idx: u16,
    /// Base physical address of the queue memory
    base_phys: u64,
    /// Base virtual address of the queue memory
    base_virt: u64,
    /// Number of descriptors
    num: u16,
    /// Next free descriptor index
    free_head: u16,
    /// Number of free descriptors
    free_count: u16,
    /// Last seen used index
    last_used_idx: u16,
}

impl VirtQueue {
    /// Calculate required memory size for a VirtQueue
    fn mem_size(num: u16) -> usize {
        let desc_size = (num as usize) * core::mem::size_of::<VringDesc>();
        let avail_size = 6 + 2 * (num as usize); // flags(2) + idx(2) + ring(2*n) + used_event(2)
        let used_size = 6 + 8 * (num as usize);  // flags(2) + idx(2) + ring(8*n) + avail_event(2)
        // Available ring must follow descriptors, Used ring aligned to 4096
        let avail_end = desc_size + avail_size;
        let used_start = (avail_end + 4095) & !4095; // Page-align the Used ring
        used_start + used_size
    }

    /// Get pointer to descriptor table
    fn desc_ptr(&self) -> *mut VringDesc {
        self.base_virt as *mut VringDesc
    }

    /// Get pointer to available ring flags
    fn avail_flags_ptr(&self) -> *mut u16 {
        let desc_end = self.base_virt + (self.num as u64) * core::mem::size_of::<VringDesc>() as u64;
        desc_end as *mut u16
    }

    /// Get pointer to available ring idx
    fn avail_idx_ptr(&self) -> *mut u16 {
        unsafe { self.avail_flags_ptr().add(1) }
    }

    /// Get pointer to available ring entry i
    fn avail_ring_ptr(&self, i: u16) -> *mut u16 {
        unsafe { self.avail_flags_ptr().add(2 + i as usize) }
    }

    /// Get pointer to used ring flags
    fn used_flags_ptr(&self) -> *const u16 {
        let desc_size = (self.num as usize) * core::mem::size_of::<VringDesc>();
        let avail_size = 6 + 2 * (self.num as usize);
        let avail_end = desc_size + avail_size;
        let used_start = (avail_end + 4095) & !4095;
        (self.base_virt + used_start as u64) as *const u16
    }

    /// Get pointer to used ring idx
    fn used_idx_ptr(&self) -> *const u16 {
        unsafe { self.used_flags_ptr().add(1) }
    }

    /// Get pointer to used ring entry i (id: u32, len: u32)
    fn used_ring_entry(&self, i: u16) -> (*const u32, *const u32) {
        let base = unsafe { self.used_flags_ptr().add(2) } as *const u32;
        unsafe {
            let id = base.add(2 * i as usize);
            let len = base.add(2 * i as usize + 1);
            (id, len)
        }
    }

    /// Allocate a free descriptor and return its index
    fn alloc_desc(&mut self) -> Option<u16> {
        if self.free_count == 0 {
            return None;
        }
        let idx = self.free_head;
        let desc = unsafe { &*self.desc_ptr().add(idx as usize) };
        self.free_head = desc.next;
        self.free_count -= 1;
        Some(idx)
    }

    /// Free a descriptor chain
    fn free_desc(&mut self, head: u16) {
        let mut idx = head;
        loop {
            let desc = unsafe { &mut *self.desc_ptr().add(idx as usize) };
            let has_next = desc.flags & VRING_DESC_F_NEXT != 0;
            let next = desc.next;
            desc.next = self.free_head;
            self.free_head = idx;
            self.free_count += 1;
            if has_next {
                idx = next;
            } else {
                break;
            }
        }
    }

    /// Submit a single buffer to the available ring
    fn submit(&mut self, desc_idx: u16) {
        unsafe {
            let avail_idx = core::ptr::read_volatile(self.avail_idx_ptr());
            let ring_idx = avail_idx % self.num;
            core::ptr::write_volatile(self.avail_ring_ptr(ring_idx), desc_idx);
            // Memory barrier
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            core::ptr::write_volatile(self.avail_idx_ptr(), avail_idx.wrapping_add(1));
            // Notify device
            outw(self.io_base + 0x10, self.queue_idx);
        }
    }
}

// ===== VirtIO-Net Device =====

/// RX buffer pool
const RX_BUF_SIZE: usize = 2048;
const RX_BUF_COUNT: usize = 64;

/// TX buffer
const TX_BUF_SIZE: usize = 2048;

pub struct VirtioNetDevice {
    /// PCI I/O base address (from BAR0)
    io_base: u16,
    /// MAC address of the device
    pub mac: MacAddress,
    /// RX VirtQueue
    rx_queue: VirtQueue,
    /// TX VirtQueue
    tx_queue: VirtQueue,
    /// RX buffer pool - physical addresses
    rx_bufs_phys: [u64; RX_BUF_COUNT],
    /// RX buffer pool - virtual addresses
    rx_bufs_virt: [u64; RX_BUF_COUNT],
    /// TX buffer - physical address
    tx_buf_phys: u64,
    /// TX buffer - virtual address
    tx_buf_virt: u64,
    /// Statistics
    pub tx_packets: u64,
    pub rx_packets: u64,
    pub tx_bytes: u64,
    pub rx_bytes: u64,
    /// Device initialized
    pub initialized: bool,
}

impl VirtioNetDevice {
    /// Initialize the VirtIO-Net device from a PCI device
    pub fn init(bus: u8, device: u8, function: u8) -> Option<Self> {
        use crate::arch::x86_64::pci;

        // Read BAR0 (I/O port base)
        let bar0 = pci::read_bar(bus, device, function, 0);
        if bar0 & 0x01 == 0 {
            // BAR0 is memory-mapped, not I/O
            crate::serial_println!("[VIRTIO-NET] BAR0 is MMIO (0x{:08X}), need I/O port", bar0);
            return None;
        }
        let io_base = (bar0 & 0xFFFC) as u16;
        crate::serial_println!("[VIRTIO-NET] BAR0 I/O base: 0x{:04X}", io_base);

        // Enable PCI bus mastering (command register offset 0x04)
        let cmd = pci::read_config_u32(bus, device, function, 0x04);
        let new_cmd = cmd | 0x04; // Set Bus Master bit
        pci::write_config_u32(bus, device, function, 0x04, new_cmd);
        crate::serial_println!("[VIRTIO-NET] PCI bus mastering enabled");

        // === VirtIO Device Initialization Sequence ===

        // 1. Reset
        unsafe { outb(io_base + 0x12, VIRTIO_STATUS_RESET); }

        // 2. Acknowledge
        unsafe { outb(io_base + 0x12, VIRTIO_STATUS_ACKNOWLEDGE); }

        // 3. Driver
        unsafe { outb(io_base + 0x12, VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER); }

        // 4. Read device features
        let device_features = unsafe { inl(io_base + 0x00) };
        crate::serial_println!("[VIRTIO-NET] Device features: 0x{:08X}", device_features);

        // Accept MAC feature
        let guest_features = device_features & (VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS);
        unsafe { outl(io_base + 0x04, guest_features); }

        // 5. Features OK
        unsafe {
            let status = inb(io_base + 0x12);
            outb(io_base + 0x12, status | VIRTIO_STATUS_FEATURES_OK);
        }

        // 6. Read MAC address from device config (offset 0x14..0x19)
        let mut mac_bytes = [0u8; 6];
        for i in 0..6 {
            mac_bytes[i] = unsafe { inb(io_base + 0x14 + i as u16) };
        }
        let mac = MacAddress(mac_bytes);
        crate::serial_println!("[VIRTIO-NET] MAC address: {}", mac);

        // 7. Setup VirtQueues (RX=0, TX=1)
        let phys_offset = crate::elf::phys_offset();

        // --- Setup RX Queue (index 0) ---
        let rx_queue = Self::setup_queue(io_base, 0, phys_offset)?;
        // --- Setup TX Queue (index 1) ---
        let tx_queue = Self::setup_queue(io_base, 1, phys_offset)?;

        // 8. Allocate RX buffers
        let rx_bufs_phys = [0u64; RX_BUF_COUNT];
        let rx_bufs_virt = [0u64; RX_BUF_COUNT];

        // Allocate TX buffer
        let tx_phys = unsafe { crate::elf::alloc_demand_frame()? };
        let tx_virt = tx_phys + phys_offset;

        // We'll fill RX queue with buffers after full init
        let mut dev = VirtioNetDevice {
            io_base,
            mac,
            rx_queue,
            tx_queue,
            rx_bufs_phys,
            rx_bufs_virt,
            tx_buf_phys: tx_phys,
            tx_buf_virt: tx_virt,
            tx_packets: 0,
            rx_packets: 0,
            tx_bytes: 0,
            rx_bytes: 0,
            initialized: false,
        };

        // Populate RX queue with buffers
        for i in 0..RX_BUF_COUNT.min(dev.rx_queue.num as usize / 2) {
            if let Some(phys) = unsafe { crate::elf::alloc_demand_frame() } {
                let virt = phys + phys_offset;
                dev.rx_bufs_phys[i] = phys;
                dev.rx_bufs_virt[i] = virt;

                // Zero the buffer
                unsafe {
                    core::ptr::write_bytes(virt as *mut u8, 0, 4096);
                }

                // Create descriptor for this RX buffer
                if let Some(desc_idx) = dev.rx_queue.alloc_desc() {
                    unsafe {
                        let desc = &mut *dev.rx_queue.desc_ptr().add(desc_idx as usize);
                        desc.addr = phys;
                        desc.len = RX_BUF_SIZE as u32;
                        desc.flags = VRING_DESC_F_WRITE; // Device writes to this buffer
                        desc.next = 0;
                    }
                    dev.rx_queue.submit(desc_idx);
                }
            }
        }

        // 9. Driver OK
        unsafe {
            let status = inb(io_base + 0x12);
            outb(io_base + 0x12, status | VIRTIO_STATUS_DRIVER_OK);
        }

        let final_status = unsafe { inb(io_base + 0x12) };
        crate::serial_println!("[VIRTIO-NET] Device status: 0x{:02X} (DRIVER_OK)", final_status);

        dev.initialized = true;
        Some(dev)
    }

    /// Setup a single VirtQueue
    fn setup_queue(io_base: u16, queue_idx: u16, phys_offset: u64) -> Option<VirtQueue> {
        unsafe {
            // Select queue
            outw(io_base + 0x0E, queue_idx);

            // Read queue size
            let queue_size = inw(io_base + 0x0C);
            if queue_size == 0 {
                crate::serial_println!("[VIRTIO-NET] Queue {} size is 0!", queue_idx);
                return None;
            }
            let effective_size = queue_size.min(QUEUE_SIZE);
            crate::serial_println!("[VIRTIO-NET] Queue {} size: {}", queue_idx, effective_size);

            // Allocate memory for the queue
            let mem_size = VirtQueue::mem_size(effective_size);
            let num_pages = (mem_size + 4095) / 4096;
            let mut base_phys = 0u64;
            for i in 0..num_pages {
                let frame = crate::elf::alloc_demand_frame()?;
                if i == 0 { base_phys = frame; }
                // Zero the frame
                core::ptr::write_bytes((frame + phys_offset) as *mut u8, 0, 4096);
            }

            let base_virt = base_phys + phys_offset;

            // Initialize free list
            let desc_ptr = base_virt as *mut VringDesc;
            for i in 0..effective_size {
                let desc = &mut *desc_ptr.add(i as usize);
                desc.addr = 0;
                desc.len = 0;
                desc.flags = 0;
                desc.next = if i + 1 < effective_size { i + 1 } else { 0 };
            }

            // Tell device the physical page number of this queue
            let pfn = (base_phys / 4096) as u32;
            outl(io_base + 0x08, pfn);

            Some(VirtQueue {
                io_base,
                queue_idx,
                base_phys,
                base_virt,
                num: effective_size,
                free_head: 0,
                free_count: effective_size,
                last_used_idx: 0,
            })
        }
    }

    /// Send a raw Ethernet frame
    pub fn transmit(&mut self, frame: &[u8]) -> bool {
        if !self.initialized || frame.len() > TX_BUF_SIZE - VirtioNetHeader::LEN {
            return false;
        }

        // Get a free descriptor
        let desc_idx = match self.tx_queue.alloc_desc() {
            Some(idx) => idx,
            None => {
                // Try to reclaim used descriptors
                self.reclaim_tx();
                match self.tx_queue.alloc_desc() {
                    Some(idx) => idx,
                    None => return false,
                }
            }
        };

        unsafe {
            // Write VirtIO-Net header + frame into TX buffer
            let buf = self.tx_buf_virt as *mut u8;
            // Zero the header
            core::ptr::write_bytes(buf, 0, VirtioNetHeader::LEN);
            // Copy frame data after header
            core::ptr::copy_nonoverlapping(
                frame.as_ptr(),
                buf.add(VirtioNetHeader::LEN),
                frame.len(),
            );

            // Setup descriptor
            let desc = &mut *self.tx_queue.desc_ptr().add(desc_idx as usize);
            desc.addr = self.tx_buf_phys;
            desc.len = (VirtioNetHeader::LEN + frame.len()) as u32;
            desc.flags = 0; // Device reads from this buffer
            desc.next = 0;
        }

        // Submit to available ring
        self.tx_queue.submit(desc_idx);

        self.tx_packets += 1;
        self.tx_bytes += frame.len() as u64;
        true
    }

    /// Try to receive a packet
    pub fn receive(&mut self) -> Option<Vec<u8>> {
        let used_idx = unsafe { core::ptr::read_volatile(self.rx_queue.used_idx_ptr()) };
        if used_idx == self.rx_queue.last_used_idx {
            return None; // No new packets
        }

        let ring_idx = self.rx_queue.last_used_idx % self.rx_queue.num;
        let (id_ptr, len_ptr) = self.rx_queue.used_ring_entry(ring_idx);

        let desc_idx = unsafe { core::ptr::read_volatile(id_ptr) } as u16;
        let total_len = unsafe { core::ptr::read_volatile(len_ptr) } as usize;

        if total_len <= VirtioNetHeader::LEN {
            // Too short, recycle buffer
            self.rx_queue.last_used_idx = self.rx_queue.last_used_idx.wrapping_add(1);
            return None;
        }

        // Read the frame data (skip VirtIO-Net header)
        let desc = unsafe { &*self.rx_queue.desc_ptr().add(desc_idx as usize) };
        let buf_virt = desc.addr + crate::elf::phys_offset();
        let frame_len = total_len - VirtioNetHeader::LEN;

        let mut frame = vec![0u8; frame_len];
        unsafe {
            core::ptr::copy_nonoverlapping(
                (buf_virt + VirtioNetHeader::LEN as u64) as *const u8,
                frame.as_mut_ptr(),
                frame_len,
            );
        }

        // Re-submit this buffer for future RX
        unsafe {
            let desc_mut = &mut *self.rx_queue.desc_ptr().add(desc_idx as usize);
            desc_mut.len = RX_BUF_SIZE as u32;
            desc_mut.flags = VRING_DESC_F_WRITE;
        }
        self.rx_queue.submit(desc_idx);
        self.rx_queue.last_used_idx = self.rx_queue.last_used_idx.wrapping_add(1);

        self.rx_packets += 1;
        self.rx_bytes += frame_len as u64;

        Some(frame)
    }

    /// Reclaim used TX descriptors
    fn reclaim_tx(&mut self) {
        loop {
            let used_idx = unsafe { core::ptr::read_volatile(self.tx_queue.used_idx_ptr()) };
            if used_idx == self.tx_queue.last_used_idx {
                break;
            }

            let ring_idx = self.tx_queue.last_used_idx % self.tx_queue.num;
            let (id_ptr, _len_ptr) = self.tx_queue.used_ring_entry(ring_idx);
            let desc_idx = unsafe { core::ptr::read_volatile(id_ptr) } as u16;

            self.tx_queue.free_desc(desc_idx);
            self.tx_queue.last_used_idx = self.tx_queue.last_used_idx.wrapping_add(1);
        }
    }
}
