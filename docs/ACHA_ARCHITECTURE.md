# ACHA Architecture -- Aetherion Cognitive Hierarchical Architecture

*Technical Design Document for AetherionOS v2.0.0*

---

## 1. Overview

AetherionOS implements the **Aetherion Cognitive Hierarchical Architecture (ACHA)**,
a security model where every user-space entity is an **agent** embedded in a strict
hierarchy. Agents do not interact with hardware directly; instead they publish
**Intents** (structured IPC messages) onto the **Cognitive Bus**, where a Ring 0
**Verifier** decides whether each Intent is permitted before the kernel executes it.

This design enforces two invariants:

1. **Least Privilege**: a Worker cannot elevate beyond its role without the
   Matriarch's approval.
2. **Auditability**: every action taken by an agent is logged as an Intent,
   enabling post-hoc analysis and ML-based anomaly detection.

---

## 2. The Matriarchal Hierarchy

### 2.1 Roles

Each process carries an `AgentRole` assigned at creation and stored in
`kernel/src/process/task.rs`:

```rust
pub enum AgentRole {
    Matriarch,       // PID 1 -- system-wide authority
    SubMatriarch,    // Domain controllers (network, storage, GPU)
    Worker,          // Regular user applications and AI agents
    KernelThread,    // In-kernel services (timer, IRQ bottom-halves)
}
```

| Role | Base Priority | Capabilities |
|------|---------------|-------------|
| `Matriarch` | 20 | Spawn SubMatriarchs, set global policy, kill any process |
| `SubMatriarch` | 15 | Manage Workers within its domain, publish high-priority Intents |
| `Worker` | 5 | Execute user code, publish low-priority Intents, cannot spawn SubMatriarchs |
| `KernelThread` | 25 | Run in Ring 0, no IRETQ needed, highest scheduling priority |

### 2.2 Hierarchy Enforcement

`kernel/src/process/mod.rs` enforces:

- Only a `Matriarch` can spawn a `SubMatriarch`.
- Only a `Matriarch` or `SubMatriarch` can spawn a `Worker` with elevated
  privileges.
- The `Matriarch` is a singleton: `has_matriarch()` prevents a second from being
  created.
- Parent-child relationships are tracked in `Process.children: Vec<u64>`.
- `fork_process()` copies the parent's `uid`, `gid`, `AgentRole`, and `FdTable`
  to the child.

### 2.3 Process Lifecycle

```
  spawn_userspace(name, ppid, entry, stack, pml4)
       |
       v
    [Ready] --schedule--> [Running] --sys_exit--> [Terminated]
       ^                      |
       |                      | sys_wait / sys_fork
       +--- [Blocked] <------+
```

Key transitions:

- `sys_fork()`: deep-copies the parent's PML4 (all user pages), creates a child
  in `Ready` state with `is_forked = true`.
- `sys_wait()`: blocks the parent, launches the child via `sysretq` (forked) or
  `iretq` (thread).
- `sys_exit()`: marks the process `Terminated`, resumes the blocked parent with
  the wait result.

---

## 3. The Cognitive Bus (Couche 3)

### 3.1 Design

The Cognitive Bus is a lock-free MPMC (Multiple-Producer, Multiple-Consumer)
ring buffer implemented in `kernel/src/ipc/bus.rs`. Agents publish **IntentMessages**:

```rust
pub struct IntentMessage {
    pub sender_pid: u64,
    pub intent_type: u64,    // e.g., 0x1000 = FILE_READ, 0x6000 = BUS_PUBLISH
    pub priority: u8,
    pub data: u64,
    pub timestamp: u64,
}
```

### 3.2 Intent Types

| Code | Name | Description |
|------|------|-------------|
| `0x1000` | `FILE_READ` | Read a file from VFS |
| `0x1001` | `FILE_WRITE` | Write to a file |
| `0x2000` | `NET_CONNECT` | Open a TCP connection |
| `0x3000` | `PROCESS_SPAWN` | Create a new child process |
| `0x4000` | `GPU_DRAW` | Submit a framebuffer draw command |
| `0x5000` | `ML_INFER` | Request ML inference |
| `0x6000` | `BUS_PUBLISH` | Generic data publication |

### 3.3 Flow

```
  [Worker]  --publish Intent-->  [Cognitive Bus]
                                      |
                                      v
                              [Verifier (Ring 0)]
                              /                \
                          ALLOW              DENY
                            |                  |
                            v                  v
                     [Kernel executes]   [EPERM returned]
```

---

## 4. The Verifier (Couche 5)

### 4.1 Purpose

The Verifier sits between the Cognitive Bus and the kernel's execution layer. It
is a **policy engine** implemented in `kernel/src/security/mod.rs` that evaluates
every Intent against a set of rules before allowing execution.

### 4.2 Policy Rules

The current policy set includes:

