#!/bin/bash
# AetherionOS — Automated Regression Test Suite v2.0
# Robust, deterministic tests with proper binary-log handling.
# Usage: ./scripts/regression-test.sh [--timeout SECONDS] [--rebuild]
#
# Exit codes:
#   0 = ALL TESTS PASSED
#   1 = Test failure(s) detected
#   2 = Build failure
#   3 = QEMU launch failure

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BOOTIMAGE="$PROJECT_DIR/kernel/target/x86_64-aetherion/release/bootimage-aetherion-kernel.bin"
LOG_FILE="/tmp/aetherion_regression_$(date +%s).log"
CLEAN_LOG="${LOG_FILE}.clean"
TIMEOUT=45
REBUILD=false

# Parse args
while [ $# -gt 0 ]; do
    case "$1" in
        --rebuild) REBUILD=true; shift ;;
        --timeout) shift; TIMEOUT="${1:-45}"; shift ;;
        [0-9]*) TIMEOUT="$1"; shift ;;
        *) shift ;;
    esac
done

echo "=============================================="
echo "  AetherionOS Regression Test Suite v2.0"
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
    cargo bootimage --release --target x86_64-aetherion.json 2>&1 | tail -5
    if [ ! -f "$BOOTIMAGE" ]; then
        echo "[FAIL] Bootimage not created!"
        exit 2
    fi
    echo "[BUILD] OK ($(stat -c%s "$BOOTIMAGE") bytes)"
else
    echo "[BUILD] Using existing bootimage ($(stat -c%s "$BOOTIMAGE") bytes)"
fi
echo ""

# ── Step 2: Run QEMU headless with -cpu Haswell ──
echo "[QEMU] Launching headless QEMU (timeout=${TIMEOUT}s, cpu=Haswell, ram=256M)..."
cd "$PROJECT_DIR"
timeout "$TIMEOUT" qemu-system-x86_64 \
    -drive format=raw,file="$BOOTIMAGE" \
    -m 256M -serial stdio -display none \
    -cpu Haswell -no-reboot \
    -device virtio-net-pci,netdev=net0 -netdev user,id=net0 \
    -device qemu-xhci \
    2>/dev/null > "$LOG_FILE" || true

BYTE_COUNT=$(stat -c%s "$LOG_FILE" 2>/dev/null || echo 0)

# Extract printable strings from binary serial output (min length 4)
strings -n 4 "$LOG_FILE" > "$CLEAN_LOG" 2>/dev/null
LINE_COUNT=$(wc -l < "$CLEAN_LOG" 2>/dev/null || echo 0)
echo "[QEMU] Captured $BYTE_COUNT raw bytes, $LINE_COUNT text lines"
echo ""

# ── Step 3: Test Framework ──
TOTAL=0
PASSED=0
FAILED=0
FAIL_LIST=""

# Check: pattern present in clean log
check() {
    local name="$1"
    local pattern="$2"
    TOTAL=$((TOTAL + 1))
    if grep -qP "$pattern" "$CLEAN_LOG" 2>/dev/null || grep -qE "$pattern" "$CLEAN_LOG" 2>/dev/null; then
        echo "  [PASS] $name"
        PASSED=$((PASSED + 1))
    else
        echo "  [FAIL] $name"
        FAILED=$((FAILED + 1))
        FAIL_LIST="$FAIL_LIST\n    - $name"
    fi
}

# Check: pattern NOT present
check_not() {
    local name="$1"
    local pattern="$2"
    TOTAL=$((TOTAL + 1))
    if ! grep -qiE "$pattern" "$CLEAN_LOG" 2>/dev/null; then
        echo "  [PASS] $name"
        PASSED=$((PASSED + 1))
    else
        echo "  [FAIL] $name"
        FAILED=$((FAILED + 1))
        FAIL_LIST="$FAIL_LIST\n    - $name"
    fi
}

# Check: pattern appears at least N times
check_count() {
    local name="$1"
    local pattern="$2"
    local min="$3"
    TOTAL=$((TOTAL + 1))
    local count
    count=$(grep -cE "$pattern" "$CLEAN_LOG" 2>/dev/null || true)
    count=$(echo "$count" | tr -d '[:space:]')
    count=${count:-0}
    if [ "$count" -ge "$min" ] 2>/dev/null; then
        echo "  [PASS] $name (count=$count >= $min)"
        PASSED=$((PASSED + 1))
    else
        echo "  [FAIL] $name (count=$count < $min)"
        FAILED=$((FAILED + 1))
        FAIL_LIST="$FAIL_LIST\n    - $name (got $count, need $min)"
    fi
}

