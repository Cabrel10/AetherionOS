#!/bin/bash
set -e

# =====================================================================
# setup_musl_dynlink.sh — Download musl dynamic linker from Alpine Linux
# and copy ld-musl-x86_64.so.1 + libc.musl-x86_64.so.1 to disk.img
# =====================================================================

REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$REPO_DIR"

DISK=disk.img
WORKDIR=/tmp/musl_setup
ALPINE_URL="https://dl-cdn.alpinelinux.org/alpine/v3.20/main/x86_64/musl-1.2.5-r3.apk"

echo "=== Setting up musl dynamic linker for AetherionOS ==="

# Clean workspace
rm -rf "$WORKDIR"
mkdir -p "$WORKDIR"

# Download Alpine musl package
echo "[1/4] Downloading musl from Alpine Linux..."
if ! wget -q -O "$WORKDIR/musl.apk" "$ALPINE_URL" 2>/dev/null; then
    echo "[WARN] Primary URL failed, trying alternative..."
    ALPINE_URL="https://dl-cdn.alpinelinux.org/alpine/v3.19/main/x86_64/musl-1.2.4_git20230717-r4.apk"
    wget -q -O "$WORKDIR/musl.apk" "$ALPINE_URL" || {
        echo "[ERROR] Failed to download musl package"
        exit 1
    }
fi
echo "       Downloaded $(du -h $WORKDIR/musl.apk | cut -f1)"

# Extract
echo "[2/4] Extracting musl libraries..."
cd "$WORKDIR"
tar xzf musl.apk 2>/dev/null || {
    # Alpine APKs are gzipped tar
    gzip -d musl.apk 2>/dev/null || true
    tar xf musl.apk 2>/dev/null || tar xf musl 2>/dev/null
}

# Find the dynamic linker
LDMUSL=$(find "$WORKDIR" -name "ld-musl-x86_64.so.1" -o -name "libc.musl-x86_64.so.1" 2>/dev/null | head -1)
if [ -z "$LDMUSL" ]; then
    echo "[ERROR] Could not find ld-musl-x86_64.so.1 in package"
    find "$WORKDIR" -name "*.so*" -ls
    exit 1
fi
LDMUSL_DIR=$(dirname "$LDMUSL")
echo "       Found libraries in $LDMUSL_DIR"

# Show what we have
ls -lh "$LDMUSL_DIR/"*.so* 2>/dev/null || ls -lh "$LDMUSL_DIR/"

echo "[3/4] Creating /lib directory on disk.img..."
cd "$REPO_DIR"

# Create /lib directory on FAT32
mmd -i "$DISK" ::/lib 2>/dev/null || true

# Copy ld-musl-x86_64.so.1 to /lib/
# In musl, ld-musl-x86_64.so.1 IS libc.musl-x86_64.so.1 (same file / hardlink)
echo "[4/4] Copying musl libraries to disk.img:/lib/..."

# Find the actual .so file (it could be either name)
if [ -f "$LDMUSL_DIR/ld-musl-x86_64.so.1" ]; then
    mcopy -i "$DISK" -o "$LDMUSL_DIR/ld-musl-x86_64.so.1" ::/lib/ld-musl-x86_64.so.1
    echo "       Copied ld-musl-x86_64.so.1 ($(du -h "$LDMUSL_DIR/ld-musl-x86_64.so.1" | cut -f1))"
fi

if [ -f "$LDMUSL_DIR/libc.musl-x86_64.so.1" ]; then
    mcopy -i "$DISK" -o "$LDMUSL_DIR/libc.musl-x86_64.so.1" ::/lib/libc.musl-x86_64.so.1
    echo "       Copied libc.musl-x86_64.so.1 ($(du -h "$LDMUSL_DIR/libc.musl-x86_64.so.1" | cut -f1))"
fi

# If only one exists, copy it under both names
if [ -f "$LDMUSL_DIR/ld-musl-x86_64.so.1" ] && [ ! -f "$LDMUSL_DIR/libc.musl-x86_64.so.1" ]; then
    mcopy -i "$DISK" -o "$LDMUSL_DIR/ld-musl-x86_64.so.1" ::/lib/libc.musl-x86_64.so.1
    echo "       Aliased ld-musl → libc.musl"
fi
if [ ! -f "$LDMUSL_DIR/ld-musl-x86_64.so.1" ] && [ -f "$LDMUSL_DIR/libc.musl-x86_64.so.1" ]; then
    mcopy -i "$DISK" -o "$LDMUSL_DIR/libc.musl-x86_64.so.1" ::/lib/ld-musl-x86_64.so.1
    echo "       Aliased libc.musl → ld-musl"
fi

echo ""
echo "=== Verification ==="
mdir -i "$DISK" ::/lib/
echo ""
echo "=== Done! Musl dynamic linker installed on disk.img:/lib/ ==="

# Cleanup
rm -rf "$WORKDIR"
