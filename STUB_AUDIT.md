# AetherionOS Linux ABI Stub Audit

**Date**: May 9, 2026  
**Status**: CRITICAL - Systematic stub tracking for Python3/Musl/GCC compatibility  
**Methodology**: Static analysis + dynamic syscall tracing

---

## Executive Summary

The Linux ABI layer (kernel/src/compat/linux_abi.rs) contains **4 categories of stubs** that silently fail or return fake data. These stubs are the root cause of Python3 execution failures and will block Musl/GCC/BusyBox.

**Critical Finding**: The previous Python3 failure was NOT a filesystem bug, but a **"vicieux" stub** (linux_fcntl returning fd+100 as fake FD). This pattern repeats throughout the codebase.

---

## Stub Categories & Detection Methods

### Category A: "Honnête" Stubs (Honest - Return ENOSYS)

These stubs correctly signal failure. Programs can adapt.

**Detection**: `grep -n "ENOSYS\|(-38i64)" kernel/src/compat/linux_abi.rs`

**Found**:
- `linux_rseq()` - Line 450 - Returns -ENOSYS ✓ (Correct)
- `linux_epoll_create1()` - Line 945 - Returns -ENOSYS ✓ (Correct)
- `linux_epoll_create()` - Line 960 - Returns -ENOSYS ✓ (Correct)
- `linux_pselect6()` - Line 1047 - Returns -ENOSYS ✓ (Correct)

**Status**: ✅ SAFE - These are honest about their limitations.

---

### Category B: "Mensonger" Stubs (Liars - Return 0 without doing anything)

These are the MOST DANGEROUS. They claim success but do nothing.

**Detection**: `grep -B2 "{ 0 }$" kernel/src/compat/linux_abi.rs | grep "pub fn linux_"`

**Found** (12 critical stubs):

| Line | Function | Arguments Ignored | Impact | Priority |
|------|----------|-------------------|--------|----------|
| 443 | `linux_set_robust_list()` | `_head, _len` | Mutex robustness broken | MEDIUM |
| 446 | `linux_get_robust_list()` | `_pid, _head, _len` | Mutex queries fail silently | MEDIUM |
| 472 | `linux_setuid()` | `_uid` | UID spoofing (security hole!) | **HIGH** |
| 474 | `linux_setgid()` | `_gid` | GID spoofing (security hole!) | **HIGH** |
| 476 | `linux_setreuid()` | `_ruid, _euid` | UID/EUID spoofing | **HIGH** |
| 478 | `linux_setregid()` | `_rgid, _egid` | GID/EGID spoofing | **HIGH** |
| 480 | `linux_setresuid()` | `_ruid, _euid, _suid` | UID/EUID/SUID spoofing | **HIGH** |
| 482 | `linux_setresgid()` | `_rgid, _egid, _sgid` | GID/EGID/SGID spoofing | **HIGH** |
| 504 | `linux_setgroups()` | `_size, _list` | Group membership broken | MEDIUM |
| 524 | `linux_capset()` | `_hdr, _data` | Capability setting ignored | MEDIUM |
| 679 | `linux_munmap()` | `_addr, _len` | **MEMORY LEAK** - Pages never freed | **CRITICAL** |
| 3131 | `linux_rename()` | `_oldpath, _newpath` | File rename broken | MEDIUM |

**Status**: 🔴 CRITICAL - These will cause silent data corruption and memory leaks.

---

### Category C: "Vicieux" Stubs (Tricky - Return calculated fake values)

These are INSIDIOUS. They return plausible-looking values that mask the real problem.

**Detection**: Look for functions that ignore path/fd arguments but return computed values.

#### C1: Path-Ignoring Stubs (Delegating to wrong FD)

**Found**:

```rust
// Line 782-783: STUB - Ignores _path, always uses FD 0
pub fn linux_stat(_path: u64, buf: u64) -> u64 { linux_fstat(0, buf) }
pub fn linux_lstat(_path: u64, buf: u64) -> u64 { linux_fstat(0, buf) }
```

**Impact**: 
- `stat("/etc/passwd")` returns metadata of FD 0 (stdin)
- `stat("/usr/lib/python3.12")` returns fake data
- Python3 importlib caches wrong directory listings

**Status**: 🔴 CRITICAL - Breaks all path-based file operations.

#### C2: DirFD-Ignoring Stubs

**Found**:

