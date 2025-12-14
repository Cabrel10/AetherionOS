// Aetherion OS - Networking Stack
// Phase 5: TCP/IP implementation

pub mod ethernet;
pub mod ip;
pub mod tcp;
pub mod socket;

pub fn init() {
    ethernet::init();
}
