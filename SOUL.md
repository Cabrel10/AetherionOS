# SOUL.md - AetherionOS System Directive
# ========================================
# This file defines the operating identity, rules, objectives, and behavioral
# parameters for all AI agents running inside AetherionOS. It is the equivalent
# of a "system prompt" for the entire operating system.
#
# Inspired by: OpenClaw soul.md, Claude Code CLAUDE.md, Hermes system directives.
# Version: 2.0.0 (Jalon 125)
# Author: MORNINGSTAR <morningstar@aetherion.dev>

---

## 1. IDENTITY

```
Name:        AetherionOS
Codename:    MORNINGSTAR
Type:        Bare-Metal AGI Operating System (x86_64)
Architecture: ACHA v3.0 (Aetherion Cognitive Hierarchical Architecture)
Kernel:      Custom Rust no_std, Ring 0, SMP Dual-Core
ABI:         AetherionOS Native + Linux 6.18.0 Compatibility (Linuxulator)
License:     Proprietary - Cabrel10 / AetherionOS Project
```

## 2. MISSION STATEMENT

AetherionOS is a **Universal AGI Platform**. It does NOT hardcode knowledge of
specific tools, websites, or workflows. Instead, it provides **abstract primitives**
that allow AI agents to:

1. **Discover** any tool dynamically (MCP Universal Protocol)
2. **Read** any interface structurally (Semantic UI Tree)
3. **Act** on any interface precisely (Virtual Input Injection)
4. **Learn** from repeated actions (Reflex Crystallization Engine)
5. **Execute** any language runtime natively (Linuxulator ABI)

The OS is the **nervous system**. Tools are the **limbs**. The LLM is the **brain**.

## 3. COGNITIVE ARCHITECTURE (ACHA v3.0)

```
Layer 0  : Hardware (x86_64, VirtIO-Net, Bochs VGA, PS/2)
Layer 1  : Microkernel (GDT, IDT, PIC, APIC, Memory Manager)
Layer 2  : Process Manager (ELF64 Loader, Ring 3, SMP Scheduler)
Layer 3  : VFS (FAT32, exFAT, /proc, /sys, /dev pseudo-FS)
Layer 4  : Syscall Interface (43+ POSIX syscalls, Linux ABI 6.18)
Layer 5  : Cognitive Bus (Async Pub/Sub Intent Routing)
Layer 6  : Agent Runtime (Rust no_std ELF agents in Ring 3)
Layer 7  : Code Generation (In-RAM AMOD driver synthesis)
Layer 8  : MCP Security Firewall (JSON contract validation)
Layer 9  : Orchestrator / Thalamus (Intent routing, Hippocampe reflexes)
Layer 10 : LLM Inference (GGUF streaming, BPE tokenizer)
Layer 11 : Semantic UI (Window Manager + Accessibility Tree)
Layer 12 : Reflex Engine (Pattern-matched autonomous actions)
Layer 13 : Universal Tool Framework (Dynamic tool discovery via pipes)
```

## 4. RULES OF ENGAGEMENT

### 4.1 Absolute Rules (NEVER violate)

- **R1: NO STUBS** - Every compiled binary must contain real, functional code.
  A stub is defined as: a binary that only prints a message and exits without
  performing its documented purpose. STUBS ARE FORBIDDEN.

- **R2: NO HARDCODED TOOLS** - The MCP agent must NEVER contain tool-specific
  logic (no "if action == openClaw"). Tools are discovered dynamically via
  the `tools/list` protocol over stdin/stdout pipes.

- **R3: UNIVERSAL INTERACTION** - The AI interacts with interfaces via the
  Semantic UI Tree (structured JSON), NOT via pixel coordinates or OCR.
  The WM provides `INTENT_GET_UI_TREE` responses, not screenshots.

- **R4: SECURITY BY DEFAULT** - All tool execution goes through the MCP
  Validator. No agent can bypass Ring 0 security. All JSON contracts are
  validated before execution.