# Check: numeric value comparison
check_val() {
    local name="$1"
    local val="$2"
    local op="$3"   # -ge, -gt, -eq, -le, -lt
    local target="$4"
    TOTAL=$((TOTAL + 1))
    val=$(echo "$val" | tr -d '[:space:]')
    val=${val:-0}
    if [ "$val" "$op" "$target" ] 2>/dev/null; then
        echo "  [PASS] $name (value=$val)"
        PASSED=$((PASSED + 1))
    else
        echo "  [FAIL] $name (value=$val, expected $op $target)"
        FAILED=$((FAILED + 1))
        FAIL_LIST="$FAIL_LIST\n    - $name (val=$val vs $target)"
    fi
}

# =============================================
# Test Category 1: Boot & Initialization (5 tests)
# =============================================
echo "=== [Cat 1] Boot & Initialization ==="
check_val "T01 Kernel boots (output > 1KB)" "$BYTE_COUNT" "-ge" "1000"
check "T02 Kernel version string" "[Vv]ersion.*[0-9]+\.[0-9]+|v[0-9]+\.[0-9]+ "
check "T03 Boot banner present" "AetherionOS"
check_not "T04 No triple fault" "triple fault|Triple Fault"
check_not "T05 No double fault panic" "double.fault|Double Fault"

# =============================================
# Test Category 2: Hardware Initialization (6 tests)
# =============================================
echo ""
echo "=== [Cat 2] Hardware Initialization ==="
check "T06 GDT + TSS initialized" "GDT.*TSS|TSS.*GDT|GDT loaded"
check "T07 PIC remapped" "PIC remapped"
check "T08 PS/2 keyboard initialized" "PS/2 keyboard"
check "T09 Memory manager ready" "Memory manager ready|memory.*ready"
check "T10 Heap allocator ready" "Heap allocator ready|heap.*ready"
check "T11 IDT loaded" "IDT|Interrupt.*loaded|interrupt.*handler"

# =============================================
# Test Category 3: Kernel Test Suites (10 tests)
# =============================================
echo ""
echo "=== [Cat 3] Kernel Self-Test Suites ==="
check "T12 Heap stress test passed" "Stress.*100 alloc.*OK|HEAP.*PASS|alloc.*free.*OK"
check "T13 VFS tests passed" "VFS TESTS.*ALL TESTS PASSED|VFS.*PASS"
check "T14 Verifier tests passed" "VERIFIER TESTS.*passed|verifier.*PASS"
check "T15 Process tests passed" "PROCESS TESTS.*ALL TESTS PASSED|PROCESS.*PASS"
check "T16 Scheduler tests passed" "SCHEDULER TESTS.*ALL TESTS PASSED|SCHEDULER.*PASS"
check "T17 Context switch tests passed" "CONTEXT SWITCH TESTS.*ALL TESTS PASSED|CONTEXT.*PASS"
check "T18 Syscall tests passed" "SYSCALL TESTS.*ALL TESTS PASSED|SYSCALL.*PASS"
check "T19 ELF loader tests passed" "ELF TESTS.*ALL TESTS PASSED|ELF.*PASS"
check "T20 Network tests passed" "NET TESTS.*ALL TESTS PASSED|NET.*PASS"
check "T21 Mouse tests passed" "MOUSE TESTS.*ALL TESTS PASSED|MOUSE.*PASS"

# =============================================
# Test Category 4: Security (4 tests)
# =============================================
echo ""
echo "=== [Cat 4] Security ==="
check "T22 Path traversal protection" "traversal.*(blocked|OK|denied|prevented)"
check "T23 Stack protector / TPM" "(stack protector|TPM|security.*init|canary)"
check "T24 Verifier default-deny" "default.deny|deny.*rule|policy.*deny"
check "T25 ELF page isolation (KPTI)" "PML4.*isolated|KPTI|kernel entries cloned"

# =============================================
# Test Category 5: VFS & Filesystem (4 tests)
# =============================================
echo ""
echo "=== [Cat 5] VFS & Filesystem ==="
check "T26 Agent binaries mounted" "agent.*binaries mounted|mounted.*binaries|/bin/.*elf"
check "T27 /var/state.bin created" "state.bin created|state\.bin"
check "T28 VFS mount operations" "mount|VFS.*init|filesystem"
check "T29 VFS tests all passed" "VFS TESTS.*passed.*0 failed|VFS.*ALL TESTS PASSED"

