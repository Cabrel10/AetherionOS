#!/bin/bash
# build_hello.sh - Build hello.elf for AetherionOS Ring 3 user space
# Links at PML4[1] base address (0x0000_0080_0000_0000 = 512 GiB)
# This ensures COMPLETE isolation from kernel in PML4[0]
# Uses a linker script to force ALL segments into PML4[1]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
USERSPACE_DIR="$SCRIPT_DIR/../userspace"

echo "[BUILD] Assembling hello.asm..."
nasm -f elf64 -o "$USERSPACE_DIR/hello.o" "$USERSPACE_DIR/hello.asm"

echo "[BUILD] Linking with hello.ld at 0x0000008000000000 (PML4[1])..."
ld -o "$USERSPACE_DIR/hello.elf" \
   -T "$USERSPACE_DIR/hello.ld" \
   -static \
   "$USERSPACE_DIR/hello.o"

# Strip to minimize size
strip "$USERSPACE_DIR/hello.elf"

echo "[BUILD] Verifying binary..."
file "$USERSPACE_DIR/hello.elf"
readelf -l "$USERSPACE_DIR/hello.elf"
SIZE=$(stat -c%s "$USERSPACE_DIR/hello.elf")
echo "[BUILD] Size: $SIZE bytes"

echo ""
echo "[BUILD] === hello.elf ready for AetherionOS Ring 3 ==="
echo "[BUILD] Entry:  0x0000008000000000 (PML4[1])"
echo "[BUILD] All segments isolated from kernel PML4[0]"
