#!/bin/bash
# AetherionOS Kernel-Only Build Script
# Jalon 148 - FRONT 2 (lightweight alternative)
#
# Builds the kernel ELF binary without creating a full disk image.
# Use this on memory-constrained systems (< 2GB RAM) where the full
# bootloader 0.11 build cannot run.
#
# The output kernel ELF can be booted directly with QEMU:
#   qemu-system-x86_64 -kernel target/x86_64-unknown-none/release/aetherion-kernel \
#     -serial stdio -m 512M -no-reboot
#
# For a full BIOS disk image, use scripts/build-boot-011.sh on a machine with 2GB+ RAM.
#
# Usage:
#   bash scripts/build-kernel-only.sh [--release]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

# Parse arguments
PROFILE="dev"
PROFILE_DIR="debug"
PROFILE_FLAG=""
if [[ "${1:-}" == "--release" ]]; then
    PROFILE="release"
    PROFILE_DIR="release"
    PROFILE_FLAG="--release"
fi

echo "╔══════════════════════════════════════════════════╗"
echo "║  AetherionOS Kernel Build (Jalon 148)           ║"
echo "║  Profile: $PROFILE                              ║"
echo "╚══════════════════════════════════════════════════╝"

# Step 1: Verify toolchain
echo ""
echo "=== [1/5] Verifying toolchain ==="
rustc --version
rustup component list --installed | grep -q rust-src || {
    echo "ERROR: rust-src not installed. Run: rustup component add rust-src"
    exit 1
}
echo "[OK] Toolchain verified"

# Step 2: Build kernel ELF
echo ""
echo "=== [2/5] Building kernel ELF ==="
export CARGO_BUILD_JOBS=1
cargo build -p aetherion-kernel --target x86_64-unknown-none $PROFILE_FLAG 2>&1
KERNEL_ELF="target/x86_64-unknown-none/$PROFILE_DIR/aetherion-kernel"
if [ ! -f "$KERNEL_ELF" ]; then
    echo "ERROR: Kernel ELF not found at $KERNEL_ELF"
    exit 1
fi
KERNEL_SIZE=$(du -h "$KERNEL_ELF" | cut -f1)
echo "[OK] Kernel ELF: $KERNEL_ELF ($KERNEL_SIZE)"

# Step 3: Extract binary with llvm-objcopy
echo ""
echo "=== [3/5] Extracting kernel binary ==="
OBJCOPY=$(find $(rustc --print sysroot) -name "llvm-objcopy" 2>/dev/null | head -1)
if [ -z "$OBJCOPY" ]; then
    echo "WARN: llvm-objcopy not found, skipping binary extraction"
    echo "      Install with: rustup component add llvm-tools-preview"
else
    KERNEL_BIN="target/x86_64-unknown-none/$PROFILE_DIR/aetherion-kernel.bin"
    "$OBJCOPY" -O binary "$KERNEL_ELF" "$KERNEL_BIN"
    BIN_SIZE=$(du -h "$KERNEL_BIN" | cut -f1)
    echo "[OK] Kernel binary: $KERNEL_BIN ($BIN_SIZE)"
fi

# Step 4: Verify ELF structure
echo ""
echo "=== [4/5] Verifying kernel ELF ==="
file "$KERNEL_ELF"
# Check for entry point symbol
if command -v readelf &>/dev/null; then
    ENTRY=$(readelf -h "$KERNEL_ELF" 2>/dev/null | grep "Entry point" | awk '{print $NF}')
    echo "[OK] Entry point: $ENTRY"
elif [ -n "$OBJCOPY" ]; then
    OBJDUMP=$(echo "$OBJCOPY" | sed 's/objcopy/objdump/')
    if [ -x "$OBJDUMP" ]; then
        ENTRY=$("$OBJDUMP" -f "$KERNEL_ELF" 2>/dev/null | grep "start address" | awk '{print $NF}')
        echo "[OK] Entry point: $ENTRY"
    fi
fi

# Step 5: Summary
echo ""
echo "=== [5/5] Build Summary ==="
echo "[OK] Kernel ELF built successfully"
echo ""
echo "To boot with QEMU (direct kernel boot):"
echo "  qemu-system-x86_64 -kernel $KERNEL_ELF \\"
echo "    -serial stdio -m 512M -no-reboot -display none"
echo ""
echo "To create a full BIOS disk image (requires 2GB+ RAM):"
echo "  CARGO_BUILD_JOBS=1 cargo build -p aetherion-boot"
echo "  # Or: bash scripts/build-boot-011.sh"
echo ""
echo "╔══════════════════════════════════════════════════╗"
echo "║  Kernel build finished successfully              ║"
echo "╚══════════════════════════════════════════════════╝"
