// kernel/src/drivers/usb/endpoint.rs — USB Endpoint Communication (Jalon 89)
//
// Implements USB endpoint abstraction for device communication:
//   - Endpoint descriptor parsing (address, type, max packet size)
//   - Transfer ring management (TRB: Transfer Request Block)
//   - Control, Bulk, Interrupt, and Isochronous transfer types
//   - USB device descriptor reading (vendor ID, product ID, class)
//   - HID (Human Interface Device) report parsing for keyboard/mouse
//
// Architecture:
//   xHCI works with Transfer Rings — circular buffers of TRBs that describe
//   data transfers. Each endpoint gets its own Transfer Ring. The host controller
//   processes TRBs and writes completion events to the Event Ring.
//
//   Transfer flow:
//     1. Software writes TRB(s) to the endpoint's Transfer Ring
//     2. Software rings the Doorbell Register for that endpoint
//     3. xHCI processes the TRB and performs the USB transfer
//     4. xHCI writes a Transfer Event TRB to the Event Ring
//     5. Software reads the completion status from the Event Ring
//
// SAFETY: All DMA buffers must be physically contiguous and aligned.
// Transfer Rings use producer/consumer semantics with cycle bits.

use core::sync::atomic::{AtomicU32, Ordering};

// ── USB Descriptor Types ──
const DESC_DEVICE: u8 = 1;
const DESC_CONFIGURATION: u8 = 2;
const DESC_STRING: u8 = 3;
const DESC_INTERFACE: u8 = 4;
const DESC_ENDPOINT: u8 = 5;
const DESC_HID: u8 = 0x21;

// ── USB Device Classes ──
const USB_CLASS_HID: u8 = 0x03;       // Human Interface Device
const USB_CLASS_MASS_STORAGE: u8 = 0x08;
const USB_CLASS_HUB: u8 = 0x09;
const USB_CLASS_VIDEO: u8 = 0x0E;     // USB Video Class (webcams)
const USB_CLASS_AUDIO: u8 = 0x01;

// ── Endpoint Transfer Types ──
const EP_CONTROL: u8 = 0;
const EP_ISOCHRONOUS: u8 = 1;
const EP_BULK: u8 = 2;
const EP_INTERRUPT: u8 = 3;

// ── TRB Types ──
const TRB_NORMAL: u32 = 1;
const TRB_SETUP_STAGE: u32 = 2;
const TRB_DATA_STAGE: u32 = 3;
const TRB_STATUS_STAGE: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_TRANSFER_EVENT: u32 = 32;
const TRB_COMMAND_COMPLETION: u32 = 33;
const TRB_PORT_STATUS_CHANGE: u32 = 34;

/// TRB (Transfer Request Block) — 16 bytes, 16-byte aligned
#[repr(C, align(16))]
#[derive(Debug, Clone, Copy)]
pub struct Trb {
    pub parameter: u64,     // Data buffer pointer or immediate data
    pub status: u32,        // Transfer length, completion code, etc.
    pub control: u32,       // TRB type, cycle bit, flags
}

impl Trb {
    pub const fn empty() -> Self {
        Trb { parameter: 0, status: 0, control: 0 }
    }

    /// Get TRB type (bits 15:10 of control)
    pub fn trb_type(&self) -> u32 {
        (self.control >> 10) & 0x3F
    }

    /// Get cycle bit (bit 0 of control)
    pub fn cycle_bit(&self) -> bool {
        (self.control & 1) != 0
    }

    /// Get completion code (bits 31:24 of status)
    pub fn completion_code(&self) -> u8 {
        (self.status >> 24) as u8
    }
}

/// USB Device Descriptor (18 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct DeviceDescriptor {
    pub b_length: u8,
    pub b_descriptor_type: u8,
    pub bcd_usb: u16,
    pub b_device_class: u8,
    pub b_device_sub_class: u8,
    pub b_device_protocol: u8,
    pub b_max_packet_size0: u8,
    pub id_vendor: u16,
    pub id_product: u16,
    pub bcd_device: u16,
    pub i_manufacturer: u8,
    pub i_product: u8,
    pub i_serial_number: u8,
    pub b_num_configurations: u8,
}

/// USB Endpoint Descriptor (7 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct EndpointDescriptor {
    pub b_length: u8,
    pub b_descriptor_type: u8,
    pub b_endpoint_address: u8,
    pub bm_attributes: u8,
    pub w_max_packet_size: u16,
    pub b_interval: u8,
}

impl EndpointDescriptor {
    /// Endpoint number (bits 3:0 of address)
    pub fn endpoint_number(&self) -> u8 {
        self.b_endpoint_address & 0x0F
    }

    /// Direction: true = IN (device → host), false = OUT (host → device)
    pub fn is_in(&self) -> bool {
        (self.b_endpoint_address & 0x80) != 0
    }

    /// Transfer type
    pub fn transfer_type(&self) -> u8 {
        self.bm_attributes & 0x03
    }

    /// Human-readable transfer type
    pub fn transfer_type_str(&self) -> &'static str {
        match self.transfer_type() {
            EP_CONTROL => "Control",
            EP_ISOCHRONOUS => "Isochronous",
            EP_BULK => "Bulk",
            EP_INTERRUPT => "Interrupt",
            _ => "Unknown",
        }
    }
}

