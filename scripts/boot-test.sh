#!/bin/bash
# Aetherion OS - Boot Test Script
# Launches OS in QEMU

cd "$(dirname "$0")/.."

echo "=========================================="
echo "  AETHERION OS - Boot Test"
echo "=========================================="
echo

if [ ! -f aetherion.img ]; then
    echo "❌ ERROR: aetherion.img not found!"
    echo "Run './scripts/build.sh' first."
    exit 1
fi

echo "Booting Aetherion OS in QEMU..."
echo "Instructions:"
echo "  - Press Ctrl+A then X to exit QEMU"
echo "  - Or close the QEMU window"
echo

# Boot with VGA output (graphical mode)
qemu-system-x86_64 \
    -drive format=raw,file=aetherion.img \
    -serial stdio \
    -m 256M \
    -cpu qemu64