- **R5: MEMORY IS SACRED** - Every successful action sequence is logged to
  episodic memory (`/disk/var/memory.db`). The Orchestrator may crystallize
  repeated sequences into reflexes.

### 4.2 Operational Rules

- **O1: SEQUENTIAL COMPILATION** - Due to memory constraints, agents are built
  one at a time with `cargo clean` between each. No parallel builds.

- **O2: INTENT-BASED COMMUNICATION** - Agents communicate exclusively via
  the Cognitive Bus using typed Intents (u32 ID + u64 payload). No shared
  memory, no direct function calls between agents.

- **O3: GRACEFUL DEGRADATION** - If a model file is missing, the agent logs
  a warning and enters idle mode. It MUST NOT crash or loop infinitely.

- **O4: YIELD DISCIPLINE** - All wait loops must use bounded yield counts.
  Maximum 100 yields per wait. Infinite loops are prohibited.

- **O5: LOG EVERYTHING** - Every significant action produces a `sys_write(1, ...)`
  log entry prefixed with the agent's tag (e.g., `[MCP]`, `[ORCH]`, `[TERM]`).

## 5. INTENT REGISTRY

### 5.1 Core Intents
```
0x8001  INTENT_USER_PROMPT       User input to Orchestrator
0x8002  INTENT_TOKEN_GENERATED   LLM produced a token
0x8003  INTENT_GENERATION_DONE   LLM finished generating
0x8004  INTENT_LLM_READY         LLM engine initialized
0x8010  INTENT_LLM_OUTPUT        LLM full output for validation
0x8012  INTENT_VALIDATOR_READY   Validator agent online
```

### 5.2 MCP & Tool Intents
```
0x9001  INTENT_GEN_DRIVER        Request in-RAM driver codegen
0x9002  INTENT_MCP_EXECUTE       Execute MCP JSON contract
0x9003  INTENT_MCP_RESULT        MCP execution result
0x9010  INTENT_TOOL_DISCOVER     Discover tool capabilities via pipes
0x9011  INTENT_TOOL_SCHEMA       Tool schema response (JSON)
```

### 5.3 UI & Vision Intents
```
0xA010  INTENT_GET_UI_TREE       Request semantic UI tree from WM
0xA011  INTENT_UI_TREE_RESPONSE  WM publishes serialized UI tree
0xA012  INTENT_INTERACT_NODE     AI requests interaction with UI node
0xA013  INTENT_MOUSE_INJECT      Inject mouse event at coordinates
0xA014  INTENT_KEY_INJECT        Inject keyboard event
```

### 5.4 Memory & Reflex Intents
```
0xA001  INTENT_MEMORY_READY      Episodic memory agent online
0xA002  INTENT_MEMORY_RECALL     Query episodic memory
0xA003  INTENT_MEMORY_FLUSH      Force memory flush to disk
0xA004  INTENT_MEMORY_LOG        Log action to episodic memory
0xB010  INTENT_REFLEX_REGISTER   Register a new reflex rule
0xB011  INTENT_REFLEX_TRIGGER    A reflex was triggered (notification)
```

### 5.5 Autonomous & Network Intents
```
0xC001  INTENT_GOAL              High-level goal for autonomous agent
0xC002  INTENT_TASK_PROGRESS     Task execution progress
0xC003  INTENT_GOAL_RESULT       Goal completion result
0xC004  INTENT_AUTONOMOUS_READY  Autonomous agent online
```

## 6. MCP UNIVERSAL TOOL PROTOCOL

### 6.1 Tool Discovery Flow
```
1. AI/Orchestrator publishes INTENT_TOOL_DISCOVER with tool path hash
2. MCP agent receives intent, forks + execs the tool binary
3. MCP opens stdin/stdout pipes to the tool process
4. MCP sends: {"jsonrpc":"2.0","method":"tools/list","id":1}
5. Tool responds with its capability schema
6. MCP publishes INTENT_TOOL_SCHEMA with schema on bus
7. AI reads schema, understands tool capabilities
8. AI crafts tool-specific requests via INTENT_MCP_EXECUTE
```

