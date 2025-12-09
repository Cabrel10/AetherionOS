#!/bin/bash
# Aetherion OS - Build Script
# Compiles kernel and bootloader, creates bootable image

set -e  # Exit on error

cd "$(dirname "$0")/.."  # Go to project root

echo "=========================================="
echo "  AETHERION OS - Build Script"
echo "=========================================="
echo

# Step 1: Build Kernel
echo "[1/4] Building kernel..."
cd kernel
cargo build --target x86_64-unknown-none --release 2>&1 | grep -E "(Compiling|Finished|error)" || true
if [ $? -ne 0 ]; then
    echo "❌ Kernel build failed!"
    exit 1
fi
cd ..
echo "  ✅ Kernel built: kernel/target/x86_64-unknown-none/release/aetherion-kernel"

# Step 2: Build Bootloader
echo "[2/4] Building bootloader..."
cd bootloader
nasm -f bin src/boot.asm -o boot.bin
if [ ! -f boot.bin ]; then
    echo "❌ Bootloader build failed!"
    exit 1
fi
cd ..
echo "  ✅ Bootloader built: bootloader/boot.bin"

# Step 3: Create bootable image
echo "[3/4] Creating bootable image..."
./scripts/create-image.sh
echo "  ✅ Image created: aetherion.img"

# Step 4: Display info
echo "[4/4] Build information:"
KERNEL_SIZE=$(stat -f%z kernel/target/x86_64-unknown-none/release/aetherion-kernel 2>/dev/null || stat -c%s kernel/target/x86_64-unknown-none/release/aetherion-kernel)
BOOT_SIZE=$(stat -f%z bootloader/boot.bin 2>/dev/null || stat -c%s bootloader/boot.bin)
IMAGE_SIZE=$(stat -f%z aetherion.img 2>/dev/null || stat -c%s aetherion.img)

echo "  - Kernel size:     $(numfmt --to=iec $KERNEL_SIZE 2>/dev/null || echo $KERNEL_SIZE bytes)"
echo "  - Bootloader size: $BOOT_SIZE bytes"
echo "  - Image size:      $(numfmt --to=iec $IMAGE_SIZE 2>/dev/null || echo $IMAGE_SIZE bytes)"

echo
echo "=========================================="
echo "  ✅ Build Complete!"
echo "=========================================="
echo
echo "Run './scripts/boot-test.sh' to test in QEMU"
echo
