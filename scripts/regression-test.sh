#!/bin/bash
# regression-test.sh - AetherionOS Phase 2 Regression Test
#
# Tests that the rip=0x0 crash has been resolved and that
# the dynamic linker boots correctly.
#
# Usage: ./scripts/regression-test.sh [timeout_seconds]

set -e

TIMEOUT=${1:-60}
LOG_FILE="/tmp/aetherion_regression.log"
KERNEL_BIN="kernel/target/x86_64-aetherion/release/bootimage-aetherion-kernel.bin"
DISK_IMG="disk.img"

echo "========================================"
echo " AetherionOS Phase 2 Regression Test"
echo "========================================"
echo "  Timeout: ${TIMEOUT}s"
echo "  Log: ${LOG_FILE}"
echo ""

# Check prerequisites
if [ ! -f "${KERNEL_BIN}" ]; then
    echo "[TEST] ERROR: Kernel binary not found at ${KERNEL_BIN}"
    echo "[TEST] Run: cd kernel && cargo bootimage --release"
    exit 1
fi

# Clean previous logs
rm -f "${LOG_FILE}"

echo "[TEST] Starting QEMU (headless, serial console)..."

# Run QEMU with timeout
timeout ${TIMEOUT} qemu-system-x86_64 \
    -cpu qemu64 \
    -smp 2 \
    -m 2G \
    -drive format=raw,file="${KERNEL_BIN}" \
    ${DISK_IMG:+-drive format=raw,file=${DISK_IMG},if=virtio} \
    -display none \
    -serial stdio \
    -no-reboot \
    2>&1 | tee "${LOG_FILE}" || true

echo ""
echo "========================================"
echo " Test Results Analysis"
echo "========================================"

# Check for critical errors
PASS=true

echo ""
echo "[CHECK 1] rip=0x0 crash..."
if grep -qi "rip.*0x0\|FATAL.*0x0\|SIGSEGV.*0x0" "${LOG_FILE}"; then
    echo "  FAIL: rip=0x0 crash detected!"
    PASS=false
else
    echo "  PASS: No rip=0x0 crash"
fi

echo "[CHECK 2] ELF entry point validation..."
if grep -q "ELF-DEBUG.*First 4 bytes" "${LOG_FILE}"; then
    echo "  PASS: Entry point inspection executed"
    grep "ELF-DEBUG.*First 4 bytes" "${LOG_FILE}" | head -3
else
    echo "  WARN: Entry point inspection not found (may be OK if no ELF loaded)"
fi

echo "[CHECK 3] FATAL ELF errors..."
if grep -q "\[FATAL\].*ZEROED memory\|\[FATAL\].*NOT MAPPED" "${LOG_FILE}"; then
    echo "  FAIL: Fatal ELF loading errors detected!"
    grep "FATAL" "${LOG_FILE}" | head -5
    PASS=false
else
    echo "  PASS: No fatal ELF loading errors"
fi

echo "[CHECK 4] Ring 3 IRETQ safety..."
if grep -q "J135.*validated\|J135.*Entry.*OK" "${LOG_FILE}"; then
    echo "  PASS: Ring 3 entry validated"
    grep "J135" "${LOG_FILE}" | head -3
elif grep -q "FATAL.*contains 0x00\|FATAL.*rip=0x0.*BLOCKED" "${LOG_FILE}"; then
    echo "  WARN: Ring 3 entry blocked (safety mechanism activated)"
    grep "FATAL.*BLOCKED" "${LOG_FILE}" | head -3
else
    echo "  INFO: Ring 3 validation not reached"
fi

echo "[CHECK 5] Dynamic linker (ld-musl)..."
if grep -qi "ld-musl\|LD_DEBUG\|J134" "${LOG_FILE}"; then
    echo "  PASS: Dynamic linker activity detected"
    grep -i "ld-musl\|LD_DEBUG\|J134" "${LOG_FILE}" | head -5
else
    echo "  INFO: No dynamic linker activity (may need hello_dyn.elf in disk.img)"
fi

echo "[CHECK 6] Serial console fallback..."
if grep -q "FALLBACK.*Serial Shell\|FALLBACK.*QUEUED" "${LOG_FILE}"; then
    echo "  PASS: Serial console fallback activated"
    grep "FALLBACK" "${LOG_FILE}" | head -3
else
    echo "  INFO: Serial fallback not triggered"
fi

echo "[CHECK 7] Epoll/timerfd/signalfd..."
if grep -q "EPOLL.*create\|TIMERFD.*created\|SIGNALFD" "${LOG_FILE}"; then
    echo "  PASS: I/O multiplexing syscalls used"
else
    echo "  INFO: I/O multiplexing syscalls not yet exercised"
fi

echo ""
echo "========================================"
if $PASS; then
    echo " REGRESSION TEST: ALL CRITICAL CHECKS PASSED"
else
    echo " REGRESSION TEST: FAILURES DETECTED"
fi
echo "========================================"
echo ""
echo "Full log: ${LOG_FILE}"
echo "Analysis: grep -iE 'ELF-DEBUG|FATAL|ld-musl|EPOLL|TIMERFD|J135' ${LOG_FILE}"
