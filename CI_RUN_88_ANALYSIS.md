# CI Run #88 Analysis - General Protection Fault at KPTI Trampoline

**Date**: May 9, 2026  
**CI Run**: #25610600588  
**Status**: FAILED - #GP at trampoline address  
**Root Cause**: The global IRETQ trampoline is not properly mapped in the user PML4

---

## The Crash

```
[ELF-DEBUG] exec_switch_cr3_and_ring3: CR3=0xC0D6000 RIP=0x7FC000062B17 RSP=0x7FFFFFFFEA90
[ELF-DEBUG] Entry 0x7FC000062B17 -> phys frame 0xBE65000 (OK)
[TRAMPOLINE] Using KPTI-safe trampoline for CR3 switch
[EXCEPTION] #GP code=0x6000 rip=VirtAddr(0x444444446387) ring3=false last_syscall=0
[EXCEPTION] #GP RSP=VirtAddr(0xffff80001ff573a0) RFLAGS=RFlags(INTERRUPT_FLAG | 0x2) SS=SegmentSelector { index: 2, rpl: Ring0 }
[KERNEL-PANIC] General protection fault (kernel) code=0x6000 at kernel/src/arch/x86_64/idt.rs:424
System halted.
```

**Key observations**:
1. The trampoline address is `0x444444446387` (in the heap region)
2. The #GP error code is `0x6000` (selector index 3, TI=0, RPL=0)
3. The fault occurs in kernel mode (ring3=false)
4. The RSP is in kernel space (`0xffff80001ff573a0`)

---

## The Problem: Trampoline Not Mapped in User PML4

The global IRETQ trampoline is allocated at `0x444444446380` (in the kernel heap region). This address is mapped in the **kernel PML4** (via PML4[1] which contains the kernel heap).

However, when we create a user PML4 for the Python3 process, we only clone:
- PML4[256] (phys-offset region for KPTI)
- PML4[511] (kernel code/data)

We do **NOT** clone PML4[1] (kernel heap), so the trampoline at `0x444444446380` is not accessible in the user PML4!

---

## Why This Happens

In `kernel/src/elf/mod.rs`, the `create_user_pml4()` function clones only specific PML4 entries:

```rust
// Clone only kernel regions needed for user mode
// PML4[256] = phys-offset (KPTI trampoline region)
// PML4[511] = kernel code/data
// But NOT PML4[1] = kernel heap!
```

The trampoline is in the kernel heap (PML4[1]), which is not cloned. So when we jump to the trampoline from user mode, the CPU can't find the page and raises #GP.

---

## The Solution: Two Options

### Option A: Clone PML4[1] into User PML4 (WRONG - Security Issue)
This would expose the entire kernel heap to user processes, breaking KPTI isolation.

### Option B: Move Trampoline to Phys-Offset Region (CORRECT)
The trampoline should be allocated in the phys-offset region (PML4[256]), which is already cloned into user PML4s. This region is mapped in both kernel and user page tables.

---

## The Real Issue: Trampoline Allocation

Looking at the code in `kernel/src/elf/mod.rs`:

```rust
pub fn init_global_iretq_trampoline() {
    let buf = alloc::vec![0u8; 64]; // Allocated on kernel heap!
    let ptr = buf.leak().as_mut_ptr();
    // ... write trampoline code ...
    let addr = ptr as u64;
    GLOBAL_IRETQ_TRAMPOLINE.store(addr, Ordering::SeqCst);
}
```

The trampoline is allocated using `alloc::vec!`, which allocates from the kernel heap. The kernel heap is at `0x444444440000` (PML4[1]), which is NOT cloned into user PML4s.

---

## Why the Previous Approach Worked

The old `exec_switch_cr3_and_ring3_naked` function didn't use a trampoline. It executed the CR3 switch directly in kernel code, which was mapped in the kernel PML4. But this caused a page fault because after the CR3 switch, the kernel code page became inaccessible.

The new approach tries to use a trampoline, but the trampoline is in the wrong place (kernel heap instead of phys-offset region).

---

## The Fix: Allocate Trampoline in Phys-Offset Region

The trampoline needs to be allocated in a region that's present in both kernel and user PML4s. The phys-offset region (PML4[256]) is the right place.

However, allocating in the phys-offset region is tricky because:
1. The phys-offset region is mapped via HHDM (0xFFFF800000000000 + physical_address)
2. We need to allocate a physical frame and map it in the phys-offset region
3. We need to ensure the trampoline code is accessible at a fixed virtual address

---

## Alternative Fix: Use exec_trampoline Instead

Looking at the code, there's already an `exec_trampoline()` function that's designed to be called from the phys-offset region. But it's not being used correctly.

The issue is that `exec_trampoline()` is a naked function that expects to be called with specific register values (r8, r9, r10, r11). But we're trying to jump to it using `jmp {trampoline}` with the trampoline address in a register.

---

## The Real Root Cause

The `exec_switch_cr3_and_ring3` function is trying to jump to the global IRETQ trampoline, but:

1. The trampoline is allocated in the kernel heap (PML4[1])
2. The user PML4 doesn't have PML4[1] mapped
3. When we jump to the trampoline after setting up registers, the CPU tries to fetch the instruction at `0x444444446387`
4. The page is not present in the user PML4, so #GP is raised

---

## Why #GP Instead of #PF?

The #GP error code `0x6000` indicates a selector error. The CPU is trying to access a page that's not present, but because we're in a special state (after `mov cr3`), the CPU raises #GP instead of #PF.

---

## The Correct Solution

We need to either:

1. **Allocate the trampoline in the phys-offset region** - This requires:
   - Allocating a physical frame
   - Mapping it in the phys-offset region (PML4[256])
   - Writing the trampoline code to that frame
   - Ensuring the address is accessible in both kernel and user PML4s

2. **Use a different approach** - Instead of jumping to a trampoline, we could:
   - Build the IRETQ frame on the user stack
   - Use a syscall to switch to user mode (but this requires the user to call a syscall first)
   - Use a different mechanism entirely

3. **Revert to the original approach** - But fix the page fault issue by:
   - Ensuring the kernel code page is mapped in both kernel and user PML4s
   - Or using a different instruction sequence that doesn't require the kernel code page after CR3 switch

---

## Immediate Diagnosis

The problem is clear: **the trampoline is in the wrong place**. It's in the kernel heap, which is not accessible from user mode.

The fix requires either:
1. Moving the trampoline to the phys-offset region, or
2. Using a different approach that doesn't require a trampoline in the user PML4

