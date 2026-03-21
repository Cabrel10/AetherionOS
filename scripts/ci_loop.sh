#!/bin/bash
# AetherionOS — Continuous Integration Loop v2.0
# Rebuilds kernel, runs regression tests (v3.0, 90 tests) every 5 minutes.
# Detects regressions and alerts immediately.
# Usage: ./scripts/ci_loop.sh [--interval SECONDS]
#
# Runs until Ctrl+C. Logs all results to /tmp/aetherion_ci_*.log

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
INTERVAL=${1:-300}  # 5 minutes default
CI_LOG="/tmp/aetherion_ci_$(date +%Y%m%d_%H%M%S).log"
ITERATION=0
LAST_PASS=0
LAST_FAIL=0
LAST_TOTAL=0

echo "=============================================="
echo "  AetherionOS CI Loop — Continuous Regression"
echo "=============================================="
echo "  Project:  $PROJECT_DIR"
echo "  Interval: ${INTERVAL}s"
echo "  CI Log:   $CI_LOG"
echo "  Press Ctrl+C to stop"
echo ""

log() {
    local msg="[$(date '+%H:%M:%S')] $1"
    echo "$msg"
    echo "$msg" >> "$CI_LOG"
}

while true; do
    ITERATION=$((ITERATION + 1))
    log "========== CI Iteration #$ITERATION =========="

    # Step 1: Check for code changes
    cd "$PROJECT_DIR"
    CHANGES=$(git diff --shortstat 2>/dev/null)
    if [ -n "$CHANGES" ]; then
        log "[CHANGES] $CHANGES"
    else
        log "[CLEAN] No uncommitted changes"
    fi

    # Step 2: Rebuild kernel
    log "[BUILD] Rebuilding kernel..."
    cd "$PROJECT_DIR/kernel"
    BUILD_START=$(date +%s)
    BUILD_OUT=$(CARGO_BUILD_JOBS=2 \
        RUSTFLAGS="-C relocation-model=static -C code-model=kernel -C overflow-checks=yes" \
        cargo bootimage --release --target x86_64-aetherion.json 2>&1)
    BUILD_RC=$?
    BUILD_END=$(date +%s)
    BUILD_TIME=$((BUILD_END - BUILD_START))

    if [ $BUILD_RC -ne 0 ]; then
        log "[BUILD] FAILED (exit $BUILD_RC) after ${BUILD_TIME}s"
        echo "$BUILD_OUT" >> "$CI_LOG"
        log "[ALERT] >>> BUILD FAILURE — REGRESSION DETECTED <<<"
        sleep "$INTERVAL"
        continue
    fi
    log "[BUILD] OK (${BUILD_TIME}s)"

    # Step 3: Run regression tests
    log "[TEST] Running regression suite (timeout=45s)..."
    cd "$PROJECT_DIR"
    TEST_OUT=$(bash scripts/regression-test.sh 45 2>&1)
    TEST_RC=$?

    # Parse results
    RESULT_LINE=$(echo "$TEST_OUT" | grep "RESULTS:" | head -1)
    PASS=$(echo "$RESULT_LINE" | grep -oP '\d+(?=/\d+ passed)' || echo "0")
    TOTAL=$(echo "$RESULT_LINE" | grep -oP '\d+(?= passed)' | head -1 || echo "0")
    FAIL=$(echo "$RESULT_LINE" | grep -oP '\d+(?= failed)' || echo "0")

    PASS=$(echo "$PASS" | tr -d '[:space:]')
    TOTAL=$(echo "$TOTAL" | tr -d '[:space:]')
    FAIL=$(echo "$FAIL" | tr -d '[:space:]')
    PASS=${PASS:-0}
    TOTAL=${TOTAL:-0}
    FAIL=${FAIL:-0}

    if [ "$TEST_RC" -eq 0 ]; then
        log "[TEST] ALL PASS: $PASS/$TOTAL tests"
    else
        log "[TEST] FAILURES: $PASS/$TOTAL passed, $FAIL failed"
        # Show failed tests
        echo "$TEST_OUT" | grep "\[FAIL\]" >> "$CI_LOG"
    fi

    # Step 4: Detect regression from previous run
    if [ "$ITERATION" -gt 1 ]; then
        if [ "$FAIL" -gt "$LAST_FAIL" ]; then
            log "[ALERT] >>> REGRESSION: failures increased from $LAST_FAIL to $FAIL <<<"
        elif [ "$PASS" -lt "$LAST_PASS" ]; then
            log "[ALERT] >>> REGRESSION: pass count dropped from $LAST_PASS to $PASS <<<"
        elif [ "$FAIL" -lt "$LAST_FAIL" ]; then
            log "[IMPROVEMENT] Failures reduced from $LAST_FAIL to $FAIL"
        fi
    fi

    LAST_PASS=$PASS
    LAST_FAIL=$FAIL
    LAST_TOTAL=$TOTAL

    # Step 5: Log summary
    YIELDS=$(echo "$TEST_OUT" | grep "YIELD exchanges:" | grep -oP '\d+' | head -1 || echo "0")
    TOKENS=$(echo "$TEST_OUT" | grep "tokens:" | grep -oP '\d+' | tail -1 || echo "0")
    log "[SUMMARY] Pass=$PASS/$TOTAL Fail=$FAIL Yields=$YIELDS Tokens=$TOKENS"
    echo "" >> "$CI_LOG"

    # Step 6: Wait for next iteration
    log "[WAIT] Next run in ${INTERVAL}s ($(date -d "+${INTERVAL} seconds" '+%H:%M:%S' 2>/dev/null || echo 'soon'))..."
    sleep "$INTERVAL"
done
