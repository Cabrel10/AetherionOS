#!/bin/bash
# Aetherion OS - Create Bootable Image Script

set -e

cd "$(dirname "$0")/.."

echo "Creating bootable disk image..."

# Create 1.44MB floppy image (standard size)
dd if=/dev/zero of=aetherion.img bs=1024 count=1440 2>/dev/null

# Write bootloader (first 512 bytes)
dd if=bootloader/boot.bin of=aetherion.img conv=notrunc 2>/dev/null

# Write kernel starting at sector 2 (byte 512)
dd if=kernel/target/x86_64-unknown-none/release/aetherion-kernel \
   of=aetherion.img \
   seek=1 \
   conv=notrunc 2>/dev/null

echo "  ✅ Bootable image created: aetherion.img"
