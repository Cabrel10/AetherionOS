#!/bin/bash
# AetherionOS v4.0-smp-stable — Demo Script
# Demonstrates the full kernel boot, multi-agent scheduling, and GGUF model loading.
#
# Usage: ./scripts/demo.sh [TIMEOUT_SECONDS]
#   Default timeout: 60 seconds
#
# Prerequisites:
#   - QEMU system (qemu-system-x86_64)
#   - Kernel bootimage built: kernel/target/x86_64-aetherion/release/bootimage-aetherion-kernel.bin
#   - Disk image: disk.img (with /models/smollm.gguf)

set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BOOTIMAGE="$PROJECT_DIR/kernel/target/x86_64-aetherion/release/bootimage-aetherion-kernel.bin"
DISK_IMG="$PROJECT_DIR/disk.img"
TIMEOUT="${1:-60}"
LOG="/tmp/aetherion_demo_$$.log"

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║          AetherionOS v4.0-smp-stable — LIVE DEMO           ║"
echo "║        Jalon 109c: Zero-Crash SMP Kernel + LLM Agents      ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""

# Check prerequisites
if [ ! -f "$BOOTIMAGE" ]; then
    echo "[ERROR] Bootimage not found. Run: cd kernel && cargo bootimage --release"
    exit 1
fi

echo "[1/4] Starting QEMU (2 cores, 1 GiB RAM, ${TIMEOUT}s timeout)..."
DISK_OPT=""
[ -f "$DISK_IMG" ] && DISK_OPT="-drive format=raw,file=$DISK_IMG,if=virtio"

timeout "$TIMEOUT" qemu-system-x86_64 \
    -drive format=raw,file="$BOOTIMAGE" \
    $DISK_OPT \
    -serial stdio -display none -no-reboot \
    -cpu Haswell -smp 2 -m 1024M 2>/dev/null > "$LOG" || true

echo "[2/4] Processing output..."
CLEAN_LOG="/tmp/aetherion_demo_clean_$$.log"
strings -n 4 "$LOG" > "$CLEAN_LOG"

echo "[3/4] Results:"
echo ""
echo "  ┌─ Stability ──────────────────────────────────────┐"
printf "  │ SIGSEGV:       %6d  (target: 0)                │\n" "$(grep -c 'SIGSEGV' "$CLEAN_LOG" 2>/dev/null || echo 0)"
printf "  │ Double Fault:  %6d  (target: 0)                │\n" "$(grep -c 'DOUBLE FAULT' "$CLEAN_LOG" 2>/dev/null || echo 0)"
printf "  │ Panic:         %6d  (target: 0)                │\n" "$(grep -c 'PANIC' "$CLEAN_LOG" 2>/dev/null || echo 0)"
printf "  │ PF-FATAL:      %6d  (target: 0)                │\n" "$(grep -c 'PF-FATAL' "$CLEAN_LOG" 2>/dev/null || echo 0)"
echo "  └──────────────────────────────────────────────────┘"
echo ""
echo "  ┌─ Agents ─────────────────────────────────────────┐"
printf "  │ Terminal ready: %5d                             │\n" "$(grep -c 'Terminal ready' "$CLEAN_LOG" 2>/dev/null || echo 0)"
printf "  │ Bus publish:    %5d  (target: ≥5)              │\n" "$(grep -c 'bus_publish' "$CLEAN_LOG" 2>/dev/null || echo 0)"
printf "  │ Bus consume:    %5d                             │\n" "$(grep -c 'bus_consume' "$CLEAN_LOG" 2>/dev/null || echo 0)"
printf "  │ YIELD:          %5d                             │\n" "$(grep -c 'YIELD' "$CLEAN_LOG" 2>/dev/null || echo 0)"
printf "  │ Agent clock:    %5d                             │\n" "$(grep -c 'agent_clock' "$CLEAN_LOG" 2>/dev/null || echo 0)"
echo "  └──────────────────────────────────────────────────┘"
echo ""
echo "  ┌─ LLM/Model ─────────────────────────────────────┐"
HAS_GGUF=$(grep -c 'GGUF' "$CLEAN_LOG" 2>/dev/null || echo 0)
HAS_TENSOR=$(grep -c 'tensor' "$CLEAN_LOG" 2>/dev/null || echo 0)
printf "  │ GGUF loading:   %5d lines                      │\n" "$HAS_GGUF"
printf "  │ Tensor refs:    %5d                             │\n" "$HAS_TENSOR"
echo "  └──────────────────────────────────────────────────┘"

echo ""
echo "[4/4] Key log excerpts:"
echo "  ---"
grep -E "Terminal ready|GGUF v3|KV.*dim|KV.*layer|bus_publish.*#[0-9]|VALIDATOR.*READY" "$CLEAN_LOG" | head -10 | while read line; do
    echo "  $line"
done
echo "  ---"

# Cleanup
rm -f "$CLEAN_LOG"

echo ""
echo "Full log: $LOG ($(wc -c < "$LOG") bytes)"
echo ""
echo "╔══════════════════════════════════════════════════════════════╗"
echo "║                    DEMO COMPLETE                            ║"
echo "╚══════════════════════════════════════════════════════════════╝"
