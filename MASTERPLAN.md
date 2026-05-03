# AetherionOS — MASTERPLAN v4.0

## Comprehensive Development Roadmap: Blocks A–P

**Status**: Active Development — 39,687 lines across 65 kernel source files
**Target**: Fully POSIX-compliant OS with real hardware features, dynamic linking, native packages, GPU, and optional LLM inference.
**Kernel Language**: Rust `no_std` (nightly), x86_64, Limine bootloader
**Session**: PR #46 — Pillar 1 (Dynamic Linking + TTY) substantially complete

---

## Current State Summary

| Component | File | Lines | Status |
|-----------|------|-------|--------|
| Syscall dispatch | `arch/x86_64/syscall.rs` | 7,999 | Active — 230+ syscalls routed |
| Linux ABI compat | `compat/linux_abi.rs` | 4,056 | Active — per-process signals, PTY routing |
| ELF loader + dynamic linker | `elf/mod.rs` | 2,364 | **Complete** — PT_INTERP + AuxV + interp loading |
| Process management | `process/mod.rs` + `task.rs` | 1,837 | Active — fork/clone/wait4/signals/FD table |
| PTY subsystem | `drivers/pty.rs` | 723 | **NEW** — Full master/slave + line discipline |
| VFS multi-backend | `fs/vfs_backend.rs` | 625 | **NEW** — procfs/devfs/sysfs backends |
| VFS core | `fs/vfs.rs` | ~1,024 | Stable — BTreeMap-based, security hardened |
| Network stack | `net/` | ~1,651 | Active — TCP/UDP/DNS/ARP/ICMP/VirtIO-Net |
| VirtIO-blk driver | `drivers/virtio_blk.rs` | ~598 | Stable — Read/write, PCI discovery |
| FAT32 filesystem | `fs/fat32.rs` | ~800 | Stable — Read/write/directory ops |
| Memory management | `memory/` | ~700 | Stable — Frame alloc, paging, heap |
| GPU/Framebuffer | `drivers/virtio_gpu.rs` | ~600 | Stub — VirtIO-GPU + framebuffer |

---

## Pillar 1 — Dynamic Linking & TTY ✅ (This PR)

### Block F — Dynamic Linker (ELF PT_INTERP) ✅

**Implemented in `kernel/src/elf/mod.rs`**:
- PT_INTERP segment detection and NUL-terminated path extraction
- `load_interp_into_pml4()` — maps interpreter (ld-musl/ld-linux) at `0x7FC0_0000_0000`
- `build_sysv_stack()` with correct AuxV entries:
  - `AT_BASE` = interpreter load address
  - `AT_ENTRY` = main binary's original entry point
  - `AT_PHDR` = main binary's program headers in memory
  - `AT_PHNUM` = main binary's program header count
  - `AT_RANDOM` = 16 bytes of RDTSC-based entropy
  - `AT_HWCAP` = fpu + sse + sse2 + avx + avx2
  - `AT_PAGESZ`, `AT_PHENT`, `AT_CLKTCK`, `AT_UID`, `AT_GID`, etc.
- VFS fallback paths: `/lib/ld-musl-x86_64.so.1`, `/disk/lib/...`
- Entry point routing: static → binary entry, dynamic → interpreter entry
- Process creation with Ring 3 context (CS=0x23, SS=0x1B, RFLAGS=0x202)

**Remaining for full dynamic linking**:
- [ ] TLS initialization (arch_prctl FS_BASE already working)
- [ ] LD_PRELOAD / LD_LIBRARY_PATH
- [ ] dlopen/dlsym/dlclose runtime linking

### Block I — Full PTY/TTY Terminal ✅

**Implemented in `kernel/src/drivers/pty.rs` (723 lines)**:
- **Data structures**: `PtyPair` with dual `RingBuf` (4096 bytes each), `Termios`, `WinSize`
- **Master/Slave model**: `/dev/ptmx` allocates pair, `/dev/pts/N` opens slave
- **Line discipline**:
  - Canonical mode (ICANON) with line editing (VERASE, VKILL, VEOF, VWERASE)
  - Echo mode with proper character echoing
  - Raw mode (non-canonical) passthrough
- **Input processing**: ICRNL (CR→NL), IGNCR, INLCR, ISTRIP, IUCLC
- **Output processing**: OPOST, ONLCR (NL→CR+NL), OCRNL, ONOCR
- **Signal generation**: VINTR→SIGINT, VQUIT→SIGQUIT, VSUSP→SIGTSTP
  - Calls `crate::process::send_signal_to_pgrp()` for real delivery
