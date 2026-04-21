use std::env;
use std::process::{Command, exit};

fn main() {
    let bios_path = env!("BIOS_PATH");

    let args: Vec<String> = env::args().collect();
    let prog = &args[0];

    // Parse optional arguments
    let mut qemu_args: Vec<String> = Vec::new();
    let mut show_help = false;

    for arg in &args[1..] {
        match arg.as_str() {
            "-h" | "--help" => show_help = true,
            _ => qemu_args.push(arg.clone()),
        }
    }

    if show_help {
        println!("Usage: {prog} [qemu-args...]");
        println!("  Boots AetherionOS BIOS image in QEMU");
        println!("  Additional arguments are passed to qemu-system-x86_64");
        exit(0);
    }

    let mut cmd = Command::new("qemu-system-x86_64");

    // Serial output to terminal
    cmd.arg("-serial").arg("mon:stdio");

    // 512 MB RAM (enough for kernel + agents + GGUF)
    cmd.arg("-m").arg("512M");

    // Enable guest exit via debug port
    cmd.arg("-device").arg("isa-debug-exit,iobase=0xf4,iosize=0x04");

    // Boot from BIOS disk image
    cmd.arg("-drive").arg(format!("format=raw,file={bios_path}"));

    // Add any extra QEMU arguments from command line
    for arg in &qemu_args {
        cmd.arg(arg);
    }

    println!("[AetherionOS Boot] Launching QEMU with BIOS image: {bios_path}");
    println!("[AetherionOS Boot] QEMU command: {:?}", cmd);

    let mut child = cmd.spawn().unwrap_or_else(|e| {
        eprintln!("Failed to start qemu-system-x86_64: {e}");
        eprintln!("Make sure QEMU is installed: apt install qemu-system-x86");
        exit(1);
    });

    let status = child.wait().expect("Failed to wait on QEMU");
    let code = status.code().unwrap_or(1);

    // QEMU exit codes with isa-debug-exit:
    // (exit_code << 1) | 1
    // So exit(0) -> 1, exit(1) -> 3
    match code {
        0x21 => {  // success (0x10 << 1 | 1 = 0x21 = 33)
            println!("[AetherionOS Boot] Kernel exited successfully");
            exit(0);
        }
        0x23 => {  // failure (0x11 << 1 | 1 = 0x23 = 35)
            eprintln!("[AetherionOS Boot] Kernel reported failure");
            exit(1);
        }
        _ => {
            println!("[AetherionOS Boot] QEMU exited with code: {code}");
            exit(code);
        }
    }
}
