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

# Build libc_stub.o (shared by all apps)
echo ""
echo "[BUILD] Compiling libc_stub.c..."
gcc -c $GCC_FLAGS \
    -o "$C_APPS_DIR/libc_stub.o" \
    "$C_APPS_DIR/libc_stub.c"
echo "[OK] libc_stub.o"

# List of all C apps to build
APPS="hello_c j19_test ls cat wget threads ui"

for APP in $APPS; do
    SRC="$C_APPS_DIR/${APP}.c"
    OBJ="$C_APPS_DIR/${APP}.o"
    ELF="$OUTPUT_DIR/${APP}.elf"

    if [ ! -f "$SRC" ]; then
        echo "[SKIP] $SRC not found"
        continue
    fi

    echo ""
    echo "[BUILD] Compiling ${APP}.c..."
    gcc -c $GCC_FLAGS \
        -o "$OBJ" \
        "$SRC"
    echo "[OK] ${APP}.o"

    echo "[LINK] Linking ${APP}.elf..."
    ld -T "$LINKER_SCRIPT" -static \
        -o "$ELF" \
        "$OBJ" \
        "$C_APPS_DIR/libc_stub.o"
    echo "[OK] ${APP}.elf ($(stat -c %s "$ELF") bytes)"
done

# Verify all ELFs
echo ""
echo "[VERIFY] Built ELF binaries:"
for APP in $APPS; do
    ELF="$OUTPUT_DIR/${APP}.elf"
    if [ -f "$ELF" ]; then
        echo "  $(file "$ELF") - $(stat -c %s "$ELF") bytes"
    fi
done

# Cleanup object files
rm -f "$C_APPS_DIR"/*.o

echo ""
echo "========================================="
echo "[BUILD_C] SUCCESS: All C apps built!"
echo "========================================="
