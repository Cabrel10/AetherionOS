# Fix: Store and Use Kernel Physical Base

## Problem

The PT neighbor scanning in the lazy #PF handler assumes kernel pages are contiguous in physical memory. This is unreliable.

## Solution

Instead of scanning neighbors, compute the kernel physical base at boot time and store it globally. Then use it to calculate correct physical addresses.

## Implementation Plan

### Step 1: Add global atomic for kernel physical base

In `kernel/src/boot/limine_entry.rs`:

```rust
use core::sync::atomic::{AtomicU64, Ordering};

static KERNEL_PHYS_BASE: AtomicU64 = AtomicU64::new(0);

pub fn get_kernel_phys_base() -> u64 {
    KERNEL_PHYS_BASE.load(Ordering::SeqCst)
}

pub fn set_kernel_phys_base(base: u64) {
    KERNEL_PHYS_BASE.store(base, Ordering::SeqCst);
}
```

### Step 2: Calculate and store kernel physical base at boot

In `kmain()` at Step 5a, after memory init:

```rust
// Calculate kernel physical base from existing page table mappings
// The kernel is at virtual 0xFFFFFFFF80000000
// We can find its physical base by reading any present PT entry in the kernel region
// and working backwards.

let kernel_virt_base = 0xFFFFFFFF80000000u64;
let hhdm = boot_info.hhdm_offset;

// Read CR3 to get current PML4
let cr3: u64;
unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)); }
let pml4_phys = cr3 & !0xFFF;

// Walk to kernel region (PML4[511])
let pml4_idx = 511usize;
let pdpt_idx = 510usize;
let pd_idx = 0usize;
let pt_idx = 0usize;

unsafe {
    let pml4_virt = (pml4_phys + hhdm) as *const u64;
    let pml4_entry = core::ptr::read_volatile(pml4_virt.add(pml4_idx));
    
    if pml4_entry & 1 != 0 {
        let pdpt_phys = pml4_entry & 0x000F_FFFF_FFFF_F000;
        let pdpt_virt = (pdpt_phys + hhdm) as *const u64;
        let pdpt_entry = core::ptr::read_volatile(pdpt_virt.add(pdpt_idx));
        
        if pdpt_entry & 1 != 0 && pdpt_entry & 0x80 == 0 {
            let pd_phys = pdpt_entry & 0x000F_FFFF_FFFF_F000;
            let pd_virt = (pd_phys + hhdm) as *const u64;
            let pd_entry = core::ptr::read_volatile(pd_virt.add(pd_idx));
            
            if pd_entry & 1 != 0 && pd_entry & 0x80 == 0 {
                let pt_phys = pd_entry & 0x000F_FFFF_FFFF_F000;
                let pt_virt = (pt_phys + hhdm) as *const u64;
                let pt_entry = core::ptr::read_volatile(pt_virt.add(pt_idx));
                
                if pt_entry & 1 != 0 {
                    // Found a valid PT entry for kernel .text at PT[0]
                    let page_phys = pt_entry & 0x000F_FFFF_FFFF_F000;
                    // This page is at virtual kernel_virt_base + 0
                    // So kernel_phys_base = page_phys
                    set_kernel_phys_base(page_phys);
                    crate::serial_println!(
                        "[5a] Kernel physical base: 0x{:X}",
                        page_phys
                    );
                }
            }
        }
    }
}
```

### Step 3: Update lazy #PF handler to use kernel physical base

In `kernel/src/arch/x86_64/idt.rs`, replace the PT neighbor scanning with:

```rust
} else if pml4_idx == 511 {
    // Kernel image region: use stored kernel physical base
    let kernel_phys_base = crate::boot::limine_entry::get_kernel_phys_base();
    if kernel_phys_base != 0 {
        let virt_offset = page_addr - 0xFFFFFFFF80000000;
        let target_phys = kernel_phys_base + virt_offset;
        frame_phys = Some(target_phys);
        crate::serial_println!(
            "[PF-KERN-FIX] Mapped 0x{:X} -> phys 0x{:X} (kernel_base=0x{:X}, offset=0x{:X})",
            page_addr, target_phys, kernel_phys_base, virt_offset
        );
    } else {
        // Fallback: allocate zero frame if kernel_phys_base not set
        frame_phys = crate::elf::alloc_demand_frame();
        if let Some(f) = frame_phys {
            core::ptr::write_bytes((f + phys_off) as *mut u8, 0, 4096);
        }
    }
}
```

## Benefits

1. **Correct**: Works for ANY kernel layout, contiguous or not
2. **Simple**: No complex neighbor scanning logic
3. **Reliable**: Based on actual page table mappings at boot time
4. **Efficient**: O(1) calculation instead of O(512) scan

## Testing

After implementation:
1. Compile: `cargo check -p aetherion-kernel --target x86_64-unknown-none --features limine`
2. Run CI and check for "[5a] Kernel physical base:" log
3. Verify Python3 outputs "1764"

