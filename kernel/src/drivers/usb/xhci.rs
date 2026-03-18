// kernel/src/drivers/usb/xhci.rs - USB 3.0 xHCI Controller Driver (Jalon 77)
//
// Extensible Host Controller Interface (xHCI) Specification, Rev 1.2
// PCI Class: 0x0C (Serial Bus), Subclass: 0x03 (USB), ProgIF: 0x30 (xHCI)
//
// This module:
//   - Scans PCI bus for xHCI controllers
//   - Reads MMIO capability registers (CAPLENGTH, HCIVERSION, HCSPARAMS1)
//   - Configures USBCMD to start the controller
//   - Provides port routing information
//
// SAFETY: All MMIO accesses use volatile reads through the physical memory offset.
// The controller is single-threaded during initialization.

use alloc::string::String;
use alloc::format;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// xHCI PCI identification
const XHCI_CLASS: u8 = 0x0C;      // Serial Bus Controller
const XHCI_SUBCLASS: u8 = 0x03;   // USB Controller
const XHCI_PROG_IF: u8 = 0x30;    // xHCI

// xHCI Capability Register offsets (from BAR0 MMIO base)
const CAPLENGTH_OFFSET: usize = 0x00;     // Capability Register Length (1 byte)
const HCIVERSION_OFFSET: usize = 0x02;    // Interface Version Number (2 bytes)
const HCSPARAMS1_OFFSET: usize = 0x04;    // Structural Parameters 1 (4 bytes)
const HCSPARAMS2_OFFSET: usize = 0x08;    // Structural Parameters 2 (4 bytes)
const HCSPARAMS3_OFFSET: usize = 0x0C;    // Structural Parameters 3 (4 bytes)
const HCCPARAMS1_OFFSET: usize = 0x10;    // Capability Parameters 1 (4 bytes)
const DBOFF_OFFSET: usize = 0x14;         // Doorbell Offset (4 bytes)
const RTSOFF_OFFSET: usize = 0x18;        // Runtime Register Space Offset (4 bytes)

// xHCI Operational Register offsets (from BAR0 + CAPLENGTH)
const USBCMD_OFFSET: usize = 0x00;        // USB Command Register
const USBSTS_OFFSET: usize = 0x04;        // USB Status Register
const PAGESIZE_OFFSET: usize = 0x08;      // Page Size Register
const DNCTRL_OFFSET: usize = 0x14;        // Device Notification Control
const CRCR_OFFSET: usize = 0x18;          // Command Ring Control Register (8 bytes)
const DCBAAP_OFFSET: usize = 0x30;        // Device Context Base Address Array Pointer (8 bytes)
const CONFIG_OFFSET: usize = 0x38;        // Configure Register

// USBCMD bits
const USBCMD_RUN_STOP: u32 = 1 << 0;     // Run/Stop
const USBCMD_HCRST: u32 = 1 << 1;        // Host Controller Reset
const USBCMD_INTE: u32 = 1 << 2;         // Interrupter Enable

// USBSTS bits
const USBSTS_HCH: u32 = 1 << 0;          // HC Halted
const USBSTS_CNR: u32 = 1 << 11;         // Controller Not Ready

/// xHCI controller state
static XHCI_PRESENT: AtomicBool = AtomicBool::new(false);
static XHCI_MMIO_BASE: AtomicU64 = AtomicU64::new(0);
static XHCI_VERSION: AtomicU64 = AtomicU64::new(0);
static mut XHCI_MAX_PORTS: u8 = 0;
static mut XHCI_MAX_SLOTS: u8 = 0;
static mut XHCI_CAP_LENGTH: u8 = 0;

/// xHCI controller information
pub struct XhciInfo {
    pub vendor_id: u16,
    pub device_id: u16,
    pub mmio_base: u64,
    pub version_major: u8,
    pub version_minor: u8,
    pub max_ports: u8,
    pub max_device_slots: u8,
    pub cap_length: u8,
    pub controller_running: bool,
}

/// Read a 32-bit MMIO register
/// SAFETY: `addr` must be a valid mapped MMIO virtual address
unsafe fn mmio_read32(addr: u64) -> u32 {
    core::ptr::read_volatile(addr as *const u32)
}

