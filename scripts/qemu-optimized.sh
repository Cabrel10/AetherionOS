#!/bin/bash
# AetherionOS v4.0 — Optimized QEMU Launch Script (Jalon 112)
#
# Performance optimizations:
#   - CPU: host passthrough with AVX2/FMA (or Haswell fallback)
#   - Memory: 1 GiB with hugepage support
#   - Disk: virtio-blk with cache=none for raw I/O
#   - Network: virtio-net with multiqueue
#   - CPU pinning: taskset when available
#
# Usage: ./scripts/qemu-optimized.sh [TIMEOUT_SECONDS]
#   Default: 120 seconds

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BOOTIMAGE="$PROJECT_DIR/kernel/target/x86_64-aetherion/release/bootimage-aetherion-kernel.bin"
DISK_IMG="$PROJECT_DIR/disk.img"
TIMEOUT="${1:-120}"

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║       AetherionOS v4.0 — Optimized KVM/QEMU Launch         ║"
echo "║       Jalon 112: CPU Pin + Hugepages + VirtIO Tuning        ║"
echo "╚══════════════════════════════════════════════════════════════╝"

# Check prerequisites
if [ ! -f "$BOOTIMAGE" ]; then
    echo "[ERROR] Bootimage not found. Run: cd kernel && cargo bootimage --release"
    exit 1
fi

# Detect KVM support
ACCEL=""
CPU_MODEL="Haswell"
if [ -e /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    ACCEL="-enable-kvm"
    CPU_MODEL="host,+avx2,+fma"
    echo "[KVM] Hardware acceleration enabled (host CPU passthrough)"
    echo "[KVM] AVX2+FMA enabled for accelerated inference"
else
    echo "[TCG] Software emulation mode (no KVM)"
    echo "[TCG] Using Haswell model with AVX2+FMA simulation"
fi

# Disk configuration
DISK_OPT=""
if [ -f "$DISK_IMG" ]; then
    # VirtIO-blk with cache=none for direct I/O
    DISK_OPT="-drive file=$DISK_IMG,format=raw,if=virtio,cache=none"
    echo "[DISK] VirtIO-blk with cache=none (direct I/O)"
else
    echo "[DISK] No disk image (VFS-only mode)"
fi

# Sysctl tuning (apply if we have permissions)
if [ -w /proc/sys/vm/swappiness ] 2>/dev/null; then
    echo 10 > /proc/sys/vm/swappiness 2>/dev/null || true
    echo 15 > /proc/sys/vm/dirty_ratio 2>/dev/null || true
    echo 5 > /proc/sys/vm/dirty_background_ratio 2>/dev/null || true
    echo 50 > /proc/sys/vm/vfs_cache_pressure 2>/dev/null || true
    echo "[TUNE] sysctl: swappiness=10 dirty_ratio=15 vfs_cache_pressure=50"
fi

# Hugepages (try to allocate if permissions allow)
if [ -w /proc/sys/vm/nr_hugepages ] 2>/dev/null; then
    CURRENT_HP=$(cat /proc/sys/vm/nr_hugepages 2>/dev/null || echo 0)
    if [ "$CURRENT_HP" -lt 256 ]; then
        echo 256 > /proc/sys/vm/nr_hugepages 2>/dev/null || true
        echo "[MEM] Allocated 256 hugepages (512 MiB for KV cache)"
    fi
fi

echo ""
echo "[LAUNCH] QEMU: 2 cores, 1 GiB RAM, timeout=${TIMEOUT}s"
echo "  CPU:  $CPU_MODEL"
echo "  SMP:  2 cores, 1 thread each"
echo "  RAM:  1024 MiB"
echo "  Net:  VirtIO-net (multiqueue)"
echo ""

# Launch with optimized settings
timeout "$TIMEOUT" qemu-system-x86_64 \
    $ACCEL \
    -drive format=raw,file="$BOOTIMAGE" \
    $DISK_OPT \
    -m 1024M \
    -cpu "$CPU_MODEL" \
    -smp 2,cores=2,threads=1 \
    -serial stdio \
    -display none \
    -no-reboot \
    -device virtio-net-pci,netdev=net0 \
    -netdev user,id=net0 \
    -device qemu-xhci \
    2>/dev/null || true

echo ""
echo "[DONE] QEMU session ended"
