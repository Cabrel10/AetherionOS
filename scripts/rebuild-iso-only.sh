#!/bin/bash
# Rebuild ISO from existing kernel ELF (no recompilation)
# Usage: bash scripts/rebuild-iso-only.sh
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_DIR"

KERNEL_ELF="target/x86_64-unknown-none/release/aetherion-kernel"
if [ ! -f "$KERNEL_ELF" ]; then
    echo "ERROR: Kernel ELF not found at $KERNEL_ELF"
    echo "       Run 'cargo build' first or use scripts/build-limine.sh --release"
    exit 1
fi
echo "[OK] Kernel ELF: $KERNEL_ELF ($(du -h "$KERNEL_ELF" | cut -f1))"

# Verify ELF type
ELF_TYPE=$(readelf -h "$KERNEL_ELF" 2>/dev/null | grep "Type:" | awk '{print $2}')
echo "[OK] ELF type: $ELF_TYPE"

# Create ISO structure
ISO_DIR="$PROJECT_DIR/target/limine-iso"
rm -rf "$ISO_DIR"
mkdir -p "$ISO_DIR/boot" "$ISO_DIR/boot/limine" "$ISO_DIR/EFI/BOOT"

# Copy kernel
cp "$KERNEL_ELF" "$ISO_DIR/boot/aetherion-kernel"

# Copy Limine config
cp "$PROJECT_DIR/limine.conf" "$ISO_DIR/boot/limine/limine.conf"

# Copy Limine binaries
LIMINE_DIR="$PROJECT_DIR/third_party/limine"
[ -f "$LIMINE_DIR/limine-bios-cd.bin" ] && cp "$LIMINE_DIR/limine-bios-cd.bin" "$ISO_DIR/boot/limine/"
[ -f "$LIMINE_DIR/limine-bios.sys" ] && cp "$LIMINE_DIR/limine-bios.sys" "$ISO_DIR/boot/limine/"
[ -f "$LIMINE_DIR/BOOTX64.EFI" ] && cp "$LIMINE_DIR/BOOTX64.EFI" "$ISO_DIR/EFI/BOOT/BOOTX64.EFI"

# Build ISO
ISO_OUTPUT="$PROJECT_DIR/target/aetherion-limine.iso"
xorriso -as mkisofs \
    -b boot/limine/limine-bios-cd.bin \
    -no-emul-boot \
    -boot-load-size 4 \
    -boot-info-table \
    --efi-boot EFI/BOOT/BOOTX64.EFI \
    -efi-boot-part --efi-boot-image \
    --protective-msdos-label \
    "$ISO_DIR" \
    -o "$ISO_OUTPUT" 2>&1

echo ""
echo "[OK] ISO rebuilt: $ISO_OUTPUT ($(du -h "$ISO_OUTPUT" | cut -f1))"
echo ""
echo "Test with:"
echo "  (sleep 10; printf 'exec /bin/agent_inference --model /models/smollm2.gguf --prompt \"Hello\"\r'; sleep 60) | \\"
echo "  timeout 90 qemu-system-x86_64 \\"
echo "    -cdrom target/aetherion-limine.iso \\"
echo "    -drive file=disk.img,format=raw,if=virtio \\"
echo "    -m 4G -smp 4 -cpu Haswell,+avx2,+fma \\"
echo "    -nographic -no-reboot \\"
echo "    -serial mon:stdio 2>/dev/null | tee /tmp/fstat_fix_test.log"
echo ""
echo "Expected in logs:"
echo "  [LINUX-FSTAT] fd=X path='/models/smollm2-135m-q4_0.gguf' VFS size=6016"
echo "  [LLM] File size: 6016"
echo "  [LLM] Generated: ..."
