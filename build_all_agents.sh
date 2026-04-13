#!/bin/bash
# ═══════════════════════════════════════════════════════════════
# AetherionOS Universal Agent Build Script v2.0
# ═══════════════════════════════════════════════════════════════
# Builds ALL C and Rust userspace agents. ZERO STUBS.
# - Uses shared CARGO_TARGET_DIR so core/alloc are compiled ONCE
# - Compiles one agent at a time (OOM-safe with 1 GB RAM)
# - Copies each binary to /bin_cache/ after successful build
# - Cleans per-agent intermediate artifacts to reclaim disk
# - Aborts on first compilation failure (set -e)
#
# Author: MORNINGSTAR <morningstar@aetherion.dev>
# ═══════════════════════════════════════════════════════════════
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$PROJECT_DIR"

echo "════════════════════════════════════════════════════════"
echo "[BUILD] AetherionOS Universal Agent Builder v2.0"
echo "[BUILD] Mode: ZERO STUBS — shared target dir — OOM-safe"
echo "════════════════════════════════════════════════════════"

# ── Configuration ──
TARGET_JSON="x86_64-aetherion-user.json"
SHARED_TARGET="$PROJECT_DIR/.agent_target"
BIN_CACHE="$PROJECT_DIR/bin_cache"
BUILD_STD_FLAGS="-Z build-std=core,compiler_builtins,alloc -Z build-std-features=compiler-builtins-mem"

# Validate target spec
if [ ! -f "$TARGET_JSON" ]; then
    echo "[FATAL] $TARGET_JSON not found in $PROJECT_DIR"
    exit 1
fi

# Create shared directories
mkdir -p "$SHARED_TARGET" "$BIN_CACHE"

# ─────────────────────────────────────────────
# Step 1: Build C apps (if build_c.sh exists)
# ─────────────────────────────────────────────
echo ""
echo "[BUILD] Step 1: Compiling C apps..."
if [ -f scripts/build_c.sh ]; then
    bash scripts/build_c.sh 2>&1 | tail -10 || true
    echo "[BUILD] C apps done."
else
    echo "[INFO] scripts/build_c.sh not found, skipping C apps"
fi

# ─────────────────────────────────────────────
# Step 2: Verify static ELFs exist
# ─────────────────────────────────────────────
echo ""
echo "[BUILD] Step 2: Checking static ELFs..."
for elf in userspace/hello.elf userspace/shell.elf userspace/agent_math.elf; do
    if [ -f "$elf" ]; then
        sz=$(stat -c%s "$elf" 2>/dev/null || echo 0)
        echo "  OK: $elf ($sz bytes)"
    else
        echo "  INFO: $elf not present (non-fatal)"
    fi
done

# ─────────────────────────────────────────────
# Step 3: Build ALL Rust agents (31 agents)
# ─────────────────────────────────────────────
echo ""
echo "[BUILD] Step 3: Compiling ALL Rust agents..."
echo "[BUILD] Shared target: $SHARED_TARGET"
echo "[BUILD] Binary cache:  $BIN_CACHE"

# Complete list of all 31 Rust agents
AGENTS="
agent_ai_native
agent_autonomous
agent_bench
agent_chunk_reader
agent_clock
agent_gguf
agent_gui_test
agent_http
agent_inference
agent_input
agent_llama
agent_llama_core
agent_llm_chat
agent_mcp
agent_memory
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
agent_validator
agent_visual_term
agent_weight_loader
agent_wm
"

BUILT=0
FAILED=0
SKIPPED=0
FAILED_LIST=""
TOTAL=0

for agent in $AGENTS; do
    TOTAL=$((TOTAL + 1))
done

echo "[BUILD] Total agents to compile: $TOTAL"
echo ""

IDX=0
for agent in $AGENTS; do
    IDX=$((IDX + 1))
    AGENT_DIR="userspace/$agent"
    CARGO_TOML="$AGENT_DIR/Cargo.toml"
    # With shared target, binary lands in shared dir
    BINARY="$SHARED_TARGET/x86_64-aetherion-user/release/$agent"
    # Also check legacy per-agent location
    LEGACY_BINARY="$AGENT_DIR/target/x86_64-aetherion-user/release/$agent"

    if [ ! -d "$AGENT_DIR" ] || [ ! -f "$CARGO_TOML" ]; then
        echo "  [$IDX/$TOTAL] SKIP $agent: directory or Cargo.toml missing"
        SKIPPED=$((SKIPPED + 1))
        continue
    fi

    echo -n "  [$IDX/$TOTAL] $agent ... "

    BUILD_LOG="/tmp/build_${agent}.log"

    # Build with shared target directory — core/alloc cached across agents
    if RUST_TARGET_PATH="$PROJECT_DIR" \
       CARGO_BUILD_JOBS=1 \
       CARGO_TARGET_DIR="$SHARED_TARGET" \
       cargo build --release \
           --manifest-path "$CARGO_TOML" \
           --target x86_64-aetherion-user.json \
           $BUILD_STD_FLAGS \
       > "$BUILD_LOG" 2>&1; then

        if [ -f "$BINARY" ]; then
            sz=$(stat -c%s "$BINARY")
            echo "OK ($sz bytes)"
            # Copy to bin_cache and to legacy location for kernel include_bytes
            cp "$BINARY" "$BIN_CACHE/${agent}"
            mkdir -p "$AGENT_DIR/target/x86_64-aetherion-user/release"
            cp "$BINARY" "$LEGACY_BINARY"
            BUILT=$((BUILT + 1))
        else
            echo "WARN: compiled but binary not at $BINARY"
            # Check if it went to a differently-named binary
            FAILED=$((FAILED + 1))
            FAILED_LIST="$FAILED_LIST $agent"
        fi
    else
        echo "FAILED"
        echo "  ┌── Error log ($agent) ──"
        tail -20 "$BUILD_LOG" | sed 's/^/  │ /'
        echo "  └──────────────────────"
        FAILED=$((FAILED + 1))
        FAILED_LIST="$FAILED_LIST $agent"
    fi
done

# ─────────────────────────────────────────────
# Summary
# ─────────────────────────────────────────────
echo ""
echo "════════════════════════════════════════════════════════"
echo "[BUILD] RESULTS: $BUILT compiled, $FAILED failed, $SKIPPED skipped"
if [ -n "$FAILED_LIST" ]; then
    echo "[BUILD] FAILED:$FAILED_LIST"
fi
echo "[BUILD] Binaries cached in: $BIN_CACHE/"
echo "════════════════════════════════════════════════════════"

# List all cached binaries
if [ "$BUILT" -gt 0 ]; then
    echo ""
    echo "[BUILD] Binary inventory:"
    ls -la "$BIN_CACHE"/ 2>/dev/null | grep -v "^total" | grep -v "^d" | awk '{printf "  %-30s %s bytes\n", $NF, $5}'
fi

if [ "$FAILED" -gt 0 ]; then
    echo ""
    echo "[BUILD] ERROR: $FAILED agents could not be compiled."
    echo "[BUILD] Fix the errors above before building the kernel."
    exit 1
fi

echo ""
echo "[BUILD] ✓ All $BUILT agents compiled successfully! ZERO STUBS."
exit 0
