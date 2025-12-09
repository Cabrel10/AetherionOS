#!/bin/bash
# Aetherion OS - Setup Script
# Installs all required dependencies

set -e  # Exit on error

echo "=========================================="
echo "  AETHERION OS - Setup Script"
echo "=========================================="
echo

# Check if running as root (needed for apt install)
if [[ $EUID -eq 0 ]]; then
    SUDO=""
else
    SUDO="sudo"
fi

echo "[1/6] Updating package list..."
$SUDO apt update -qq

echo "[2/6] Installing build tools..."
$SUDO apt install -y build-essential nasm qemu-system-x86 curl

echo "[3/6] Installing Rust..."
if ! command -v rustc &> /dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
else
    echo "  Rust already installed: $(rustc --version)"
fi

echo "[4/6] Setting up Rust nightly..."
rustup default nightly
rustup component add rust-src llvm-tools-preview

echo "[5/6] Adding x86_64 bare-metal target..."
rustup target add x86_64-unknown-none

echo "[6/6] Verifying installation..."
echo "  - rustc: $(rustc --version)"
echo "  - cargo: $(cargo --version)"
echo "  - nasm: $(nasm -v | head -1)"
echo "  - qemu: $(qemu-system-x86_64 --version | head -1)"

echo
echo "=========================================="
echo "  ✅ Setup Complete!"
echo "=========================================="
echo
echo "Next steps:"
echo "  1. Build kernel:    cd kernel && cargo build --release"
echo "  2. Build bootloader: cd bootloader && nasm -f bin src/boot.asm -o boot.bin"
echo "  3. Create image:    ./scripts/create-image.sh"
echo "  4. Boot OS:         ./scripts/boot-test.sh"
echo
