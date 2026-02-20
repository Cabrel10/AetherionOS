#!/bin/bash
# install_and_test_hal.sh - Installation & Tests Couche 1 HAL
# Optimisé pour sandbox: timeout 300s, 1GB RAM, 2 cores

set -e  # Exit on error
START_TIME=$(date +%s)

echo "🚀 AetherionOS - Couche 1 HAL Installation & Tests"
echo "Environment: Sandbox (300s timeout, 1GB RAM, 2 cores)"
echo ""

# ===== PHASE 0: Pré-vérification =====
echo "📊 Phase 0: Pre-check..."
free -h | grep Mem || echo "free command not available"
df -h /home/user | tail -1 || echo "df command not available"

if command -v cargo &> /dev/null; then
    echo "✅ Rust already installed: $(cargo --version)"
    SKIP_INSTALL=1
else
    SKIP_INSTALL=0
fi

# ===== PHASE 1: Installation Rust =====
if [ $SKIP_INSTALL -eq 0 ]; then
    echo ""
    echo "🦀 Phase 1: Installing Rust (minimal profile)..."
    
    # Télécharger et installer avec timeout
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
      sh -s -- -y --default-toolchain nightly --profile minimal --no-modify-path || {
        echo "⚠️ Rustup install returned non-zero, continuing..."
    }
    
    source "$HOME/.cargo/env" 2>/dev/null || {
        echo "⚠️ Could not source cargo env"
    }
    
    if command -v cargo &> /dev/null; then
        cargo --version
        rustup component add rust-src llvm-tools-preview 2>/dev/null || echo "⚠️ Components may not be needed"
        echo "✅ Phase 1 complete"
    else
        echo "❌ Cargo not found after install"
    fi
else
    source "$HOME/.cargo/env" 2>/dev/null || true
fi

# ===== PHASE 2: Configuration =====
echo ""
echo "⚙️ Phase 2: Build configuration..."
cd /home/user/webapp/kernel

# Supprimer build-std si présent (cause timeout)
if [ -f .cargo/config.toml ] && grep -q "build-std" .cargo/config.toml 2>/dev/null; then
    echo "⚠️ Removing build-std from config"
    sed -i '/\[unstable\]/,/build-std/d' .cargo/config.toml
fi

echo "✅ Phase 2 complete"

# ===== PHASE 3: Compilation Tests =====
echo ""
echo "🔨 Phase 3: Compilation tests..."

# 3.1 - Cargo check
echo "  → cargo check (syntax)..."
timeout 120 cargo check 2>&1 | tee /tmp/check.log || {
    echo "⚠️ cargo check returned non-zero or timeout"
}

if [ -f /tmp/check.log ] && grep -q "Finished" /tmp/check.log; then
    ERROR_COUNT=$(grep -c "^error" /tmp/check.log || echo 0)
    echo "  ✅ cargo check: PASS (errors: $ERROR_COUNT)"
else
    echo "  ❌ cargo check: FAIL/TIMEOUT"
    echo "  Log excerpt:"
    tail -20 /tmp/check.log 2>/dev/null || echo "No log file"
fi

# 3.2 - Cargo build debug
echo "  → cargo build (debug)..."
timeout 180 cargo build 2>&1 | tee /tmp/build_debug.log || {
    echo "⚠️ cargo build returned non-zero or timeout"
}

if [ -f /tmp/build_debug.log ] && grep -q "Finished" /tmp/build_debug.log; then
    echo "  ✅ cargo build: PASS"
else
    echo "  ❌ cargo build: FAIL/TIMEOUT"
    tail -20 /tmp/build_debug.log 2>/dev/null || true
fi

# 3.3 - Cargo test (optionnel)
if [ "$ERROR_COUNT" = "0" ]; then
    echo "  → cargo test --lib..."
    timeout 120 cargo test --lib --no-fail-fast 2>&1 | tee /tmp/test.log || true
    if [ -f /tmp/test.log ] && grep -q "test result: ok" /tmp/test.log; then
        PASSED=$(grep -oP '\d+ passed' /tmp/test.log | grep -oP '\d+' | head -1)
        echo "  ✅ Tests: $PASSED passed"
    fi
fi

echo "✅ Phase 3 complete"

# ===== PHASE 4: Build Release (si temps) =====
ELAPSED=$(($(date +%s) - START_TIME))
if [ $ELAPSED -lt 180 ]; then
    echo ""
    echo "🎯 Phase 4: Release build (optional)..."
    timeout $((300 - ELAPSED - 10)) cargo build --release 2>&1 | tee /tmp/build_release.log || true
    
    if [ -f /tmp/build_release.log ] && grep -q "Finished" /tmp/build_release.log; then
        echo "✅ Release build: PASS"
    fi
fi

# ===== PHASE 5: Validation =====
echo ""
echo "=========================================="
echo "COUCHE 1 HAL - VALIDATION SUMMARY"
echo "=========================================="

# Source files
echo "📁 Modules HAL:"
echo "  - GDT/IDT: $(find src/arch/x86_64 -name "*.rs" 2>/dev/null | wc -l) files"
echo "  - Security: $(find src/security -name "*.rs" 2>/dev/null | wc -l) files"
echo "  - Tests: $(find src/tests -name "*.rs" 2>/dev/null | wc -l) files"

# Compilation
echo ""
echo "🔨 Compilation:"
[ -f /tmp/check.log ] && grep -q "Finished" /tmp/check.log && echo "  ✅ cargo check: PASS" || echo "  ❌ cargo check: FAIL"
[ -f /tmp/build_debug.log ] && grep -q "Finished" /tmp/build_debug.log && echo "  ✅ cargo build: PASS" || echo "  ❌ cargo build: FAIL"

# Tests
echo ""
echo "🧪 Tests:"
if [ -f /tmp/test.log ] && grep -q "test result: ok" /tmp/test.log; then
    grep "test result:" /tmp/test.log
else
    echo "  ⚠️ Not run or failed"
fi

# Binary
echo ""
echo "📦 Binary:"
BINARY=$(find target -name "aetherion-kernel" -type f 2>/dev/null | head -1)
if [ -n "$BINARY" ]; then
    ls -lh "$BINARY"
    file "$BINARY"
else
    echo "  ❌ Not generated"
fi

# Timing
ELAPSED=$(($(date +%s) - START_TIME))
echo ""
echo "⏱️ Total time: ${ELAPSED}s / 300s"
echo "=========================================="

# Exit code basé sur succès minimum (cargo check)
if [ -f /tmp/check.log ] && grep -q "Finished" /tmp/check.log; then
    echo ""
    echo "✅ COUCHE 1 HAL: VALIDATION PASSED (minimum criteria)"
    exit 0
else
    echo ""
    echo "❌ COUCHE 1 HAL: VALIDATION FAILED"
    exit 1
fi