```rust
// Line 786: STUB - Ignores _dirfd, always uses absolute path
pub fn linux_newfstatat(_dirfd: u64, _path: u64, buf: u64, _flag: u64) -> u64 {
    linux_fstat(0, buf)  // WRONG: should use dirfd + relative path
}

// Line 823: STUB - Ignores _dirfd for relative path resolution
pub fn linux_faccessat(_dirfd: u64, path_addr: u64, mode: u64, _flags: u64) -> u64 {
    // Should resolve path relative to dirfd, but ignores dirfd
}

// Line 4427: STUB - Ignores _dirfd
pub fn linux_newfstatat_vfs(_dirfd: u64, path: u64, buf: u64, _flag: u64) -> u64 {
    // Should use dirfd for relative path resolution
}

// Line 4442: STUB - Ignores _dirfd
pub fn linux_statx(_dirfd: u64, pathname: u64, _flags: u64, _mask: u64, statxbuf: u64) -> u64 {
    // Should use dirfd for relative path resolution
}
```

**Impact**:
- `openat(3, "encodings/__init__.py", ...)` fails because dirfd=3 is ignored
- Python3 cannot find encodings directory
- Musl's dynamic linker cannot resolve relative paths

**Status**: 🔴 CRITICAL - Breaks Python3 and all dirfd-based syscalls.

#### C3: The Original Python3 Killer (linux_fcntl)

**Found** (Line 828):

```rust
pub fn linux_fcntl(fd: u64, cmd: u64, _arg: u64) -> u64 {
    match cmd {
        // ...
        F_DUPFD | F_DUPFD_CLOEXEC => {
            // STUB: Returns fd + 100 as fake new FD
            // This FD is NOT in the FD table!
            fd + 100
        }
        // ...
    }
}
```

