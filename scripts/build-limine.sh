#!/bin/bash
# AetherionOS Limine Boot Image Builder
# FRONT 3 - Limine Protocol Migration
#
# Builds the kernel ELF with --features limine, creates an ISO using
# pre-built Limine binaries (no compilation needed).
#
# Usage:
#   bash scripts/build-limine.sh [--release]
#
# Prerequisites:
#   - Rust nightly-2026-04-21 with rust-src, llvm-tools-preview
#   - x86_64-unknown-none target installed
#   - xorriso (for ISO creation): apt install xorriso
#   - Limine binaries in third_party/limine/

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_DIR"

# Parse arguments
PROFILE="debug"
PROFILE_FLAG=""
if [[ "${1:-}" == "--release" ]]; then
    PROFILE="release"
    PROFILE_FLAG="--release"
fi

echo "╔══════════════════════════════════════════════════╗"
echo "║  AetherionOS Limine Boot Image Builder           ║"
echo "║  Profile: $PROFILE                               ║"
echo "║  Branch: migrate/limine                          ║"
echo "╚══════════════════════════════════════════════════╝"

# === Step 1: Check prerequisites ===
echo ""
echo "=== [1/5] Checking prerequisites ==="
rustc --version
rustup component list --installed | grep -q rust-src || {
    echo "ERROR: rust-src not installed. Run: rustup component add rust-src"
    exit 1
}
rustup component list --installed | grep -q llvm-tools || {
    echo "ERROR: llvm-tools-preview not installed. Run: rustup component add llvm-tools-preview"
    exit 1
}

# Find llvm-objcopy
OBJCOPY=$(find "$(rustc --print sysroot)" -name llvm-objcopy 2>/dev/null | head -1)
if [ -z "$OBJCOPY" ]; then
    echo "ERROR: llvm-objcopy not found. Run: rustup component add llvm-tools-preview"
    exit 1
fi
echo "[OK] llvm-objcopy: $OBJCOPY"

# Check for xorriso (needed for ISO creation)
if ! command -v xorriso &>/dev/null; then
    echo "[WARN] xorriso not found. ISO creation will be skipped."
    echo "[INFO] Install with: apt install xorriso"
    HAS_XORRISO=false
else
    echo "[OK] xorriso found"
    HAS_XORRISO=true
fi

echo "[OK] Prerequisites verified"

# === Step 2: Download/check Limine binaries ===
echo ""
echo "=== [2/5] Checking Limine binaries ==="
LIMINE_DIR="$PROJECT_DIR/third_party/limine"
LIMINE_VERSION="8.7.0"

if [ ! -d "$LIMINE_DIR" ]; then
    echo "[INFO] Downloading Limine v${LIMINE_VERSION}..."
    mkdir -p "$LIMINE_DIR"
    cd "$LIMINE_DIR"
    
    # Download pre-built Limine binaries
    LIMINE_URL="https://github.com/limine-bootloader/limine/releases/download/v${LIMINE_VERSION}/limine-${LIMINE_VERSION}.tar.gz"
    if command -v wget &>/dev/null; then
        wget -q "$LIMINE_URL" -O "limine.tar.gz" || {
            echo "ERROR: Failed to download Limine. Download manually from:"
            echo "  $LIMINE_URL"
            exit 1
        }
    elif command -v curl &>/dev/null; then
        curl -sL "$LIMINE_URL" -o "limine.tar.gz" || {
            echo "ERROR: Failed to download Limine. Download manually from:"
            echo "  $LIMINE_URL"
            exit 1
        }
    else
        echo "ERROR: Neither wget nor curl found. Install one and retry."
        exit 1
    fi
    
    tar xzf limine.tar.gz --strip-components=1
    rm -f limine.tar.gz
    cd "$PROJECT_DIR"
    echo "[OK] Limine v${LIMINE_VERSION} downloaded"
else
    echo "[OK] Limine directory exists: $LIMINE_DIR"
fi

# Check for key Limine files
LIMINE_BIOS_CD="$LIMINE_DIR/limine-bios-cd.bin"
LIMINE_BIOS_SYS="$LIMINE_DIR/limine-bios.sys"
LIMINE_UEFI="$LIMINE_DIR/BOOTX64.EFI"

for f in "$LIMINE_BIOS_CD" "$LIMINE_BIOS_SYS"; do
    if [ -f "$f" ]; then
        echo "[OK] Found: $(basename "$f")"
    else
        echo "[WARN] Missing: $f (BIOS boot may not work)"
    fi
done

# === Step 3: Build kernel with Limine feature ===
echo ""
echo "=== [3/5] Building kernel ELF (--features limine) ==="
export CARGO_BUILD_JOBS=1

# Use the Limine-specific linker script
LINKER_SCRIPT="$PROJECT_DIR/kernel/linker-x86_64.ld"
if [ ! -f "$LINKER_SCRIPT" ]; then
    echo "ERROR: Linker script not found at $LINKER_SCRIPT"
    exit 1
fi

# Build kernel with limine feature and custom linker script
# CRITICAL: Must pass -no-pie -static -relocation-model=static to produce ET_EXEC.
# Limine v8.x panics on ET_DYN (PIE) without PT_DYNAMIC segment.
# Setting RUSTFLAGS overrides .cargo/config.toml, so ALL flags must be listed here.
RUSTFLAGS="-C link-arg=-T$LINKER_SCRIPT -C link-arg=-no-pie -C link-arg=-static -C relocation-model=static -C code-model=kernel" \
    cargo build -p aetherion-kernel \
    --target x86_64-unknown-none \
    --features limine \
    $PROFILE_FLAG 2>&1

