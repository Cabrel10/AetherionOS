# Session Summary: Python3 Execution Debugging & Stub Audit

**Date**: May 9-10, 2026  
**Status**: In Progress - Systematic stub tracking and kernel page table fixes  
**Commits**: 3 major fixes pushed

---

## What We Accomplished

### 1. Fixed KPTI Trampoline CR3 Switch (Commit 8db7f5c)

**Problem**: The `exec_switch_cr3_and_ring3_naked` function was executing `mov cr3, rdi` followed by `iretq` directly in kernel code. After the CR3 switch, the kernel code page became inaccessible because it's not mapped in the user PML4, causing a page fault at the IRETQ instruction.

**Solution**: Use the global IRETQ trampoline (mapped in both kernel and user PML4 via PML4[256]) instead of executing CR3 switch directly in kernel code. The trampoline is in the phys_off region, which remains accessible after the CR3 switch.

**Changes**:
- Replaced `exec_switch_cr3_and_ring3_naked` with a call to the global IRETQ trampoline
- Set up registers (r8=PML4, r9=RSP, r10=RIP, r11=phys_off) before jumping to trampoline
- Removed unused `jump_to_ring3` function
- Updated main.rs to use `exec_switch_cr3_and_ring3` instead of manual CR3 switch

---

### 2. Added TLB/Cache Flush Before Kernel Page Verification (Commit ad69ace)

**Problem**: The CPU data cache was returning stale TLB-cached values when reading page table entries through the HHDM, hiding pages that were actually NOT mapped in physical memory.

**Solution**: Add CR3 reload (TLB flush) + clflush+mfence before reading each PT entry during the Step 5a verification. This ensures we read actual physical memory contents, not cached values from Limine's boot-time page walks.

**Changes**:
- Added CR3 reload at the start of verification to flush TLB
- Added clflush+mfence before each PT entry read
- Added mfence after clflush to ensure memory ordering

---

### 3. Added Linker Symbols for Kernel Bounds (Commit bf6f52d)

**Problem**: The kernel page table verification in Step 5a was using undefined linker symbols, causing the verification to use incorrect kernel bounds. This resulted in the verification reporting '1533 present, 0 fixed' even though pages in the .bss section were not actually mapped by Limine.

**Solution**: Added `__kernel_start` and `__kernel_end` markers in linker-x86_64.ld to properly define the kernel's virtual address range, allowing the verification code to correctly identify and force-map all kernel pages including the .bss section.

**Changes**:
- Added `__kernel_start = .;` at the beginning of SECTIONS
- Added `__kernel_end = .;` at the end of SECTIONS
- This allows the verification code to use the actual kernel bounds instead of guessing

---

## Comprehensive Stub Audit (STUB_AUDIT.md)

Created a systematic audit of all Linux ABI stubs in the codebase, categorizing them into 4 types:

### Category A: "Honnête" Stubs (Honest - Return ENOSYS)
- ✅ SAFE - These correctly signal failure
- Examples: `linux_rseq`, `linux_epoll_create1`, `linux_pselect6`

### Category B: "Mensonger" Stubs (Liars - Return 0 without doing anything)
- 🔴 CRITICAL - These claim success but do nothing
- 12 critical stubs found including:
  - `linux_munmap` - Memory leak (pages never freed)
  - `linux_setuid/setgid/setreuid/setregid/setresuid/setresgid` - UID/GID spoofing
  - `linux_set_robust_list/get_robust_list` - Mutex robustness broken
  - `linux_setgroups` - Group membership broken
  - `linux_capset` - Capability setting ignored
  - `linux_rename` - File rename broken

### Category C: "Vicieux" Stubs (Tricky - Return calculated fake values)
- 🔴 CRITICAL - These return plausible-looking values that mask the real problem
- Examples:
  - `linux_stat/linux_lstat` - Ignore path, always use FD 0
  - `linux_newfstatat` - Ignore dirfd, always use absolute path
  - `linux_statx` - Ignore dirfd for relative path resolution
  - `linux_fcntl` - F_DUPFD/F_DUPFD_CLOEXEC returns fd+100 as fake FD (THE PYTHON3 KILLER)
  - `linux_faccessat` - Ignore dirfd for relative path resolution