**Impact** (EXACT SEQUENCE THAT KILLED PYTHON3):
1. Python opens `/usr/lib/python3.12/encodings/__init__.py` → FD 3 ✓
2. Python calls `fcntl(3, F_DUPFD_CLOEXEC, 0)` → Returns FD 103 (fake!)
3. Python tries `read(103, buf, 1024)` → 0 bytes (FD 103 doesn't exist)
4. Python thinks file is empty → partial module init
5. Tries to import 'aliases' → ImportError (circular import illusion)

**Status**: 🔴 CRITICAL - This is THE bug that was masked as a filesystem issue.

---

### Category D: "Dangereux par délégation" (Dangerous by delegation)

Functions that delegate to wrong implementations, ignoring critical arguments.

**Found**:

```rust
// Line 514: STUB - Ignores _hdr, always returns fake capabilities
pub fn linux_capget(_hdr: u64, data: u64) -> u64 {
    // Should read capabilities from _hdr, but ignores it
}

// Line 700: STUB - Ignores _dirfd
pub fn linux_readlinkat(_dirfd: u64, path: u64, buf: u64, bufsiz: u64) -> u64 {
    // Should use dirfd for relative path resolution
}

// Line 705: STUB - Ignores _fd, cmd, arg
pub fn linux_ioctl(_fd: u64, cmd: u64, arg: u64) -> u64 {
    // Returns -ENOTTY for all ioctl calls
}

// Line 3397: STUB - Ignores _dirfd
pub fn linux_mkdirat(_dirfd: u64, pathname: u64, mode: u64) -> u64 {
    // Should use dirfd for relative path resolution
}

// Line 3402: STUB - Ignores _dirfd
pub fn linux_unlinkat(_dirfd: u64, pathname: u64, flags: u64) -> u64 {
    // Should use dirfd for relative path resolution
}

// Line 3411: STUB - Ignores _path
pub fn linux_statfs(_path: u64, buf: u64) -> u64 {
    // Should stat the filesystem at _path
}

// Line 3437: STUB - Ignores _fd
pub fn linux_fstatfs(_fd: u64, buf: u64) -> u64 {
    // Should stat the filesystem of FD _fd
}

// Line 3442: STUB - Ignores _pid
pub fn linux_sched_getparam(_pid: u64, param: u64) -> u64 {
    // Should get scheduling params for PID _pid
}

// Line 3452: STUB - Ignores _pid
pub fn linux_sched_getaffinity(_pid: u64, cpusetsize: u64, mask: u64) -> u64 {
    // Should get CPU affinity for PID _pid
}

// Line 3481: STUB - Ignores _clockid
pub fn linux_clock_getres(_clockid: u64, tp: u64) -> u64 {
    // Should return resolution for _clockid
}

// Line 3492: STUB - Ignores _resource
pub fn linux_getrlimit(_resource: u64, rlim: u64) -> u64 {
    // Should get limit for _resource
}

// Line 3559: STUB - Ignores _which
pub fn linux_getitimer(_which: u64, curr_value: u64) -> u64 {
    // Should get timer for _which
}

// Line 4604: STUB - Ignores _fd_in, _off_in, _fd_out, _off_out
pub fn linux_copy_file_range(_fd_in: u64, _off_in: u64, _fd_out: u64, _off_out: u64) -> u64 {
    // Should copy between FDs with offsets
}

// Line 4724: STUB - Ignores _who
pub fn linux_getrusage(_who: u64, usage_buf: u64) -> u64 {
    // Should get resource usage for _who
}
```

**Status**: 🟡 MEDIUM - These will cause issues when programs query system state.

---

## Syscall Tracing Implementation

To catch stubs dynamically, add this to `kernel/src/arch/x86_64/syscall.rs`:

```rust
// Add at the start of syscall_entry (before routing)
#[cfg(feature = "syscall_trace")]
{
    crate::serial_println!(
        "[SYSCALL-TRACE] PID={} nr={} a1=0x{:X} a2=0x{:X} a3=0x{:X}",
        crate::scheduler::current_pid(),
        rax,
        rdi, rsi, rdx
    );
}

// Add after syscall returns (before SYSRET)
#[cfg(feature = "syscall_trace")]
{
    crate::serial_println!(
        "[SYSCALL-TRACE] PID={} nr={} result=0x{:X}",
        crate::scheduler::current_pid(),
        rax, result
    );
}
```

Enable with: `cargo build --features syscall_trace`

---

## Priority Fix List

### 🔴 CRITICAL (Block Python3/Musl/GCC)

1. **linux_fcntl** (Line 828) - F_DUPFD/F_DUPFD_CLOEXEC must return real FD
2. **linux_stat/linux_lstat** (Lines 782-783) - Must read actual file metadata
3. **linux_newfstatat** (Line 786) - Must use dirfd for relative paths
4. **linux_statx** (Line 4442) - Must use dirfd for relative paths
5. **linux_munmap** (Line 679) - Must actually free memory
6. **linux_faccessat** (Line 823) - Must use dirfd for relative paths

### 🟡 HIGH (Security/Functionality)

7. **linux_setuid/setgid/setreuid/setregid/setresuid/setresgid** (Lines 472-482) - UID/GID spoofing
8. **linux_readlinkat** (Line 700) - Must use dirfd
9. **linux_mkdirat** (Line 3397) - Must use dirfd
10. **linux_unlinkat** (Line 3402) - Must use dirfd

### 🟠 MEDIUM (System State Queries)

11. **linux_set_robust_list/get_robust_list** (Lines 443-446) - Mutex robustness
12. **linux_setgroups** (Line 504) - Group membership
13. **linux_capget/capset** (Lines 514, 524) - Capabilities
14. **linux_statfs/fstatfs** (Lines 3411, 3437) - Filesystem stats
15. **linux_sched_getparam/getaffinity** (Lines 3442, 3452) - Scheduler queries
16. **linux_clock_getres** (Line 3481) - Clock resolution
17. **linux_getrlimit** (Line 3492) - Resource limits
18. **linux_getitimer** (Line 3559) - Timer queries
19. **linux_getrusage** (Line 4724) - Resource usage
20. **linux_copy_file_range** (Line 4604) - File copying

---

## Next Steps

1. **Immediate**: Fix the 6 CRITICAL stubs (especially linux_fcntl, linux_stat, linux_newfstatat)
2. **Short-term**: Implement syscall tracing to catch remaining stubs dynamically
3. **Ongoing**: Add unit tests for each syscall to verify they actually work
4. **Long-term**: Audit all 300+ syscalls for similar patterns

---

## Files to Modify

- `kernel/src/compat/linux_abi.rs` - Main stub implementations
- `kernel/src/arch/x86_64/syscall.rs` - Add syscall tracing
- `kernel/src/fs/ext2.rs` - Verify path resolution
- `kernel/src/fs/vfs.rs` - Verify dirfd handling

---

## Verification Checklist

- [ ] linux_fcntl returns real FD in FD table
- [ ] linux_stat reads actual file metadata
- [ ] linux_newfstatat uses dirfd for relative paths
- [ ] linux_statx uses dirfd for relative paths
- [ ] linux_munmap actually frees memory
- [ ] linux_faccessat uses dirfd for relative paths
- [ ] Syscall tracing enabled and logging all calls
- [ ] Python3 successfully executes print(42*42)
- [ ] Musl dynamic linker can resolve relative paths
- [ ] GCC can compile code

