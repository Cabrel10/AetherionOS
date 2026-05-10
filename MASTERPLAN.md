# AetherionOS MASTERPLAN v5

> "Un OS qui ne peut pas penser est un OS mort-ne."

## Vision

AetherionOS is a sovereign operating system capable of running AGI.
It boots bare-metal on x86_64, mounts a real filesystem, executes real Linux
binaries through a POSIX-compatible Linuxulator, connects to the Internet,
and runs LLM inference natively in Ring 3.

---

## PHASE 1 — Runtime Stability (Python & Dev Tools)

**Objective:** The OS proves it can execute complex runtimes with dynamic
library loading (.so), heap allocation, and full POSIX syscall coverage.

**Validation Criteria:**
- `python3 -c "print(42*42)"` prints `1764` from Alpine ext2 disk
- `node -v` prints a version string
- `gcc -o hello hello.c && ./hello` compiles and runs a C program
- BusyBox interactive shell works (already proven)

**Key Subsystems:**
- ext2 driver (read-only, 784 lines) — DONE
- Dynamic linker (ld-musl-x86_64.so.1) — DONE (KPTI-safe mmap, /proc/self/maps)
- getdents64 (syscall 217) — DONE
- stat/fstat with ext2 fallback — DONE
- mprotect with NX/EXEC flag — DONE
- VMA executable flag in page fault handler — DONE

**Status:** IN PROGRESS — Python3 launches but may hit missing syscalls during
libpython3.12.so initialization.

---

## PHASE 2 — Sovereign Networking (APK Ecosystem)

**Objective:** The OS can enrich itself from the Internet autonomously.

**Validation Criteria:**
- `wget http://example.com/` downloads HTML successfully
- `apk update` fetches APKINDEX from Alpine CDN
- `apk add curl` installs a real package
- DNS resolution works (proven in CI)
- TCP handles slow responses with RDTSC-based timeouts

**Key Subsystems:**
- HTTP/1.1 client with redirect support — DONE
- TCP stack with retransmit — DONE
- DNS resolver — DONE
- VirtIO-Net driver — DONE
- TLS 1.3 client (X25519 + AES-128-GCM + SHA-256) — DONE
- HTTPS wget with automatic SNI from DNS cache — DONE
- HTTP→HTTPS redirect following — DONE

**Status:** ACTIVE — HTTP proven in CI, HTTPS handshake implemented and wired.

---

## PHASE 3 — Bare-Metal LLM Inference (The Brain)

**Objective:** The autonomous agent thinks for real, not simulated.

**Validation Criteria:**
- `agent_inference.elf` loads `smollm2-135m-q4_0.gguf` via zero-copy mmap
- AVX2 matmul computes the forward pass
- The model generates a coherent response to "What is the capital of France?"
- Token generation is printed to serial console

**Key Subsystems:**
- GGUF parser (header, tensors, metadata) — DONE (agent_gguf)
- Q4_0 dequantization — DONE (kernel benchmark)
- Zero-copy mmap with demand paging — DONE
- SMP parallel matmul — DONE (sys_spawn_thread_on_core)
- Tokenizer (BPE) — TODO
- Full forward pass (attention, FFN, softmax, sampling) — TODO

**Status:** SCAFFOLDED — Kernel benchmark runs matmul, but no real model loaded yet.

---

## PHASE 4 — Agentic ReAct Loop (Self-Improvement)

**Objective:** The AI compiles a program, executes it, and observes the result.

**Validation Criteria:**
- LLM receives prompt: "Write a C program that prints Hello World"
- LLM generates C code, writes it to `/tmp/hello.c`
- Agent calls `gcc -o /tmp/hello /tmp/hello.c`
- Agent calls `/tmp/hello` and reads "Hello World" from stdout
- Agent reports success back to the LLM

**Key Subsystems:**
- LLM text generation API (Phase 3) — prerequisite
- Process spawning from agent (sys_execve) — DONE
- File creation (ext2 write or tmpfs) — TODO
- Stdout capture (pipe + read) — DONE (captured_by_pid)
- ReAct orchestration loop — TODO

**Status:** NOT STARTED — Requires Phase 3 completion.

---

## Architecture Summary

```
+----------------------------------------------------------+
|                    AetherionOS v4.3.0                     |
+----------------------------------------------------------+
|  Ring 3: Agents (Python, Node, GCC, LLM Inference)       |
|  +-----------+  +-----------+  +-------------------+     |
|  | BusyBox   |  | Python3   |  | agent_inference   |     |
|  | (Alpine)  |  | (Alpine)  |  | (GGUF + AVX2)     |     |
|  +-----------+  +-----------+  +-------------------+     |
+----------------------------------------------------------+
|  Linuxulator: POSIX Syscalls (300+ implemented)           |
|  execve, mmap, read, write, open, getdents64, stat,      |
|  fork, clone, pipe, socket, poll, futex, mprotect...      |
+----------------------------------------------------------+
|  Kernel: KPTI + Scheduler + VFS + ext2 + TCP/IP          |
|  +--------+  +--------+  +------+  +--------+            |
|  | Memory |  | Sched  |  | VFS  |  | Net    |            |
|  | (mmap) |  | (RR)   |  | +ext2|  | +TCP   |            |
|  +--------+  +--------+  +------+  +--------+            |
+----------------------------------------------------------+
|  Hardware: VirtIO-BLK, VirtIO-Net, Serial, Framebuffer   |
+----------------------------------------------------------+
```

---

## Current Commit

The codebase is on branch `genspark_ai_developer`.
Total kernel: ~22,000 lines of Rust. Userspace: ~30 agents.

## CI Test Matrix

| Test | Description | Status |
|------|-------------|--------|
| CI-TEST-1 | Ext2 mounted (15 entries) | ✅ PASS |
| CI-TEST-2 | Python3 lookup in ext2 | ⏸ SKIP |
| CI-TEST-3 | /etc/os-release read | ✅ PASS |
| CI-TEST-4 | /proc/self/maps | ✅ PASS |
| CI-TEST-5 | ICMP ping | ✅ PASS |
| CI-TEST-6 | HTTP wget example.com | ✅ PASS |
| CI-TEST-7 | MatMul benchmark | ℹ INFO |
| CI-TEST-8 | APKINDEX download | ✅ PASS |
| CI-TEST-9 | HTTPS wget example.com | 🔄 NEW |
| CI-TEST-10 | Python3 print(1764) | ⏸ PENDING |
