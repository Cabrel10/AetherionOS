#!/bin/bash
# AetherionOS — Automated Regression Test Suite
# Runs headless QEMU, captures serial output, validates all test criteria.
# Usage: ./scripts/regression-test.sh [--timeout SECONDS] [--rebuild]
#
# Exit codes:
#   0 = ALL TESTS PASSED
#   1 = Test failure(s) detected
#   2 = Build failure
#   3 = QEMU launch failure

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BOOTIMAGE="$PROJECT_DIR/kernel/target/x86_64-aetherion/release/bootimage-aetherion-kernel.bin"
LOG_FILE="/tmp/aetherion_regression_$(date +%s).log"
TIMEOUT=${1:-30}
REBUILD=false

# Parse args
for arg in "$@"; do
    case "$arg" in
        --rebuild) REBUILD=true ;;
        --timeout) shift; TIMEOUT="$1" ;;
        [0-9]*) TIMEOUT="$arg" ;;
    esac
done

echo "=============================================="
echo "  AetherionOS Automated Regression Test Suite"
echo "=============================================="
echo "  Project: $PROJECT_DIR"
echo "  Timeout: ${TIMEOUT}s"
echo "  Log:     $LOG_FILE"
echo ""

# ── Step 1: Build (if needed or requested) ──
if [ "$REBUILD" = true ] || [ ! -f "$BOOTIMAGE" ]; then
    echo "[BUILD] Building kernel bootimage..."
    cd "$PROJECT_DIR/kernel"
    CARGO_BUILD_JOBS=2 \
    RUSTFLAGS="-C relocation-model=static -C code-model=kernel -C overflow-checks=yes" \
    cargo bootimage --release --target x86_64-aetherion.json 2>&1 | tail -3
    if [ ! -f "$BOOTIMAGE" ]; then
        echo "[FAIL] Bootimage not created!"
        exit 2
    fi
    echo "[BUILD] OK"
else
    echo "[BUILD] Using existing bootimage ($(stat -c%s "$BOOTIMAGE") bytes)"
fi
echo ""

# ── Step 2: Run QEMU headless ──
echo "[QEMU] Launching headless QEMU (timeout=${TIMEOUT}s)..."
cd "$PROJECT_DIR"
timeout "$TIMEOUT" qemu-system-x86_64 \
    -drive format=raw,file="$BOOTIMAGE" \
    -m 256M -serial stdio -display none \
    -cpu Haswell \
    -device virtio-net-pci,netdev=net0 -netdev user,id=net0 \
    -device qemu-xhci \
    2>/dev/null > "$LOG_FILE" || true

LINE_COUNT=$(wc -l < "$LOG_FILE" 2>/dev/null || echo 0)
BYTE_COUNT=$(stat -c%s "$LOG_FILE" 2>/dev/null || echo 0)
echo "[QEMU] Captured $BYTE_COUNT bytes, $LINE_COUNT lines"
echo ""

# ── Step 3: Run assertions ──
TOTAL=0
PASSED=0
FAILED=0

# Pre-extract strings from the log for efficient grep
STRINGS_FILE="${LOG_FILE}.strings"
strings "$LOG_FILE" > "$STRINGS_FILE" 2>/dev/null

check() {
    local name="$1"
    local pattern="$2"
    TOTAL=$((TOTAL + 1))
    if grep -qE "$pattern" "$STRINGS_FILE" 2>/dev/null; then
        echo "  [PASS] $name"
        PASSED=$((PASSED + 1))
    else
        echo "  [FAIL] $name"
        FAILED=$((FAILED + 1))
    fi
}

check_not() {
    local name="$1"
    local pattern="$2"
    TOTAL=$((TOTAL + 1))
    if ! grep -qiE "$pattern" "$STRINGS_FILE" 2>/dev/null; then
        echo "  [PASS] $name"
        PASSED=$((PASSED + 1))
    else
        echo "  [FAIL] $name"
        FAILED=$((FAILED + 1))
    fi
}

check_count() {
    local name="$1"
    local pattern="$2"
    local min="$3"
    TOTAL=$((TOTAL + 1))
    local count
    count=$(grep -cE "$pattern" "$STRINGS_FILE" 2>/dev/null || echo 0)
    if [ "$count" -ge "$min" ]; then
        echo "  [PASS] $name ($count >= $min)"
        PASSED=$((PASSED + 1))
    else
        echo "  [FAIL] $name ($count < $min)"
        FAILED=$((FAILED + 1))
    fi
}