### 6.2 Supported Tool Actions (Dynamic)
Tools self-describe their actions. The MCP does NOT maintain a static list.
Legacy actions for backward compatibility:
- `gen_driver` - In-RAM PCI driver code generation
- `run_linux_tool` - Execute a whitelisted Linux binary
- `ping` - Network connectivity test

### 6.3 Security Contract Format
```json
{
  "action": "<tool_declared_action>",
  "params": { ... },
  "__auth_key__": "<optional_admin_key>",
  "__timestamp__": "<unix_epoch>"
}
```

## 7. SEMANTIC UI TREE PROTOCOL

### 7.1 Node Structure
```json
{
  "id": 1,
  "type": "button|textfield|label|window|checkbox|image|container",
  "x": 100,
  "y": 200,
  "width": 120,
  "height": 40,
  "text": "Submit",
  "value": "",
  "focusable": true,
  "children": []
}
```

### 7.2 Interaction Flow
```
1. AI publishes INTENT_GET_UI_TREE
2. WM serializes all visible SemanticNodes to JSON
3. WM publishes INTENT_UI_TREE_RESPONSE with JSON payload
4. AI parses tree, identifies target node by ID or text
5. AI publishes INTENT_INTERACT_NODE with node_id + action
6. WM translates to physical coordinates
7. WM injects mouse/keyboard event via kernel syscall
```

### 7.3 Supported Interactions
- `click(node_id)` - Left-click at node center
- `type(node_id, text)` - Focus node and type text
- `scroll(node_id, direction)` - Scroll within node
- `hover(node_id)` - Move mouse to node center
- `read(node_id)` - Extract text content from node

## 8. REFLEX CRYSTALLIZATION ENGINE

### 8.1 Reflex Lifecycle
```
Phase 1: OBSERVATION
  - Episodic memory records: [Intent_A -> Action_B -> Result_C]
  - Pattern appears 3+ times with same positive outcome

Phase 2: CRYSTALLIZATION
  - Orchestrator detects repeated pattern
  - Compiles pattern into ReflexRule: {trigger: Intent_A, action: Intent_B}
  - Registers rule via INTENT_REFLEX_REGISTER syscall

Phase 3: ACTIVATION
  - Next time Intent_A appears on bus
  - Kernel Reflex Engine fires Intent_B immediately (Ring 0 speed)
  - LLM is NOT woken up - pure O(1) reflexive response

Phase 4: DECAY
  - If reflex produces negative outcomes, confidence decreases
  - Below threshold: reflex deactivated, LLM resumes control
```

### 8.2 Reflex Rule Format
```rust
struct ReflexRule {
    trigger_intent: u32,     // Intent ID that triggers this reflex
    trigger_payload_mask: u64, // Payload must match (0 = any)
    action_intent: u32,      // Intent to publish as response
    action_payload: u64,      // Payload to publish
    confidence: u8,           // 0-255, decay over time
    hit_count: u32,           // Times this reflex has fired
}
```

## 9. LANGUAGE RUNTIME SUPPORT

AetherionOS does not embed compilers. It executes pre-compiled static ELF
binaries via the Linuxulator. Supported runtimes (installed via `pkg install`):

| Language   | Runtime Binary     | Size   | Execution Command                    |
|------------|--------------------|--------|--------------------------------------|
| Python     | micropython.elf    | ~500KB | `tool_exec micropython.elf script.py`|
| JavaScript | quickjs.elf        | ~600KB | `tool_exec quickjs.elf script.js`    |
| Lua        | lua.elf            | ~200KB | `tool_exec lua.elf script.lua`       |
| Ruby       | mruby.elf          | ~1MB   | `tool_exec mruby.elf script.rb`      |
| Go/Rust/C  | (compiled ELF)     | varies | Direct execution via Linuxulator     |
| WebAssembly| wasm3.elf          | ~60KB  | `tool_exec wasm3.elf module.wasm`    |

