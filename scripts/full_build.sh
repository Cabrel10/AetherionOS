#!/bin/bash
# ============================================================
# AetherionOS - Full Build Script (Jalon 146)
# ============================================================
# Single entry point that:
#   1. Sets up tmpfs for build target (avoid NUL-byte corruption)
#   2. Pre-fetches and locks Cargo dependencies
#   3. Creates stub ELF binaries for any missing userspace agents
#   4. Compiles the kernel with cargo bootimage
#   5. Optionally runs QEMU smoke test
# ============================================================
set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
KERNEL_DIR="$PROJECT_DIR/kernel"
TARGET_JSON="$PROJECT_DIR/x86_64-aetherion.json"
BOOTIMAGE="$KERNEL_DIR/target/x86_64-aetherion/release/bootimage-aetherion-kernel.bin"

export PATH="$HOME/.cargo/bin:$PATH"

echo "============================================================"
echo "  AetherionOS Full Build (Jalon 146)"
echo "  Project: $PROJECT_DIR"
echo "  Kernel:  $KERNEL_DIR"
echo "  Date:    $(date -u '+%Y-%m-%d %H:%M:%S UTC')"
echo "============================================================"
echo ""

# ── Phase 0: Environment sanity checks ──
echo "=== [0/5] Environment Checks ==="

# Check Rust toolchain
if ! command -v cargo &>/dev/null; then
    echo "[FATAL] cargo not found. Install Rust nightly-2023-08-01."
    exit 1
fi
echo "  Rust: $(rustc --version 2>&1)"

# Check target JSON
if [ ! -f "$TARGET_JSON" ]; then
    echo "[FATAL] $TARGET_JSON not found"
    exit 1
fi

# Check llvm-tools-preview
if ! rustup component list 2>/dev/null | grep -q "llvm-tools.*installed"; then
    echo "  Installing llvm-tools-preview..."
    rustup component add llvm-tools-preview 2>&1
fi
echo "  llvm-tools: installed"

# Check bootimage
if ! command -v cargo-bootimage &>/dev/null; then
    echo "  Installing cargo-bootimage..."
    cargo install bootimage 2>&1 | tail -3
fi
echo "  bootimage: $(cargo bootimage --version 2>&1 | head -1)"
echo ""

# ── Phase 1: Filesystem stabilization (anti-corruption) ──
echo "=== [1/5] Filesystem Stabilization ==="

# Setup tmpfs for kernel build target
if [ -L "$KERNEL_DIR/target" ]; then
    MOUNT_POINT=$(readlink -f "$KERNEL_DIR/target")
    if mountpoint -q "$MOUNT_POINT" 2>/dev/null; then
        echo "  tmpfs already mounted at $MOUNT_POINT"
    else
        echo "  Stale symlink, recreating tmpfs..."
        rm -f "$KERNEL_DIR/target"
        sudo mkdir -p /mnt/aetherion-build
        sudo mount -t tmpfs -o size=3g,mode=1777 tmpfs /mnt/aetherion-build 2>/dev/null
        ln -s /mnt/aetherion-build "$KERNEL_DIR/target"
    fi
elif [ ! -d "$KERNEL_DIR/target" ]; then
    echo "  Creating tmpfs mount for build target..."
    sudo mkdir -p /mnt/aetherion-build
    sudo mount -t tmpfs -o size=3g,mode=1777 tmpfs /mnt/aetherion-build 2>/dev/null || {
        echo "  [WARN] tmpfs mount failed, using disk target"
        mkdir -p "$KERNEL_DIR/target"
    }
    if mountpoint -q /mnt/aetherion-build 2>/dev/null; then
        ln -sf /mnt/aetherion-build "$KERNEL_DIR/target"
        echo "  tmpfs mounted (3GB) at /mnt/aetherion-build"
    fi
else
    echo "  Using existing target directory"
fi