KERNEL_ELF="target/x86_64-unknown-none/$PROFILE/aetherion-kernel"
if [ ! -f "$KERNEL_ELF" ]; then
    echo "ERROR: Kernel ELF not found at $KERNEL_ELF"
    exit 1
fi
echo "[OK] Kernel ELF: $KERNEL_ELF ($(du -h "$KERNEL_ELF" | cut -f1))"

# Verify ELF format — MUST be ET_EXEC, not ET_DYN (PIE)
file "$KERNEL_ELF"
ELF_TYPE=$(readelf -h "$KERNEL_ELF" 2>/dev/null | grep "Type:" | awk '{print $2}')
if [ "$ELF_TYPE" = "DYN" ]; then
    echo ""
    echo "ERROR: Kernel ELF is ET_DYN (PIE) instead of ET_EXEC!"
    echo "       Limine v8.x cannot load PIE kernels without PT_DYNAMIC."
    echo "       Check RUSTFLAGS: must include -C link-arg=-no-pie -C relocation-model=static"
    exit 1
fi
echo "[OK] ELF type: $ELF_TYPE (expected EXEC)"

# === Step 4: Create ISO directory structure ===
echo ""
echo "=== [4/5] Creating ISO structure ==="
ISO_DIR="$PROJECT_DIR/target/limine-iso"
rm -rf "$ISO_DIR"
mkdir -p "$ISO_DIR/boot" "$ISO_DIR/boot/limine" "$ISO_DIR/EFI/BOOT"

# Copy kernel
cp "$KERNEL_ELF" "$ISO_DIR/boot/aetherion-kernel"
echo "[OK] Kernel copied to ISO"

# Copy Limine config
cp "$PROJECT_DIR/limine.conf" "$ISO_DIR/boot/limine/limine.conf"
echo "[OK] limine.conf copied"

# Copy Limine binaries
if [ -f "$LIMINE_BIOS_CD" ]; then
    cp "$LIMINE_BIOS_CD" "$ISO_DIR/boot/limine/"
fi
if [ -f "$LIMINE_BIOS_SYS" ]; then
    cp "$LIMINE_BIOS_SYS" "$ISO_DIR/boot/limine/"
fi
if [ -f "$LIMINE_UEFI" ]; then
    cp "$LIMINE_UEFI" "$ISO_DIR/EFI/BOOT/BOOTX64.EFI"
    echo "[OK] UEFI bootloader copied"
fi
echo "[OK] ISO structure ready"

# List ISO contents
echo "ISO contents:"
find "$ISO_DIR" -type f | sort | while read f; do
    echo "  $(echo "$f" | sed "s|$ISO_DIR/||") ($(du -h "$f" | cut -f1))"
done

# === Step 5: Create ISO ===
echo ""
echo "=== [5/5] Creating bootable ISO ==="
ISO_OUTPUT="$PROJECT_DIR/target/aetherion-limine.iso"

if [ "$HAS_XORRISO" = true ]; then
    xorriso -as mkisofs \
        -b boot/limine/limine-bios-cd.bin \
        -no-emul-boot \
        -boot-load-size 4 \
        -boot-info-table \
        --efi-boot EFI/BOOT/BOOTX64.EFI \
        -efi-boot-part --efi-boot-image \
        --protective-msdos-label \
        "$ISO_DIR" \
        -o "$ISO_OUTPUT" 2>&1 || {
            echo "[WARN] xorriso failed; falling back to directory-only output"
            echo "[INFO] ISO directory prepared at: $ISO_DIR/"
            echo "[INFO] You can create the ISO manually with xorriso."
            HAS_XORRISO=false
        }
fi

if [ "$HAS_XORRISO" = true ] && [ -f "$ISO_OUTPUT" ]; then
    # Install Limine to the ISO (for BIOS boot)
    LIMINE_DEPLOY="$LIMINE_DIR/limine"
    if [ -x "$LIMINE_DEPLOY" ]; then
        "$LIMINE_DEPLOY" bios-install "$ISO_OUTPUT" 2>&1 || true
        echo "[OK] Limine BIOS installed on ISO"
    else
        echo "[INFO] limine bios-install not available (compile limine CLI on host)"
    fi
    
    echo "[OK] ISO created: $ISO_OUTPUT ($(du -h "$ISO_OUTPUT" | cut -f1))"
else
    echo "[INFO] ISO creation skipped (xorriso not available or failed)"
    echo "[INFO] ISO directory ready at: $ISO_DIR/"
    echo "[INFO] Install xorriso and re-run, or create ISO manually."
fi

# === Summary ===
echo ""
echo "╔══════════════════════════════════════════════════╗"
echo "║  Limine Build Complete                            ║"
echo "╠══════════════════════════════════════════════════╣"
echo "║  Kernel ELF: $KERNEL_ELF"
if [ -f "$ISO_OUTPUT" ]; then
echo "║  ISO:        $ISO_OUTPUT"
fi
echo "╠══════════════════════════════════════════════════╣"
echo "║  QEMU (BIOS):                                    ║"
echo "║    qemu-system-x86_64 \\                          ║"
echo "║      -cdrom target/aetherion-limine.iso \\        ║"
echo "║      -serial stdio -m 4G -smp 2 -no-reboot      ║"
echo "╚══════════════════════════════════════════════════╝"
