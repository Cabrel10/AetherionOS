# AetherionOS Status & Blockers -- Phase 6 (Session 7)

## Status: CI GREEN + Limine Boot + Timer IRQ + ELF Loader + Exec Ready

### Verified (2026-04-23, Session 7)
- [x] CI: All 4 jobs green (Kernel Check, Build C Userspace Apps, Build Rust Agents, Build Kernel + Limine ISO)
- [x] Kernel: 0 errors, 0 warnings (both `--features limine` and default)
- [x] Boot: Limine v8.7.0, base revision 3, HHDM at 0xFFFF800000000000
- [x] ISO: ~100 MiB -- full Alpine rootfs + Python 3 + BusyBox + 33 agents
- [x] QEMU boot: reaches `$` prompt, all init steps pass
- [x] **#GP(0x30) FIX**: Segment registers reloaded after GDT init (SS=DS=ES=0x10, FS=GS=0)
- [x] **Timer interrupts**: ENABLED -- `uptime` shows >0 ticks
- [x] Memory: `init_from_limine()` -- bitmap frame allocator, OffsetPageTable, 64 MiB heap
- [x] **ELF frame pool**: 8192 frames (32 MiB) initialized in Limine path
- [x] **KPTI trampolines**: IRETQ trampoline + SYSRET trampoline + LSTAR relocation
- [x] **ELF binaries in VFS**: /bin/hello.elf, /bin/hello_c.elf, /bin/sh.elf mounted
- [x] **load_elf() fixed**: Uses `spawn_userspace()` with correct entry_point/stack_pointer/pml4
- [x] Scheduler: PriorityScheduler with 5 queues, anti-starvation aging, SMP-aware
- [x] VFS: BTreeMap hierarchy, device manifests, security checks
- [x] IPC: Cognitive Bus (priority-aware BinaryHeap, 1024 capacity)
- [x] Framebuffer: Limine GOP 1280x800 @ 32bpp
- [x] Network: `net::init()` in boot path, PCI scan for VirtIO-Net
- [x] Interactive shell: help, uname, free, ps, uptime, heap, bus, net, exec, ping, wget, clear, halt
- [x] Syscalls: 13 core Linux syscalls mapped (read, write, open, close, mmap, munmap, brk, ioctl, execve, exit, uname, getcwd, chdir)

### Session 7 Changes (This Commit)
1. **ELF frame pool init**: `elf::init_frame_pool()` called after heap init (8192 frames = 32 MiB)
   - Without this, `load_elf_binary()` cannot allocate frames for per-process page tables
2. **KPTI trampoline init**: `init_global_iretq_trampoline()`, `init_global_sysret_trampoline()`, `relocate_lstar_for_kpti()`
   - Required for the timer ISR to context-switch to Ring 3 processes via `exec_switch_cr3_and_ring3()`
   - LSTAR points syscall entry to the HHDM-mapped address (safe under user CR3)
3. **ELF binaries mounted in VFS**: hello.elf (8496 B), hello_c.elf (16552 B), sh.elf into /bin/
   - Previously only mounted in the bootloader_api path (main.rs), not in the Limine path
4. **load_elf() bug fix**: Changed `spawn_kernel_thread()` to `spawn_userspace()` 
   - `spawn_kernel_thread()` created processes with `entry_point=0, stack_pointer=0`
   - Scheduler's `tick_preemptive()` skipped these because `get_entry_state()` returned entry=0
   - `spawn_userspace()` correctly sets entry_point, stack_pointer, pml4_phys, saved_user_rip/rsp
5. **ps command improved**: Shows all active processes from process table
6. **Version bump**: v4.2.0-phase6-exec

### Previous Session Changes
- **#GP(0x30) root-cause fix**: Reload SS=DS=ES=0x10, FS=GS=0 after `gdt::init()` in limine_entry.rs
- **Interrupts re-enabled**: Timer + keyboard IRQs active after all subsystems init
- **`exec <path>` shell command**: Loads ELF from VFS via `elf::load_elf()`, creates PID, enqueues in scheduler
- **Network initialization**: `net::init()` called in boot path, PCI scan for VirtIO-Net
- **Shell commands**: ping, wget (stub), net added
- **Syscall verification**: All 13 required BusyBox syscalls confirmed in dispatch table

### Resolved Blockers
- **B-GP**: #GP(0x30) on timer iretq -- FIXED (segment register reload)
- **B-REV**: Limine base revision mismatch -- FIXED (`BaseRevision::with_revision(3)`)
- **B-OPEN**: `agent_saga.c` open() 3-arg vs 2-arg -- FIXED
- **B-ASCII**: `agent_autonomous` non-ASCII byte literal -- FIXED
- **B-IRQ**: Timer interrupts disabled -- FIXED (Phase 6)
- **B1**: ELF frame pool not initialized in Limine path -- FIXED (Session 7)
- **B-SPAWN**: load_elf() used spawn_kernel_thread (entry=0) -- FIXED (Session 7, spawn_userspace)

### Remaining Blockers / TODO
- **B2**: Most Rust agents are 136-byte stubs (CI SDK/linker issue)
- **B3**: LLM model not embedded in ISO
- **B4**: TLS not functional (tls_bridge.c placeholder)
- **B5**: VirtIO-Net requires QEMU `-device virtio-net-pci` flag
- **B6**: HTTP GET (wget) requires TCP state machine completion
- **B7**: BusyBox `sh` requires full terminal emulation (stdin/stdout FD plumbing)
- **B8**: `exec /bin/hello.elf` needs QEMU test verification (process context switch untested)

### Architecture Summary
- **Kernel ELF**: ~616 KiB, entry 0xffffffff80004ac0
- **ISO**: ~100 MiB (kernel + Alpine rootfs + BusyBox 1.1 MiB + Python 3)
- **Boot**: Limine v8.7.0 (v8.x-binary), base revision 3
- **Memory**: HHDM 0xFFFF800000000000, 2045 MiB usable, bitmap allocator (16 GB max)
- **Heap**: 64 MiB at 0x4444_4444_0000, linked_list_allocator
- **Scheduler**: 5-priority queues, anti-starvation aging (100 tick threshold), SMP CPU affinity
- **Syscalls**: 110+ Linux syscalls dispatched (13 core + stubs)
- **Network**: VirtIO-Net, Ethernet/ARP/IPv4/ICMP/UDP/TCP stack

### LLM Bare-Metal Roadmap
1. **Path A (burn crate)**: `burn` + `burn-ndarray` backend, SmolLM-135M, ~30-50 tok/s on x86-64
2. **Path B (llama.cpp)**: Static musl binary, Linuxulator, SmolLM2-135M-Instruct GGUF ~100 MiB
3. **Benchmarks**: 15 tok/s RPi4, 30-80 tok/s modern x86-64 (arXiv 2511.07425)

### Invariants
- `open()` in C apps: 2 args (path, flags) -- no mode parameter
- Rust byte strings: ASCII only
- No `std` in kernel
- Limine base revision 3
- Segment registers reloaded after every GDT init (BSP + AP)
