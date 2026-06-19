#!/bin/bash
set -uo pipefail

export PATH="/home/ubuntu/.cargo/bin:$PATH"
export RUSTUP_HOME="/home/ubuntu/.rustup"
export CARGO_HOME="/home/ubuntu/.cargo"

PROJECT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$PROJECT_DIR"

TARGET_JSON="x86_64-aetherion-user.json"
SHARED_TARGET="$PROJECT_DIR/.agent_target"
BIN_CACHE="$PROJECT_DIR/bin_cache"
BUILD_STD_FLAGS="-Z build-std=core,compiler_builtins,alloc -Z build-std-features=compiler-builtins-mem -Z json-target-spec"

mkdir -p "$SHARED_TARGET" "$BIN_CACHE"

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
FAILED_LIST=""
TOTAL=$(echo "$AGENTS" | grep -c '[a-z]')
echo "════════════════════════════════════════════"
echo "[BUILD] Building $TOTAL agents (workspace fix applied)"
echo "════════════════════════════════════════════"

IDX=0
for agent in $AGENTS; do
    [ -z "$agent" ] && continue
    IDX=$((IDX + 1))
    AGENT_DIR="userspace/$agent"
    CARGO_TOML="$AGENT_DIR/Cargo.toml"
    BINARY="$SHARED_TARGET/x86_64-aetherion-user/release/$agent"
    
    if [ ! -f "$CARGO_TOML" ]; then
        echo "  [$IDX/$TOTAL] SKIP $agent: Cargo.toml missing"
        continue
    fi
    
    echo -n "  [$IDX/$TOTAL] $agent ... "
    
    if RUST_TARGET_PATH="$PROJECT_DIR" \
       CARGO_BUILD_JOBS=1 \
       CARGO_TARGET_DIR="$SHARED_TARGET" \
       cargo build --release \
           --manifest-path "$CARGO_TOML" \
           --target "$TARGET_JSON" \
           $BUILD_STD_FLAGS \
       2>/tmp/build_${agent}.log; then
        if [ -f "$BINARY" ]; then
            sz=$(stat -c%s "$BINARY")
            echo "OK ($sz bytes)"
            cp "$BINARY" "$BIN_CACHE/${agent}"
            BUILT=$((BUILT + 1))
        else
            echo "WARN: no binary at expected path"
            FAILED=$((FAILED + 1))
            FAILED_LIST="$FAILED_LIST $agent"
        fi
    else
        echo "FAILED"
        tail -5 /tmp/build_${agent}.log | sed 's/^/    /'
        FAILED=$((FAILED + 1))
        FAILED_LIST="$FAILED_LIST $agent"
    fi
done

echo ""
echo "════════════════════════════════════════════"
echo "[BUILD] RESULTS: $BUILT OK, $FAILED FAILED"
if [ -n "$FAILED_LIST" ]; then
    echo "[BUILD] FAILED:$FAILED_LIST"
fi
echo "════════════════════════════════════════════"
echo ""
echo "[BUILD] bin_cache contents:"
ls -la "$BIN_CACHE"/ | grep -v '^total' | grep -v '^d'
