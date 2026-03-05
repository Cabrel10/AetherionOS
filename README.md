# AetherionOS

**Bare-Metal AI-Native Operating System in Rust `no_std` -- ACHA Architecture**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-nightly--2023--08--01-orange.svg)](https://www.rust-lang.org)
[![Arch](https://img.shields.io/badge/arch-x86__64-green.svg)](https://en.wikipedia.org/wiki/X86-64)
[![Version](https://img.shields.io/badge/version-v2.1.0--malloc--preempt-blue.svg)](#)
[![Jalons](https://img.shields.io/badge/jalons-28%2F28-brightgreen.svg)](#development-milestones)

---

## Vision

AetherionOS is an experimental bare-metal operating system written entirely in Rust
(`no_std`, zero external C dependencies) targeting x86_64. It implements the **ACHA**
(Aetherion Cognitive Hierarchical Architecture) -- a matriarchal process hierarchy
where AI agents communicate through a mediated Intent Bus rather than direct hardware
access.

As of **v2.1.0** the system has crossed the **Unix Wall** and added dynamic memory
management: full POSIX process lifecycle (`fork`, `exec`, `wait`, `exit`, `pipe`),
`malloc`/`free`/`calloc`/`realloc` via `sys_brk`, preemptive multi-threaded scheduling,
a Ring 3 interactive shell, network stack, persistent storage, and integrated bare-metal
ML inference -- all running on real hardware emulated by QEMU with 256 MiB RAM.

### Capabilities (v2.1.0-malloc-preempt)

| Domain | Features |
|--------|----------|
| **Isolation** | Ring 0 / Ring 3 separation, per-process PML4, KPTI-lite (no USER_ACCESSIBLE on kernel pages) |
| **Processes** | Matriarchal hierarchy (Matriarch / SubMatriarch / Worker), preemptive scheduling with priority aging |
| **POSIX** | `fork` (deep PML4 copy), `exec` (ELF reload), `wait`/`waitpid`, `pipe`, `dup2`, `mmap`, `getdents`, `brk` |
| **Memory** | `malloc`/`free`/`calloc`/`realloc` via `sys_brk`; first-fit allocator, 16-byte aligned, block coalescence |
| **Threads** | `clone` (shared address space), multi-threaded join via `sys_wait`, preemptive timer-based scheduling |
| **Network** | VirtIO-net, Ethernet, ARP, IPv4, UDP, TCP (3-way handshake + retransmit), DNS resolver |
| **Storage** | VirtIO-Block driver, read-only FAT32, VFS with `/bin`, `/sys`, `/disk` mount points |
| **Shell** | `sh.elf` -- Ring 3 POSIX shell: `AETHER>` prompt, built-ins (`help`, `ls`, `ps`, `echo`, `pid`), external command execution |
| **AI / ML** | Bare-metal fixed-point matrix multiply (naive + tiled), RAG cosine-similarity vector search (256 vectors, dim 64) |
| **Security** | Cognitive Bus intent mediation, Ring 0 policy verifier, capability checks, ASLR stubs |

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

All 26 Jalons (milestones) completed:

| Jalon | Name | Key Deliverable | Status |
|-------|------|-----------------|--------|
| 1-9 | Kernel Foundations | GDT, IDT, Paging, Heap, IPC, VFS, Scheduler, Syscalls | DONE |
| 11 | ELF Loader | ELF64 per-process paging, Ring 3 isolation | DONE |
| 13 | POSIX Syscalls | Full Linux-compatible syscall table | DONE |
| 16 | C Userspace | `libc_stub` + `hello_c.elf` running in Ring 3 | DONE |
| 17-18 | Network | VirtIO-net, TCP/IP, DNS, HTTP wget.elf | DONE |
| 19 | Storage | VirtIO-Block, FAT32, `ls.elf`, `cat.elf` | DONE |
| 20 | Multithreading | `sys_clone`, shared PML4, thread join | DONE |
| 21 | GUI Framebuffer | 1024x768 RGBA framebuffer, pixel drawing | DONE |
| 22 | ML Inference | 128x128 fixed-point matmul (naive + tiled) | DONE |
| 23 | RAG Engine | Cosine similarity, top-K vector search | DONE |
| 24 | Pipes & FD | `sys_pipe`, `sys_dup2`, `sys_getdents` | DONE |
| 25 | Fork + Exec | Deep PML4 copy, ELF reload, parent resume | DONE |
| 26 | POSIX Shell | `sh.elf` with fork/exec/wait loop | DONE |

---

## Metrics (v2.0.0)

| Metric | Value |
|--------|-------|
| Boot time | ~2 s (QEMU) |
| Binary size | ~2.5 MB (release) |
| Kernel heap | 8 MiB |
| ELF frame pool | 64 MiB (16 384 frames) |
| User stack | 1 MiB (256 pages) |
| Max processes | 256 |
| Ring 3 programs | 10 (sh, ls, cat, wget, hello_c, threads, ui, agent_ai, agent_rag, j19_test) |
| Test suites | 12/12 pass |
| Jalon tests | J20 + J21 + J22 + J23 + J26 all green |
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
