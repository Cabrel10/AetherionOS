#!/bin/bash
# AetherionOS Boot Image Builder (Bootloader 0.11)
# Jalon 148 - FRONT 2
#
# Builds the kernel ELF and creates a BIOS disk image using bootloader 0.11.
# Uses CARGO_BUILD_JOBS=1 to avoid OOM on memory-constrained systems (1GB RAM).
#
# Usage:
#   bash scripts/build-boot-011.sh [--release]
#
# Prerequisites:
#   - Rust nightly-2026-04-21 with rust-src, llvm-tools-preview
#   - x86_64-unknown-none target installed

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

# Parse arguments
PROFILE="debug"
PROFILE_FLAG=""
if [[ "${1:-}" == "--release" ]]; then
    PROFILE="release"
    PROFILE_FLAG="--release"
fi

echo "╔══════════════════════════════════════════════════╗"
echo "║  AetherionOS Boot Image Builder (Jalon 148)     ║"
echo "║  Profile: $PROFILE                              ║"
echo "╚══════════════════════════════════════════════════╝"

# Step 1: Verify toolchain
echo ""
echo "=== [1/4] Verifying toolchain ==="
rustc --version
rustup component list --installed | grep -q rust-src || {
    echo "ERROR: rust-src not installed. Run: rustup component add rust-src"
    exit 1
}
rustup component list --installed | grep -q llvm-tools || {
    echo "ERROR: llvm-tools-preview not installed. Run: rustup component add llvm-tools-preview"
    exit 1
}
echo "[OK] Toolchain verified"

# Step 2: Build kernel
echo ""
echo "=== [2/4] Building kernel ELF ==="
export CARGO_BUILD_JOBS=1
cargo build -p aetherion-kernel --target x86_64-unknown-none $PROFILE_FLAG 2>&1
KERNEL_ELF="target/x86_64-unknown-none/$PROFILE/aetherion-kernel"
if [ ! -f "$KERNEL_ELF" ]; then
    echo "ERROR: Kernel ELF not found at $KERNEL_ELF"
    exit 1
fi
echo "[OK] Kernel ELF: $(du -h "$KERNEL_ELF" | cut -f1)"

# Step 3: Build boot runner (creates disk image via bootloader 0.11)
echo ""
echo "=== [3/4] Building boot runner + disk image ==="
CARGO_BUILD_JOBS=1 cargo build -p aetherion-boot $PROFILE_FLAG 2>&1
echo "[OK] Boot runner built"

# Step 4: Report
echo ""
echo "=== [4/4] Build complete ==="
BOOT_BIN="target/$PROFILE/aetherion-boot"
if [ -f "$BOOT_BIN" ]; then
    echo "[OK] Boot runner: $BOOT_BIN"
    echo ""
    echo "To boot in QEMU:"
    echo "  cargo run -p aetherion-boot"
    echo ""
    echo "Or manually:"
    echo "  $BOOT_BIN bios"
fi

# Find the disk image
DISK_IMG=$(find target/ -name "bios.img" -path "*/aetherion-boot*" 2>/dev/null | head -1)
if [ -n "$DISK_IMG" ]; then
    echo "[OK] BIOS disk image: $DISK_IMG ($(du -h "$DISK_IMG" | cut -f1))"
    # Copy to a stable location
    cp "$DISK_IMG" target/aetherion-boot.img
    echo "[OK] Copied to: target/aetherion-boot.img"
else
    echo "[WARN] Disk image not found (boot runner build may have failed)"
    echo "[INFO] Use 'cargo run -p aetherion-boot' to create it"
fi

echo ""
echo "╔══════════════════════════════════════════════════╗"
echo "║  Build finished successfully                     ║"
echo "╚══════════════════════════════════════════════════╝"