# =============================================
# Test Category 6: ELF Loading (4 tests)
# =============================================
echo ""
echo "=== [Cat 6] ELF Loading ==="
check "T30 ELF headers parsed" "entry=0x[0-9a-fA-F]+.*stack=0x"
check "T31 User PML4 created" "User PML4 created|PML4.*isolated"
check "T32 ELF segments loaded" "segments=|Load complete|load_elf_binary"
check "T33 User stack mapped" "user stack|stack.*mapped|0x7FFF"

# =============================================
# Test Category 7: Multi-Agent Scheduling (5 tests)
# =============================================
echo ""
echo "=== [Cat 7] Multi-Agent Scheduling ==="
check "T34 Agent llm_chat queued" "agent_llm_chat.*QUEUED|llm_chat.*queue"
check "T35 Agent llama_core queued" "agent_llama_core.*QUEUED|llama_core.*queue"
check "T36 IRETQ to Ring 3" "IRETQ.*Ring 3|Ring 3|launches NOW"
check_not "T37 No SIGSEGV crashes" "SIGSEGV"
# Count YIELD exchanges from both raw and clean logs
YIELD_RAW=$(grep -c 'YIELD' "$CLEAN_LOG" 2>/dev/null || true)
YIELD_RAW=$(echo "$YIELD_RAW" | tr -d '[:space:]')
YIELD_RAW=${YIELD_RAW:-0}
check_val "T38 YIELD exchanges >= 5" "$YIELD_RAW" "-ge" "5"

# =============================================
# Test Category 8: Cognitive Bus / LLM Pipeline (4 tests)
# =============================================
echo ""
echo "=== [Cat 8] Cognitive Bus & LLM Pipeline ==="
BUS_PUB=$(grep -c 'bus_publish' "$CLEAN_LOG" 2>/dev/null || true)
BUS_PUB=$(echo "$BUS_PUB" | tr -d '[:space:]')
BUS_PUB=${BUS_PUB:-0}
check_val "T39 Bus publish events >= 1" "$BUS_PUB" "-ge" "1"

TOKEN_EV=$(grep -c '0x8063' "$CLEAN_LOG" 2>/dev/null || true)
TOKEN_EV=$(echo "$TOKEN_EV" | tr -d '[:space:]')
TOKEN_EV=${TOKEN_EV:-0}
check_val "T40 Token gen events (0x8063) >= 1" "$TOKEN_EV" "-ge" "1"

check "T41 sys_brk syscall handled" "sys_brk"
check "T42 Scheduler init with processes" "Scheduler.*init|queued=|queue.*len"

# =============================================
# Test Category 9: Stability (4 tests)
# =============================================
echo ""
echo "=== [Cat 9] Stability ==="
check_not "T43 No kernel panic" "\[PANIC\].*fault|panic.*kernel"
check_not "T44 No page fault crash" "page.fault.*halted|page.*fault.*panic"
check_not "T45 No stack overflow" "stack overflow|STACK OVERFLOW"
# Check that QEMU ran for meaningful duration (>1KB output means not instant crash)
check_val "T46 Boot completes (output > 10KB)" "$BYTE_COUNT" "-ge" "10000"

# =============================================
# Test Category 10: Keyboard & Terminal (2 tests)
# =============================================
echo ""
echo "=== [Cat 10] Keyboard & Terminal ==="
check "T47 PS/2 keyboard handler active" "PS/2 keyboard|keyboard.*init|KBD"
check_not "T48 No spurious break-code floods" "\[KBD\].*release.*release.*release"

# =============================================
# Cleanup and Summary
# =============================================
rm -f "$CLEAN_LOG"

echo ""
echo "=============================================="
echo "  RESULTS: $PASSED/$TOTAL passed, $FAILED failed"
echo "  YIELD exchanges: $YIELD_RAW"
echo "  Bus events: $BUS_PUB (tokens: $TOKEN_EV)"
echo "  Log: $LOG_FILE"
echo "=============================================="

if [ "$FAILED" -eq 0 ]; then
    echo ""
    echo "  >>> ALL $TOTAL TESTS PASSED <<<"
    echo ""
    exit 0
else
    echo ""
    echo "  >>> $FAILED TEST(S) FAILED <<<"
    echo -e "  Failed tests:$FAIL_LIST"
    echo ""
    exit 1
fi
