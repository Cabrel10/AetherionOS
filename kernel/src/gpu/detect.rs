//! Native GPU detection & reporting at boot.
//!
//! Complements `gpu::init()` (which sets up the VRAM allocator) with a
//! vendor-aware enumeration whose sole job is to print, over the serial line,
//! exactly which display controller(s) are on the PCI bus. When AetherionOS
//! boots on the target i3 Gen11 hardware, the serial log will reveal the iGPU's
//! `vendor_id`/`device_id`/BAR0, which is the information needed to write a
//! targeted bare-metal driver.
//!
//! Uses the existing `arch::x86_64::pci::scan_all()` enumerator — no new PCI
//! mechanism is introduced.

use crate::arch::x86_64::pci;

/// PCI base class for display controllers.
const PCI_CLASS_DISPLAY: u8 = 0x03;

/// Map a PCI vendor id to a human-readable GPU vendor name.
fn vendor_name(vendor_id: u16) -> &'static str {
    match vendor_id {
        0x8086 => "Intel iGPU",
        0x1002 => "AMD GPU",
        0x10DE => "NVIDIA GPU",
        0x1AF4 => "VirtIO-GPU (VM)",
        0x1234 => "QEMU/Bochs VGA (VM)",
        0x15AD => "VMware SVGA (VM)",
        _ => "Unknown GPU",
    }
}

/// Scan PCI for display controllers and report each over serial.
///
/// Returns the number of display controllers found. Safe to call at boot after
/// the heap/allocator is available (it uses `scan_all`, which allocates a Vec).
pub fn detect_and_report_gpu() -> usize {
    crate::serial_println!("[GPU-DETECT] Enumerating PCI display controllers...");
    let devices = pci::scan_all();
    let mut count = 0usize;

    for dev in &devices {
        if dev.class_code != PCI_CLASS_DISPLAY {
            continue;
        }
        count += 1;

        // Read BAR0 (memory-mapped base for the framebuffer / MMIO registers).
        let bar0_raw = pci::read_bar(dev.bus, dev.device, dev.function, 0);
        let bar0_addr = (bar0_raw & 0xFFFF_FFF0) as u64;

        crate::serial_println!(
            "[GPU-DETECT] {:02x}:{:02x}.{} vendor=0x{:04X} device=0x{:04X} bar0=0x{:08X} ({})",
            dev.bus,
            dev.device,
            dev.function,
            dev.vendor_id,
            dev.device_id,
            bar0_addr,
            vendor_name(dev.vendor_id),
        );

        match dev.vendor_id {
            0x8086 => crate::serial_println!(
                "[GPU-DETECT] -> Intel iGPU detected (Gen11 target): write driver against device=0x{:04X}",
                dev.device_id
            ),
            0x1002 => crate::serial_println!("[GPU-DETECT] -> AMD GPU detected"),
            0x10DE => crate::serial_println!("[GPU-DETECT] -> NVIDIA GPU detected"),
            0x1AF4 => crate::serial_println!("[GPU-DETECT] -> VirtIO-GPU detected (virtualised)"),
            _ => crate::serial_println!("[GPU-DETECT] -> Unrecognised display controller"),
        }
    }

    if count == 0 {
        crate::serial_println!("[GPU-DETECT] No PCI display controller found (headless / pre-FB)");
    } else {
        crate::serial_println!("[GPU-DETECT] Total display controllers: {}", count);
    }
    count
}