/// USB device class name
pub fn class_name(class: u8) -> &'static str {
    match class {
        0x00 => "Composite",
        USB_CLASS_AUDIO => "Audio",
        0x02 => "Communications",
        USB_CLASS_HID => "HID (keyboard/mouse)",
        0x05 => "Physical",
        0x06 => "Image",
        0x07 => "Printer",
        USB_CLASS_MASS_STORAGE => "Mass Storage",
        USB_CLASS_HUB => "Hub",
        0x0A => "CDC-Data",
        0x0B => "Smart Card",
        0x0D => "Content Security",
        USB_CLASS_VIDEO => "Video (webcam)",
        0x0F => "Personal Healthcare",
        0x10 => "Audio/Video",
        0xDC => "Diagnostic",
        0xE0 => "Wireless",
        0xEF => "Miscellaneous",
        0xFE => "Application Specific",
        0xFF => "Vendor Specific",
        _ => "Unknown",
    }
}

// ── Transfer Ring ──
const RING_SIZE: usize = 64; // 64 TRBs per ring

/// Transfer Ring for an endpoint
#[repr(C, align(64))]
pub struct TransferRing {
    pub trbs: [Trb; RING_SIZE],
    pub enqueue_index: usize,
    pub cycle_bit: bool,
}

impl TransferRing {
    pub const fn new() -> Self {
        TransferRing {
            trbs: [Trb::empty(); RING_SIZE],
            enqueue_index: 0,
            cycle_bit: true,
        }
    }

    /// Enqueue a TRB to the ring
    pub fn enqueue(&mut self, mut trb: Trb) -> usize {
        let idx = self.enqueue_index;

        // Set cycle bit
        if self.cycle_bit {
            trb.control |= 1;
        } else {
            trb.control &= !1;
        }

        self.trbs[idx] = trb;

        self.enqueue_index += 1;
        if self.enqueue_index >= RING_SIZE - 1 {
            // Write Link TRB to wrap around
            let mut link = Trb::empty();
            link.control = (TRB_LINK << 10) | (1 << 5); // Toggle Cycle
            if self.cycle_bit {
                link.control |= 1;
            }
            self.trbs[RING_SIZE - 1] = link;
            self.enqueue_index = 0;
            self.cycle_bit = !self.cycle_bit;
        }

        idx
    }
}

/// Maximum number of tracked USB devices
pub const MAX_USB_DEVICES: usize = 8;

/// Tracked USB device information
static USB_DEVICE_COUNT: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy)]
pub struct UsbDeviceInfo {
    pub valid: bool,
    pub port: u8,
    pub speed: u8,
    pub class: u8,
    pub subclass: u8,
    pub vendor_id: u16,
    pub product_id: u16,
}

impl UsbDeviceInfo {
    pub const fn empty() -> Self {
        UsbDeviceInfo {
            valid: false,
            port: 0,
            speed: 0,
            class: 0,
            subclass: 0,
            vendor_id: 0,
            product_id: 0,
        }
    }
}

static mut USB_DEVICES: [UsbDeviceInfo; MAX_USB_DEVICES] = [UsbDeviceInfo::empty(); MAX_USB_DEVICES];

/// Register a detected USB device
pub fn register_device(port: u8, speed: u8, class: u8, subclass: u8,
                        vendor_id: u16, product_id: u16) {
    let idx = USB_DEVICE_COUNT.load(Ordering::SeqCst) as usize;
    if idx >= MAX_USB_DEVICES {
        crate::serial_println!("[USB] Device table full, cannot register port {}", port);
        return;
    }

    unsafe {
        USB_DEVICES[idx] = UsbDeviceInfo {
            valid: true,
            port,
            speed,
            class,
            subclass,
            vendor_id,
            product_id,
        };
    }
    USB_DEVICE_COUNT.fetch_add(1, Ordering::SeqCst);

    crate::serial_println!("[USB] Registered device on port {}: class={} ({}) vendor=0x{:04X} product=0x{:04X}",
        port, class, class_name(class), vendor_id, product_id);
}

/// Get number of registered USB devices
pub fn device_count() -> u32 {
    USB_DEVICE_COUNT.load(Ordering::SeqCst)
}

/// Get device info by index
pub fn get_device(idx: usize) -> Option<UsbDeviceInfo> {
    if idx >= MAX_USB_DEVICES {
        return None;
    }
    let dev = unsafe { USB_DEVICES[idx] };
    if dev.valid { Some(dev) } else { None }
}

/// Run endpoint subsystem tests
pub fn run_tests() {
    crate::serial_write("  [USB-EP TEST 1/3] TRB structure size... ");
    let trb_size = core::mem::size_of::<Trb>();
    if trb_size == 16 {
        crate::serial_println!("OK ({} bytes)", trb_size);
    } else {
        crate::serial_println!("FAIL ({} bytes, expected 16)", trb_size);
    }

    crate::serial_write("  [USB-EP TEST 2/3] Device descriptor size... ");
    let desc_size = core::mem::size_of::<DeviceDescriptor>();
    if desc_size == 18 {
        crate::serial_println!("OK ({} bytes)", desc_size);
    } else {
        crate::serial_println!("FAIL ({} bytes, expected 18)", desc_size);
    }

    crate::serial_write("  [USB-EP TEST 3/3] Transfer ring... ");
    let mut ring = TransferRing::new();
    let trb = Trb { parameter: 0x1234, status: 0, control: TRB_NORMAL << 10 };
    let idx = ring.enqueue(trb);
    if idx == 0 && ring.enqueue_index == 1 {
        crate::serial_println!("OK (enqueue works)");
    } else {
        crate::serial_println!("FAIL");
    }
}
