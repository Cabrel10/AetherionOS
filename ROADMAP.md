# AetherionOS Roadmap

**Master Plan -- From Bare Metal to AI-Native Desktop**

Last updated: 2026-03-17 (v3.0-terminal-stable)

---

## Current System State

**Reference:** `d3da007f875164d5ce12a93f7f3a9eb579ea0732`

| Phase | Status | Notes |
|-------|--------|-------|
| Phase 1: Kernel Foundations | ✅ DONE | All jalons 1-9 complete |
| Phase 2: POSIX Userspace | ✅ DONE | All jalons 11-28 complete |
| Phase 3: Expansion | ⚠️ PARTIAL | Visual terminal functional, LLM disabled |

**Active Components:**
- Visual Terminal (`agent_visual_term.elf`) - Interactive shell with keyboard
- Framebuffer 1024x768x32bpp
- PS/2 Keyboard (AZERTY layout, translated scancodes)
- Preemptive scheduler via `sys_yield()`
- FAT32 filesystem read-only

**Disabled Components:**
- LLM Chat Agent (`agent_llm_chat.elf`) - Crash at rip=0x0
- Orchestrator Agent - Disabled to prevent interference
- Llama Core Agent - Disabled to prevent interference

---

## Phase 1: Kernel Foundations [DONE]

*Jalons 1-9 -- The bones of the system.*

| Jalon | Deliverable | Key Files |
|-------|-------------|-----------|
| 1 | GDT, IDT, PIC, Secure Boot stubs | `arch/x86_64/gdt.rs`, `idt.rs` |
| 2 | Frame allocator, 4-level paging, 8 MiB heap | `memory/frame.rs`, `paging.rs`, `heap.rs` |
| 3 | Cognitive Bus -- lock-free MPMC Intent IPC | `ipc/bus.rs` |
| 4 | Virtual Filesystem with path-traversal security | `fs/vfs.rs` |
| 5 | Policy Verifier -- Intent filtering engine | `security/mod.rs` |
| 6 | Matriarchal process hierarchy, Ring 3 isolation | `process/mod.rs`, `task.rs` |
| 7 | Priority scheduler with MLFQ and aging | `scheduler/mod.rs` |
| 8 | PCI GPU detection, VRAM allocation stub | `gpu/mod.rs` |
| 9 | SYSCALL/SYSRET (LSTAR MSR), context switch | `arch/x86_64/syscall.rs` |

---

## Phase 2: POSIX Userspace [DONE]

*Jalons 11-26 -- Crossing the Unix Wall.*

| Jalon | Deliverable | Notes |
|-------|-------------|-------|
| 11 | ELF64 loader, per-process PML4 | 4096-frame bump pool, USER_STACK 1 MiB |
| 13 | Full POSIX syscall table | Linux x86_64 ABI compatible |
| 16 | C userspace (`libc_stub` + `hello_c.elf`) | `-mcmodel=large -fPIC -ffreestanding` |
| 17 | Network: VirtIO-net, ETH, ARP, IPv4, UDP, TCP, DNS | Full 3-way handshake, retransmit |
| 18 | HTTP client (`wget.elf`) | DNS resolve + TCP + HTTP/1.1 GET |
| 19 | Storage: VirtIO-Block, FAT32 read-only | `ls.elf`, `cat.elf`, `j19_test.elf` |
| 20 | Multithreading: `sys_clone`, shared PML4 | Thread join via `sys_wait` |
| 21 | GUI framebuffer: 1024x768 RGBA | `sys_mmap_fb`, pixel drawing |
| 22 | ML inference: 128x128 Q16.16 matmul | Naive + tiled, rdtsc benchmark |
| 23 | RAG vector engine: cosine similarity | 256 vectors dim-64, top-K search |
| 24 | Pipes & FD management | `sys_pipe`, `sys_dup2`, `sys_getdents` |
| 25 | `sys_fork` (deep PML4 copy) + `sys_exec` | PML4 heap sharing fix (PML4[136]) |
| 26 | POSIX shell (`sh.elf`) | `AETHER>` prompt, fork/exec/wait loop |
| 27 | Dynamic memory: `sys_brk`, `malloc`/`free` | First-fit, 16-byte aligned, coalescence |
| 28 | Preemptive scheduler + multi-threaded demo | PIT timer tick, priority queues, aging |

---

## Phase 3: Expansion [IN PROGRESS]

### Voie 1 -- Aetherion SDK & Toolchain

*Goal: Provide a first-class C/Rust development kit so third-party code compiles
against AetherionOS without touching the kernel.*

| Step | Description | Status |
|------|-------------|--------|
| 1a | Extract `libc_stub` into `sdk/c/`, build `libaetherion.a` | DONE |
| 1b | Port `newlib` stubs (malloc, stdio, string) on top of Aetherion syscalls | Planned |
| 1c | Create `x86_64-aetherion-none` Rust target JSON | Planned |
| 1d | Cargo wrapper (`cargo-aetherion`) to cross-compile Ring 3 crates | Planned |
| 1e | Port `picolibc` as drop-in newlib replacement for smaller footprint | Planned |

### Voie 2 -- AI Native Runtime

*Goal: Run real neural-network inference in Ring 3 with kernel-managed VRAM.*

| Step | Description | Status |
|------|-------------|--------|
| 2a | Port `candle` or ONNX-runtime to `no_std` + Aetherion syscalls | Planned |
| 2b | Kernel VRAM manager: `sys_vram_alloc`, `sys_vram_map` | Planned |
| 2c | Ring 3 LLM agent: tokenizer + transformer forward pass | Planned |
| 2d | Hippocampe: persistent vector DB via FAT32 + mmap | Planned |
| 2e | Cognitive Bus: LLM agent publishes Intents, Verifier audits | Planned |

### Voie 3 -- Cognitive UI

*Goal: A graphical desktop driven by AI agents.*

| Step | Description | Status |
|------|-------------|--------|
| 3a | Double-buffered framebuffer, vsync via PIT | Planned |
| 3b | PS/2 mouse + keyboard input events to Ring 3 | Planned |
| 3c | Window compositor (tiling or stacking) | Planned |
| 3d | Port Slint UI toolkit to Aetherion framebuffer backend | Planned |
| 3e | AI-driven window management via Cognitive Bus Intents | Planned |

---

## Phase 4: Hardening & Distribution

| Step | Description |
|------|-------------|
| 4a | Writable FAT32 (`sys_write` to `/disk/`) |
| 4b | SMP: multi-core boot (AP startup, per-CPU data) |
| 4c | UEFI boot (replace legacy bootloader) |
| 4d | Real hardware validation (Intel NUC, ThinkPad) |
| 4e | ISO image generation, live-USB boot |
| 4f | Continuous integration (GitHub Actions, QEMU farm) |

---

## Guiding Principles

1. **Rust everywhere** -- the kernel has zero lines of C; userspace allows C via SDK
2. **Intent mediation** -- no process talks to hardware directly; all requests flow
   through the Cognitive Bus and are audited by the Verifier
3. **AI-first** -- scheduling, security, and eventually the UI are informed by
   on-device inference
4. **Minimal dependencies** -- no external crates in the kernel beyond `spin`,
   `lazy_static`, `x86_64`, and `bootloader`
5. **Test-driven** -- every Jalon adds automated QEMU validation; regressions are
   caught before merge

---

*"We do not build an OS to replicate Unix. We build it to surpass it."*
-- MORNINGSTAR