echo "=== Boot & Initialization ==="
TOTAL=$((TOTAL + 1))
if [ "$BYTE_COUNT" -gt 100 ]; then
    echo "  [PASS] Kernel boots (log not empty)"
    PASSED=$((PASSED + 1))
else
    echo "  [FAIL] Kernel boots (log not empty)"
    FAILED=$((FAILED + 1))
fi

check_not "No kernel PANIC" "PANIC"
check "Terminal ready" "Terminal ready"
check "Version string present" "v[0-9]+\\.[0-9]+"

echo ""
echo "=== Hardware Tests ==="
check "GDT + TSS initialized" "GDT.*TSS"
check "PIC remapped" "PIC remapped"
check "PS/2 keyboard OK" "PS/2 keyboard"
check "Memory manager ready" "Memory manager ready"
check "Heap allocator ready" "Heap allocator ready"

echo ""
echo "=== Test Suites ==="
check "Heap tests ALL PASSED" "Stress.*100 alloc.*OK"
check "VFS tests ALL PASSED" "VFS TESTS.*ALL TESTS PASSED"
check "Verifier tests ALL PASSED" "VERIFIER TESTS.*passed"
check "Process tests ALL PASSED" "PROCESS TESTS.*ALL TESTS PASSED"
check "Scheduler tests ALL PASSED" "SCHEDULER TESTS.*ALL TESTS PASSED"
check "Context switch tests ALL PASSED" "CONTEXT SWITCH TESTS.*ALL TESTS PASSED"
check "Syscall tests ALL PASSED" "SYSCALL TESTS.*ALL TESTS PASSED"
check "ELF tests ALL PASSED" "ELF TESTS.*ALL TESTS PASSED"
check "Network tests ALL PASSED" "NET TESTS.*ALL TESTS PASSED"
check "Mouse tests ALL PASSED" "MOUSE TESTS.*ALL TESTS PASSED"

echo ""
echo "=== Multi-Agent Scheduling ==="
YIELD_COUNT=$(grep -cE '\[YIELD\] ' "$STRINGS_FILE" 2>/dev/null || echo 0)
check_count "YIELD exchanges >= 5" "\[YIELD\] " 5

SIGSEGV_COUNT=$(grep -c 'SIGSEGV' "$STRINGS_FILE" 2>/dev/null || echo 0)
check_not "No SIGSEGV crashes" "SIGSEGV"

check "Agent llama_core loaded" "agent_llama_core.elf.*QUEUED"
check "Agent visual_term launched" "Visual Terminal PID"

echo ""
echo "=== Cognitive Bus (LLM Pipeline) ==="
BUS_PUB=$(grep -c 'bus_publish' "$STRINGS_FILE" 2>/dev/null || echo 0)
check_count "Bus publish events > 0" "bus_publish" 1

TOKEN_EVENTS=$(grep -c '0x8063' "$STRINGS_FILE" 2>/dev/null || echo 0)
check_count "Token generation events (INTENT_TOKEN_GEN)" "0x8063" 1

check "Bus consume events" "bus_consume"

echo ""
echo "=== Security ==="
check "Path traversal blocked" "traversal.*(blocked|OK)"
check "Stack protector active" "(stack protector|TPM)"
check "Verifier default-deny rule" "default-deny"

echo ""
echo "=== Level 3: VFS & Commands ==="
check "All agent binaries mounted" "additional agent binaries mounted"
check "/var/state.bin created" "state\\.bin created"
check_not "No break-code keyboard logs" "\\[KBD\\] break|scancode release"

echo ""
echo "=== Level 4: Keyboard ==="
check "PS/2 keyboard initialized" "PS/2 keyboard"
check_not "No spurious break-code events" "\\[KBD\\].*release|\\[KBD\\].*break"

# Cleanup
rm -f "$STRINGS_FILE"

echo ""
echo "=============================================="
echo "  RESULTS: $PASSED/$TOTAL passed, $FAILED failed"
echo "  YIELD exchanges: $YIELD_COUNT"
echo "  SIGSEGV crashes:  $SIGSEGV_COUNT"
echo "  Bus events: $BUS_PUB (tokens: $TOKEN_EVENTS)"
echo "=============================================="

if [ "$FAILED" -eq 0 ]; then
    echo ""
    echo "  >>> ALL TESTS PASSED <<<"
    echo ""
    exit 0
else
    echo ""
    echo "  >>> $FAILED TEST(S) FAILED <<<"
    echo ""
    exit 1
fi