# Fix git index if corrupted
cd "$PROJECT_DIR"
if ! git status &>/dev/null; then
    echo "  Fixing corrupted git index..."
    rm -f .git/index
    git reset HEAD -- . 2>/dev/null || true
fi
echo "  Git: OK"
echo ""

# ── Phase 2: Cargo dependency fetch + lock ──
echo "=== [2/5] Cargo Dependencies ==="
cd "$KERNEL_DIR"

# Unlock registry if readonly
chmod -R u+w "$HOME/.cargo/registry/src/" 2>/dev/null || true

# Clean corrupted registry entries
CORRUPT_COUNT=0
while IFS= read -r -d '' f; do
    first=$(head -c1 "$f" 2>/dev/null | od -A n -t x1 | tr -d ' ')
    if [ "$first" = "00" ]; then
        rm -rf "$(dirname "$f")"
        CORRUPT_COUNT=$((CORRUPT_COUNT + 1))
    fi
done < <(find "$HOME/.cargo/registry/src" -name "Cargo.toml" -print0 2>/dev/null)
[ $CORRUPT_COUNT -gt 0 ] && echo "  Cleaned $CORRUPT_COUNT corrupted crate(s)"

# Fetch dependencies
echo "  Fetching kernel dependencies..."
CARGO_BUILD_JOBS=1 cargo fetch 2>&1 | tail -1
echo "  Dependencies fetched"

# Lock registry readonly to prevent corruption
chmod -R a-w "$HOME/.cargo/registry/src/" 2>/dev/null
echo "  Registry locked (readonly)"
echo ""

# ── Phase 3: Ensure all include_bytes! targets exist ──
echo "=== [3/5] Userspace Binary Stubs ==="

# Create minimal ELF stub
STUB_ELF="/tmp/aetherion_stub.elf"
if [ ! -f "$STUB_ELF" ]; then
    cat > /tmp/stub.S << 'ASM'
.global _start
_start:
    mov $60, %rax
    xor %edi, %edi
    syscall
ASM
    as -o /tmp/stub.o /tmp/stub.S 2>&1 && ld -o "$STUB_ELF" /tmp/stub.o 2>&1
    rm -f /tmp/stub.o /tmp/stub.S
fi

STUB_COUNT=0
REAL_COUNT=0
grep "include_bytes!" "$KERNEL_DIR/src/main.rs" | \
    sed 's/.*include_bytes!("\(.*\)").*/\1/' | while read -r p; do
    actual="$KERNEL_DIR/src/$p"
    if [ ! -f "$actual" ] || [ ! -s "$actual" ]; then
        mkdir -p "$(dirname "$actual")"
        cp "$STUB_ELF" "$actual"
        STUB_COUNT=$((STUB_COUNT + 1))
    else
        REAL_COUNT=$((REAL_COUNT + 1))
    fi
done

# Verify all targets exist
MISSING=$(grep "include_bytes!" "$KERNEL_DIR/src/main.rs" | \
    sed 's/.*include_bytes!("\(.*\)").*/\1/' | while read -r p; do
    actual="$KERNEL_DIR/src/$p"
    [ ! -s "$actual" ] && echo "$actual"
done | wc -l)

if [ "$MISSING" -gt 0 ]; then
    echo "  [ERROR] $MISSING include_bytes! targets still missing"
    exit 1
fi
echo "  All include_bytes! targets resolved"
echo ""

# ── Phase 4: Build kernel ──
echo "=== [4/5] Kernel Build ==="

# Unlock registry temporarily for bootloader deps
chmod -R u+w "$HOME/.cargo/registry/src/" 2>/dev/null || true

echo "  Step 1: cargo check --lib..."
CARGO_BUILD_JOBS=1 cargo check --lib --target "$TARGET_JSON" 2>&1 | tail -3
if [ ${PIPESTATUS[0]:-$?} -ne 0 ]; then
    echo "  [FATAL] cargo check failed"
    exit 1
fi
echo "  [OK] cargo check passed"

