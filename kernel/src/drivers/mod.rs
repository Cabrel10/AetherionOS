// kernel/src/drivers/mod.rs - Device Drivers (Couche 19+)
//
// VirtIO-Block driver for persistent storage
// PS/2 Mouse driver for HID input (Jalon 37)
// PS/2 Controller (8042) initialization
// USB 3.0 xHCI controller (Jalon 77)

pub mod virtio_blk;
pub mod mouse;
pub mod ps2;
pub mod usb;