- **Termios ioctls**: TCGETS, TCSETS, TCSETSW, TCSETSF
- **Window size**: TIOCGWINSZ, TIOCSWINSZ
- **PTY control**: TIOCGPTN, TIOCSPTLCK, TIOCSCTTY, TIOCNOTTY, FIONREAD
- **Session/PGRP**: TIOCGPGRP, TIOCSPGRP
- **FD integration**: `FdType::PtyMaster`, `FdType::PtySlave` in process FD table

**Syscall integration** (in `syscall.rs`):
- `sys_read` dispatches PtyMaster/PtySlave reads
- `sys_write` dispatches PtyMaster/PtySlave writes
- `sys_open` handles `/dev/ptmx` → `pty_alloc()`, `/dev/pts/N` → `pty_open_slave()`
- `epoll_wait` checks PTY readiness
- `ioctl` routes to `pty_ioctl()` for all TIOC* and TC* commands

**Remaining**:
- [ ] SIGWINCH delivery on window resize
- [ ] XON/XOFF flow control
- [ ] Background process group I/O blocking (SIGTTIN/SIGTTOU)

---

## Pillar 2 — Native Package & Network

### Block E — Network Stack (Active)

**Implemented** (~1,651 lines in `net/`):
- Ethernet frame parsing/construction
- IPv4 header parsing + checksum
- ARP request/reply with cache
- ICMP echo (ping)
- UDP send/receive
- TCP: SYN/SYN-ACK/ACK handshake, FIN close
- DNS resolver with cache (A records, TTL)
- VirtIO-Net driver (receive/send)
- Socket API: socket, bind, listen, connect, accept, send, recv

**Remaining**:
- [ ] TCP retransmission timer (RTO calculation, exponential backoff)
- [ ] TCP sliding window (receive window advertisement, congestion control)
- [ ] TCP keep-alive probes
- [ ] DHCP client (currently hardcoded 10.0.2.15)
- [ ] DNS AAAA records (IPv6)
- [ ] Raw sockets (for ping utility)
- [ ] Unix domain sockets (AF_UNIX)
- [ ] `/proc/net/tcp`, `/proc/net/udp`

### Block C — Multi-Backend VFS ✅

**Implemented in `kernel/src/fs/vfs_backend.rs` (625 lines)**:
- **`FsBackend` trait**: read/write/stat/readdir/mkdir/unlink/symlink/readlink
- **Mount table**: longest-prefix matching, sorted by path length
- **ProcFs** (`/proc`):
  - `/proc/meminfo`, `/proc/cpuinfo`, `/proc/version`, `/proc/uptime`
  - `/proc/loadavg`, `/proc/stat`, `/proc/mounts`, `/proc/filesystems`
  - `/proc/cmdline`, `/proc/self/{status,maps,cmdline,environ,exe,fd}`
- **DevFs** (`/dev`):
  - `/dev/null`, `/dev/zero`, `/dev/urandom`, `/dev/random`
  - `/dev/tty`, `/dev/console`, `/dev/ptmx`, `/dev/pts/*`
  - `/dev/stdin`, `/dev/stdout`, `/dev/stderr` (symlinks)
  - `/dev/fd/` directory
- **SysFs** (`/sys`):
  - `/sys/kernel/{hostname,ostype,osrelease,version}`
  - `/sys/devices/system/cpu/{online,possible,present,cpu0/...}`
  - `/sys/devices/system/memory/block_size_bytes`
- **`init_virtual_filesystems()`** for boot-time mounting

**Remaining**:
- [ ] RamFs backend (wrap existing BTreeMap VFS)
- [ ] Ext4 read-only backend (parse superblock + inode table)
- [ ] FAT32 backend (wrap existing fat32.rs)
- [ ] `mount`/`umount` syscall integration
- [ ] `chroot` jail enforcement
- [ ] File locking (`flock`, `fcntl F_SETLK`)

### Block H — APK + Alpine rootfs

**Remaining** (all):
- [ ] Download Alpine minirootfs tarball
- [ ] Implement tar.gz extraction (gzip + tar parsers) in `no_std`
- [ ] Extract to FAT32/ramfs root
- [ ] Mount rootfs at `/`
- [ ] `apk` static binary support
- [ ] Package database parsing (`APKINDEX.tar.gz`)
- [ ] Dependency resolution
- [ ] `/etc/apk/repositories` configuration

---

## Pillar 3 — GPU Subsystem

### Block N — Graphical Compositor