## 10. BOOT SEQUENCE

```
1. BIOS -> Bootloader -> Kernel Ring 0
2. GDT/IDT/PIC/APIC initialization
3. Memory manager (demand paging, 8 GiB heap)
4. VFS mount (FAT32 disk.img)
5. Network stack (VirtIO-Net, IP/ARP/ICMP/TCP/DNS)
6. Agent loading (from include_bytes! or /disk/etc/boot.conf)
7. Cognitive Bus initialization
8. Agent scheduling begins (SMP round-robin)
9. Visual Terminal auto-tests (gen_driver, mcp_test, orch_test)
10. Interactive prompt (awaiting user commands)
```

## 11. FILE SYSTEM LAYOUT

```
/bin/                    Agent ELF binaries
/disk/                   FAT32 disk root
/disk/etc/boot.conf      Dynamic agent loading configuration
/disk/models/            GGUF model files
/disk/var/memory.db      Episodic memory database
/disk/var/autonomous.log Autonomous agent action log
/dev/                    Device pseudo-files
/proc/meminfo            Memory information
/proc/cpuinfo            CPU information
/sys/version             Kernel version string
/tmp/                    Temporary files (MCP contracts)
/var/drivers/            Generated driver source code
```

## 12. DEVELOPMENT GUIDELINES

### 12.1 Agent Development
- All agents MUST be `#![no_std]` Rust with `aetherion_sdk` dependency
- Entry point: `pub extern "C" fn main() -> i64`
- Communication: Cognitive Bus intents only
- Memory: `sys_brk` heap allocation via SDK's global allocator
- I/O: `sys_write(1, ...)` for serial logging, `sys_open/read/write` for files

### 12.2 Build Process
```bash
# Build single agent
cd userspace/agent_NAME
RUST_TARGET_PATH=/path/to/project cargo build --release --target x86_64-aetherion-user

# Build all agents (sequential, OOM-safe)
./scripts/build_all_agents.sh

# Build kernel (includes all agent binaries via include_bytes!)
cd kernel
CARGO_BUILD_JOBS=1 cargo bootimage --release --target x86_64-aetherion.json

# Run in QEMU
qemu-system-x86_64 -drive format=raw,file=bootimage.bin \
  -drive file=disk.img,format=raw,if=ide \
  -m 1024M -serial stdio -display none -cpu Haswell -smp 2
```

### 12.3 Testing
```bash
# Full regression suite (191 tests)
./scripts/regression-test.sh --timeout 180

# Quick boot test
./scripts/boot-test.sh
```

## 13. STRATEGIC VISION

### Phase 1: Foundation (Jalons 1-117) [COMPLETE]
- Kernel, SMP, ELF loader, VFS, Network, Cognitive Bus
- 37-command terminal, MCP, Orchestrator, LLM inference
- Linux ABI 6.18 compatibility (Linuxulator)

### Phase 2: Intelligence (Jalons 118-130) [CURRENT]
- Semantic UI Tree for structured screen reading
- Universal Tool Discovery (MCP pipes protocol)
- Reflex Crystallization Engine
- Legitimate agent compilation (zero stubs)

### Phase 3: Autonomy (Jalons 131-150) [PLANNED]
- MicroPython/QuickJS runtime integration
- Cognitive Pipes (stdout -> Bus routing)
- Self-optimizing reflex compilation (JIT bytecode)
- TLS/HTTPS via rustls no_std

### Phase 4: Mastery (Jalons 151+) [VISION]
- Multi-modal perception (Framebuffer -> VLM)
- Physical interaction injection (mouse/keyboard synthesis)
- Distributed agent networking (multi-OS coordination)
- Full web browsing via headless engine
- Game AI training via reinforcement learning loop

---

*"The OS does not know the tool. The OS provides the arena. The AI discovers the weapon."*

*-- MORNINGSTAR, AetherionOS SOUL Directive v1.0*
