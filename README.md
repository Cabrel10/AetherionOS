# AetherionOS

**Bare-Metal AI-Native Operating System in Rust `no_std` -- ACHA Architecture**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-nightly--2023--08--01-orange.svg)](https://www.rust-lang.org)
[![Arch](https://img.shields.io/badge/arch-x86__64-green.svg)](https://en.wikipedia.org/wiki/X86-64)
[![Version](https://img.shields.io/badge/version-v4.0--smp--stable-blue.svg)](#)
[![Jalons](https://img.shields.io/badge/jalons-109c%2F115-brightgreen.svg)](#development-milestones)

---

## Current System State (v4.0-smp-stable — Jalon 109c)

**Reference Branch:** `genspark_ai_developer`

| Component | Status |
|-----------|--------|
| **SMP Dual-Core** | ✅ Core 0 (BSP) + Core 1 (AP) via INIT-SIPI-SIPI |
| **Visual Terminal (PID 18)** | ✅ AetherionOS v4.0 Production Terminal — "Terminal ready" |
| **LLM Chat Agent (PID 11)** | ✅ Running on Core 1, GGUF model loading |
| **Llama Core Agent (PID 13)** | ✅ Running on Core 1, tensor processing |
| **Orchestrator Agent** | ✅ Functional — bus intent routing |
| **Validator Agent (PID 15)** | ✅ Strict mode, INTENT_VALIDATOR_READY published |
| **MCP Agent** | ✅ Level 8 security firewall, subscribing to intents |
| **BusyBox (PID 16)** | ✅ Linux-compatible, 33 commands |
| **Agent Clock** | ✅ J112a — QUEUED on Core 0 |
| **Cognitive Bus** | ✅ 5+ bus_publish events, intent routing active |
| **Keyboard Input** | ✅ PS/2 AZERTY, scancode translation |
| **Framebuffer** | ✅ 1024x768x32bpp |

### Regression Test Results (300s, SMP 2-core, 1 GiB RAM)
- ✅ **0 SIGSEGV** — Zero process crashes
- ✅ **0 Double Fault** — No SMP kernel panics
- ✅ **0 PANIC** — Kernel completely stable
- ✅ **0 PF-FATAL / addr=0x0 / rip=0x0** — No null pointer faults
- ✅ **0 System halted** — Full 300s uptime
- ✅ **Terminal ready** achieved
- ✅ **5+ bus_publish** intents published
- ✅ **agent_clock.elf** queued (J112a)
- ✅ **166/185 regression tests pass**

### Critical Fix: Jalon 109c — IRETQ Register Clobbering
The LLVM optimizer was coalescing `in(reg)` operands in inline assembly IRETQ blocks,
causing the PML4 physical address to overwrite the user RIP/RSP values. Fixed by
forcing explicit GPR allocation (`r8`, `r9`, `r10`, `r11`, `r12`) across **all 8 IRETQ
sites** in `main.rs`, `syscall.rs`, `idt.rs`, and `elf/mod.rs`.

---

## Vision

AetherionOS is an experimental bare-metal operating system written entirely in Rust
(`no_std`, zero external C dependencies) targeting x86_64. It implements the **ACHA**
(Aetherion Cognitive Hierarchical Architecture) -- a matriarchal process hierarchy
where AI agents communicate through a mediated Intent Bus rather than direct hardware
access.

As of **v4.0** the system has crossed the **SMP + AGI Wall**: true dual-core execution
(INIT-SIPI-SIPI), bare-metal GGUF LLM inference on a dedicated core, cognitive bus
intent routing, Linux ABI compatibility (BusyBox runs natively), a Model Context
Protocol (MCP) security firewall, and a visual terminal with 10+ concurrent agents --
all running bare-metal on QEMU with 1 GiB RAM and 2 CPU cores.

### Capabilities (v4.0-smp-stable)

| Domain | Features |
|--------|----------|
| **SMP** | True dual-core (INIT-SIPI-SIPI), per-core kernel stacks, APIC timer scheduling |
| **Isolation** | Ring 0 / Ring 3, per-process PML4, KPTI-lite, IRETQ hardcoded GPR allocation |
| **Processes** | ACHA matriarchal hierarchy, preemptive scheduling, 18+ concurrent processes |
| **POSIX** | `fork`, `exec`, `wait`, `exit`, `pipe`, `dup2`, `mmap`, `brk`, `getdents`, `clone`, `futex`, `ptrace` |
| **Memory** | `sys_brk` heap (4 GiB user), demand paging, deferred PML4 GC |
| **Linux ABI** | ELF auxiliary vector, AT_RANDOM, BusyBox 33-command shell, /proc stubs |
| **Network** | VirtIO-net, Ethernet, ARP, IPv4, UDP, TCP, DNS, ICMP echo |
| **Storage** | VirtIO-Block, FAT32, VFS `/bin /sys /disk /proc` |
| **AI / LLM** | GGUF v3 model loading, SmolLM 135M inference, INT8 KV cache, BPE tokenizer |
| **Cognitive Bus** | Lock-free MPMC intent pub/sub, 12+ intent types, session/correlation IDs |
| **MCP** | Model Context Protocol agent — security firewall between LLM and syscalls |
| **Security** | Policy verifier, capability checks, validator agent (strict/admin/god mode) |
| **Terminal** | Visual terminal with framebuffer, AetherionOS v4.0 Production Terminal |

---

## Architecture

```
 ============================================================================
 |                         RING 3  --  USER SPACE                           |
 |                                                                          |
 |  +-----------+   +----------+   +--------+   +---------+   +--------+   |
 |  | Matriarch |   | SubMatri |   | Worker |   | Worker  |   | Worker |   |
 |  | (PID 1)   |   | (PID 2)  |   | sh.elf |   | ls.elf  |   |wget.elf|  |
 |  +-----------+   +----------+   +--------+   +---------+   +--------+   |
 |        |               |             |              |            |       |
 |  ======|===============|=============|==============|============|=====  |
 |                     SYSCALL  Interface (LSTAR MSR)                       |
 |          fork  exec  wait  exit  read  write  pipe  mmap  socket ...     |
 ============================================================================
 |                         RING 0  --  KERNEL                               |
 |                                                                          |
 |  +-------------------+  +------------------+  +--------------------+     |
 |  |   Scheduler       |  |  Memory Manager  |  |   VFS + FAT32      |    |
 |  |  Priority aging   |  |  Frame allocator |  |   /bin /sys /disk  |    |
 |  |  MLFQ (4 queues)  |  |  Per-proc PML4   |  |   Pipes, FD table  |    |
 |  +-------------------+  |  8 MiB heap       |  +--------------------+    |
 |                          |  64 MiB ELF pool  |                           |
 |  +-------------------+  +------------------+  +--------------------+     |
 |  |  Cognitive Bus    |  |  ELF64 Loader    |  |   Network Stack    |    |
 |  |  Intent pub/sub   |  |  Per-proc paging |  |   ETH/ARP/IP/TCP   |    |
 |  |  MPMC lock-free   |  |  1 MiB user stk  |  |   UDP/DNS/HTTP     |    |
 |  +-------------------+  +------------------+  +--------------------+     |
 |                                                                          |
 |  +-------------------+  +------------------+  +--------------------+     |
 |  |  Policy Verifier  |  |  Process Table   |  |   GPU / Display    |    |
 |  |  Intent filtering |  |  256 slots max   |  |   VGA + Framebuf   |    |
 |  |  Capability check |  |  Matriarchal tree |  |   1024x768 RGBA    |    |
 |  +-------------------+  +------------------+  +--------------------+     |
 |                                                                          |
 ============================================================================
 |                     HARDWARE  ABSTRACTION  LAYER                         |
 |  GDT  IDT  PIC/APIC  PIT  Serial  PS/2  PCI  VirtIO-net  VirtIO-blk    |
 ============================================================================
```

### ACHA Matriarchal Hierarchy

Processes are organized in a strict hierarchy enforced by `AgentRole`:

| Role | Priority | Purpose |
|------|----------|---------|
| `Matriarch` | 20 | Root authority, system-wide policy |
| `SubMatriarch` | 15 | Domain controller (network, storage) |
| `Worker` | 5 | User applications, AI agents |
| `KernelThread` | 25 | In-kernel services |

Ring 3 processes publish **Intents** to the Cognitive Bus. The Ring 0 **Verifier**
(Couche 5) inspects each Intent against the active policy before allowing execution.
This mediates all IPC, preventing rogue agents from bypassing security.

---

## Building

### Prerequisites

```bash
# System packages
sudo apt-get install qemu-system-x86 gcc nasm mtools

# Rust (exact version required)
rustup toolchain install nightly-2023-08-01 --profile minimal
rustup target add x86_64-unknown-none --toolchain nightly-2023-08-01
rustup component add rust-src llvm-tools-preview --toolchain nightly-2023-08-01

# Bootimage (exact version)
cargo install bootimage --version 0.10.3
```

### Compile

```bash
# Build C userspace apps (sh.elf, ls.elf, hello_c.elf, wget.elf ...)
bash scripts/build_c.sh

# Build kernel + bootimage
cd kernel
CARGO_BUILD_JOBS=1 cargo bootimage --release
```

### Run in QEMU

```bash
qemu-system-x86_64 \
    -drive format=raw,file=kernel/target/x86_64-unknown-none/release/bootimage-aetherion-kernel.bin \
    -m 256M -serial stdio -display none
```

---

## Development Milestones

109 Jalons completed (115 planned):

| Jalon | Name | Key Deliverable | Status |
|-------|------|-----------------|--------|
| 1-9 | Kernel Foundations | GDT, IDT, Paging, Heap, IPC, VFS, Scheduler, Syscalls | DONE |
| 11 | ELF Loader | ELF64 per-process paging, Ring 3 isolation | DONE |
| 13 | POSIX Syscalls | Full Linux-compatible syscall table | DONE |
| 16-26 | Unix Layer | C userspace, network, storage, fork/exec, shell | DONE |
| 65 | Visual Terminal | Interactive terminal with keyboard, framebuffer | DONE |
| 97 | True SMP | INIT-SIPI-SIPI, dual-core, per-core stacks | DONE |
| 102-105 | Ring 3 AI | SmolLM token generation, Linux ABI compatibility | DONE |
| 107-108 | MCP + Syscalls | MCP tool execution, clone/futex/ptrace, cognitive desktop | DONE |
| 109 | Deferred PML4 GC | Precise ELF segment intersection, valid RIP enforcement | DONE |
| 109b | IRETQ read_volatile | Boot-stack overflow protection via volatile barriers | DONE |
| 109c | **GPR Hardcoding** | **Explicit r8-r12 register allocation in ALL IRETQ paths** | **DONE** |
| 110 | Kernel Session Memory | *Planned* | TODO |
| 111 | Episodic Memory Agent | *Planned* | TODO |
| 112a | Agent Clock | Clock sensor ELF on Core 0 | DONE |
| 113 | Policy Engine | *Planned* | TODO |
| 114 | Worker Pool | *Planned* | TODO |
| 115 | Live Migration | *Planned* | TODO |

---

## Metrics (v4.0-smp-stable)

| Metric | Value |
|--------|-------|
| Boot time | ~3 s (QEMU, 2 cores) |
| Binary size | ~3.5 MB (release, bootimage) |
| Kernel heap | 8 MiB |
| ELF frame pool | 64 MiB (16 384 frames) |
| User heap (sys_brk) | Up to 8 GiB per process |
| User stack | 2 MiB (512 pages) |
| Max processes | 256 |
| Active agents | 18+ (LLM, orchestrator, validator, MCP, terminal, clock, busybox...) |
| CPU cores | 2 (SMP via INIT-SIPI-SIPI) |
| RAM | 1 GiB |
| Regression tests | **166/185 pass** (300s run) |
| Crash count (300s) | **0 SIGSEGV, 0 panic, 0 double-fault** |
| Bus events | 5+ publish, 1+ consume |
| Toolchain | `nightly-2023-08-01` + `bootimage 0.10.3` (strict) |

---

## Project Structure

```
AetherionOS/
  kernel/
    src/
      arch/x86_64/       # GDT, IDT, syscall entry, context switch
      memory/             # Frame allocator, paging, heap (8 MiB)
      process/            # Process table, task struct, scheduler
      fs/                 # VFS core, FAT32 driver
      ipc/                # Cognitive Bus (Intent pub/sub)
      elf/                # ELF64 loader, per-process PML4
      net/                # Ethernet, ARP, IPv4, TCP, UDP, DNS, HTTP
      security/           # ASLR stubs, capability model
      scheduler/          # MLFQ with priority aging
    Cargo.toml
    rust-toolchain.toml   # Pins nightly-2023-08-01
  userspace/
    c_apps/
      sh.c                # POSIX shell
      libc_stub.c/.h      # Minimal libc (syscall wrappers)
      c_app.ld            # Linker script (base 0x8000000000)
      *.elf               # Compiled Ring 3 binaries
  scripts/
    build_c.sh            # Cross-compile C apps
  docs/                   # Architecture documents
  README.md
  ROADMAP.md
  CHANGELOG.md
```

---

## License

MIT License. See [LICENSE](LICENSE).

## Author

**MORNINGSTAR** -- morningstar@aetherion.dev
[github.com/Cabrel10/AetherionOS](https://github.com/Cabrel10/AetherionOS)

---

*Built with Rust and bare-metal determination.*
