# Next Session Action Plan — Secure Frame Allocator & Fix PML4 Cloning

## Immediate Goal
**Get Python3 (CI-TEST-10) to print 1764 before attempting APK again.**

This isolates the problem: if Python3 works alone, the issue is APK-specific. If Python3 also hangs, the problem is in the core PML4 cloning logic.

---

## Step 1: Verify Frame Allocator Bounds (15 min)

### Current Code Location
`kernel/src/elf/mod.rs` — function `alloc_elf_frame()` (line ~267)

### What to Check
```rust
pub unsafe fn alloc_elf_frame() -> Option<u64> {
    if !ELF_POOL_INITIALIZED.load(Ordering::SeqCst) {
        return None;
    }
    // Priority 1: Reuse a freed frame from the freelist
    if ELF_POOL.freelist_count > 0 {
        ELF_POOL.freelist_count -= 1;
        let phys = ELF_POOL.freelist[ELF_POOL.freelist_count];
        // ⚠️ NO BOUNDS CHECK — could return kernel memory!
        return Some(phys);
    }
    // Priority 2: Bump allocate a new frame
    if ELF_POOL.frames_used >= ELF_POOL.max_frames {
        return None;
    }
    let phys = ELF_POOL.base_frame + (ELF_POOL.frames_used as u64) * PAGE_SIZE;
    // ⚠️ NO BOUNDS CHECK — could allocate into kernel zone!
    ELF_POOL.frames_used += 1;
    Some(phys)
}
```

### Fix Required
Add bounds checking to reject frames in kernel memory zones:
```rust
// Get kernel memory ranges from Limine (stored during boot)
let kernel_start = crate::memory::KERNEL_PHYS_START; // e.g., 0x1F2E6000
let kernel_end = crate::memory::KERNEL_PHYS_END;     // e.g., 0x1F8E2000

if phys >= kernel_start && phys < kernel_end {
    crate::serial_println!("[POOL] ERROR: Attempted to allocate kernel memory 0x{:X}", phys);
    return None; // Reject allocation
}
```

---

## Step 2: Verify PML4 Cloning Logic (20 min)

### Current Code Location
`kernel/src/elf/mod.rs` — function `create_user_pml4()` (search for it)

### What to Check
1. Does it allocate a NEW physical frame for the user PML4?
2. Does it copy entries from the kernel PML4 to the new PML4?
3. Does it ever write to the active kernel CR3?

### Expected Behavior
```rust
pub unsafe fn create_user_pml4() -> Result<u64, ElfError> {
    // Step 1: Allocate a NEW physical frame for user PML4
    let user_pml4_phys = alloc_elf_frame().ok_or(ElfError::OutOfMemory)?;
    
    // Step 2: Zero the new frame
    let user_pml4_virt = phys_to_virt(user_pml4_phys) as *mut u64;
    core::ptr::write_bytes(user_pml4_virt, 0, PAGE_SIZE as usize);
    
    // Step 3: Read the KERNEL PML4 (via CR3)
    let kernel_cr3: u64;
    core::arch::asm!("mov {}, cr3", out(reg) kernel_cr3);
    let kernel_pml4_phys = kernel_cr3 & !0xFFF;
    let kernel_pml4_virt = phys_to_virt(kernel_pml4_phys) as *const u64;
    
    // Step 4: Copy kernel entries to user PML4 (PML4[256], PML4[511], etc.)
    // ⚠️ CRITICAL: Only copy, NEVER write to kernel_pml4_virt!
    for i in [256, 511] {
        let entry = core::ptr::read_volatile(kernel_pml4_virt.add(i));
        core::ptr::write_volatile((user_pml4_virt as *mut u64).add(i), entry);
    }
    
    Ok(user_pml4_phys)
}
```

### Red Flags to Look For
- ❌ Writing to `kernel_pml4_virt` (the active kernel PML4)
- ❌ Modifying PML4[0] in the kernel PML4 instead of the user PML4
- ❌ Not zeroing the new user PML4 before copying entries

---

