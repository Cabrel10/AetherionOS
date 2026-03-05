# Changelog -- AetherionOS

All notable changes to this project will be documented in this file.
Format based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [v2.0.0] -- 2026-03-05 -- Franchissement du Mur UNIX

### Critical Fix
- **PML4 Heap Mapping Bug**: `clone_pml4_deep` was deep-copying kernel heap pages
  at PML4 index 136 (virtual address `0x4444_4444_0000`), causing forked child
  processes to see a stale copy of `PROCESS_TABLE`. Fixed by sharing any PML4 entry
  without `USER_ACCESSIBLE` (bit 2) verbatim instead of deep-copying it.

### Added -- Jalon 25: sys_fork + sys_exec
- `sys_fork` (syscall 57): allocates a new PML4, deep-copies all user-space pages
  (each 4 KB frame with flags preserved), copies the FD table and all saved
  registers. Parent receives child PID in RAX; child receives 0.
- `sys_exec` (syscall 59): reads a user-space path string, resolves to absolute
  VFS path (prepends `/bin/` if needed), loads the ELF binary via
  `crate::elf::load_elf_binary`, replaces the current process PML4, entry point
  and stack, resets FD table, then jumps to user mode.
- Parent-resume logic in `sys_exit`: when a forked child (non-thread) exits, the
  kernel unblocks the parent (waiting in `sys_wait`), restores its full register
  state and PML4, and returns the wait result via `sysretq`.

### Added -- Jalon 26: POSIX Shell
- `userspace/c_apps/sh.c`: Ring 3 interactive shell displaying `AETHER>` prompt.
  Built-in commands: `help`, `exit`, `ps`, `ls`, `echo`, `pid`. External commands
  executed via `fork()` + `execve()` + `waitpid()` loop.
- `libc_stub.c/.h`: added `fork()`, `execve()`, `waitpid()` syscall wrappers.
- `scripts/build_c.sh`: updated to compile `sh.elf` and include it in the VFS.
- Kernel startup launches `sh.elf` as the last userspace process.

### Added -- Jalon 24: Pipes & FD Management
- `sys_pipe` (syscall 22): creates a unidirectional byte pipe (4 KB circular buffer).
- `sys_dup2` (syscall 33): duplicates file descriptors for I/O redirection.
- `sys_getdents` (syscall 78): lists directory entries from VFS.
- 16 global pipe buffers with reader/writer close tracking.

### Added -- Jalon 22+23: Bare-Metal AI
- Jalon 22: 128x128 fixed-point (Q16.16) matrix multiplication with naive and
  tiled implementations, `rdtsc` cycle-accurate benchmarking.
- Jalon 23: RAG vector engine with 256 vectors (dimension 64), L2 norm
  pre-computation, cosine similarity top-K search.

### Added -- Jalon 21: GUI Framebuffer
- `sys_mmap_fb` (syscall 10): maps the linear framebuffer into user space.
- 1024x768 RGBA framebuffer with pixel-level drawing from Ring 3.

### Added -- Jalon 20: Multithreading
- `sys_clone` (syscall 56): creates a thread sharing the parent's PML4.
- Thread join via `sys_wait`: parent blocks until all child threads terminate.
- Counter test: 2 threads incrementing a shared variable to 200.

### Changed
- ELF frame pool increased from 4096 to 16384 frames (64 MiB) to accommodate
  deep-copy fork of multiple loaded processes.
- Linker script (`c_app.ld`): merged `.got.plt` and `.plt` into `.text` section
  to prevent NX faults on PIC-generated GOT trampolines.
- `process/task.rs`: added `is_forked`, `saved_syscall_regs`, `saved_kernel_rsp`
  fields for fork/wait state management.
- `process/mod.rs`: added `find_ready_forked_child()`, `fork_process()` with
  full FD table copy.

### Validation
- All 12 kernel test suites pass (heap, VFS, process, scheduler, GPU, syscall,
  ELF, context-switch, VFS-stress, verifier).
- J20 (threads), J21 (GUI), J22 (ML), J23 (RAG), J26 (fork+exec+shell) all green.
- QEMU 256 MiB, no panics, clean shutdown.

---

## [v1.9.0] -- 2026-03-02 -- Jalon 19: Storage Layer

### Added
- VirtIO-Block driver (PCI discovery, virtqueue I/O).
- FAT32 read-only filesystem driver (BPB, cluster chains, directory parsing).
- `/disk/` mount point in VFS.
- Ring 3 apps: `ls.elf`, `cat.elf`, `j19_test.elf`.
- TSS RSP0 for safe Ring 3 interrupt handling.
- 256 KiB kernel syscall stack for deep VFS/FAT32/VirtIO call chains.

---

## [v1.8.0] -- 2026-02-28 -- Jalons 17-18: Network

### Added
- VirtIO-net driver (PCI, virtqueue, MAC).
- Ethernet frame TX/RX, ARP resolution.
- IPv4 + UDP + TCP (3-way handshake, retransmit, FIN).
- DNS resolver (recursive A-record lookup).
- HTTP/1.1 GET client.
- Ring 3 app: `wget.elf` (DNS + TCP + HTTP).

---

## [v1.0.0] -- 2026-02-20 -- Jalons 1-16: Foundation

### Added
- GDT, IDT, PIC, timer, serial, keyboard drivers.
- Frame allocator, 4-level paging, 8 MiB kernel heap.
- Cognitive Bus (lock-free MPMC IPC).
- Virtual Filesystem with security hardening.
- Policy Verifier (Intent filtering engine).
- Matriarchal process hierarchy.
- Priority scheduler with MLFQ and aging.
- PCI GPU detection stub.
- SYSCALL/SYSRET MSR configuration.
- ELF64 loader with per-process PML4.
- Full POSIX syscall table.
- C userspace: `libc_stub` + `hello_c.elf`.

---

## [v0.0.1] -- 2025-12-09 -- Initial Commit

### Added
- Project structure, bootloader, VGA/serial drivers.
- Build scripts, documentation, MIT license.

---

**Maintainer**: MORNINGSTAR -- morningstar@aetherion.dev
**Repository**: https://github.com/Cabrel10/AetherionOS
