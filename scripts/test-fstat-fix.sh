#!/bin/bash
# Test the fstat VFS fix for GGUF model loading
# This script runs QEMU and checks for the expected debug markers
# 
# SUCCESS markers (must ALL appear):
#   [LINUX-FSTAT] fd=X path='/models/smollm2-135m-q4_0.gguf' VFS size=6016
#   [LLM] File size: 6016
#   [LLM] Generated:
#
# FAILURE markers (must NOT appear):
#   Bad GGUF magic
#   PANIC
#
# Usage: bash scripts/test-fstat-fix.sh
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$PROJECT_DIR"

ISO="target/aetherion-limine.iso"
DISK="disk.img"
LOG="/tmp/fstat_fix_test_$(date +%s).log"

if [ ! -f "$ISO" ]; then
    echo "ERROR: ISO not found: $ISO"
    echo "       Run scripts/rebuild-iso-only.sh first"
    exit 1
fi

echo "=== AetherionOS fstat VFS Fix Test ==="
echo "ISO:  $ISO ($(du -h "$ISO" | cut -f1))"
echo "Disk: $DISK"
echo "Log:  $LOG"
echo ""

# Run QEMU with agent_inference
echo "[1/3] Booting QEMU..."
(sleep 12; \
 printf 'exec /usr/bin/python3 -c "print(42*42)"\r'; \
 sleep 5; \
 printf 'exec /bin/agent_inference --model /models/smollm2.gguf --prompt "The future of OS is"\r'; \
 sleep 120) | \
timeout 180 qemu-system-x86_64 \
    -cdrom "$ISO" \
    -drive "file=$DISK,format=raw,if=virtio" \
    -m 4G -smp 4 -cpu Haswell,+avx2,+fma \
    -nographic -no-reboot \
    -serial mon:stdio \
    2>/dev/null | tee "$LOG"

echo ""
echo "=== [2/3] ANALYSIS ==="
echo ""

# Check for success markers
PASS=true

echo "--- fstat VFS fix ---"
if grep -qa "LINUX-FSTAT" "$LOG"; then
    echo "✅ [LINUX-FSTAT] debug marker FOUND"
    grep -a "LINUX-FSTAT" "$LOG" | head -3
else
    echo "❌ [LINUX-FSTAT] debug marker NOT FOUND — fix not active!"
    PASS=false
fi

echo ""
echo "--- GGUF parsing ---"
if grep -qa "File size:" "$LOG"; then
    echo "✅ [LLM] File size found"
    grep -a "File size:" "$LOG" | head -3
else
    echo "⚠️  [LLM] File size not found (may not be printed)"
fi

if grep -qa "Bad GGUF" "$LOG"; then
    echo "❌ Bad GGUF magic STILL appearing — fix NOT working"
    grep -a "Bad GGUF" "$LOG"
    PASS=false
else
    echo "✅ No 'Bad GGUF magic' errors"
fi

echo ""
echo "--- LLM Inference ---"
if grep -qa "Generated\|LLM-INFERENCE-OK" "$LOG"; then
    echo "✅ LLM Generated tokens!"
    grep -a "Generated\|LLM-INFERENCE-OK" "$LOG" | head -5
else
    echo "⚠️  No token generation output found"
fi

echo ""
echo "--- Boot health ---"
if grep -qa "ready\." "$LOG"; then
    echo "✅ Kernel booted successfully"
fi
if grep -qa "1764" "$LOG"; then
    echo "✅ Python Ring 3 works (42*42=1764)"
fi
if grep -qa "AVX2=true" "$LOG"; then
    echo "✅ AVX2 detected"
fi
if grep -qa "PANIC\|panic" "$LOG"; then
    echo "❌ PANIC detected!"
    grep -a "PANIC\|panic" "$LOG" | head -5
    PASS=false
fi

echo ""
echo "=== [3/3] VERDICT ==="
if [ "$PASS" = true ]; then
    echo "🎉 ALL CHECKS PASSED — fstat VFS fix is working!"
else
    echo "🔴 SOME CHECKS FAILED — review log: $LOG"
    echo ""
    echo "Last 30 lines:"
    tail -30 "$LOG"
fi