## Step 3: Add Diagnostic Logging (10 min)

Add these logs to `load_elf()` before and after PML4 cloning:

```rust
// Before cloning
let cr3_before: u64;
unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3_before); }
crate::serial_println!("[ELF-DIAG] CR3 before PML4 clone: 0x{:X}", cr3_before);

// After cloning
let cr3_after: u64;
unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3_after); }
crate::serial_println!("[ELF-DIAG] CR3 after PML4 clone: 0x{:X}", cr3_after);

if cr3_before != cr3_after {
    crate::serial_println!("[ELF-DIAG] WARNING: CR3 changed during PML4 clone!");
}
```

---

## Step 4: Test Python3 in Isolation (30 min)

### Modify `limine_entry.rs` to skip APK
Comment out or remove the CI-TEST-16 (APK) block:

```rust
// TEMPORARILY DISABLED FOR DEBUGGING
// if crate::fs::ext2::lookup_path("/sbin/apk").is_some() {
//     crate::serial_write("[CI-TEST-16] apk --version (Ring 3 userspace)\n");
//     ...
// }
```

### Run the test
```bash
cargo check -p aetherion-kernel --target x86_64-unknown-none --features limine
bash scripts/build-limine.sh --release
timeout 180 qemu-system-x86_64 \
  -cdrom target/aetherion-limine.iso \
  -drive file=/tmp/rootfs.ext2,format=raw,if=virtio \
  -netdev user,id=net0 \
  -device virtio-net-pci,netdev=net0 \
  -cpu qemu64,+rdrand,+rdseed \
  -m 2G -smp 2 -no-reboot -nographic -serial mon:stdio \
  2>&1 | tee /tmp/boot-python3-only.log | grep -E "1764|CI-TEST-10|PASS|FAIL"
```

### Expected Result
If Python3 works:
```
[CI-TEST-10] Python3 execution: print(42*42)
1764
[CI-TEST-10] PASS
```

If Python3 also hangs:
```
[ELF-DEBUG] CRITICAL: RIP=0xFFFFFFFF... CR3 switch likely failed.
... (repeats, then hangs)
```

---

## Step 5: Fix Based on Results (Variable)

### If Python3 Works Alone
- ✅ Problem is APK-specific (different ELF structure)
- ✅ PML4 cloning logic is correct
- ✅ Re-enable APK and investigate its specific ELF segments

### If Python3 Also Hangs
- ❌ Problem is in core PML4 cloning logic
- ❌ Check `create_user_pml4()` for corruption
- ❌ Verify frame allocator bounds checking

---

## Estimated Time
- Step 1 (Frame allocator): 15 min
- Step 2 (PML4 cloning): 20 min
- Step 3 (Logging): 10 min
- Step 4 (Python3 test): 30 min
- Step 5 (Fix): 30-60 min depending on results

**Total: 1.5-2 hours**

---

## Success Criteria

✅ **Session Success:**
- Python3 prints 1764
- No kernel page fault loops
- APK loads (even if it doesn't execute)

❌ **Session Failure:**
- Python3 still hangs
- Kernel page fault loop persists
- Root cause not identified

---

## Notes for Next Agent

1. **Don't skip the frame allocator bounds check** — this is the most likely culprit
2. **Test Python3 in isolation first** — this isolates the problem scope
3. **Keep diagnostic logging** — it's invaluable for debugging bare-metal issues
4. **Document findings** — even if the fix doesn't work, the diagnosis is valuable

---

## Related Files to Review
- `kernel/src/elf/mod.rs` — `alloc_elf_frame()`, `create_user_pml4()`, `load_elf()`
- `kernel/src/boot/limine_entry.rs` — CI-TEST-16 (APK loading)
- `kernel/src/memory/frame.rs` — frame allocation (if separate from ELF pool)

---

## Conclusion

The path forward is clear. The PF-KERN-FIX is correct. The new problem is likely in the frame allocator or PML4 cloning logic. Secure the allocator, verify the cloning, test Python3 in isolation, and the system will boot cleanly into Ring 3.
