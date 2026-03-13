#!/bin/bash
# Test script for Jalon 71 - LLM Integration via Cognitive Bus

set -e

echo "========================================="
echo "Jalon 71 - Test de l'Intégration LLM"
echo "========================================="
echo ""

# Compile SDK
echo "[1/4] Compilation du SDK Rust..."
cd userspace/rust_sdk
cargo build --release --quiet
echo "✓ SDK compilé"

# Compile Terminal
echo "[2/4] Compilation du Terminal..."
cd ../agent_visual_term
cargo clean --quiet
cargo build --release --target ../../x86_64-aetherion-user.json -Z build-std=core,alloc --quiet
echo "✓ Terminal compilé"

# Compile Kernel
echo "[3/4] Compilation du Kernel..."
cd ../../kernel
rm -rf target/
CARGO_BUILD_JOBS=2 cargo bootimage --release 2>&1 | grep -E "(Compiling aetherion|Finished)" | tail -3
echo "✓ Kernel compilé"

# Test Boot
echo "[4/4] Test de démarrage (30s)..."
timeout 30 qemu-system-x86_64 \
    -cpu qemu64 -smp 2 -m 4G \
    -drive format=raw,file=target/x86_64-unknown-none/release/bootimage-aetherion-kernel.bin \
    -drive format=raw,file=../disk.img,if=virtio \
    -display none -vga std -serial stdio \
    2>&1 | tee /tmp/j71_test.log | grep -E "J71|J70|J65|bus_consume|SIGSEGV" &

sleep 30
pkill -9 qemu-system-x86_64 2>/dev/null || true

echo ""
echo "========================================="
echo "Résultats du Test"
echo "========================================="

# Analyze results
if grep -q "J71.*ENABLED" /tmp/j71_test.log; then
    echo "✓ Agent LLM activé"
else
    echo "✗ Agent LLM non activé"
fi

if grep -q "J71.*PID=.*queued" /tmp/j71_test.log; then
    echo "✓ Agent LLM chargé"
else
    echo "✗ Agent LLM non chargé"
fi

if grep -q "J65.*Visual Terminal.*registered" /tmp/j71_test.log; then
    echo "✓ Terminal chargé"
else
    echo "✗ Terminal non chargé"
fi

if grep -q "bus_consume" /tmp/j71_test.log; then
    echo "✓ sys_bus_consume() fonctionnel"
else
    echo "✗ sys_bus_consume() non appelé"
fi

if grep -q "SIGSEGV" /tmp/j71_test.log; then
    echo "✗ CRASH DÉTECTÉ (SIGSEGV)"
    echo ""
    echo "Détails du crash:"
    grep "SIGSEGV" /tmp/j71_test.log | head -5
else
    echo "✓ Aucun crash détecté"
fi

echo ""
echo "Log complet: /tmp/j71_test.log"
echo "========================================="
