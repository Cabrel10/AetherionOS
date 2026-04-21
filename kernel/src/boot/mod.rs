// boot/mod.rs - Boot protocol abstraction layer
//
// This module provides feature-gated boot protocol implementations.
// - Default: bootloader_api 0.11 (via entry_point! macro in main.rs)
// - limine: Limine protocol (via limine_entry.rs)

#[cfg(feature = "limine")]
pub mod limine_entry;
