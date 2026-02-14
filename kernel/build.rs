// Aetherion OS - Build Script for Bootloader 0.11
// Locates the bootloader dependency for linking

use std::path::PathBuf;

fn main() {
    // Locate the bootloader dependency using bootloader-locator
    let bootloader_path = bootloader_locator::locate_bootloader()
        .expect("Failed to locate bootloader dependency");

    println!(
        "cargo:rustc-link-arg=--library-path={}",
        bootloader_path.display()
    );

    // Pass the bootloader path to the linker
    let bootloader_elf = bootloader_path.join("bootloader-x86_64-bios-boot-sector.bin");
    println!(
        "cargo:rustc-env=BOOTLOADER_PATH={}",
        bootloader_path.display()
    );

    // Tell cargo to re-run if build.rs changes
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");

    // Print status
    eprintln!("[BUILD] Bootloader located at: {}", bootloader_path.display());
    eprintln!("[BUILD] Aetherion OS Couche 1 HAL - Build script complete");
}