### Category D: "Dangereux par délégation" (Dangerous by delegation)
- 🟡 MEDIUM - Functions that delegate to wrong implementations
- Examples: `linux_capget`, `linux_readlinkat`, `linux_ioctl`, `linux_mkdirat`, `linux_unlinkat`, `linux_statfs`, `linux_fstatfs`, `linux_sched_getparam`, `linux_sched_getaffinity`, `linux_clock_getres`, `linux_getrlimit`, `linux_getitimer`, `linux_copy_file_range`, `linux_getrusage`

---

## Key Insights

### The Python3 Killer: linux_fcntl

The original Python3 failure was NOT a filesystem bug, but a **"vicieux" stub** (linux_fcntl returning fd+100 as fake FD). This pattern repeats throughout the codebase:

1. Python opens `/usr/lib/python3.12/encodings/__init__.py` → FD 3 ✓
2. Python calls `fcntl(3, F_DUPFD_CLOEXEC, 0)` → Returns FD 103 (fake!)
3. Python tries `read(103, buf, 1024)` → 0 bytes (FD 103 doesn't exist)
4. Python thinks file is empty → partial module init
5. Tries to import 'aliases' → ImportError (circular import illusion)

### The Verification False-Positive

The kernel page table verification reported "1533 present, 0 fixed" but the kernel still crashed at PT[413]=0x0. Root causes:

1. **Linker symbols were undefined** - The verification code couldn't determine the actual kernel bounds
2. **TLB caching** - The CPU was returning stale cached values during verification
3. **Limine's incomplete mapping** - Limine only mapped pages containing actual kernel code/data, not the entire .bss section

---

## Next Steps

### Immediate (Critical for Python3)
1. ✅ Fix KPTI trampoline CR3 switch
2. ✅ Add TLB/cache flush before verification
3. ✅ Add linker symbols for kernel bounds
4. ⏳ Verify CI passes with these fixes
5. 🔴 Fix `linux_fcntl` F_DUPFD/F_DUPFD_CLOEXEC to return real FD
6. 🔴 Fix `linux_stat/linux_lstat` to read actual file metadata
7. 🔴 Fix `linux_newfstatat` to use dirfd for relative paths
8. 🔴 Fix `linux_statx` to use dirfd for relative paths
9. 🔴 Fix `linux_munmap` to actually free memory
10. 🔴 Fix `linux_faccessat` to use dirfd for relative paths

### Short-term (Security/Functionality)
11. Fix UID/GID spoofing stubs (linux_setuid, linux_setgid, etc.)
12. Fix `linux_readlinkat` to use dirfd
13. Fix `linux_mkdirat` to use dirfd
14. Fix `linux_unlinkat` to use dirfd

### Medium-term (System State Queries)
15. Implement `linux_set_robust_list/get_robust_list` for mutex robustness
16. Implement `linux_setgroups` for group membership
17. Implement `linux_capget/capset` for capabilities
18. Implement `linux_statfs/fstatfs` for filesystem stats
19. Implement `linux_sched_getparam/getaffinity` for scheduler queries
20. Implement `linux_clock_getres` for clock resolution
21. Implement `linux_getrlimit` for resource limits
22. Implement `linux_getitimer` for timer queries
23. Implement `linux_getrusage` for resource usage
24. Implement `linux_copy_file_range` for file copying

---

## Files Modified

- `kernel/src/elf/mod.rs` - Fixed KPTI trampoline CR3 switch
- `kernel/src/boot/limine_entry.rs` - Added TLB/cache flush before verification
- `kernel/linker-x86_64.ld` - Added __kernel_start and __kernel_end symbols
- `kernel/src/main.rs` - Updated to use exec_switch_cr3_and_ring3
- `AetherionOS/STUB_AUDIT.md` - Comprehensive stub audit
- `AetherionOS/CI_RUN_87_ANALYSIS.md` - Analysis of page fault issue

---

## Verification Checklist

- [ ] CI passes with linker symbol fix
- [ ] Python3 successfully executes print(42*42)
- [ ] Musl dynamic linker can resolve relative paths
- [ ] GCC can compile code
- [ ] All 6 CRITICAL stubs fixed
- [ ] Syscall tracing enabled and logging all calls
- [ ] No memory leaks from linux_munmap
- [ ] No UID/GID spoofing

