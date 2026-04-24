#!/bin/bash
# scripts/build_c.sh - Build C userspace applications for AetherionOS
#
# Phase 1: Builds libaetherion.a from sdk/c/ (the AetherionOS C-SDK)
# Phase 2: Compiles each C app and links against -laetherion
#
# The resulting ELF is:
#   - x86_64 static executable
#   - No standard library (bare metal)
#   - Text segment at 0x8000000000 (PML4[1] isolation)
#   - Compatible with AetherionOS ELF loader

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
SDK_DIR="$ROOT_DIR/sdk/c"
C_APPS_DIR="$ROOT_DIR/userspace/c_apps"
OUTPUT_DIR="$C_APPS_DIR"

echo "========================================="
echo "[BUILD_C] AetherionOS C-SDK Toolchain"
echo "========================================="
echo ""

# Check GCC and AR
if ! command -v gcc &> /dev/null; then
    echo "[ERROR] gcc not found. Install with: sudo apt-get install gcc"
    exit 1
fi
echo "[OK] GCC: $(gcc --version | head -1)"

# =========================================
# Phase 1: Build libaetherion.a (C-SDK)
# =========================================
echo ""
echo "[SDK] Building libaetherion.a from sdk/c/ ..."

# GCC flags for bare-metal x86_64 WITH SSE2 (Jalon 33: SSE active in Ring 3)
# The kernel enables SSE via CR0/CR4 before jumping to Ring 3, and the kernel
# itself is built with -sse,+soft-float so it never touches XMM/YMM registers.
# User FPU state therefore survives syscalls automatically.
GCC_FLAGS="-nostdlib -fno-builtin -fno-stack-protector -ffreestanding \
    -msse2 -mfpmath=sse -mno-mmx -mno-red-zone \
    -fno-PIC -fno-pic -O2 -Wall -Wextra -mcmodel=large"

# Compile SDK source
gcc -c $GCC_FLAGS -o "$SDK_DIR/aetherion.o" "$SDK_DIR/aetherion.c"

# Compile CRT0 (C runtime startup with _start entry point)
gcc -c $GCC_FLAGS -o "$SDK_DIR/crt0.o" "$SDK_DIR/crt0.c"

# Create static library (crt0.o + aetherion.o)
ar rcs "$SDK_DIR/libaetherion.a" "$SDK_DIR/crt0.o" "$SDK_DIR/aetherion.o"
echo "[OK] libaetherion.a ($(stat -c %s "$SDK_DIR/libaetherion.a") bytes, includes crt0)"

# Also build a backwards-compatible libc_stub.o for any legacy references
# (symlink the object so old link commands still work)
cp "$SDK_DIR/aetherion.o" "$C_APPS_DIR/libc_stub.o"
echo "[OK] libc_stub.o (backwards-compat copy)"

# =========================================
# Phase 2: Generate linker script
# =========================================
# The canonical linker script lives in sdk/c/aetherion.ld
# We also write a copy to c_apps/ for backwards compat
LINKER_SCRIPT="$SDK_DIR/aetherion.ld"
cp "$LINKER_SCRIPT" "$C_APPS_DIR/c_app.ld"
echo "[OK] Linker script: $LINKER_SCRIPT"

# =========================================
# Phase 3: Build each C application
# =========================================
APPS="hello_c j19_test ls cat wget threads ui agent_ai agent_rag sh test_malloc test_preempt agent_sse agent_rust agent_saga wget_real agent_math tls_bridge"

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
    gcc -c $GCC_FLAGS -I"$SDK_DIR" -I"$C_APPS_DIR" \
        -o "$OBJ" \
        "$SRC"
    echo "[OK] ${APP}.o"

    echo "[LINK] Linking ${APP}.elf against crt0 + libaetherion.a..."
    ld -T "$LINKER_SCRIPT" -static \
        -o "$ELF" \
        "$SDK_DIR/crt0.o" \
        "$OBJ" \
        -L"$SDK_DIR" -laetherion
    echo "[OK] ${APP}.elf ($(stat -c %s "$ELF") bytes)"
done

# Cleanup object files
rm -f "$C_APPS_DIR"/*.o "$SDK_DIR"/*.o

# =========================================
# Verify all ELFs
# =========================================
echo ""
echo "[VERIFY] Built ELF binaries:"
for APP in $APPS; do
    ELF="$OUTPUT_DIR/${APP}.elf"
    if [ -f "$ELF" ]; then
        SIZE=$(stat -c %s "$ELF")
        echo "  ${APP}.elf  ${SIZE} bytes"
    fi
done

echo ""
echo "========================================="
echo "[BUILD_C] SUCCESS: SDK + all C apps built!"
echo "========================================="