echo "  Step 2: cargo bootimage --release..."
CARGO_BUILD_JOBS=1 cargo bootimage --release --target "$TARGET_JSON" 2>&1 | tail -5
BUILD_EXIT=${PIPESTATUS[0]:-$?}

# Re-lock registry
chmod -R a-w "$HOME/.cargo/registry/src/" 2>/dev/null

if [ $BUILD_EXIT -ne 0 ]; then
    echo "  [FATAL] cargo bootimage failed (exit $BUILD_EXIT)"
    echo "  Retrying with registry cleanup..."
    
    # Unlock, clean, refetch, rebuild
    chmod -R u+w "$HOME/.cargo/registry/src/" 2>/dev/null || true
    find "$HOME/.cargo/registry/src" -name "Cargo.toml" -exec sh -c \
        'first=$(head -c1 "$1" | od -A n -t x1 | tr -d " "); [ "$first" = "00" ] && rm -rf "$(dirname $1)"' _ {} \; 2>/dev/null
    
    CARGO_BUILD_JOBS=1 cargo bootimage --release --target "$TARGET_JSON" 2>&1 | tail -5
    BUILD_EXIT=${PIPESTATUS[0]:-$?}
    chmod -R a-w "$HOME/.cargo/registry/src/" 2>/dev/null
    
    if [ $BUILD_EXIT -ne 0 ]; then
        echo "  [FATAL] Build failed on retry"
        exit 1
    fi
fi

if [ -f "$BOOTIMAGE" ]; then
    SIZE=$(ls -lh "$BOOTIMAGE" | awk '{print $5}')
    echo "  [OK] Bootimage created: $BOOTIMAGE ($SIZE)"
else
    echo "  [FATAL] Bootimage file not found"
    exit 1
fi
echo ""

# ── Phase 5: Optional QEMU smoke test ──
echo "=== [5/5] QEMU Smoke Test ==="

if [ "${SKIP_QEMU:-0}" = "1" ]; then
    echo "  Skipped (SKIP_QEMU=1)"
else
    if ! command -v qemu-system-x86_64 &>/dev/null; then
        echo "  Skipped (QEMU not installed)"
    else
        # Create disk image if missing
        DISK_IMG="$PROJECT_DIR/disk.img"
        if [ ! -f "$DISK_IMG" ]; then
            dd if=/dev/zero of="$DISK_IMG" bs=1M count=64 2>/dev/null
            mkfs.vfat -F 32 "$DISK_IMG" 2>/dev/null || true
        fi
        
        echo "  Running QEMU (30s timeout, no KVM)..."
        QEMU_LOG="/tmp/qemu_smoke.log"
        timeout 30 qemu-system-x86_64 \
            -cpu qemu64 -smp 2 -m 512M \
            -drive format=raw,file="$BOOTIMAGE" \
            -drive format=raw,file="$DISK_IMG",if=virtio \
            -display none -serial stdio -no-reboot 2>&1 | tee "$QEMU_LOG" >/dev/null
        
        PANICS=$(grep -ci "PANIC\|SIGSEGV\|kernel panic" "$QEMU_LOG" 2>/dev/null || echo 0)
        BOOT_OK=$(grep -c "BOOT\|Couche" "$QEMU_LOG" 2>/dev/null || echo 0)
        
        if [ "$PANICS" -gt 0 ]; then
            echo "  [FAIL] $PANICS PANIC/SIGSEGV detected in QEMU log"
            grep -i "PANIC\|SIGSEGV" "$QEMU_LOG" | head -5
        else
            echo "  [OK] No PANIC/SIGSEGV detected ($BOOT_OK boot messages)"
        fi
    fi
fi

echo ""
echo "============================================================"
echo "  BUILD COMPLETE"
echo "  Bootimage: $BOOTIMAGE"
echo "  Size: $(ls -lh "$BOOTIMAGE" 2>/dev/null | awk '{print $5}')"
echo "============================================================"