**Remaining** (all):
- [ ] VirtIO-GPU driver: VIRTIO_GPU_CMD_RESOURCE_CREATE_2D, TRANSFER_TO_HOST_2D, SET_SCANOUT
- [ ] DRM/KMS abstraction layer (mode setting, CRTC, plane, connector)
- [ ] Vsync / page flip
- [ ] Rust `no_std` window compositor (z-ordering, alpha blending)
- [ ] Input event routing (/dev/input/eventN → compositor → client)
- [ ] Simple X11 or Wayland protocol subset
- [ ] Mouse cursor rendering

### Block O — Hardware Drivers

**Implemented**:
- VirtIO-blk (PCI discovery, virtqueue, sector read/write)
- VirtIO-GPU (stub: resource create, basic framebuffer)
- VirtIO-Net (packet send/receive via virtqueue)
- PS/2 keyboard + mouse (IRQ 1/12, scancode set 2)
- USB 3.0 xHCI (stub: PCI enumeration)

**Remaining**:
- [ ] GPU: full VirtIO-GPU 2D/3D command set
- [ ] NVMe: PCI discovery, admin/IO queue pairs, namespace support
- [ ] USB: device enumeration, mass storage class, HID class
- [ ] AHCI/SATA for real disk controllers
- [ ] Sound: AC97 or Intel HDA
- [ ] RTC real-time clock (CMOS 0x70/0x71)

---

## Pillar 4 — Hardware-Accelerated LLM Inference

### Block M — LLM Inference (GGUF/AVX2)

**Prerequisites**: Block B (HugePages), Block D (XSAVE)

**Remaining** (all):
- [ ] XSAVE/XRSTOR for full AVX2/AVX-512 register save in scheduler
  - Detect XSAVE support via CPUID(0x0D, ECX=0)
  - Allocate XSAVE area per process (≥832 bytes for AVX2, 2560+ for AVX-512)
  - Save/restore via `xsave [rdi]` / `xrstor [rdi]` on context switch
- [ ] HugePages (2 MiB / 1 GiB) for model mmap
  - 2 MiB: use PDE with PS bit set (bit 7)
  - 1 GiB: use PDPTE with PS bit set
  - `mmap(MAP_HUGETLB)` syscall support
- [ ] Port llama.c or `no_std` Rust inference engine
  - Bare-metal GGUF format parser (magic, version, tensors metadata)
  - Matrix multiplication with AVX2/FMA intrinsics
  - Token sampling (top-k, top-p, temperature)
  - KV-cache management

---

## Supporting Blocks

### Block A — Linux Syscall Implementation

**Current**: ~230 syscall numbers mapped, ~200 with real implementations

**Priority 1 — Dynamic linking prereqs** ✅:
- `mmap` (9), `mprotect` (10), `munmap` (11) — enhanced with VMA tracking
- `brk` (12) — Linux-compatible
- `arch_prctl` (158) — FS_BASE/GS_BASE for TLS
- `set_tid_address` (218) — thread ID

**Priority 2 — Process management** ✅:
- `clone` (56) — full flags (CLONE_VM, CLONE_FILES, CLONE_SETTLS, etc.)
- `fork` (57), `vfork` (58) — PML4 deep copy
- `wait4` (61) — child reaping with wstatus
- `execve` (59) — falls through to native (ELF reload)
- `exit` (60), `exit_group` (231)

**Priority 3 — File I/O** ✅:
- `openat` (257), `close` (3), `read` (0), `write` (1)
- `stat` (4), `fstat` (5), `lstat` (6), `statx` (332)
- `getdents64` (217), `readlink` (89), `readlinkat` (267)
- `fcntl` (72), `dup` (32), `dup2` (33), `pipe2` (293)
- `lseek` (8), `pread64` (17), `pwrite64` (18)
- `renameat2` (316), `unlinkat` (263), `mkdirat` (258)
- `symlink` (88), `symlinkat` (266)

**Priority 4 — Signals** ✅ (this session):
- `rt_sigaction` (13) — per-process handler table (reads full 32-byte sigaction)
- `rt_sigprocmask` (14) — per-process mask (SIG_BLOCK/UNBLOCK/SETMASK)
- `rt_sigreturn` (15) — context restoration stub
- `kill` (62), `tkill` (200), `tgkill` (234)
- Signal delivery: `send_signal()`, `send_signal_to_pgrp()`, `dequeue_signal()`
- SIGKILL/SIGSTOP cannot be caught (EINVAL from rt_sigaction)
- Default actions: TERM (kill), STOP (block), CONT (unblock)

