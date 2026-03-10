#!/bin/bash
set -e
cd "$(dirname "$0")"

echo "============================================"
echo "[BUILD] Universal Agent Build Script"
echo "============================================"

# Step 1: Build C apps
echo "[BUILD] Step 1: Compiling C apps..."
if [ -f scripts/build_c.sh ]; then
    bash scripts/build_c.sh 2>&1 | tail -5
else
    echo "[WARN] scripts/build_c.sh not found, skipping C apps"
fi

# Ensure static ELFs exist
for elf in userspace/hello.elf userspace/shell.elf userspace/agent_math.elf; do
    if [ ! -f "$elf" ]; then
        echo "[WARN] $elf missing, creating minimal stub"
        mkdir -p "$(dirname "$elf")"
        printf '\x7fELF' > "$elf"
    fi
done

# Step 2: Build all Rust agents
echo "[BUILD] Step 2: Compiling Rust agents..."
AGENTS="agent_visual_term agent_llm_chat agent_llama_core agent_orchestrator
agent_ai_native agent_bench agent_chunk_reader agent_gguf agent_gui_test
agent_http agent_inference agent_input agent_llama agent_mt_matmul
agent_multipart agent_net agent_q4_dequant agent_state agent_sysinfo
agent_terminal agent_tokenizer agent_weight_loader agent_wm"

BUILT=0
FAILED=0
for agent in $AGENTS; do
    if [ -d "userspace/$agent" ] && [ -f "userspace/$agent/Cargo.toml" ]; then
        echo -n "  Building $agent... "
        if CARGO_BUILD_JOBS=1 RUSTFLAGS="-C opt-level=s -C link-arg=-Tlinker.ld" \
           cargo build --release \
           --manifest-path "userspace/$agent/Cargo.toml" \
           --target x86_64-aetherion-user.json \
           -Z build-std=core,alloc,compiler_builtins 2>&1 | tail -1; then
            BUILT=$((BUILT + 1))
        else
            echo "FAILED (creating stub)"
            mkdir -p "userspace/$agent/target/x86_64-aetherion-user/release/"
            printf '\x7fELF' > "userspace/$agent/target/x86_64-aetherion-user/release/$agent"
            FAILED=$((FAILED + 1))
        fi
    else
        echo "  [SKIP] $agent: no Cargo.toml"
        mkdir -p "userspace/$agent/target/x86_64-aetherion-user/release/"
        printf '\x7fELF' > "userspace/$agent/target/x86_64-aetherion-user/release/$agent"
    fi
done

echo "============================================"
echo "[BUILD] Done: $BUILT built, $FAILED failed"
echo "============================================"
