# AetherionOS

**Bare-Metal AI-Native Operating System in Rust `no_std` -- ACHA Architecture**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-nightly--2026-orange.svg)](https://www.rust-lang.org)
[![Arch](https://img.shields.io/badge/arch-x86__64-green.svg)](https://en.wikipedia.org/wiki/X86-64)
[![Version](https://img.shields.io/badge/version-v4.1.0--phase6-blue.svg)](#)
[![CI](https://img.shields.io/badge/CI-green-brightgreen.svg)](#)

---

## Current System State (v4.1.0-phase6 -- 2026-04-23)

**Reference Branch:** `genspark_ai_developer`

| Component | Status |
|-----------|--------|
| **Boot** | Limine v8.7.0, base revision 3, HHDM 0xFFFF800000000000 |
| **CI** | All 4 jobs green (Kernel Check, C Apps, Rust Agents, ISO Build) |
| **ISO** | ~100 MiB (kernel 616 KiB + Alpine rootfs + Python 3 + BusyBox) |
| **Memory** | Bitmap frame allocator + OffsetPageTable + 64 MiB heap |
| **Scheduler** | PriorityScheduler (5 queues, anti-starvation aging, SMP-aware) |
| **VFS** | BTreeMap hierarchy, capability checks, path traversal protection |
| **IPC** | Cognitive Bus (priority BinaryHeap, 1024 capacity) |
| **Framebuffer** | Limine GOP 1280x800 @ 32bpp |
| **Interrupts** | ENABLED -- timer + keyboard (Phase 6 #GP fix) |
| **Network** | VirtIO-Net init, PCI scan, IP 10.0.2.15 (QEMU user-mode) |
| **Shell** | help/uname/free/ps/uptime/heap/bus/net/exec/ping/wget/clear/halt |
| **Syscalls** | 13 core Linux syscalls + 100+ stubs for BusyBox compatibility |
| **Userspace** | 18 C ELF apps + 33 Rust agents built |

### QEMU Boot Verification
- Boots to `$` prompt with all 12 init steps passing
- 2045 MiB usable RAM, 14 memory map entries
- CPU: QEMU Virtual CPU v2.5+ (SSE, no AVX)
- GDT+TSS (segments reloaded SS=0x10), IDT (20 handlers), PIC (remapped 32-47), PS/2 keyboard
- Timer interrupts active (uptime >0 ticks)
- Security: TPM stub, KPTI-Lite, stack protector
- Network: PCI scan for VirtIO-Net on boot

---

## Vision

AetherionOS is an experimental bare-metal operating system written entirely in Rust
(`no_std`, zero external C dependencies) targeting x86_64. It implements the **ACHA**
(Aetherion Cognitive Hierarchical Architecture) -- a matriarchal process hierarchy
where AI agents communicate through a mediated Intent Bus rather than direct hardware
access.

As of **v4.1.0-phase6** the system boots via Limine v8.7.0 with a fully functional kernel:
memory management (bitmap frame allocator, OffsetPageTable, 64 MiB heap), a priority
scheduler with anti-starvation aging, VFS with security checks, a priority-aware
Cognitive Bus for inter-agent IPC, and an interactive serial shell. The kernel runs
bare-metal on QEMU with 2 GiB RAM and boots in ~3 seconds.

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