1. **Role Check**: Workers cannot publish `PROCESS_SPAWN` Intents.
2. **UID/GID Check**: Only `uid=0` processes can access `/sys/` VFS paths.
3. **Rate Limiting**: No process may publish more than 1000 Intents per scheduling
   quantum.
4. **Hierarchy Check**: A Worker cannot kill a SubMatriarch or Matriarch.
5. **Capability Check**: Network syscalls require a `NET_CAPABLE` flag.

### 4.3 Audit Trail

Every Intent evaluation is logged to the serial console with the format:
```
[VERIFIER] PID {pid} intent={type:#x} prio={prio} -> ALLOW/DENY
```

This provides a complete audit trail for forensic analysis.

---

## 5. Memory Architecture

### 5.1 Virtual Address Space Layout

```
0x0000_0000_0000_0000  +---------------------+
                       |  Kernel code/data   |  PML4[0] -- shared across all PML4s
0x0000_4444_4444_0000  |  Kernel Heap (8 MiB)|  PML4[136] -- shared (no USER_ACCESSIBLE)
                       +---------------------+
0x0000_4000_0000_0000  |  User mmap region   |  PML4[128] -- per-process, on-demand
                       +---------------------+
0x0000_5000_0000_0000  |  Framebuffer mmap   |  PML4[160] -- per-process
                       +---------------------+
0x0000_8000_0000_0000  |  ELF code + data    |  PML4[256+] -- per-process
                       |  User stack (1 MiB) |  Top at 0x7FFF_FFFF_F000
                       +---------------------+
0xFFFF_8000_0000_0000  |  Physical mem offset |  PML4[256..511] -- identity mapped
                       +---------------------+
```

### 5.2 Fork Deep Copy

`clone_pml4_deep()` in `kernel/src/arch/x86_64/syscall.rs`:

- PML4 entries **without** `USER_ACCESSIBLE` (bit 2) are shared verbatim --
  this includes PML4[0] (kernel code), PML4[136] (kernel heap), and
  PML4[256..511] (physical memory offset).
- PML4 entries **with** `USER_ACCESSIBLE` are deep-copied: new PDPT, PD, PT
  frames are allocated and each 4 KB user page is memcpy'd.
- This ensures the child has its own user-space pages but shares the kernel's
  data structures (PROCESS_TABLE, allocator state, etc.).

### 5.3 KPTI-lite

`create_user_pml4()` in `kernel/src/elf/mod.rs` copies kernel PML4 entries
**without** setting `USER_ACCESSIBLE`, preventing Ring 3 code from reading kernel
memory. This is a lightweight version of Kernel Page Table Isolation (KPTI).

---

## 6. Syscall Interface

The syscall entry point is configured via the LSTAR MSR and implemented as a
`#[naked]` function in `kernel/src/arch/x86_64/syscall.rs`. The calling convention
follows the Linux x86_64 ABI:

| Register | Purpose |
|----------|---------|
| RAX | Syscall number |
| RDI | Argument 1 |
| RSI | Argument 2 |
| RDX | Argument 3 |
| RCX | Saved RIP (by CPU) |
| R11 | Saved RFLAGS (by CPU) |
| RAX | Return value |

### Entry Sequence

1. `swapgs` -- switch to kernel GS (PER_CPU struct)
2. Save user RSP to `gs:[8]`, load kernel RSP from `gs:[0]`
3. Push callee-saved registers (r15..rcx)
4. Call `syscall_handler_rust(nr, a1, a2, a3)`
5. Pop registers, restore user RSP from `gs:[8]`
6. `swapgs`, `sysretq` back to Ring 3

---

## 7. Scheduling

The scheduler (`kernel/src/scheduler/mod.rs`) implements a Multi-Level Feedback
Queue (MLFQ) with 4 priority levels:

| Queue | Name | Base Priority | Agents |
|-------|------|---------------|--------|
| 3 | HIGH | 20-25 | Matriarch, KernelThreads |
| 2 | NORMAL | 10-19 | SubMatriarchs |
| 1 | LOW | 5-9 | Workers |
| 0 | IDLE | 0-4 | Background tasks |

**Aging**: processes waiting more than `AGING_THRESHOLD` ticks in a lower queue
are promoted to the next higher queue, preventing starvation.

---

## 8. Future: Cognitive Learning Loop

The long-term vision integrates on-device ML inference into the scheduling and
security decisions:

```
  [Scheduler] --telemetry--> [ML Agent (Ring 3)]
       ^                            |
       |                            v
       +--- policy update <--- [Cognitive Bus]
```

The ML agent observes process behavior (CPU cycles, memory usage, Intent patterns)
and publishes scheduling hints back through the Cognitive Bus. The Verifier
validates these hints before the Scheduler applies them.

---

*Document version: 2.0.0 -- 2026-03-05*
*Author: MORNINGSTAR*
