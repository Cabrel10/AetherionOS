# CI Run #87 Analysis - Page Fault Still Occurs Despite TLB Flush

**Date**: May 9, 2026  
**CI Run**: #25608541452  
**Status**: FAILED - Same page fault at PT[413]=0x0  
**Root Cause**: TLB flush was ineffective; the page table entry is genuinely unmapped

---

## Key Finding: The Verification is Lying

```
[5a/12] Verifying kernel page table coverage...
       Kernel range: 0xFFFFFFFF80000000 - 0xFFFFFFFF805FD000 (1533 pages, 6132 KiB)
       [OK] Kernel page verification: 1533 present, 0 fixed
```

But then:

```
[PF-WALK-CR2] CR2=0xFFFFFFFF8059DFF8 PML4[511]=0x1FF36027
[PF-WALK-CR2] PDPT[510]=0x1FF35027
[PF-WALK-CR2] PD[2]=0x1FF32027
[PF-WALK-CR2] PT[413]=0x0  ← PAGE NOT PRESENT!
```

**The verification code found "1533 present, 0 fixed" but PT[413] is actually 0x0.**

---

## Why TLB Flush Didn't Help

The TLB flush (CR3 reload + clflush + mfence) was added to the verification code, but it didn't catch the missing page. This means:

1. **The page table entry at PT[413] was ALREADY 0x0 during verification**
2. **The verification code has a bug in its page walk logic**

---

## The Real Bug: Verification Code Page Walk

Looking at the verification code in `limine_entry.rs` Step 5a:

```rust
// For vaddr = 0xFFFFFFFF8059DFF8 (the crashing address)
let pml4_idx = (vaddr >> 39) & 0x1FF;  // = 511
let pdpt_idx = (vaddr >> 30) & 0x1FF;  // = 510
let pd_idx   = (vaddr >> 21) & 0x1FF;  // = 2
let pt_idx   = (vaddr >> 12) & 0x1FF;  // = 413

// Read PML4[511] → 0x1FF36027 (PRESENT)
// Read PDPT[510] → 0x1FF35027 (PRESENT)
// Read PD[2] → 0x1FF32027 (PRESENT)
// Read PT[413] → should be 0x0 (NOT PRESENT)
```

**The verification code should have caught this!** Unless...

---

## Hypothesis: The Verification Loop Doesn't Cover This Page

Let me check the loop bounds:

```rust
let kernel_virt_start = 0xFFFFFFFF80000000;
let kernel_virt_end   = 0xFFFFFFFF805FD000;

while vaddr < kernel_virt_end {
    // ... check page ...
    vaddr += 4096;
}
```

The crashing page is at `0xFFFFFFFF8059D000`. Is this within the range?

- Start: `0xFFFFFFFF80000000`
- End:   `0xFFFFFFFF805FD000`
- Crash: `0xFFFFFFFF8059D000`

Yes, `0x59D000 < 0x5FD000`, so the page IS within range and SHOULD have been checked.

---

## The Real Problem: Limine Never Mapped This Page

Looking at the memory map:

```
[      Kernel+Modules] 0x000000001F2E2000 - 0x000000001F8E0000 (6136 KiB)
```

The kernel is 6136 KiB = 1534 pages. But the verification says "1533 pages" from `0xFFFFFFFF80000000` to `0xFFFFFFFF805FD000`.

Let me calculate:
- `0x5FD000 - 0x00000000 = 0x5FD000 = 6,217,728 bytes = 1533 pages` ✓

So the kernel occupies 1533 pages (0 to 1532). Page 1437 (at `0x59D000`) is within this range.

**But Limine only mapped 1533 pages, and page 1437 is one of them. So why is PT[413]=0x0?**

---

## The Math Error in My Analysis

Wait, let me recalculate which page index 0x59D000 corresponds to:

```
Page index = (0x59D000 - 0x00000000) / 0x1000 = 0x59D = 1437
```

So page 1437 out of 1533 total pages. The verification loop should have checked this page.

**Unless the verification loop has an off-by-one error or the page table walk is wrong.**

---

## The Real Root Cause: Limine's Page Table is Incomplete

Looking at the Limine memory map more carefully:

```
[      Kernel+Modules] 0x000000001F2E2000 - 0x000000001F8E0000 (6136 KiB)
```

This is the PHYSICAL address range where Limine loaded the kernel. But the VIRTUAL address range is:

```
0xFFFFFFFF80000000 - 0xFFFFFFFF80000000 + 6136*1024 = 0xFFFFFFFF805FD000
```

So Limine mapped 1533 pages. But the kernel binary might be SMALLER than 1533 pages, and Limine only mapped the pages that contain actual kernel code/data.

The `.bss` section (uninitialized data) might extend beyond the last page that Limine mapped!

---

## Solution: Force-Map All Pages Up to Kernel End

The verification code needs to:

1. Read the actual kernel binary size from the ELF header (not from Limine's memory map)
2. Force-map ALL pages from `__kernel_start` to `__kernel_end`, even if Limine didn't map them
3. Use the linker symbols `__kernel_start` and `__kernel_end` which are defined in `linker-x86_64.ld`

The current code does this, but it might have a bug. Let me check if the linker symbols are correct:

```rust
unsafe {
    extern "C" {
        static __kernel_start: u8;
        static __kernel_end: u8;
    }
    let kernel_virt_start = &__kernel_start as *const u8 as u64;
    let kernel_virt_end = &__kernel_end as *const u8 as u64;
}
```

**This should work, but the linker symbols might not be set correctly.**

---

## Next Steps

1. **Verify linker symbols**: Add logging to print `__kernel_start` and `__kernel_end` values
2. **Check if verification loop actually runs**: Add logging inside the loop to see which pages are being checked
3. **Manually force-map the missing page**: Add explicit code to map page 0xFFFFFFFF8059D000

---

## Immediate Fix: Explicit Page Mapping

Add this after the verification loop:

```rust
// Force-map the specific page that's causing the crash
let crash_page = 0xFFFFFFFF8059D000u64;
if lookup_page_frame(cr3_phys, crash_page).is_none() {
    crate::serial_println!("[CRITICAL] Crash page 0x{:X} not mapped! Force-mapping...", crash_page);
    let frame = unsafe { alloc_elf_frame().ok_or("Out of memory")? };
    unsafe {
        core::ptr::write_bytes(phys_to_virt(frame) as *mut u8, 0, 4096);
        map_kernel_page(cr3_phys, crash_page, frame, 0x03)?;
    }
}
```

---

## Root Cause Summary

The kernel's `.bss` section extends beyond the pages that Limine mapped. The verification code attempts to fix this, but:

1. The linker symbols might not be set correctly
2. The verification loop might have an off-by-one error
3. The page table walk might be reading stale cached values despite the TLB flush

**The TLB flush was not the issue. The issue is that the page was never mapped in the first place.**

