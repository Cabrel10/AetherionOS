#!/bin/bash
# scripts/build_c.sh - Build C userspace applications for AetherionOS
#
# Compiles C programs statically linked against our libc_stub,
# targeting the AetherionOS Ring 3 execution environment.
#
# The resulting ELF is:
#   - x86_64 static executable
#   - No standard library (bare metal)
#   - Text segment at 0x8000000000 (PML4[1] isolation)
#   - Compatible with AetherionOS ELF loader

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
C_APPS_DIR="$ROOT_DIR/userspace/c_apps"
OUTPUT_DIR="$C_APPS_DIR"

echo "========================================="
echo "[BUILD_C] AetherionOS C Toolchain Builder"
echo "========================================="
echo ""

# Check GCC
if ! command -v gcc &> /dev/null; then
    echo "[ERROR] gcc not found. Install with: sudo apt-get install gcc"
    exit 1
fi
echo "[OK] GCC: $(gcc --version | head -1)"

# Create linker script for C apps
LINKER_SCRIPT="$C_APPS_DIR/c_app.ld"
cat > "$LINKER_SCRIPT" << 'LDEOF'
/* c_app.ld - Linker script for AetherionOS C userspace apps */
/* Base address in PML4[1] for kernel isolation */
ENTRY(_start)

SECTIONS
{
    . = 0x0000008000000000;

    .text : ALIGN(4096)
    {
        *(.text*)
    }

    .rodata : ALIGN(4096)
    {
        *(.rodata*)
    }

    .data : ALIGN(4096)
    {
        *(.data*)
    }

    .bss : ALIGN(4096)
    {
        *(.bss*)
        *(COMMON)
    }

    /DISCARD/ :
    {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}
LDEOF
echo "[OK] Linker script: $LINKER_SCRIPT"

# GCC flags for bare-metal x86_64 without SSE (kernel doesn't save FPU state)
GCC_FLAGS="-nostdlib -fno-builtin -fno-stack-protector -ffreestanding \
    -mno-sse -mno-sse2 -mno-mmx -mno-80387 -mno-red-zone \
    -fPIC -O2 -Wall -Wextra -mcmodel=large"

# Build libc_stub.o
echo ""
echo "[BUILD] Compiling libc_stub.c..."
gcc -c $GCC_FLAGS \
    -o "$C_APPS_DIR/libc_stub.o" \
    "$C_APPS_DIR/libc_stub.c"
echo "[OK] libc_stub.o"

# Build hello_c
echo ""
echo "[BUILD] Compiling hello_c.c..."
gcc -c $GCC_FLAGS \
    -o "$C_APPS_DIR/hello_c.o" \
    "$C_APPS_DIR/hello_c.c"
echo "[OK] hello_c.o"

# Link
echo ""
echo "[LINK] Linking hello_c.elf..."
ld -T "$LINKER_SCRIPT" -static \
    -o "$OUTPUT_DIR/hello_c.elf" \
    "$C_APPS_DIR/hello_c.o" \
    "$C_APPS_DIR/libc_stub.o"
echo "[OK] hello_c.elf"

# Verify
echo ""
echo "[VERIFY] ELF information:"
file "$OUTPUT_DIR/hello_c.elf"
echo ""
echo "Size: $(stat -c %s "$OUTPUT_DIR/hello_c.elf") bytes"
echo ""

# Read ELF entry point
readelf -h "$OUTPUT_DIR/hello_c.elf" 2>/dev/null | grep -E "Entry|Type|Machine" || true

# Cleanup object files
rm -f "$C_APPS_DIR"/*.o

echo ""
echo "========================================="
echo "[BUILD_C] SUCCESS: hello_c.elf built!"
echo "========================================="