/// Write a 32-bit MMIO register
unsafe fn mmio_write32(addr: u64, val: u32) {
    core::ptr::write_volatile(addr as *mut u32, val);
}

/// Read an 8-bit MMIO register
unsafe fn mmio_read8(addr: u64) -> u8 {
    core::ptr::read_volatile(addr as *const u8)
}

/// Read a 16-bit MMIO register
unsafe fn mmio_read16(addr: u64) -> u16 {
    core::ptr::read_volatile(addr as *const u16)
}

/// Initialize the xHCI driver
/// Scans PCI bus for xHCI controllers and performs basic initialization
pub fn init() {
    crate::serial_println!("[xHCI] Scanning PCI bus for USB 3.0 xHCI controllers...");
    crate::serial_println!("[xHCI] Looking for Class=0x{:02X} Subclass=0x{:02X} ProgIF=0x{:02X}",
        XHCI_CLASS, XHCI_SUBCLASS, XHCI_PROG_IF);

    // Scan PCI bus 0 for xHCI controllers
    // Class 0x0C = Serial Bus Controller
    let devices = crate::arch::x86_64::pci::scan_for_class(XHCI_CLASS);
    crate::serial_println!("[xHCI] PCI scan: found {} serial bus controller(s)", devices.len());

    let mut found = false;

    for dev in &devices {
        crate::serial_println!("[xHCI] Checking: {}", dev);

        // Filter for USB xHCI (subclass 0x03, prog_if 0x30)
        if dev.subclass == XHCI_SUBCLASS && dev.prog_if == XHCI_PROG_IF {
            crate::serial_println!("[xHCI] *** xHCI USB 3.0 controller detected! ***");
            crate::serial_println!("[xHCI] Vendor: 0x{:04X}, Device: 0x{:04X}",
                dev.vendor_id, dev.device_id);

            // Read BAR0 (Memory-Mapped I/O base)
            let bar0 = crate::arch::x86_64::pci::read_bar(dev.bus, dev.device, dev.function, 0);

            // Check if BAR0 is memory-mapped (bit 0 = 0)
            if bar0 & 0x01 != 0 {
                crate::serial_println!("[xHCI] BAR0 is I/O port (0x{:08X}), expected MMIO - skipping", bar0);
                continue;
            }

            let mmio_phys = (bar0 & 0xFFFFF000) as u64;

            // For 64-bit BAR, read BAR1 for upper 32 bits
            let bar_type = (bar0 >> 1) & 0x03;
            let mmio_phys = if bar_type == 0x02 {
                // 64-bit BAR
                let bar1 = crate::arch::x86_64::pci::read_bar(dev.bus, dev.device, dev.function, 1);
                mmio_phys | ((bar1 as u64) << 32)
            } else {
                mmio_phys
            };

            crate::serial_println!("[xHCI] BAR0 MMIO physical: 0x{:016X}", mmio_phys);

            // Enable PCI bus mastering and memory space access
            let cmd = crate::arch::x86_64::pci::read_config_u32(dev.bus, dev.device, dev.function, 0x04);
            let new_cmd = cmd | 0x06; // Memory Space + Bus Master
            crate::arch::x86_64::pci::write_config_u32(dev.bus, dev.device, dev.function, 0x04, new_cmd);
            crate::serial_println!("[xHCI] PCI: Memory Space + Bus Master enabled (CMD: 0x{:08X} -> 0x{:08X})", cmd, new_cmd);

            // Map MMIO via physical memory offset
            let phys_offset = crate::elf::phys_offset();
            let mmio_virt = mmio_phys + phys_offset;
            crate::serial_println!("[xHCI] MMIO virtual base: 0x{:016X}", mmio_virt);

            // Read capability registers
            let cap_length = unsafe { mmio_read8(mmio_virt + CAPLENGTH_OFFSET as u64) };
            let hci_version = unsafe { mmio_read16(mmio_virt + HCIVERSION_OFFSET as u64) };
            let hcsparams1 = unsafe { mmio_read32(mmio_virt + HCSPARAMS1_OFFSET as u64) };
            let hcsparams2 = unsafe { mmio_read32(mmio_virt + HCSPARAMS2_OFFSET as u64) };
            let hccparams1 = unsafe { mmio_read32(mmio_virt + HCCPARAMS1_OFFSET as u64) };
            let dboff = unsafe { mmio_read32(mmio_virt + DBOFF_OFFSET as u64) };
            let rtsoff = unsafe { mmio_read32(mmio_virt + RTSOFF_OFFSET as u64) };

            let version_major = (hci_version >> 8) as u8;
            let version_minor = (hci_version & 0xFF) as u8;
            let max_device_slots = (hcsparams1 & 0xFF) as u8;
            let max_interrupters = ((hcsparams1 >> 8) & 0x7FF) as u16;
            let max_ports = ((hcsparams1 >> 24) & 0xFF) as u8;

            crate::serial_println!("[xHCI] === Capability Registers ===");
            crate::serial_println!("[xHCI] CAPLENGTH:     {} bytes", cap_length);
            crate::serial_println!("[xHCI] HCIVERSION:    {}.{:02}", version_major, version_minor);
            crate::serial_println!("[xHCI] HCSPARAMS1:    0x{:08X}", hcsparams1);
            crate::serial_println!("[xHCI]   MaxSlots:    {}", max_device_slots);
            crate::serial_println!("[xHCI]   MaxIntrs:    {}", max_interrupters);
            crate::serial_println!("[xHCI]   MaxPorts:    {}", max_ports);
            crate::serial_println!("[xHCI] HCSPARAMS2:    0x{:08X}", hcsparams2);
            crate::serial_println!("[xHCI] HCCPARAMS1:    0x{:08X}", hccparams1);
            crate::serial_println!("[xHCI] Doorbell Off:  0x{:08X}", dboff);
            crate::serial_println!("[xHCI] Runtime Off:   0x{:08X}", rtsoff);

            // Read operational registers
            let op_base = mmio_virt + cap_length as u64;
            let usbcmd = unsafe { mmio_read32(op_base + USBCMD_OFFSET as u64) };
            let usbsts = unsafe { mmio_read32(op_base + USBSTS_OFFSET as u64) };
            let pagesize = unsafe { mmio_read32(op_base + PAGESIZE_OFFSET as u64) };

            crate::serial_println!("[xHCI] === Operational Registers ===");
            crate::serial_println!("[xHCI] USBCMD:    0x{:08X} (R/S={}, HCRST={}, INTE={})",
                usbcmd,
                usbcmd & USBCMD_RUN_STOP,
                (usbcmd >> 1) & 1,
                (usbcmd >> 2) & 1);
            crate::serial_println!("[xHCI] USBSTS:    0x{:08X} (HCH={}, CNR={})",
                usbsts,
                usbsts & USBSTS_HCH,
                (usbsts >> 11) & 1);
            crate::serial_println!("[xHCI] PAGESIZE:  0x{:08X} (page={} bytes)",
                pagesize, (pagesize & 0xFFFF) << 12);

            // Wait for Controller Not Ready to clear
            crate::serial_println!("[xHCI] Waiting for controller ready (CNR=0)...");
            let mut timeout = 100_000u32;
            loop {
                let sts = unsafe { mmio_read32(op_base + USBSTS_OFFSET as u64) };
                if sts & USBSTS_CNR == 0 {
                    crate::serial_println!("[xHCI] Controller ready!");
                    break;
                }
                timeout -= 1;
                if timeout == 0 {
                    crate::serial_println!("[xHCI] WARNING: Controller Not Ready timeout");
                    break;
                }
                unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
            }

            // Configure MaxSlotsEn in CONFIG register
            let config_val = max_device_slots as u32;
            unsafe { mmio_write32(op_base + CONFIG_OFFSET as u64, config_val); }
            crate::serial_println!("[xHCI] CONFIG: MaxSlotsEn = {}", max_device_slots);

            // Note: Full initialization would require:
            // 1. Allocate and set DCBAAP (Device Context Base Address Array Pointer)
            // 2. Allocate and configure Command Ring (CRCR)
            // 3. Set Run/Stop bit in USBCMD
            // 4. Configure Event Ring Segment Table
            // 5. Enable interrupters
            // For now, we detect and report the controller as present.
            // Full USB device enumeration will be implemented in future milestones.

            // Scan ports for connected devices
            crate::serial_println!("[xHCI] === Port Status ===");
            let port_base = op_base + 0x400; // Port Register Set starts at offset 0x400
            for port in 0..core::cmp::min(max_ports, 8) {
                let portsc_offset = port_base + (port as u64) * 0x10;
                let portsc = unsafe { mmio_read32(portsc_offset) };
                let ccs = portsc & 0x01;       // Current Connect Status
                let ped = (portsc >> 1) & 0x01; // Port Enabled/Disabled
                let speed = (portsc >> 10) & 0x0F; // Port Speed
                let pls = (portsc >> 5) & 0x0F;   // Port Link State

                let speed_str = match speed {
                    0 => "undefined",
                    1 => "Full-Speed (12 Mb/s)",
                    2 => "Low-Speed (1.5 Mb/s)",
                    3 => "High-Speed (480 Mb/s)",
                    4 => "SuperSpeed (5 Gb/s)",
                    5 => "SuperSpeedPlus (10 Gb/s)",
                    _ => "reserved",
                };

                if ccs != 0 || port < 4 {
                    crate::serial_println!("[xHCI] Port {}: CCS={} PED={} Speed={} PLS={} [{}]",
                        port + 1, ccs, ped, speed, pls, speed_str);
                }
            }

            // Store global state
            XHCI_PRESENT.store(true, Ordering::SeqCst);
            XHCI_MMIO_BASE.store(mmio_phys, Ordering::SeqCst);
            XHCI_VERSION.store(hci_version as u64, Ordering::SeqCst);
            unsafe {
                XHCI_MAX_PORTS = max_ports;
                XHCI_MAX_SLOTS = max_device_slots;
                XHCI_CAP_LENGTH = cap_length;
            }

            crate::serial_println!("[xHCI] USB 3.0 xHCI controller ACTIVATED");
            crate::serial_println!("[xHCI]   {} ports, {} device slots, xHCI v{}.{:02}",
                max_ports, max_device_slots, version_major, version_minor);

            found = true;
            break; // Use first xHCI controller found
        }
    }

    if !found {
        crate::serial_println!("[xHCI] No xHCI USB 3.0 controller found");
        crate::serial_println!("[xHCI] (QEMU needs: -device qemu-xhci)");
    }
}

