#!/bin/bash
# userspace/build-all.sh — Build ALL userspace binaries for AetherionOS
#
# This script is called by CI to produce real ELF binaries.
# It builds:
#   1. C SDK (libaetherion.a)
#   2. C apps (linked against AetherionOS SDK, for x86_64-aetherion-user)
#   3. musl-gcc test binaries (for Linuxulator / dynamic linking tests)
#   4. Rust agents (for x86_64-aetherion-user)
#
# ZERO STUBS policy: any binary that fails to compile is skipped
# and documented, NOT replaced with a stub.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TOTAL=0
SUCCESS=0
FAILED=0
FAIL_LIST=""

echo "╔══════════════════════════════════════════════════╗"
echo "║  AetherionOS Userspace Build (Zero Stubs)        ║"
echo "║  Project: $PROJECT_DIR                           ║"
echo "╚══════════════════════════════════════════════════╝"

# ═══════════════════════════════════════════════════════
# Phase 1: Build C SDK + C Apps (AetherionOS native)
# ═══════════════════════════════════════════════════════
echo ""
echo "=== Phase 1: C SDK + C Apps ==="
if [ -f "$PROJECT_DIR/scripts/build_c.sh" ]; then
    bash "$PROJECT_DIR/scripts/build_c.sh" && {
        echo "[OK] C SDK + C Apps built"
    } || {
        echo "[WARN] C SDK build had errors"
    }
else
    echo "[SKIP] scripts/build_c.sh not found"
fi

# ═══════════════════════════════════════════════════════
# Phase 2: Build musl-gcc test binaries
# ═══════════════════════════════════════════════════════
echo ""
echo "=== Phase 2: musl-gcc Test Binaries ==="

if command -v musl-gcc &>/dev/null; then
    MUSL_GCC="musl-gcc"
elif [ -f "/usr/bin/x86_64-linux-musl-gcc" ]; then
    MUSL_GCC="x86_64-linux-musl-gcc"
else
    echo "[WARN] musl-gcc not found. Skipping musl binaries."
    echo "[INFO] Install with: sudo apt-get install musl-tools"
    MUSL_GCC=""
fi

if [ -n "$MUSL_GCC" ]; then
    echo "[OK] musl-gcc: $($MUSL_GCC --version | head -1)"
    
    # hello_dyn.elf — dynamically linked (requires ld-musl at runtime)
    echo "[BUILD] hello_dyn.elf (dynamic, for PT_INTERP test)"
    $MUSL_GCC -o "$PROJECT_DIR/userspace/hello_dyn.elf" \
        "$PROJECT_DIR/userspace/hello_dyn.c" 2>&1 && {
        echo "[OK] hello_dyn.elf ($(stat -c %s "$PROJECT_DIR/userspace/hello_dyn.elf") bytes)"
        SUCCESS=$((SUCCESS + 1))
    } || {
        echo "[FAIL] hello_dyn.elf"
        FAIL_LIST="$FAIL_LIST hello_dyn.elf"
        FAILED=$((FAILED + 1))
    }
    TOTAL=$((TOTAL + 1))
    
    # hello-linux — static Linux binary (for Linuxulator test)
    echo "[BUILD] hello-linux (static, for Linuxulator test)"
    $MUSL_GCC -static -o "$PROJECT_DIR/userspace/linux-test/hello-linux" \
        "$PROJECT_DIR/userspace/linux-test/hello-linux.c" 2>&1 && {
        echo "[OK] hello-linux ($(stat -c %s "$PROJECT_DIR/userspace/linux-test/hello-linux") bytes)"
        SUCCESS=$((SUCCESS + 1))
    } || {
        echo "[FAIL] hello-linux"
        FAIL_LIST="$FAIL_LIST hello-linux"
        FAILED=$((FAILED + 1))
    }
    TOTAL=$((TOTAL + 1))
fi

# ═══════════════════════════════════════════════════════
# Phase 3: Build Rust Agents
# ═══════════════════════════════════════════════════════
echo ""
echo "=== Phase 3: Rust Agents ==="
if [ -f "$PROJECT_DIR/scripts/build_all_agents.sh" ]; then
    bash "$PROJECT_DIR/scripts/build_all_agents.sh" 2>&1
    echo "[INFO] See output above for individual agent status"
else
    echo "[SKIP] scripts/build_all_agents.sh not found"
fi

# ═══════════════════════════════════════════════════════
# Phase 4: Anti-Stub Verification
# ═══════════════════════════════════════════════════════
echo ""
echo "=== Phase 4: Anti-Stub Verification ==="
STUB_COUNT=0
REAL_COUNT=0

# Check bin_cache/
for f in "$PROJECT_DIR/bin_cache/"*; do
    [ -f "$f" ] || continue
    SIZE=$(stat -c %s "$f")
    NAME=$(basename "$f")
    if [ "$SIZE" -lt 1024 ]; then
        echo "  STUB: $NAME ($SIZE bytes)"
        STUB_COUNT=$((STUB_COUNT + 1))
    else
        echo "  OK:   $NAME ($SIZE bytes)"
        REAL_COUNT=$((REAL_COUNT + 1))
    fi
done

# Check userspace/*.elf
for f in "$PROJECT_DIR/userspace/"*.elf; do
    [ -f "$f" ] || continue
    SIZE=$(stat -c %s "$f")
    NAME=$(basename "$f")
    if [ "$SIZE" -lt 1024 ]; then
        echo "  STUB: $NAME ($SIZE bytes)"
        STUB_COUNT=$((STUB_COUNT + 1))
    else
        echo "  OK:   $NAME ($SIZE bytes)"
        REAL_COUNT=$((REAL_COUNT + 1))
    fi
done

echo ""
echo "╔══════════════════════════════════════════════════╗"
echo "║  Userspace Build Summary                         ║"
echo "╠══════════════════════════════════════════════════╣"
echo "║  Real binaries: $REAL_COUNT"
echo "║  Stubs remaining: $STUB_COUNT"
if [ "$STUB_COUNT" -gt 0 ]; then
echo "║  WARNING: Stubs detected! See BLOCKERS.md"
fi
echo "╚══════════════════════════════════════════════════╝"
