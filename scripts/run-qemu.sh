#!/bin/bash
# AetherionOS QEMU Runner
# Usage: bash scripts/run-qemu.sh [--bios|--uefi] [--headless]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ISO="$PROJECT_DIR/target/aetherion-os.iso"

# Parse arguments
MODE="bios"
DISPLAY_OPT="-serial stdio"
EXTRA_ARGS=""

for arg in "$@"; do
    case "$arg" in
        --uefi)  MODE="uefi" ;;
        --bios)  MODE="bios" ;;
        --headless)
            DISPLAY_OPT="-serial file:/tmp/aetherion-serial.log -nographic"
            ;;
        --net)
            EXTRA_ARGS="$EXTRA_ARGS -netdev user,id=net0,hostfwd=tcp::2222-:22 -device e1000,netdev=net0"
            ;;
    esac
done

if [ ! -f "$ISO" ]; then
    echo "ERROR: ISO not found at $ISO"
    echo "Build first with: bash scripts/build-limine.sh"
    exit 1
fi

echo "╔══════════════════════════════════════════════════╗"
echo "║  AetherionOS QEMU Runner                         ║"
echo "║  ISO:  $ISO"
echo "║  Mode: $MODE"
echo "╚══════════════════════════════════════════════════╝"

QEMU_CMD="qemu-system-x86_64 \
    -cdrom $ISO \
    -m 4G \
    -smp 2 \
    -no-reboot \
    $DISPLAY_OPT \
    $EXTRA_ARGS"

echo "Running: $QEMU_CMD"
eval $QEMU_CMD