**Priority 5 — Stubs and infrastructure** ✅:
- `select` (23) → pselect6 wrapper
- `poll` (7) — basic ready check
- `epoll_create1` (291), `epoll_ctl` (233), `epoll_wait` (232) — full impl
- `socket` (41) – `shutdown` (48) — native dispatch
- `ioctl` (16) — extended: PTY, framebuffer, input, terminal
- `futex` (202) — FUTEX_WAIT/FUTEX_WAKE
- `getrandom` (318) — RDTSC-based PRNG
- `chdir` (80), `getcwd` (79), `chroot` (161)
- `mount` (165), `umount2` (166) — stubs
- `flock` (73), `fsync` (74), `fdatasync` (75) — stubs
- `mlock/munlock/mlockall/munlockall` (149-152)
- Many more (see Block A tables in source)

### Block B — Memory Subsystem

**Implemented**:
- Frame allocator (pool of 2048 4KiB frames)
- Per-process PML4 cloning (kernel upper half shared)
- `mmap` MAP_ANONYMOUS + MAP_FIXED + VMA tracking
- `munmap` with VMA tracking
- `mprotect` (page permission changes)
- `brk` (heap expansion)

**Remaining**:
- [ ] `mremap` — VMA split/merge (currently ENOSYS)
- [ ] `madvise` MADV_DONTNEED (release physical pages)
- [ ] HugePages (2 MiB / 1 GiB) for model loading
- [ ] Demand paging / page fault handler for mmap'd files
- [ ] Copy-on-Write (COW) for fork()
- [ ] `/proc/self/maps` generation from VMA list

### Block D — Process Handling

**Implemented**:
- `fork()` with PML4 deep copy
- `clone()` with flags: CLONE_VM, CLONE_FILES, CLONE_SETTLS, CLONE_PARENT_SETTID, CLONE_CHILD_CLEARTID
- `wait4()` with wstatus, WNOHANG, WUNTRACED
- `execve()` — ELF reload with SysV ABI stack
- Per-process FD table: `FdType::{Tty, File, Socket, Pipe, Epoll, PtyMaster, PtySlave}`
- Process states: Ready, Running, Blocked, Terminated
- Priority-based preemptive scheduling
- **Signal infrastructure** ✅ (this session):
  - Per-process signal handler table (`signal_handlers: [u64; 32]`)
  - Per-process signal mask (`signal_mask: u64`)
  - Pending signal bitmap (`pending_signals: u64`)
  - `send_signal(pid, signum)` — respects mask, SIG_IGN, default actions
  - `send_signal_to_pgrp(pgid, signum)` — for PTY Ctrl+C etc.
  - `dequeue_signal(pid)` — returns (signum, handler) for delivery
  - `has_pending_signals(pid)` — poll for pending deliverable signals

**Remaining**:
- [ ] `clone3` (full struct `clone_args` support)
- [ ] Process groups (setpgid/getpgid) — real implementation
- [ ] Session management (setsid/getsid) — beyond stub
- [ ] Job control (SIGTSTP/SIGCONT/SIGTTIN/SIGTTOU)
- [ ] Full signal frame delivery (push sigframe to user stack, iretq to handler)
- [ ] `/proc/[pid]/` per-process directories

### Block G — VirtIO-blk + ext4

**Implemented**:
- VirtIO legacy block driver (PCI 0x1AF4:0x1001)
- Single virtqueue request/response
- Sector read/write (512 bytes)
- FAT32: directory traversal, file read/write, cluster chain

**Remaining**:
- [ ] ext4 superblock parsing
- [ ] ext4 inode table lookup / extent tree
- [ ] ext4 journal (JBD2) write support
- [ ] Block cache (LRU, 64 entries)

---

## Application Blocks

### Block J — Native Python 3
- [ ] Download CPython 3.x static build (musl)
- [ ] Map into VFS at `/usr/bin/python3`
- [ ] `/usr/lib/python3.x/` standard library

### Block K — Native GCC Toolchain
- [ ] Download musl-cross-make (x86_64-linux-musl-gcc)
- [ ] Extract binutils + gcc + musl-libc
- [ ] Test: compile → execute on AetherionOS

### Block L — Node.js
- [ ] Download Node.js static build (musl/Alpine)
- [ ] Requires: libuv (epoll, inotify, timerfd, eventfd)
- [ ] Map into VFS

### Block P — Applications

**Working**:
- ✅ BusyBox (ash shell, coreutils) — boots and runs

**Target**:
- [ ] Alpine apk package manager
- [ ] Python 3 REPL
- [ ] GCC: compile and run C programs
- [ ] Node.js: run JavaScript
- [ ] Simple text editor (vi/nano)
- [ ] Web browser (links/lynx, text-mode)

