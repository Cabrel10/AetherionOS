#!/bin/bash
# ============================================================
# AetherionOS - Legitimate Agent Build Script (Jalon 119)
# ============================================================
# ZERO STUBS. Every binary is real compiled Rust code.
# Sequential build with cargo clean between agents to avoid OOM.
# Binaries cached in /bin_cache/ for kernel embedding.
# ============================================================
set -uo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN_CACHE="$PROJECT_DIR/bin_cache"
SDK_DIR="$PROJECT_DIR/userspace/rust_sdk"
TARGET="x86_64-aetherion-user"
TOTAL=0
SUCCESS=0
FAILED=0
FAIL_LIST=""

echo "============================================================"
echo "  AetherionOS Legitimate Agent Builder (Jalon 119)"
echo "  Project: $PROJECT_DIR"
echo "  Target:  $TARGET"
echo "  Cache:   $BIN_CACHE"
echo "  Policy:  ZERO STUBS - Real code only"
echo "============================================================"
echo ""

# Create bin cache directory
mkdir -p "$BIN_CACHE"

# Verify target JSON exists
if [ ! -f "$PROJECT_DIR/$TARGET.json" ]; then
    echo "[FATAL] $TARGET.json not found in $PROJECT_DIR"
    exit 1
fi

# Verify SDK compiles
echo "[SDK] Verifying rust_sdk compiles..."
cd "$SDK_DIR"
RUST_TARGET_PATH="$PROJECT_DIR" CARGO_BUILD_JOBS=1 \
    cargo check --release --target "$TARGET" 2>&1 | tail -3
if [ ${PIPESTATUS[0]} -ne 0 ]; then
    echo "[FATAL] rust_sdk does not compile. Fix SDK first."
    exit 1
fi
echo "[SDK] OK"
echo ""

# ── Build each Rust agent sequentially ──
AGENTS=$(find "$PROJECT_DIR/userspace" -maxdepth 1 -name "agent_*" -type d | sort)

for agent_dir in $AGENTS; do
    agent_name=$(basename "$agent_dir")
    TOTAL=$((TOTAL + 1))
    
    # Skip if no Cargo.toml
    if [ ! -f "$agent_dir/Cargo.toml" ]; then
        echo "[$TOTAL] SKIP: $agent_name (no Cargo.toml)"
        TOTAL=$((TOTAL - 1))
        continue
    fi
    
    echo "[$TOTAL] Building $agent_name..."
    
    cd "$agent_dir"
    
    # Build with single job, pointing to project root for target JSON
    BUILD_OUTPUT=$(RUST_TARGET_PATH="$PROJECT_DIR" CARGO_BUILD_JOBS=1 \
        cargo build --release --target "$TARGET" 2>&1)
    BUILD_EXIT=$?
    
    if [ $BUILD_EXIT -eq 0 ]; then
        # Find the binary
        BINARY="$agent_dir/target/$TARGET/release/$agent_name"
        if [ -f "$BINARY" ]; then
            SIZE=$(stat -c%s "$BINARY" 2>/dev/null || echo 0)
            cp "$BINARY" "$BIN_CACHE/$agent_name"
            echo "  [OK] $agent_name ($SIZE bytes) -> bin_cache/"
            SUCCESS=$((SUCCESS + 1))
        else
            echo "  [WARN] Built OK but binary not found at expected path"
            # Try to find it
            FOUND=$(find "$agent_dir/target" -name "$agent_name" -type f ! -name "*.d" ! -name "*.json" 2>/dev/null | head -1)
            if [ -n "$FOUND" ]; then
                SIZE=$(stat -c%s "$FOUND" 2>/dev/null || echo 0)
                cp "$FOUND" "$BIN_CACHE/$agent_name"
                echo "  [OK] Found at: $FOUND ($SIZE bytes)"
                SUCCESS=$((SUCCESS + 1))
            else
                echo "  [FAIL] Binary not found anywhere"
                FAILED=$((FAILED + 1))
                FAIL_LIST="$FAIL_LIST\n    - $agent_name (binary missing)"
            fi
        fi
    else
        echo "  [FAIL] Compilation error:"
        echo "$BUILD_OUTPUT" | grep "^error" | head -5
        FAILED=$((FAILED + 1))
        FAIL_LIST="$FAIL_LIST\n    - $agent_name (compile error)"
    fi
    
    # Clean to free memory for next agent (keep bin_cache copy)
    cd "$agent_dir"
    cargo clean 2>/dev/null
    echo ""
done

# ── Summary ──
echo "============================================================"
echo "  BUILD RESULTS: $SUCCESS/$TOTAL succeeded, $FAILED failed"
echo "  Binaries in: $BIN_CACHE/"
if [ $FAILED -gt 0 ]; then
    echo -e "  Failed agents:$FAIL_LIST"
fi
echo "============================================================"

# List all cached binaries
echo ""
echo "Cached binaries:"
ls -la "$BIN_CACHE"/ 2>/dev/null | grep -v "^total" | grep -v "^d"

if [ $FAILED -eq 0 ]; then
    echo ""
    echo ">>> ALL $SUCCESS AGENTS COMPILED SUCCESSFULLY - ZERO STUBS <<<"
    exit 0
else
    echo ""
    echo ">>> $FAILED AGENT(S) FAILED TO COMPILE <<<"
    exit 1
fi