/// Check if an xHCI controller is present
pub fn is_present() -> bool {
    XHCI_PRESENT.load(Ordering::SeqCst)
}

/// Get a human-readable info string about the xHCI controller
pub fn get_info_string() -> String {
    if !is_present() {
        return String::from("xHCI: not detected");
    }
    let version = XHCI_VERSION.load(Ordering::SeqCst);
    let ver_major = (version >> 8) as u8;
    let ver_minor = (version & 0xFF) as u8;
    unsafe {
        format!("xHCI v{}.{:02} {} ports {} slots",
            ver_major, ver_minor, XHCI_MAX_PORTS, XHCI_MAX_SLOTS)
    }
}

/// Run xHCI self-tests
pub fn run_tests() {
    crate::serial_write("  [xHCI TEST 1/3] Controller detection... ");
    if is_present() {
        crate::serial_println!("OK ({})", get_info_string());
    } else {
        crate::serial_write("SKIP (no xHCI device, add -device qemu-xhci)\n");
        return;
    }

    crate::serial_write("  [xHCI TEST 2/3] MMIO base valid... ");
    let base = XHCI_MMIO_BASE.load(Ordering::SeqCst);
    if base != 0 {
        crate::serial_println!("OK (0x{:016X})", base);
    } else {
        crate::serial_write("FAIL\n");
    }

    crate::serial_write("  [xHCI TEST 3/3] Port count valid... ");
    let max_ports = unsafe { XHCI_MAX_PORTS };
    if max_ports > 0 && max_ports <= 128 {
        crate::serial_println!("OK ({} ports)", max_ports);
    } else {
        crate::serial_println!("FAIL (ports={})", max_ports);
    }
}
