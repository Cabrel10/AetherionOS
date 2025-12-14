// Aetherion OS - Device Drivers
// Phase 3: Hardware abstraction

pub mod keyboard;
pub mod vga;
pub mod ata;

pub fn init_all() {
    keyboard::init();
    vga::init();
    ata::init();
}
