#!/bin/bash
# AetherionOS Universal Agent Build Script
# Builds ALL C and Rust userspace agents. NO STUBS.
# If an agent fails to compile, the error is shown and the script aborts.
set -euo pipefail
cd "$(dirname "$0")"

echo "============================================"
echo "[BUILD] AetherionOS Universal Agent Builder"
echo "============================================"

TARGET_JSON="x86_64-aetherion-user.json"
if [ ! -f "$TARGET_JSON" ]; then
    echo "[FATAL] $TARGET_JSON not found in $(pwd)"
    exit 1
fi

# ─────────────────────────────────────────────
# Step 1: Build C apps (if build_c.sh exists)
# ─────────────────────────────────────────────
echo ""
echo "[BUILD] Step 1: Compiling C apps..."
if [ -f scripts/build_c.sh ]; then
    bash scripts/build_c.sh 2>&1 | tail -10
    echo "[BUILD] C apps done."
else
    echo "[WARN] scripts/build_c.sh not found, skipping C apps"
fi

# ─────────────────────────────────────────────
# Step 2: Verify static ELFs exist (not built by cargo)
# ─────────────────────────────────────────────
echo ""
echo "[BUILD] Step 2: Checking static ELFs..."
MISSING_STATIC=0
for elf in userspace/hello.elf userspace/shell.elf userspace/agent_math.elf; do
    if [ -f "$elf" ]; then
        sz=$(stat -c%s "$elf" 2>/dev/null || echo 0)
        echo "  OK: $elf ($sz bytes)"
    else
        echo "  MISSING: $elf (non-fatal, kernel may skip)"
        MISSING_STATIC=$((MISSING_STATIC + 1))
    fi
done

# ─────────────────────────────────────────────
# Step 3: Build ALL Rust agents
# ─────────────────────────────────────────────
echo ""
echo "[BUILD] Step 3: Compiling Rust agents..."

# All 26 Rust agents (including agent_rust, agent_saga, agent_sse)
AGENTS="
agent_ai_native
agent_bench
agent_chunk_reader
agent_gguf
agent_gui_test
agent_http
agent_inference
agent_input
agent_llama
agent_llama_core
agent_llm_chat
agent_mt_matmul
agent_multipart
agent_net
agent_orchestrator
agent_q4_dequant
agent_rust
agent_saga
agent_sse
agent_state
agent_sysinfo
agent_terminal
agent_tokenizer
agent_visual_term
agent_weight_loader
agent_wm
"

BUILT=0
FAILED=0
FAILED_LIST=""

for agent in $AGENTS; do
    AGENT_DIR="userspace/$agent"
    CARGO_TOML="$AGENT_DIR/Cargo.toml"
    BINARY="$AGENT_DIR/target/x86_64-aetherion-user/release/$agent"

    if [ ! -d "$AGENT_DIR" ] || [ ! -f "$CARGO_TOML" ]; then
        echo "  [SKIP] $agent: directory or Cargo.toml missing"
        FAILED=$((FAILED + 1))
        FAILED_LIST="$FAILED_LIST $agent"
        continue
    fi

    # Skip if binary already exists and is real (>100 bytes)
    if [ -f "$BINARY" ]; then
        sz=$(stat -c%s "$BINARY" 2>/dev/null || echo 0)
        if [ "$sz" -gt 100 ]; then
            echo "  [CACHED] $agent ($sz bytes)"
            BUILT=$((BUILT + 1))
            continue
        fi
    fi

    echo -n "  [BUILD] $agent ... "

    # Save existing binary (if any) before clean build
    BUILD_LOG="/tmp/build_${agent}.log"

    if CARGO_BUILD_JOBS=1 \
       RUSTFLAGS="-C opt-level=s -C link-arg=-Tlinker.ld" \
       cargo build --release \
           --manifest-path "$CARGO_TOML" \
           --target "$TARGET_JSON" \
           -Z build-std=core,alloc,compiler_builtins \
       > "$BUILD_LOG" 2>&1; then
        
        if [ -f "$BINARY" ]; then
            sz=$(stat -c%s "$BINARY")
            echo "OK ($sz bytes)"
            BUILT=$((BUILT + 1))

            # Clean target to save disk (keep only the binary)
            cp "$BINARY" "/tmp/_${agent}_bin"
            rm -rf "$AGENT_DIR/target"
            mkdir -p "$AGENT_DIR/target/x86_64-aetherion-user/release"
            mv "/tmp/_${agent}_bin" "$BINARY"
        else
            echo "WARN: compiled but binary not found"
            FAILED=$((FAILED + 1))
            FAILED_LIST="$FAILED_LIST $agent"
        fi
    else
        echo "FAILED"
        echo "  === Error log for $agent ==="
        tail -20 "$BUILD_LOG"
        echo "  ==========================="
        FAILED=$((FAILED + 1))
        FAILED_LIST="$FAILED_LIST $agent"
    fi
done

# ─────────────────────────────────────────────
# Summary
# ─────────────────────────────────────────────
echo ""
echo "============================================"
echo "[BUILD] RESULTS: $BUILT OK, $FAILED FAILED"
if [ -n "$FAILED_LIST" ]; then
    echo "[BUILD] FAILED agents:$FAILED_LIST"
fi
echo "============================================"

# Exit with error if any agent failed
if [ "$FAILED" -gt 0 ]; then
    echo "[BUILD] ERROR: $FAILED agents could not be compiled."
    echo "[BUILD] Fix the errors above before building the kernel."
    exit 1
fi

echo "[BUILD] All agents compiled successfully!"
exit 0