---

## Development Workflow

### Build & Test Cycle
```
cargo check -p aetherion-kernel --lib                     # Fast type check
cargo check -p aetherion-kernel --lib --features limine   # With bootloader
cargo build -p aetherion-kernel --lib -Z build-std        # Full build
qemu-system-x86_64 -kernel kernel.elf -serial stdio       # Run
```

### CI/CD Pipeline (.github/workflows/build.yml)
1. Checkout + Rust nightly (nightly-2026-04-21)
2. Download BusyBox (fallback to minimal stub)
3. `cargo check` with default + limine features
4. Verify 0 errors

### Milestone Testing
After each major milestone:
1. `cargo check` — 0 errors
2. `cargo clippy` — review all warnings
3. QEMU boot test: BusyBox `ls`, `cat`, `echo`
4. Fork/exec test: spawn child, wait, check exit code
5. Signal test: Ctrl+C kills foreground process via PTY
6. Network test: DNS lookup, TCP connect
7. FAT32 test: write file, read back, verify
8. Dynamic linking test: run musl-linked binary via PT_INTERP

---

## Architecture Diagram

```
┌─────────────────────────────────────────────────────┐
│                  Applications (P)                    │
│     BusyBox  Python  GCC  Node.js  Browser          │
├─────────────────────────────────────────────────────┤
│              Package Manager (H)                     │
│                APK / Alpine rootfs                   │
├───────────┬───────────┬─────────────────────────────┤
│  PTY (I)  │ Linker(F) │     Compositor (N)           │
│ /dev/ptmx │ ld.so     │     DRM/KMS/Wayland          │
│ line disc │ PT_INTERP │     VirtIO-GPU                │
├───────────┴───────────┴─────────────────────────────┤
│            Linux Syscall Layer (A)                   │
│          230+ syscalls, per-process signals          │
├───────────┬───────────┬─────────────────────────────┤
│Process (D)│Memory (B) │      VFS (C)                 │
│fork/clone │mmap/brk   │ procfs/devfs/sysfs           │
│signals    │mprotect   │ ext4/FAT32/ramfs             │
│FD table   │VMA track  │ mount table                  │
├───────────┴───────────┴─────────────────────────────┤
│            Network Stack (E)                         │
│      TCP/IP + DNS + ARP + ICMP + VirtIO-Net          │
├───────────┬───────────┬─────────────────────────────┤
│VirtIO (G) │ GPU (O)   │    USB/NVMe (O)              │
│blk+FAT32  │ virtio-gpu│    xHCI/AHCI                 │
├───────────┴───────────┴─────────────────────────────┤
│           Scheduler + Context Switch                 │
│      Preemptive, priority-based, SMP-ready           │
│      XSAVE/XRSTOR for FPU/SSE/AVX (planned)         │
├─────────────────────────────────────────────────────┤
│           x86_64 Hardware Abstraction                │
│     GDT/IDT/APIC/PIC/PIT/ACPI/PCI/IOAPIC            │
└─────────────────────────────────────────────────────┘
```

---

## PR #46 Session Summary

### New code added (this session):
| File | Lines | Description |
|------|-------|-------------|
| `kernel/src/drivers/pty.rs` | 723 | Full PTY/TTY subsystem |
| `kernel/src/fs/vfs_backend.rs` | 625 | Trait-based multi-backend VFS |
| `kernel/src/elf/mod.rs` | +139 | Dynamic linker PT_INTERP flow |
| `kernel/src/process/mod.rs` | +219 | Signal delivery infrastructure |
| `kernel/src/arch/x86_64/syscall.rs` | +98 | PTY FD dispatch in read/write/epoll |
| `kernel/src/compat/linux_abi.rs` | +207 | Per-process signals, PTY routing, syscall stubs |
| `kernel/src/process/task.rs` | +40 | PtyMaster/PtySlave FD types, signal fields |
| **Total** | **~2,051** | **New lines in this PR** |

### Key achievements:
1. ✅ Full PTY subsystem with line discipline and signal generation
2. ✅ Dynamic linker: PT_INTERP → load interpreter → AuxV → jump to ld.so
3. ✅ Per-process signal handlers, masks, and delivery pipeline
4. ✅ VFS multi-backend with procfs, devfs, sysfs
5. ✅ Purged all 136-byte stub binaries
6. ✅ Cargo check passes with 0 errors

---

*Generated: 2026-04-26 | AetherionOS Kernel v0.10.0 | 39,687+ lines of Rust*
