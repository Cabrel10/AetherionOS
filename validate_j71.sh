#!/bin/bash
# Validation complète du Jalon 71

set -e

echo "╔══════════════════════════════════════════════════════════╗"
echo "║     VALIDATION JALON 71 - Communication Terminal ↔ LLM   ║"
echo "╚══════════════════════════════════════════════════════════╝"
echo ""

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Test counters
PASSED=0
FAILED=0

# Helper functions
pass() {
    echo -e "${GREEN}✓${NC} $1"
    ((PASSED++))
}

fail() {
    echo -e "${RED}✗${NC} $1"
    ((FAILED++))
}

warn() {
    echo -e "${YELLOW}⚠${NC} $1"
}

# Test 1: Vérifier que les fichiers modifiés existent
echo "[1/6] Vérification des fichiers modifiés..."
if grep -q "sys_bus_consume" kernel/src/arch/x86_64/syscall.rs; then
    pass "Syscall sys_bus_consume présent dans le kernel"
else
    fail "Syscall sys_bus_consume manquant"
fi

if grep -q "SYS_BUS_CONSUME" userspace/rust_sdk/src/lib.rs; then
    pass "Constante SYS_BUS_CONSUME présente dans le SDK"
else
    fail "Constante SYS_BUS_CONSUME manquante"
fi

if grep -q "sys_bus_consume" userspace/agent_visual_term/src/main.rs; then
    pass "Terminal écoute le Cognitive Bus"
else
    fail "Terminal n'écoute pas le bus"
fi

if grep -q "J71.*ENABLED" kernel/src/main.rs; then
    pass "Agent LLM réactivé dans le kernel"
else
    fail "Agent LLM non réactivé"
fi

# Test 2: Vérifier que le bug GGUF est corrigé
echo ""
echo "[2/6] Vérification du fix GGUF..."
if grep -q "model_buffer\[23\]" userspace/agent_llm_chat/src/main.rs; then
    pass "Bug GGUF corrigé (8 bytes pour u64)"
else
    fail "Bug GGUF non corrigé"
fi

# Test 3: Compilation du SDK
echo ""
echo "[3/6] Compilation du SDK..."
cd userspace/rust_sdk
if cargo build --release --quiet 2>&1 | grep -q "Finished"; then
    pass "SDK compilé avec succès"
else
    fail "Échec compilation SDK"
fi
cd ../..

# Test 4: Compilation du Terminal
echo ""
echo "[4/6] Compilation du Terminal..."
cd userspace/agent_visual_term
if cargo build --release --target ../../x86_64-aetherion-user.json -Z build-std=core,alloc 2>&1 | grep -q "Finished"; then
    pass "Terminal compilé avec succès"
else
    fail "Échec compilation Terminal"
fi
cd ../..

# Test 5: Compilation de l'Agent LLM
echo ""
echo "[5/6] Compilation de l'Agent LLM..."
cd userspace/agent_llm_chat
if cargo build --release --target ../../x86_64-aetherion-user.json -Z build-std=core,alloc 2>&1 | grep -q "Finished"; then
    pass "Agent LLM compilé avec succès"
else
    fail "Échec compilation Agent LLM"
fi
cd ../..

# Test 6: Boot test
echo ""
echo "[6/6] Test de démarrage (30s)..."
cd kernel
timeout 30 qemu-system-x86_64 \
    -cpu qemu64 -smp 2 -m 4G \
    -drive format=raw,file=target/x86_64-unknown-none/release/bootimage-aetherion-kernel.bin \
    -drive format=raw,file=../disk.img,if=virtio \
    -display none -vga std -serial stdio \
    2>&1 | tee /tmp/j71_validation.log | grep -E "J71|J70|J65|bus_consume|SIGSEGV" &

sleep 30
pkill -9 qemu-system-x86_64 2>/dev/null || true
cd ..

# Analyze boot log
if grep -q "J71.*ENABLED" /tmp/j71_validation.log; then
    pass "Agent LLM activé au boot"
else
    fail "Agent LLM non activé"
fi

if grep -q "J71.*PID=.*queued" /tmp/j71_validation.log; then
    pass "Agent LLM chargé (PID assigné)"
else
    fail "Agent LLM non chargé"
fi

if grep -q "J65.*Visual Terminal.*registered" /tmp/j71_validation.log; then
    pass "Terminal chargé et enregistré"
else
    fail "Terminal non chargé"
fi

if grep -q "bus_consume" /tmp/j71_validation.log; then
    pass "Syscall sys_bus_consume appelé"
else
    warn "Syscall sys_bus_consume non appelé (normal si bus vide)"
fi

if grep -q "SIGSEGV" /tmp/j71_validation.log; then
    fail "CRASH DÉTECTÉ (SIGSEGV)"
    echo ""
    echo "Détails du crash:"
    grep "SIGSEGV" /tmp/j71_validation.log | head -5
else
    pass "Aucun crash détecté"
fi

# Summary
echo ""
echo "╔══════════════════════════════════════════════════════════╗"
echo "║                    RÉSULTATS                             ║"
echo "╚══════════════════════════════════════════════════════════╝"
echo ""
echo -e "Tests réussis: ${GREEN}${PASSED}${NC}"
echo -e "Tests échoués: ${RED}${FAILED}${NC}"
echo ""

if [ $FAILED -eq 0 ]; then
    echo -e "${GREEN}✓ JALON 71 VALIDÉ${NC}"
    echo ""
    echo "Prochaines étapes:"
    echo "  1. Test end-to-end: taper 'llm Bonjour' dans le terminal"
    echo "  2. Optimisations AVX2 (Jalon 73-74)"
    echo "  3. Abstraction layer (Jalon 75-77)"
    exit 0
else
    echo -e "${RED}✗ VALIDATION ÉCHOUÉE${NC}"
    echo ""
    echo "Vérifier les erreurs ci-dessus et relancer la validation."
    exit 1
fi
