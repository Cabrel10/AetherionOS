# SESSION 12 LOG - P0 Fixes: /proc, sendfile, Ctrl+C SIGINT

## Date: 2026-04-27

## Changes Made

### 1. Fixed `cat /proc/version` (sendfile bypass)

**Root cause**: BusyBox `cat` uses `sendfile(stdout, fd, ...)` to copy file data.
Our Linux ABI had `sendfile` as a stub returning 0 (= success, 0 bytes = EOF).
BusyBox interprets 0 as "nothing to send" and exits without calling `read()`.

**Fix**: Implemented `linux_sendfile()` that returns `-EINVAL` for all files.
This forces BusyBox `cat` to fall back to the `read()/write()` loop, which
properly goes through `sys_read` where `/proc/*` dynamic content generation
is implemented.

**File**: `kernel/src/compat/linux_abi.rs`

### 2. Per-PID syscall logging

**Problem**: Global `LOG_COUNT` shared across all PIDs. After PID 1 exceeded
200 syscalls, child processes (PID 2+) had no logging, making debugging impossible.

**Fix**: Changed logging to always trace child processes (PID >= 2) regardless
of global counter. PID 1 still uses throttled logging.

**File**: `kernel/src/compat/linux_abi.rs` (function `linux_syscall_override`)

### 3. Ctrl+C (SIGINT) delivery via serial

**Problem**: `\x03` bytes received on serial COM1 (0x3F8) were passed directly to
the user buffer in `sys_read`, without being interpreted as terminal control
characters.

**Fix**: Added control character detection in the serial input path of `sys_read`:
- `\x03` (Ctrl+C) -> SIGINT (signal 2) + echo `^C` + return EINTR
- `\x1A` (Ctrl+Z) -> SIGTSTP (signal 20) + echo `^Z` + return EINTR  
- `\x1C` (Ctrl+\) -> SIGQUIT (signal 3) + echo `^\` + return EINTR

**File**: `kernel/src/arch/x86_64/syscall.rs` (function `sys_read`, TTY branch)

## Test Results (QEMU -nographic -serial mon:stdio)

### P0.1: `ls /`
```
bin   data  dev   etc   home  lib   proc  root  run   sys   tmp   usr   var
```
**PASS**

### P0.2: `cat /proc/version`
```
Linux version 6.18.0-aetherion (morningstar@aetherion.dev) (rustc 1.73.0-nightly, AetherionOS ACHA) #1 SMP PREEMPT_DYNAMIC 2026-04-12
```
**PASS**

### P0.3: `sh -c 'echo FORK_EXEC_WAIT_OK'`
```
FORK_EXEC_WAIT_OK
```
**PASS** (fork -> exec -> wait4 -> reap end-to-end)

### P0.4: Ctrl+C SIGINT
```
[SIGINT] Ctrl+C from serial -> SIGINT to PID 1
^C
AFTER_CTRL_C     <- shell resumed normally after signal
```
**PASS**

### Exit behavior
3x `exit_group(0)` observed (one per child process), all reaped correctly.

## Syscall trace for `cat /proc/version` (PID 2)

```
P2 #158 arch_prctl(0x1002, 0x713198) = 0x0        ; TLS setup
P2 #218 set_tid_address(0x713FD4) = 0x2            ; musl init
P2 #102 getuid() = 0x0                              ; root
P2 #2   open(0x7FFFFFFFEC04, 0x0) = FALLTHROUGH     ; -> sys_open('/proc/version') = fd 3
P2 #40  sendfile(1, 3, 0, 16M) = -22 (EINVAL)       ; ** NEW: forces read loop **
P2 #9   mmap(0, 64K, RW) = 0x400000008000           ; read buffer
P2 #0   read(3, buf, 64K) = FALLTHROUGH              ; -> sys_read('/proc/version') = 134 bytes
P2 #1   write(1, buf, 134) = FALLTHROUGH             ; -> output to serial
P2 #0   read(3, buf, 64K) = FALLTHROUGH              ; -> sys_read = 0 (EOF)
P2 #11  munmap(buf, 64K) = 0                         ; cleanup
P2 #3   close(3) = FALLTHROUGH                       ; close fd
P2 #231 exit_group(0)                                ; clean exit
```

## Current OS Status

| Feature                | Status |
|------------------------|--------|
| Boot (Limine x86_64)   | PASS   |
| Kernel heap + sched    | PASS   |
| BusyBox sh             | PASS   |
| `ls /`                 | PASS   |
| `cat /proc/version`    | PASS   |
| `sh -c 'echo ...'`    | PASS   |
| fork/exec/wait4/reap   | PASS   |
| Ctrl+C SIGINT          | PASS   |
| TCP receive (wget)     | NOT TESTED |
| Dynamic linker         | NOT TESTED |
| DNS/TLS/HTTP           | NOT STARTED |

## Files Modified
- `kernel/src/compat/linux_abi.rs` - sendfile impl, per-PID logging
- `kernel/src/arch/x86_64/syscall.rs` - Ctrl+C/Z/\ signal delivery in serial read
