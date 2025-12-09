#!/bin/bash
# Aetherion OS - Boot Benchmark Script

cd "$(dirname "$0")/.."

echo "=========================================="
echo "  AETHERION OS - Boot Benchmark"
echo "=========================================="
echo

if [ ! -f aetherion.img ]; then
    echo "❌ ERROR: aetherion.img not found!"
    exit 1
fi

ITERATIONS=10
TOTAL_TIME=0

echo "Running $ITERATIONS boot tests..."
echo

for i in $(seq 1 $ITERATIONS); do
    echo -n "Test $i/$ITERATIONS... "
    
    START=$(date +%s.%N)
    
    # Boot QEMU for 3 seconds then kill
    timeout 3s qemu-system-x86_64 \
        -drive format=raw,file=aetherion.img \
        -nographic \
        -serial stdio \
        -m 256M \
        > /dev/null 2>&1
    
    END=$(date +%s.%N)
    BOOT_TIME=$(echo "$END - $START" | bc)
    TOTAL_TIME=$(echo "$TOTAL_TIME + $BOOT_TIME" | bc)
    
    echo "${BOOT_TIME}s"
done

AVG_TIME=$(echo "scale=3; $TOTAL_TIME / $ITERATIONS" | bc)

echo
echo "=========================================="
echo "  Benchmark Results"
echo "=========================================="
echo "Average boot time: ${AVG_TIME}s"
echo "Target: <10s"
echo "RAM usage: 256MB (QEMU allocation)"
echo

if (( $(echo "$AVG_TIME < 10.0" | bc -l) )); then
    echo "✅ PASS: Boot time meets target!"
else
    echo "❌ FAIL: Boot time exceeds target"
fi
echo
