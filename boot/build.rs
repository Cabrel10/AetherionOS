use std::path::PathBuf;

fn main() {
    // Set by cargo, build scripts should use this directory for output files
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());

    // Set by cargo's artifact dependency feature
    // The kernel binary is built as an artifact dependency
    let kernel = PathBuf::from(
        std::env::var_os("CARGO_BIN_FILE_AETHERION_KERNEL_aetherion-kernel")
            .expect("CARGO_BIN_FILE_AETHERION_KERNEL_aetherion-kernel not set - is the kernel artifact dependency configured?"),
    );

    // Create a BIOS disk image
    let bios_path = out_dir.join("bios.img");
    bootloader::BiosBoot::new(&kernel)
        .create_disk_image(&bios_path)
        .unwrap();

    // Pass the disk image path as env variable to main.rs
    println!("cargo:rustc-env=BIOS_PATH={}", bios_path.display());
}
