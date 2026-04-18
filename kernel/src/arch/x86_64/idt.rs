// src/arch/x86_64/idt.rs - IDT Implementation (Couche 1 HAL + Couche 12 Demand Paging)
// Interrupt Descriptor Table avec handlers exceptions
//
// Page fault handler implements demand paging for the user stack region:
//   0x7FFF_0000_0000 - 0x7FFF_FFFF_F000 (user stack virtual range)
// If a page fault occurs from user mode (USER_MODE error bit set) and the
// faulting address is within the stack range, the handler allocates a frame,
// maps it as USER_ACCESSIBLE | WRITABLE | NO_EXECUTE, and resumes.
// Otherwise, it kills the process (SIGSEGV).

use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use lazy_static::lazy_static;

// Import du GDT pour IST index
use super::gdt;

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // Exception: Divide by zero (#DE)
        idt.divide_error.set_handler_fn(divide_error_handler);

        // Exception: Debug (#DB)
        idt.debug.set_handler_fn(debug_handler);

        // Exception: Breakpoint (#BP)
        idt.breakpoint.set_handler_fn(breakpoint_handler);

        // Exception: Overflow (#OF)
        idt.overflow.set_handler_fn(overflow_handler);

        // Exception: Bound range exceeded (#BR)
        idt.bound_range_exceeded.set_handler_fn(bound_range_exceeded_handler);

        // Exception: Invalid opcode (#UD)
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);

        // Exception: Device not available (#NM)
        idt.device_not_available.set_handler_fn(device_not_available_handler);

        // Exception: Double fault (#DF) - utilise IST (stack separé)
        // SAFETY: The IST index is valid (0) and corresponds to a 20 KB stack
        // allocated in gdt.rs. The double-fault handler needs its own stack to
        // handle stack overflows that would otherwise cause a triple-fault.
        unsafe {
            idt.double_fault.set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::double_fault_ist_index());
        }

        // Exception: Invalid TSS (#TS)
        idt.invalid_tss.set_handler_fn(invalid_tss_handler);

        // Exception: Segment not present (#NP)
        idt.segment_not_present.set_handler_fn(segment_not_present_handler);

        // Exception: Stack segment fault (#SS)
        idt.stack_segment_fault.set_handler_fn(stack_segment_fault_handler);

        // Exception: General protection fault (#GP)
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);

        // Exception: Page fault (#PF) - demand paging for Ring 3 stack
        // Jalon 134c: Use dedicated IST1 so that PFs taken in syscall_entry
        // (before the user->kernel stack switch) have a trustworthy kernel
        // stack to run on. Without this, #PF runs on the user RSP, which is
        // fragile and can cascade into #DF.
        unsafe {
            idt.page_fault.set_handler_fn(page_fault_handler)
                .set_stack_index(gdt::page_fault_ist_index());
        }

        // Exception: x87 FPU error (#MF)
        idt.x87_floating_point.set_handler_fn(x87_floating_point_handler);

        // Exception: Alignment check (#AC)
        idt.alignment_check.set_handler_fn(alignment_check_handler);

        // Exception: Machine check (#MC)
        idt.machine_check.set_handler_fn(machine_check_handler);

        // Exception: SIMD floating point (#XF)
        idt.simd_floating_point.set_handler_fn(simd_floating_point_handler);

        // Exception: Virtualization (#VE)
        idt.virtualization.set_handler_fn(virtualization_handler);

        // Exception: Security (#SX)
        idt.security_exception.set_handler_fn(security_exception_handler);

        // IRQ Handlers (PIC 8259)
        // Timer (IRQ 0 -> vector 32)
        idt[super::interrupts::PIC1_OFFSET as usize]
            .set_handler_fn(timer_interrupt_handler);

        // Keyboard (IRQ 1 -> vector 33)
        idt[super::interrupts::PIC1_OFFSET as usize + 1]
            .set_handler_fn(keyboard_interrupt_handler);

        // Mouse (IRQ 12 -> vector 44)
        idt[super::interrupts::PIC2_OFFSET as usize + 4]
            .set_handler_fn(mouse_interrupt_handler);

        idt
    };
}

/// Charge l'IDT
pub fn init() {
    IDT.load();
    crate::serial_println!("[IDT] Loaded with 20 exception handlers + demand paging");

    // J134c: Dump the live IDT entry for #PF (vector 14) to confirm that
    // IST=1 was applied. We read from the IDTR base via SIDT and inspect
    // bytes at entry 14 (offset 14*16 = 0xE0). The IST field is byte[4], bits 0..2.
    unsafe {
        let mut idtr: [u8; 10] = [0; 10];
        core::arch::asm!("sidt [{}]", in(reg) idtr.as_mut_ptr(),
            options(nostack, preserves_flags));
        let idt_base = u64::from_le_bytes([
            idtr[2], idtr[3], idtr[4], idtr[5],
            idtr[6], idtr[7], idtr[8], idtr[9],
        ]);
        let pf_entry = (idt_base + 14 * 16) as *const u8;
        let b = [
            *pf_entry.add(0), *pf_entry.add(1), *pf_entry.add(2), *pf_entry.add(3),
            *pf_entry.add(4), *pf_entry.add(5), *pf_entry.add(6), *pf_entry.add(7),
        ];
        let ist = b[4] & 0x07;
        let type_attr = b[5];
        let handler_lo = u16::from_le_bytes([b[0], b[1]]) as u64;
        let sel = u16::from_le_bytes([b[2], b[3]]);
        let handler_mid = u16::from_le_bytes([b[6], b[7]]) as u64;
        let handler_high = u32::from_le_bytes([
            *pf_entry.add(8), *pf_entry.add(9),
            *pf_entry.add(10), *pf_entry.add(11),
        ]) as u64;
        let handler = handler_lo | (handler_mid << 16) | (handler_high << 32);
        crate::serial_println!(
            "[IDT-DIAG] IDT[14] (#PF): handler=0x{:X} sel=0x{:X} IST={} type=0x{:X}",
            handler, sel, ist, type_attr
        );
    }
}

/// Load the BSP's IDT on an AP core (Jalon 101).
/// Called from ap_main after loading the GDT.
pub fn load_for_ap() {
    IDT.load();
}

/// Retourne une reference statique a l'IDT (pour tests)
pub fn idt_ref() -> &'static InterruptDescriptorTable {
    &IDT
}

// ===== KPTI: Relocated IDT for user-mode interrupt handling =====
//
// When user ELF binaries (e.g., BusyBox at 0x400000) overlap kernel .text,
// the identity-mapped IDT handler addresses become invalid after CR3 switch.
// This relocated IDT uses the physical-offset mapping addresses
// (0xFFFF800000000000 + phys_addr) for all handlers, which are mapped in
// every PML4 via PML4[256].
//
// The IDT itself (at ~0x664CE0) is in .bss (PD[3]), outside the BusyBox
// range, so it remains accessible. But the handlers it points to are in
// .text (PD[2], 0x400000-0x5FFFFF), which gets overwritten.

/// Secondary IDT with handler addresses relocated to phys-offset mapping.
/// Stored as raw bytes because we need to patch handler addresses manually.
/// Size: 256 entries × 16 bytes = 4096 bytes (one page, aligned).
#[repr(C, align(16))]
struct RawIdt {
    entries: [u8; 256 * 16],
}

static mut KPTI_IDT: RawIdt = RawIdt { entries: [0; 256 * 16] };

/// Relocate IDT handler addresses by adding phys_offset to each entry.
/// Called after phys_offset is known (post-boot, before first execve).
///
/// This patches all 256 IDT entries: for each entry that is present
/// (has a non-zero handler address in the identity-mapped range),
/// the handler address is replaced with handler + phys_offset.
/// The relocated IDT is then loaded with LIDT.
pub fn relocate_idt_for_kpti(phys_offset: u64) {
    if phys_offset == 0 {
        crate::serial_println!("[IDT-KPTI] phys_offset is 0, skipping IDT relocation");
        return;
    }

    unsafe {
        // Get the current IDT base address from the IDTR register
        let mut idtr: [u8; 10] = [0; 10];
        core::arch::asm!(
            "sidt [{}]",
            in(reg) idtr.as_mut_ptr(),
            options(nostack, preserves_flags)
        );
        let idt_limit = u16::from_le_bytes([idtr[0], idtr[1]]);
        let idt_base = u64::from_le_bytes([
            idtr[2], idtr[3], idtr[4], idtr[5],
            idtr[6], idtr[7], idtr[8], idtr[9],
        ]);

        let num_entries = ((idt_limit as usize) + 1) / 16;
        crate::serial_println!(
            "[IDT-KPTI] Current IDT: base=0x{:X}, limit=0x{:X}, entries={}",
            idt_base, idt_limit, num_entries
        );

        // Copy the entire IDT into our KPTI_IDT buffer
        let src = idt_base as *const u8;
        let dst = KPTI_IDT.entries.as_mut_ptr();
        core::ptr::copy_nonoverlapping(src, dst, core::cmp::min((idt_limit + 1) as usize, 256 * 16));

        // Patch each entry: add phys_offset to the handler address
        let mut patched = 0u32;
        for i in 0..num_entries {
            let entry = dst.add(i * 16);
            // Read the current handler address from the IDT entry:
            //   bits 0-15:  bytes [0..2]   (offset_low)
            //   bits 16-31: bytes [6..8]   (offset_mid)
            //   bits 32-63: bytes [8..12]  (offset_high)
            let offset_low  = u16::from_le_bytes([*entry.add(0), *entry.add(1)]) as u64;
            let offset_mid  = u16::from_le_bytes([*entry.add(6), *entry.add(7)]) as u64;
            let offset_high = u32::from_le_bytes([
                *entry.add(8), *entry.add(9), *entry.add(10), *entry.add(11),
            ]) as u64;

            let handler_addr = offset_low | (offset_mid << 16) | (offset_high << 32);

            // Check if this entry has a handler (present bit in type_attr byte 5)
            let type_attr = *entry.add(5);
            if type_attr & 0x80 == 0 {
                continue; // not present
            }
            if handler_addr == 0 {
                continue;
            }

            // Add phys_offset to relocate the handler
            let new_addr = handler_addr.wrapping_add(phys_offset);

            // Write back the new address
            let new_low = (new_addr & 0xFFFF) as u16;
            let new_mid = ((new_addr >> 16) & 0xFFFF) as u16;
            let new_high = ((new_addr >> 32) & 0xFFFF_FFFF) as u32;

            let low_bytes = new_low.to_le_bytes();
            *entry.add(0) = low_bytes[0];
            *entry.add(1) = low_bytes[1];

            let mid_bytes = new_mid.to_le_bytes();
            *entry.add(6) = mid_bytes[0];
            *entry.add(7) = mid_bytes[1];

            let high_bytes = new_high.to_le_bytes();
            *entry.add(8)  = high_bytes[0];
            *entry.add(9)  = high_bytes[1];
            *entry.add(10) = high_bytes[2];
            *entry.add(11) = high_bytes[3];

            patched += 1;
        }

        crate::serial_println!(
            "[IDT-KPTI] Patched {} IDT entries with phys_offset=0x{:X}",
            patched, phys_offset
        );

        // Load the new IDT
        let new_idt_base = KPTI_IDT.entries.as_ptr() as u64;
        let new_idtr: [u8; 10] = {
            let limit_bytes = idt_limit.to_le_bytes();
            let base_bytes = new_idt_base.to_le_bytes();
            [
                limit_bytes[0], limit_bytes[1],
                base_bytes[0], base_bytes[1], base_bytes[2], base_bytes[3],
                base_bytes[4], base_bytes[5], base_bytes[6], base_bytes[7],
            ]
        };

        core::arch::asm!(
            "lidt [{}]",
            in(reg) new_idtr.as_ptr(),
            options(nostack, preserves_flags)
        );

        crate::serial_println!(
            "[IDT-KPTI] New IDT loaded at 0x{:X} (limit=0x{:X})",
            new_idt_base, idt_limit
        );
    }
}

// ===== Handlers Exceptions =====

extern "x86-interrupt" fn divide_error_handler(stack_frame: InterruptStackFrame) {
    crate::serial_println!("[EXCEPTION] #DE Divide by zero at {:?}", stack_frame.instruction_pointer);
    panic!("Divide by zero");
}

extern "x86-interrupt" fn debug_handler(stack_frame: InterruptStackFrame) {
    crate::serial_println!("[EXCEPTION] #DB Debug at {:?}", stack_frame.instruction_pointer);
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    crate::serial_println!("[EXCEPTION] #BP Breakpoint at {:?}", stack_frame.instruction_pointer);
}

extern "x86-interrupt" fn overflow_handler(stack_frame: InterruptStackFrame) {
    crate::serial_println!("[EXCEPTION] #OF Overflow at {:?}", stack_frame.instruction_pointer);
}

extern "x86-interrupt" fn bound_range_exceeded_handler(stack_frame: InterruptStackFrame) {
    crate::serial_println!("[EXCEPTION] #BR Bound range exceeded at {:?}", stack_frame.instruction_pointer);
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    let cs = stack_frame.code_segment;
    let is_ring3 = (cs & 0x3) == 3;
    crate::serial_println!("[EXCEPTION] #UD Invalid opcode at {:?} ring3={}", stack_frame.instruction_pointer, is_ring3);
    if is_ring3 {
        let current_pid = crate::scheduler::current_pid();
        crate::serial_println!("[#UD] Killing user PID {} (invalid opcode in Ring 3)", current_pid);
        if current_pid != 0 {
            // kill_user_and_switch never returns — it IRETQs to the next process
            // or enters an idle HLT loop if no other process is ready.
            kill_user_and_switch(current_pid, stack_frame.instruction_pointer.as_u64());
        }
        // If PID was 0 somehow, enter idle loop instead of panicking
        crate::serial_println!("[#UD] No user PID to kill, entering idle loop");
        crate::scheduler::set_current_pid(0);
        loop { x86_64::instructions::hlt(); }
    }
    // Only panic for kernel-mode #UD (genuine kernel bug)
    panic!("Invalid opcode (kernel mode)");
}

extern "x86-interrupt" fn device_not_available_handler(stack_frame: InterruptStackFrame) {
    crate::serial_println!("[EXCEPTION] #NM Device not available at {:?}", stack_frame.instruction_pointer);
}

extern "x86-interrupt" fn double_fault_handler(stack_frame: InterruptStackFrame, _error_code: u64) -> ! {
    use x86_64::registers::control::Cr2;
    // KPTI: restore kernel CR3 before any data access
    unsafe {
        let kcr3 = crate::arch::x86_64::syscall::get_kernel_cr3();
        if kcr3 != 0 { core::arch::asm!("mov cr3, {}", in(reg) kcr3, options(nostack)); }
    }
    let cr2 = Cr2::read().as_u64();
    let cr3: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)); }
    let gsb: u64;
    let kgsb: u64;
    unsafe {
        let (gslo, gshi): (u32, u32);
        core::arch::asm!("rdmsr", in("ecx") 0xC000_0101u32,
            out("eax") gslo, out("edx") gshi, options(nomem, nostack));
        gsb = ((gshi as u64) << 32) | (gslo as u64);
        let (klo, khi): (u32, u32);
        core::arch::asm!("rdmsr", in("ecx") 0xC000_0102u32,
            out("eax") klo, out("edx") khi, options(nomem, nostack));
        kgsb = ((khi as u64) << 32) | (klo as u64);
    }
    crate::serial_println!("[DF] DOUBLE FAULT rip={:?} rsp=0x{:X} cs=0x{:X} rflags=0x{:X}",
        stack_frame.instruction_pointer, stack_frame.stack_pointer.as_u64(),
        stack_frame.code_segment, stack_frame.cpu_flags);
    crate::serial_println!("[DF] CR2=0x{:X} CR3=0x{:X} GS_BASE=0x{:X} KERNEL_GS_BASE=0x{:X}",
        cr2, cr3, gsb, kgsb);
    panic!("Double fault - possible stack overflow or PF-in-PF cascade");
}

extern "x86-interrupt" fn invalid_tss_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    crate::serial_println!("[EXCEPTION] #TS Invalid TSS (code {}) at {:?}", error_code, stack_frame.instruction_pointer);
    panic!("Invalid TSS");
}

extern "x86-interrupt" fn segment_not_present_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    crate::serial_println!("[EXCEPTION] #NP Segment not present (code {}) at {:?}", error_code, stack_frame.instruction_pointer);
    panic!("Segment not present");
}

extern "x86-interrupt" fn stack_segment_fault_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    crate::serial_println!("[EXCEPTION] #SS Stack segment fault (code {}) at {:?}", error_code, stack_frame.instruction_pointer);
    panic!("Stack segment fault");
}

extern "x86-interrupt" fn general_protection_fault_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    // KPTI: restore kernel CR3 before any data access
    unsafe {
        let kcr3 = crate::arch::x86_64::syscall::get_kernel_cr3();
        if kcr3 != 0 { core::arch::asm!("mov cr3, {}", in(reg) kcr3, options(nostack)); }
    }
    // Check if this is a Ring 3 fault (CS selector in stack frame has RPL=3)
    let cs = stack_frame.code_segment;
    let is_ring3 = (cs & 0x3) == 3;

    crate::serial_println!(
        "[EXCEPTION] #GP code=0x{:X} rip={:?} ring3={}",
        error_code, stack_frame.instruction_pointer, is_ring3
    );

    if is_ring3 {
        let current_pid = crate::scheduler::current_pid();
        if current_pid != 0 {
            kill_user_and_switch(current_pid, stack_frame.instruction_pointer.as_u64());
            // kill_user_and_switch never returns if another process exists
        }
    }

    panic!("General protection fault (kernel) code=0x{:X}", error_code);
}

// ===== Page Fault Handler with Demand Paging =====
//
// User stack demand-paging range: 0x7FFF_0000_0000 - 0x7FFF_FFFF_F000
// This range covers the 8 MiB user stack virtual region.
// If a page fault comes from Ring 3 (USER_MODE bit set in error code) and
// the faulting address is within this range, we:
//   1. Allocate a physical frame from the ELF frame pool
//   2. Map it with USER_ACCESSIBLE | WRITABLE | NO_EXECUTE flags
//   3. Return to resume execution (the CPU will retry the faulting instruction)
// Otherwise, log and kill the process (SIGSEGV equivalent).

/// User stack demand-paging lower bound
const USER_STACK_DEMAND_LOW: u64 = 0x7FFF_0000_0000;
/// User stack demand-paging upper bound (exclusive)
const USER_STACK_DEMAND_HIGH: u64 = 0x8000_0000_0000; // Jalon 102: extended to cover guard page at 0x7FFFFFFFF000
/// User heap demand-paging lower bound (sys_brk region)
const USER_HEAP_DEMAND_LOW: u64  = 0x0000_3000_0000_0000;
/// User heap demand-paging upper bound (8 GiB — Jalon 68)
/// Expanded from 256 MiB to 8 GiB to support Mistral 7B model loading.
const USER_HEAP_DEMAND_HIGH: u64 = 0x0000_3002_0000_0000;

/// Helper: try to demand-map a user page at `page_addr`.
/// Returns true if the page was successfully mapped.
fn try_demand_map_user_page(page_addr: u64, is_instruction_fetch: bool) -> bool {
    let frame_phys = unsafe { crate::elf::alloc_demand_frame() };
    match frame_phys {
        Some(phys) => {
            let phys_offset = crate::elf::phys_offset();
            unsafe {
                core::ptr::write_bytes((phys + phys_offset) as *mut u8, 0, 4096);
            }
            let cr3: u64;
            unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)); }
            let pml4_phys = cr3 & !0xFFF;
            // Jalon 96: If this is an instruction fetch, map as executable (no NX)
            // Otherwise, map as data (WRITABLE + NX for W^X enforcement)
            let flags: u64 = if is_instruction_fetch {
                0x01 | 0x04 // PRESENT | USER (executable, read-only)
            } else {
                0x01 | 0x02 | 0x04 | (1u64 << 63) // PRESENT | WRITABLE | USER | NX
            };
            match unsafe { crate::elf::demand_map_user_page(pml4_phys, page_addr, phys, flags) } {
                Ok(()) => true,
                Err(_) => {
                    crate::serial_println!("[PF-DEMAND] FATAL: map failed at 0x{:X}", page_addr);
                    false
                }
            }
        }
        None => {
            crate::serial_println!("[PF-DEMAND] FATAL: Out of frames");
            false
        }
    }
}

/// Helper: validate that a saved RIP is in a valid userspace range.
/// Supports both AetherionOS ELF range (0x8000000000+) and Linux ABI range (0x400000+).
#[inline]
fn is_valid_user_rip(rip: u64) -> bool {
    // AetherionOS native ELF range
    (rip >= 0x0000_0080_0000_0000 && rip < 0x0000_0090_0000_0000)
    // Linux ABI ELF range (busybox, musl, etc.)
    || (rip >= 0x0000_0000_0040_0000 && rip < 0x0000_0080_0000_0000)
}

/// Helper: kill current Ring 3 process and IRETQ to next ready one.
/// Never returns if a next process is found.
///
/// JALON 69 FIX: Properly terminates the process, then uses the scheduler's
/// yield_to_next() which correctly restores CR3, RIP, RSP and performs IRETQ.
/// Validates that the next process's saved_rip is in valid userspace range.
///
/// Jalon 109 FIX: Defer page table freeing to avoid cross-core corruption.
/// On SMP, another core may still reference the terminated process's PML4.
/// Instead of freeing immediately, we simply mark the process Terminated and
/// let the next full ELF load reclaim frames.
fn kill_user_and_switch(current_pid: u64, addr_raw: u64) {
    let _ = crate::process::set_state(current_pid, crate::process::ProcessState::Terminated);
    crate::serial_println!("[SIGSEGV] PID {} terminated (addr 0x{:X})", current_pid, addr_raw);

    // ── Jalon 132: Resume parent via sysretq if it's Blocked in sys_wait ──
    // When a forked child is killed by SIGSEGV, the parent is Blocked in sys_wait.
    // We must restore the parent's FULL register context (all callee-saved regs)
    // and return the wait result (child PID << 16 | exit_code) via RAX.
    // This mirrors the forked-child-exit path in sys_exit (Jalon 25) exactly.
    let parent_pid = crate::process::with_process(current_pid, |p| p.ppid).unwrap_or(0);
    crate::serial_println!("[SIGSEGV-J132] child={} ppid={}", current_pid, parent_pid);
    if parent_pid > 1 {
        let parent_state = crate::process::get_state(parent_pid);
        let parent_blocked = parent_state == Some(crate::process::ProcessState::Blocked);
        crate::serial_println!("[SIGSEGV-J132] parent {} state={:?} blocked={}", parent_pid, parent_state, parent_blocked);
        if parent_blocked {
            // Get parent's saved context (set by sys_wait before launching child)
            let parent_ctx = crate::process::with_process(parent_pid, |p| {
                (p.saved_user_rip, p.saved_user_rsp, p.saved_kernel_rsp,
                 p.saved_syscall_regs, p.pml4_phys)
            });
            if let Some((_saved_rip, saved_rsp, _krsp, saved_regs, pml4)) = parent_ctx {
                // saved_regs[7] = rcx = the RIP to return to (after the syscall instruction)
                // saved_regs[6] = r11 = RFLAGS
                if saved_regs[7] != 0 && pml4 != 0 {
                    // ── PRIMARY PATH: sysretq (matches sys_exit exactly) ──
                    let _ = crate::process::set_state(
                        parent_pid,
                        crate::process::ProcessState::Running,
                    );
                    crate::scheduler::set_current_pid(parent_pid);

                    // Build wait result: (child_pid << 16) | (exit_code = SIGSEGV = 11)
                    let wait_result = ((current_pid & 0xFFFF) << 16) | (11u64 & 0xFFFF);

                    let r15 = saved_regs[0];
                    let r14 = saved_regs[1];
                    let r13 = saved_regs[2];
                    let r12 = saved_regs[3];
                    let rbx = saved_regs[4];
                    let rbp = saved_regs[5];
                    let r11 = saved_regs[6]; // RFLAGS for sysretq
                    let rcx = saved_regs[7]; // RIP for sysretq

                    crate::serial_println!(
                        "[SIGSEGV-J132] Resuming parent PID {} via sysretq: RAX=0x{:X} RCX(RIP)=0x{:X} RSP=0x{:X} PML4=0x{:X}",
                        parent_pid, wait_result, rcx, saved_rsp, pml4
                    );

                    // Set up GS: we need GS=PER_CPU now (for kernel use), and after
                    // swapgs GS=0 (user). reset_gs_bases sets GS=0, KERNEL_GS=PER_CPU,
                    // so we need an extra swapgs to make GS=PER_CPU before the asm block.
                    let core_id = crate::arch::x86_64::apic::current_core() as u8;
                    crate::arch::x86_64::syscall::reset_gs_bases_for_core(core_id);

                    // Write parent's user RSP into PER_CPU struct (static mut, not GS-relative)
                    unsafe {
                        crate::arch::x86_64::syscall::set_per_cpu_user_rsp(saved_rsp);
                    }

                    // Switch to parent's address space and sysretq.
                    // CRITICAL: We use the IRETQ approach (like sys_exit fallback) instead
                    // of sysretq to avoid register-clobbering issues in the inline asm.
                    // IRETQ pops RIP, CS, RFLAGS, RSP, SS from the stack, so we push them.
                    //
                    // We also need to restore callee-saved regs. We do this before the IRETQ
                    // using a staged approach: first push IRETQ frame, then restore regs, then IRETQ.
                    //
                    // Use read_volatile to prevent optimizer from reusing registers.
                    let f_pml4 = unsafe { core::ptr::read_volatile(&pml4) };
                    let f_rsp  = unsafe { core::ptr::read_volatile(&saved_rsp) };
                    let f_rip  = unsafe { core::ptr::read_volatile(&rcx) }; // rcx = saved user RIP
                    let f_rax  = unsafe { core::ptr::read_volatile(&wait_result) };

                    unsafe {
                        // Phase 1: Switch CR3 and build IRETQ frame on kernel stack
                        core::arch::asm!(
                            "cli",
                            "mov cr3, {pml4}",   // Switch to parent's page tables
                            "mov rax, {rax_val}", // Pre-load RAX with wait result
                            "push 0x1B",          // SS (Ring 3 data)
                            "push {rsp_val}",     // RSP (parent user stack)
                            "push 0x202",         // RFLAGS (IF=1)
                            "push 0x23",          // CS (Ring 3 code)
                            "push {rip_val}",     // RIP (parent return addr)
                            pml4 = in(reg) f_pml4,
                            rsp_val = in(reg) f_rsp,
                            rip_val = in(reg) f_rip,
                            rax_val = in(reg) f_rax,
                            options(preserves_flags),
                        );
                        // Phase 2: Restore callee-saved registers from saved_regs
                        core::arch::asm!(
                            "mov r15, {v_r15}",
                            "mov r14, {v_r14}",
                            "mov r13, {v_r13}",
                            "mov r12, {v_r12}",
                            "mov rbx, {v_rbx}",
                            "mov rbp, {v_rbp}",
                            v_r15 = in(reg) r15,
                            v_r14 = in(reg) r14,
                            v_r13 = in(reg) r13,
                            v_r12 = in(reg) r12,
                            v_rbx = in(reg) rbx,
                            v_rbp = in(reg) rbp,
                        );
                        // Phase 3: swapgs + iretq (GS=0 currently, KERNEL_GS=PER_CPU)
                        // After swapgs: GS=PER_CPU, KERNEL_GS=0.
                        // On next syscall entry, the CPU swapgs again → GS=0, KERNEL_GS=PER_CPU.
                        // Wait — that's wrong. We need: user mode has GS=0 (user).
                        // reset_gs_bases set GS=0, KERNEL_GS=PER_CPU.
                        // swapgs swaps them: GS=PER_CPU, KERNEL_GS=0.
                        // On next syscall: swapgs → GS=0, KERNEL_GS=PER_CPU — WRONG.
                        // So we should NOT swapgs here. GS is already 0 (user) and
                        // KERNEL_GS is PER_CPU (ready for next syscall entry swapgs).
                        core::arch::asm!(
                            "iretq",
                            options(noreturn),
                        );
                    }
                } else {
                    // ── FALLBACK: mark parent Ready for scheduler ──
                    let _ = crate::process::set_state(parent_pid, crate::process::ProcessState::Ready);
                    crate::serial_println!(
                        "[SIGSEGV-J132] Woke parent PID {} (fallback, saved_regs[7]=0x{:X} pml4=0x{:X})",
                        parent_pid, saved_regs[7], pml4
                    );
                }
            } else {
                let _ = crate::process::set_state(parent_pid, crate::process::ProcessState::Ready);
                crate::serial_println!(
                    "[SIGSEGV-J132] Woke parent PID {} (no context available)",
                    parent_pid
                );
            }
        }
    }

    // ── Jalon 109: DEFERRED page table GC ──
    // On SMP, freeing the PML4 immediately is dangerous: another core may
    // still have this PML4 in its CR3 or be walking its page tables.
    // Only free on single-core systems where no other core can reference it.
    if !crate::arch::x86_64::apic::ap_is_alive() {
        let pml4 = crate::process::with_process(current_pid, |p| p.pml4_phys).unwrap_or(0);
        if pml4 != 0 {
            let active_cr3: u64;
            unsafe { core::arch::asm!("mov {}, cr3", out(reg) active_cr3, options(nomem, nostack)); }
            if pml4 != (active_cr3 & !0xFFF) {
                unsafe { crate::elf::free_user_page_table(pml4); }
            }
        }
    }

    // ── Jalon 100: Watchdog — Auto-respawn critical agents ──
    let crashed_name = crate::process::with_process(current_pid, |p| p.name.clone())
        .unwrap_or_else(|| alloc::string::String::new());
    if !crashed_name.is_empty() {
        let respawn_pid = crate::process::watchdog_try_respawn(&crashed_name);
        if respawn_pid != 0 {
            crate::serial_println!(
                "[WATCHDOG] Agent {} (PID {}) crashed -> respawned as PID {}",
                crashed_name, current_pid, respawn_pid
            );
        }
    }

    // ── Jalon 109: NON-NESTING dispatch ──
    // CRITICAL: Do NOT do IRETQ directly from within the page fault handler!
    // A nested page fault during the IRETQ would execute in Ring 0 on the
    // same kernel stack (CPU does NOT reload RSP0 for R0→R0 faults), causing
    // cascading stack growth and an eventual double fault.
    //
    // Instead, find the next process and use scheduler yield_to_next,
    // then IRETQ from a clean kernel stack via the BSP/AP dispatch path.
    // Switch to kernel PML4 first so IRETQ targets are in a known address space.
    let next = crate::scheduler::yield_to_next(current_pid);

    if next != 0 && next != current_pid {
        // Get the next process's saved state
        let (rip, rsp, rfl, cr3) =
            if let Some((saved_rip, saved_rsp, saved_rfl, saved_pml4, _regs)) =
                crate::process::get_preempt_state(next)
            {
                if is_valid_user_rip(saved_rip) {
                    (saved_rip, saved_rsp, saved_rfl, saved_pml4)
                } else if let Some((entry, stack, pml4)) = crate::process::get_entry_state(next) {
                    crate::serial_println!(
                        "[SIGSEGV] PID {} saved_rip=0x{:X} invalid, using entry=0x{:X}",
                        next, saved_rip, entry
                    );
                    (entry, stack, 0x202u64, pml4)
                } else {
                    crate::serial_println!("[SIGSEGV] PID {} has no valid state, idle", next);
                    crate::scheduler::set_current_pid(0);
                    unsafe { core::arch::asm!("sti"); }
                    loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack)); } }
                }
            } else if let Some((entry, stack, pml4)) = crate::process::get_entry_state(next) {
                (entry, stack, 0x202u64, pml4)
            } else {
                crate::serial_println!("[SIGSEGV] PID {} has no entry state, idle", next);
                crate::scheduler::set_current_pid(0);
                unsafe { core::arch::asm!("sti"); }
                loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack)); } }
            };

        if cr3 == 0 || rip == 0 {
            crate::serial_println!("[SIGSEGV] PID {} invalid cr3/rip, idle", next);
            crate::scheduler::set_current_pid(0);
            unsafe { core::arch::asm!("sti"); }
            loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack)); } }
        }

        let _ = crate::process::set_state(next, crate::process::ProcessState::Running);
        crate::scheduler::set_current_pid(next);

        if let Some(name) = crate::process::with_process(next, |p| p.name.clone()) {
            crate::serial_println!(
                "[SIGSEGV] -> PID {} ({}) rip=0x{:X} rsp=0x{:X}",
                next, name, rip, rsp
            );
        }

        // Reset GS bases before IRETQ to Ring 3 (use per-core variant for SMP)
        let core_id = crate::arch::x86_64::apic::current_core() as u8;
        crate::arch::x86_64::syscall::reset_gs_bases_for_core(core_id);

        // ── Jalon 109: Non-nesting IRETQ via syscall stack ──
        // We read the kernel syscall RSP from the per-CPU struct directly
        // (via its known virtual address) rather than using gs:[0], because
        // the current GS base may point at user-space (GS and KERNEL_GS_BASE
        // were just reset by reset_gs_bases_for_core — KERNEL_GS_BASE is
        // correct but GS_BASE may still be 0/user until the next swapgs).
        let syscall_rsp = crate::arch::x86_64::syscall::get_kernel_rsp_for_core(core_id);
        if syscall_rsp == 0 {
            // Fallback: no valid syscall stack — idle
            crate::serial_println!("[SIGSEGV] No kernel RSP for core {}, idle", core_id);
            crate::scheduler::set_current_pid(0);
            unsafe { core::arch::asm!("sti"); }
            loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack)); } }
        }

        // Jalon 109c: FORCE explicit GPR allocation to prevent LLVM from
        // coalescing registers. Generic in(reg) allows the optimizer to
        // assign the same physical register to cr3 and rip, causing the
        // PML4 physical address to be pushed as the return RIP.
        // Hardcoded: r8=kstack, r9=cr3, r10=rsp, r11=rflags, r12=rip
        let final_rip = unsafe { core::ptr::read_volatile(&rip) };
        let final_rsp = unsafe { core::ptr::read_volatile(&rsp) };
        let final_rfl = unsafe { core::ptr::read_volatile(&rfl) };
        let final_cr3 = unsafe { core::ptr::read_volatile(&cr3) };
        let final_kstack = unsafe { core::ptr::read_volatile(&syscall_rsp) };

        unsafe {
            core::arch::asm!(
                "cli",
                // Load a fresh kernel stack (r8 = kernel stack top)
                "mov rsp, r8",
                // Switch to the target process address space (r9 = PML4)
                "mov cr3, r9",
                // Build IRETQ frame on the clean syscall stack
                "push 0x1B",           // SS (Ring 3 data)
                "push r10",            // RSP (user stack, guaranteed in r10)
                "push r11",            // RFLAGS (guaranteed in r11)
                "push 0x23",           // CS (Ring 3 code)
                "push r12",            // RIP (entry point, guaranteed in r12)
                "iretq",
                in("r8") final_kstack,
                in("r9") final_cr3,
                in("r10") final_rsp,
                in("r11") final_rfl,
                in("r12") final_rip,
                options(noreturn),
            );
        }
    }

    // No other Ready process — enter kernel idle loop
    // Enable interrupts so the APIC timer can wake us and dispatch new work.
    crate::serial_println!("[SIGSEGV] No other process ready, idle");
    crate::scheduler::set_current_pid(0);
    unsafe { core::arch::asm!("sti"); }
    loop { unsafe { core::arch::asm!("hlt", options(nomem, nostack)); } }
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    // KPTI: Ensure we are running with kernel CR3. If a fault occurs after
    // CR3 was switched to the user PML4 (e.g. during IRETQ), the IST handler
    // would otherwise run with user page tables and be unable to access kernel
    // data structures. Switch back to kernel PML4 (0x1000) immediately.
    unsafe {
        let cr3: u64;
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
        let kernel_cr3 = crate::arch::x86_64::syscall::get_kernel_cr3();
        if kernel_cr3 != 0 && cr3 != kernel_cr3 {
            core::arch::asm!("mov cr3, {}", in(reg) kernel_cr3, options(nostack));
        }
    }

    let accessed_address = Cr2::read();
    let addr_raw = accessed_address.as_u64();
    let is_user_mode = error_code.contains(PageFaultErrorCode::USER_MODE);
    let page_addr = addr_raw & !0xFFF;

    // Jalon 134b: Deep forensic diagnostic for low-address (NULL-page) kernel-mode
    // page faults — these are typically caused by GS-base misconfiguration in the
    // SYSCALL entry path (stale swapgs, corrupted KERNEL_GS_BASE MSR, etc.).
    // We log CS:RIP, CR2, error_code, CR3, GS_BASE, KERNEL_GS_BASE once per boot.
    // Jalon 134c: extended to ALL kernel-mode faults so we capture CR2 top-of-space
    // faults (e.g., 0xFFFFFFFFFFFFFFF0 stack underflow) too.
    if !is_user_mode {
        // J134c: log up to 8 kernel-mode faults to detect cascades
        static DEEP_COUNT: core::sync::atomic::AtomicU32 =
            core::sync::atomic::AtomicU32::new(0);
        let n = DEEP_COUNT.fetch_add(1, core::sync::atomic::Ordering::SeqCst);
        if n < 8 {
            let rip = stack_frame.instruction_pointer.as_u64();
            let cs  = stack_frame.code_segment;
            let rfl = stack_frame.cpu_flags;
            let rsp = stack_frame.stack_pointer.as_u64();
            let err = error_code.bits();
            let cr3: u64;
            unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)); }
            // Read GS_BASE (IA32_GS_BASE = 0xC0000101) and KERNEL_GS_BASE (0xC0000102)
            let gsb: u64;
            let kgsb: u64;
            unsafe {
                let (gslo, gshi): (u32, u32);
                core::arch::asm!("rdmsr", in("ecx") 0xC000_0101u32,
                    out("eax") gslo, out("edx") gshi, options(nomem, nostack));
                gsb = ((gshi as u64) << 32) | (gslo as u64);
                let (klo, khi): (u32, u32);
                core::arch::asm!("rdmsr", in("ecx") 0xC000_0102u32,
                    out("eax") klo, out("edx") khi, options(nomem, nostack));
                kgsb = ((khi as u64) << 32) | (klo as u64);
            }
            // J134c: also capture the handler's own RSP - if IST1 fired, we
            // should be running on PF_STACK (around IST1 top in GDT diag).
            let my_rsp: u64;
            unsafe { core::arch::asm!("mov {}, rsp", out(reg) my_rsp, options(nomem, nostack)); }
            crate::serial_println!(
                "[PF-DEEP #{}] addr=0x{:X} err=0x{:X} CS=0x{:X} RIP=0x{:X} RFL=0x{:X} RSP(saved)=0x{:X} RSP(live)=0x{:X}",
                n, addr_raw, err, cs, rip, rfl, rsp, my_rsp
            );
            crate::serial_println!(
                "[PF-DEEP] CR2=0x{:X} CR3=0x{:X} GS_BASE=0x{:X} KERNEL_GS_BASE=0x{:X}",
                addr_raw, cr3, gsb, kgsb
            );
            let pid = crate::scheduler::current_pid();
            crate::serial_println!("[PF-DEEP] PID={} user_mode={}", pid, is_user_mode);

            // J134c: Page-table walk for the faulting RIP to detect missing mappings
            // in the current CR3. We translate RIP virtual -> physical using CR3.
            unsafe {
                let phys_off: u64 = 0xFFFF_8000_0000_0000;
                let pml4_phys = cr3 & !0xFFF;
                let walk_va = rip;
                let pml4_i = (walk_va >> 39) & 0x1FF;
                let pdpt_i = (walk_va >> 30) & 0x1FF;
                let pd_i   = (walk_va >> 21) & 0x1FF;
                let pt_i   = (walk_va >> 12) & 0x1FF;
                let pml4_e = core::ptr::read_volatile(
                    (pml4_phys + phys_off + pml4_i * 8) as *const u64);
                crate::serial_println!(
                    "[PF-WALK] RIP=0x{:X} PML4[{}]=0x{:X}",
                    walk_va, pml4_i, pml4_e
                );
                if pml4_e & 0x1 != 0 {
                    let pdpt_phys = pml4_e & 0x000F_FFFF_FFFF_F000;
                    let pdpt_e = core::ptr::read_volatile(
                        (pdpt_phys + phys_off + pdpt_i * 8) as *const u64);
                    crate::serial_println!("[PF-WALK] PDPT[{}]=0x{:X}", pdpt_i, pdpt_e);
                    if pdpt_e & 0x1 != 0 && pdpt_e & 0x80 == 0 {
                        let pd_phys = pdpt_e & 0x000F_FFFF_FFFF_F000;
                        let pd_e = core::ptr::read_volatile(
                            (pd_phys + phys_off + pd_i * 8) as *const u64);
                        crate::serial_println!("[PF-WALK] PD[{}]=0x{:X}", pd_i, pd_e);
                        if pd_e & 0x1 != 0 && pd_e & 0x80 == 0 {
                            let pt_phys = pd_e & 0x000F_FFFF_FFFF_F000;
                            let pt_e = core::ptr::read_volatile(
                                (pt_phys + phys_off + pt_i * 8) as *const u64);
                            crate::serial_println!("[PF-WALK] PT[{}]=0x{:X}", pt_i, pt_e);
                        }
                    }
                }
                // J135: Full page-table walk for CR2 (the faulting data address)
                // to detect exactly which level is missing. We also dump 16 bytes
                // of instructions at RIP to verify the CPU actually decoded our
                // expected swapgs/mov sequence.
                if addr_raw < 0x0001_0000_0000_0000 || addr_raw >= 0xFFFF_0000_0000_0000 {
                    let va = addr_raw;
                    let i4 = (va >> 39) & 0x1FF;
                    let i3 = (va >> 30) & 0x1FF;
                    let i2 = (va >> 21) & 0x1FF;
                    let i1 = (va >> 12) & 0x1FF;
                    let pml4_e = core::ptr::read_volatile(
                        (pml4_phys + phys_off + i4 * 8) as *const u64);
                    crate::serial_println!("[PF-WALK-CR2] CR2=0x{:X} PML4[{}]=0x{:X}", va, i4, pml4_e);
                    if pml4_e & 0x1 != 0 {
                        let pdpt_phys = pml4_e & 0x000F_FFFF_FFFF_F000;
                        let pdpt_e = core::ptr::read_volatile(
                            (pdpt_phys + phys_off + i3 * 8) as *const u64);
                        crate::serial_println!("[PF-WALK-CR2] PDPT[{}]=0x{:X}", i3, pdpt_e);
                        if pdpt_e & 0x1 != 0 && pdpt_e & 0x80 == 0 {
                            let pd_phys = pdpt_e & 0x000F_FFFF_FFFF_F000;
                            let pd_e = core::ptr::read_volatile(
                                (pd_phys + phys_off + i2 * 8) as *const u64);
                            crate::serial_println!("[PF-WALK-CR2] PD[{}]=0x{:X}", i2, pd_e);
                            if pd_e & 0x1 != 0 && pd_e & 0x80 == 0 {
                                let pt_phys = pd_e & 0x000F_FFFF_FFFF_F000;
                                let pt_e = core::ptr::read_volatile(
                                    (pt_phys + phys_off + i1 * 8) as *const u64);
                                crate::serial_println!("[PF-WALK-CR2] PT[{}]=0x{:X}", i1, pt_e);
                            }
                        }
                    }
                }

                // J135: dump 16 bytes of code at RIP to verify the CPU decoded
                // our expected NOP+swapgs+mov sequence. We use the physical-offset
                // mapping to read, which is always accessible in kernel mode.
                let rip_phys = rip.wrapping_sub(phys_off); // assumes kernel phys-offset mapping
                let rip_ptr = rip as *const u8;
                let b0 = core::ptr::read_volatile(rip_ptr);
                let b1 = core::ptr::read_volatile(rip_ptr.add(1));
                let b2 = core::ptr::read_volatile(rip_ptr.add(2));
                let b3 = core::ptr::read_volatile(rip_ptr.add(3));
                let b4 = core::ptr::read_volatile(rip_ptr.add(4));
                let b5 = core::ptr::read_volatile(rip_ptr.add(5));
                let b6 = core::ptr::read_volatile(rip_ptr.add(6));
                let b7 = core::ptr::read_volatile(rip_ptr.add(7));
                crate::serial_println!(
                    "[PF-CODE] RIP=0x{:X} (phys=0x{:X}) bytes: {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                    rip, rip_phys, b0, b1, b2, b3, b4, b5, b6, b7
                );

                // J135: Also dump the raw error code fields for clarity.
                //   bit 0 (P):    0=not-present, 1=protection-violation
                //   bit 1 (W):    0=read, 1=write
                //   bit 2 (U):    0=supervisor, 1=user-mode access
                //   bit 3 (R):    1=reserved-bit violation
                //   bit 4 (I/D):  1=instruction-fetch fault
                let b_p = err & 1;
                let b_w = (err >> 1) & 1;
                let b_u = (err >> 2) & 1;
                let b_r = (err >> 3) & 1;
                let b_i = (err >> 4) & 1;
                crate::serial_println!(
                    "[PF-ERR-DECODE] P={} W={} U={} RSVD={} I/D={} (err=0x{:X})",
                    b_p, b_w, b_u, b_r, b_i, err
                );
            }
        }
    }

    // --- Demand paging for user stack ---
    if is_user_mode
        && addr_raw >= USER_STACK_DEMAND_LOW
        && addr_raw < USER_STACK_DEMAND_HIGH
    {
        if try_demand_map_user_page(page_addr, false) {
            return; // Resume faulting instruction
        }
        // Fall through to SIGSEGV
    }

    // --- Demand paging for user heap (within break limit) ---
    if is_user_mode
        && addr_raw >= USER_HEAP_DEMAND_LOW
        && addr_raw < USER_HEAP_DEMAND_HIGH
    {
        let current_pid = crate::scheduler::current_pid();
        let heap_break = crate::process::get_heap_break(current_pid)
            .unwrap_or(USER_HEAP_DEMAND_LOW);
        // Only map if address is below the current break (valid heap)
        if addr_raw < heap_break {
            if try_demand_map_user_page(page_addr, false) {
                return; // Resume
            }
        }
        // Fall through to SIGSEGV
    }

    // --- Demand paging for ELF BSS/Data region (Jalon 93: prevent SIGSEGV on BSS extension) ---
    // ELF binaries are mapped starting at 0x8000000000. The BSS section may extend
    // beyond the file-backed pages. When the allocator touches a BSS page that wasn't
    // pre-mapped, we demand-map it here instead of killing the process.
    // Jalon 96 fix: detect instruction-fetch faults to avoid mapping code pages as NX.
    const ELF_DEMAND_LOW: u64 = 0x0000_0080_0000_0000; // 0x8000000000
    const ELF_DEMAND_HIGH: u64 = 0x0000_0080_1000_0000; // +256 MiB guard
    if is_user_mode
        && addr_raw >= ELF_DEMAND_LOW
        && addr_raw < ELF_DEMAND_HIGH
    {
        let is_ifetch = error_code.contains(PageFaultErrorCode::INSTRUCTION_FETCH);
        if try_demand_map_user_page(page_addr, is_ifetch) {
            return; // Resume — BSS/code page now mapped
        }
        // Fall through to SIGSEGV
    }

    // --- Demand paging for VMA-backed regions (Jalon 68/76/91: Zero-Copy Model Loading + Readahead) ---
    // Check if the faulting address is in a file-backed VMA.
    // If so, allocate a frame, read the file data into it, and map it.
    // Jalon 91 enhancement: readahead up to READAHEAD_PAGES adjacent pages
    // to amortize page-fault overhead and reduce future faults during sequential access.
    // Supports: in-memory VFS files (/sys/*, /bin/*), exFAT, and FAT32 on /disk/.
    if is_user_mode {
        let current_pid = crate::scheduler::current_pid();
        if current_pid != 0 {
            if let Some((file_path, file_offset, writable)) = crate::process::find_vma(current_pid, addr_raw) {
                // Readahead: map the faulting page + up to 7 adjacent pages
                // This reduces page-fault overhead for sequential model weight access
                const READAHEAD_PAGES: u64 = 8; // 32 KiB readahead window
                
                let cr3: u64;
                unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)); }
                let pml4_phys = cr3 & !0xFFF;
                
                let mut mapped_count: u64 = 0;
                
                for ra_idx in 0..READAHEAD_PAGES {
                    let ra_page_addr = page_addr + ra_idx * 4096;
                    
                    // Check this readahead page is still within the VMA
                    if let Some((ra_path, ra_file_offset, ra_writable)) = crate::process::find_vma(current_pid, ra_page_addr) {
                        // Allocate a physical frame
                        let frame_phys = unsafe { crate::elf::alloc_demand_frame() };
                        if let Some(phys) = frame_phys {
                            let phys_offset = crate::elf::phys_offset();
                            let buf_ptr = (phys + phys_offset) as *mut u8;
                            
                            // Zero the frame first
                            unsafe { core::ptr::write_bytes(buf_ptr, 0, 4096); }
                            
                            // Read 4 KB from the file at the correct offset
                            let page_buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr, 4096) };
                            
                            // Priority 1: Try in-memory VFS (for /sys/*, /bin/*, etc.)
                            let mut bytes_read = crate::fs::vfs::file_read_at_offset(&ra_path, ra_file_offset, page_buf);
                            
                            // Priority 2: Try exFAT (for /disk/ paths)
                            if bytes_read == 0 && ra_path.starts_with("/disk/") {
                                if crate::fs::exfat::is_mounted() {
                                    let name = &ra_path[6..];
                                    bytes_read = crate::fs::exfat::read_file(name, ra_file_offset, page_buf);
                                }
                            }
                            
                            // Priority 3: FAT32 fallback
                            if bytes_read == 0 && ra_path.starts_with("/disk/") {
                                let _ = crate::fs::fat32::read_file_at_offset(&ra_path, ra_file_offset, page_buf)
                                    .unwrap_or(0);
                            }
                            
                            // Map the page (even if bytes_read < 4096, rest is zeroed)
                            let mut flags: u64 = 0x01 | 0x04 | (1u64 << 63); // PRESENT | USER | NX
                            if ra_writable { flags |= 0x02; }
                            
                            match unsafe { crate::elf::demand_map_user_page(pml4_phys, ra_page_addr, phys, flags) } {
                                Ok(()) => {
                                    // Flush TLB for this page
                                    unsafe {
                                        core::arch::asm!("invlpg [{}]", in(reg) ra_page_addr, options(nostack));
                                    }
                                    mapped_count += 1;
                                }
                                Err(_) => {
                                    // Page might already be mapped (readahead overlap), skip
                                    break;
                                }
                            }
                        } else {
                            // Out of frames, stop readahead
                            break;
                        }
                    } else {
                        // Past end of VMA, stop readahead
                        break;
                    }
                }
                
                if mapped_count > 0 {
                    return; // Resume execution — faulting page is now mapped
                }
                // Fall through to SIGSEGV if nothing was mapped
            }
        }
    }

    // --- Non-recoverable page fault ---
    if is_user_mode {
        let current_pid = crate::scheduler::current_pid();
        let is_write = error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE);
        let is_present = error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION);
        crate::serial_println!(
            "[SIGSEGV] PID {} addr=0x{:X} rip={:?} write={} present={} code={:?}",
            current_pid, addr_raw,
            stack_frame.instruction_pointer, is_write, is_present, error_code
        );
        if current_pid != 0 {
            kill_user_and_switch(current_pid, addr_raw);
            // kill_user_and_switch never returns if another process exists
        }
    }

    // Jalon 102: Kernel-mode page fault at user-visible address ranges
    // can happen during IRETQ to Ring 3 or when the kernel copies data to user buffers.
    // Try to demand-map the page instead of panicking.

    // User stack region (including guard page area)
    if !is_user_mode && addr_raw >= USER_STACK_DEMAND_LOW && addr_raw < USER_STACK_DEMAND_HIGH {
        if try_demand_map_user_page(page_addr, false) {
            return; // Resume IRETQ — user stack page now mapped
        }
    }

    // ELF BSS/Data region — kernel may write to BSS during ELF loading
    // Jalon 109: Also handles IRETQ instruction-fetch faults.  Detect IFETCH
    // from the error code so we set +X instead of NX.
    if !is_user_mode && addr_raw >= ELF_DEMAND_LOW && addr_raw < ELF_DEMAND_HIGH {
        let is_ifetch = error_code.contains(PageFaultErrorCode::INSTRUCTION_FETCH);
        if try_demand_map_user_page(page_addr, is_ifetch) {
            return;
        }
    }

    // User heap region — kernel may fault during sys_write copying to user buffer
    if !is_user_mode && addr_raw >= USER_HEAP_DEMAND_LOW && addr_raw < USER_HEAP_DEMAND_HIGH {
        if try_demand_map_user_page(page_addr, false) {
            return;
        }
    }

    // Jalon 109+133: Kernel-mode fault at a user-visible address is NOT a true
    // kernel panic — it happens during IRETQ when the CPU pushes to the
    // user stack or fetches the first user-mode instruction.  Treat it as
    // SIGSEGV (kill the offending process) instead of halting the core.
    //
    // Jalon 133: Log NULL-pointer kernel bugs (addr < 0x1000) with a warning
    // so they are visible in the serial log, but still kill the process.
    {
        let in_user_stack  = addr_raw >= USER_STACK_DEMAND_LOW && addr_raw < USER_STACK_DEMAND_HIGH;
        let in_elf_region  = addr_raw >= ELF_DEMAND_LOW && addr_raw < ELF_DEMAND_HIGH;
        let in_user_heap   = addr_raw >= USER_HEAP_DEMAND_LOW && addr_raw < USER_HEAP_DEMAND_HIGH;
        // Addresses below 0x0000_8000_0000_0000 are canonical user addresses
        let is_user_addr   = addr_raw < 0x0000_8000_0000_0000;

        if !is_user_mode && (in_user_stack || in_elf_region || in_user_heap || is_user_addr) {
            let current_pid = crate::scheduler::current_pid();
            if current_pid != 0 {
                if addr_raw < 0x1000 {
                    crate::serial_println!(
                        "[PF-NULLPTR] PID {} KERNEL NULL deref addr=0x{:X} rip={:?} (KERNEL BUG!)",
                        current_pid, addr_raw, stack_frame.instruction_pointer
                    );
                } else {
                    crate::serial_println!(
                        "[PF-IRETQ] PID {} addr=0x{:X} rip={:?} (kernel-mode fault at user addr, treating as SIGSEGV)",
                        current_pid, addr_raw, stack_frame.instruction_pointer
                    );
                }
                kill_user_and_switch(current_pid, addr_raw);
                // kill_user_and_switch does not return if another process is ready
            }
        }
    }

    // True kernel-mode page fault (address in kernel space) — log and halt this core only.
    // Jalon 109: Downgraded from panic! to hlt-loop so the other core survives.
    {
        let rip_val = stack_frame.instruction_pointer.as_u64();
        let cs_val = stack_frame.code_segment;
        let err = error_code.bits();
        let hex = b"0123456789ABCDEF";
        let mut msg = [b' '; 128];
        let prefix = b"\n[PF-KERN] CR2=0x";
        msg[..prefix.len()].copy_from_slice(prefix);
        let mut pos = prefix.len();
        // CR2 address (16 hex digits)
        let mut v = addr_raw;
        for i in (0..16).rev() { msg[pos + i] = hex[(v & 0xF) as usize]; v >>= 4; }
        pos += 16;
        let mid1 = b" RIP=0x";
        msg[pos..pos+mid1.len()].copy_from_slice(mid1);
        pos += mid1.len();
        // RIP (16 hex digits)
        v = rip_val;
        for i in (0..16).rev() { msg[pos + i] = hex[(v & 0xF) as usize]; v >>= 4; }
        pos += 16;
        let mid2 = b" CS=0x";
        msg[pos..pos+mid2.len()].copy_from_slice(mid2);
        pos += mid2.len();
        // CS (4 hex digits)
        v = cs_val as u64;
        for i in (0..4).rev() { msg[pos + i] = hex[(v & 0xF) as usize]; v >>= 4; }
        pos += 4;
        let mid3 = b" ERR=0x";
        msg[pos..pos+mid3.len()].copy_from_slice(mid3);
        pos += mid3.len();
        // ERR (4 hex digits)
        v = err;
        for i in (0..4).rev() { msg[pos + i] = hex[(v & 0xF) as usize]; v >>= 4; }
        pos += 4;
        msg[pos] = b'\n';
        pos += 1;
        unsafe { crate::serial_write(core::str::from_utf8_unchecked(&msg[..pos])); }
    }
    // Halt only this core (don't panic! which would kill the entire system)
    crate::serial_println!("[PF-KERN] Core halted (addr=0x{:X})", addr_raw);
    loop { unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)); } }
}

extern "x86-interrupt" fn x87_floating_point_handler(stack_frame: InterruptStackFrame) {
    crate::serial_println!("[EXCEPTION] #MF x87 FPU error at {:?}", stack_frame.instruction_pointer);
}

extern "x86-interrupt" fn alignment_check_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    crate::serial_println!("[EXCEPTION] #AC Alignment check (code {}) at {:?}", error_code, stack_frame.instruction_pointer);
    panic!("Alignment check");
}

extern "x86-interrupt" fn machine_check_handler(stack_frame: InterruptStackFrame) -> ! {
    crate::serial_println!("[EXCEPTION] #MC MACHINE CHECK at {:?}", stack_frame.instruction_pointer);
    panic!("Machine check");
}

extern "x86-interrupt" fn simd_floating_point_handler(stack_frame: InterruptStackFrame) {
    crate::serial_println!("[EXCEPTION] #XF SIMD FP error at {:?}", stack_frame.instruction_pointer);
}

extern "x86-interrupt" fn virtualization_handler(stack_frame: InterruptStackFrame) {
    crate::serial_println!("[EXCEPTION] #VE Virtualization at {:?}", stack_frame.instruction_pointer);
}

extern "x86-interrupt" fn security_exception_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    crate::serial_println!("[EXCEPTION] #SX Security exception (code {}) at {:?}", error_code, stack_frame.instruction_pointer);
    panic!("Security exception");
}

// ===== Preemptive Context Switch State =====
// Global flag for pending context switch from timer interrupt
// The timer handler sets this, and the kernel idle/process loop checks it

use core::sync::atomic::{AtomicU64, AtomicBool, Ordering};

static SHIFT_PRESSED: AtomicBool = AtomicBool::new(false);
static E0_PREFIX: AtomicBool = AtomicBool::new(false);

/// Pending context switch: (new_pid << 32) | old_pid, or 0 if none pending
static PENDING_SWITCH: AtomicU64 = AtomicU64::new(0);
/// Pending switch new RIP
static PENDING_RIP: AtomicU64 = AtomicU64::new(0);
/// Pending switch new RSP  
static PENDING_RSP: AtomicU64 = AtomicU64::new(0);
/// Pending switch new PML4
static PENDING_PML4: AtomicU64 = AtomicU64::new(0);

/// Check if there's a pending context switch and return the info if so
pub fn take_pending_switch() -> Option<(u64, u64, u64, u64, u64)> {
    let val = PENDING_SWITCH.swap(0, Ordering::SeqCst);
    if val == 0 {
        return None;
    }
    let old_pid = val & 0xFFFFFFFF;
    let new_pid = val >> 32;
    let rip = PENDING_RIP.load(Ordering::SeqCst);
    let rsp = PENDING_RSP.load(Ordering::SeqCst);
    let pml4 = PENDING_PML4.load(Ordering::SeqCst);
    Some((old_pid, new_pid, rip, rsp, pml4))
}

/// Set a pending context switch from the timer handler
fn set_pending_switch(old_pid: u64, new_pid: u64, rip: u64, rsp: u64, pml4: u64) {
    PENDING_RIP.store(rip, Ordering::SeqCst);
    PENDING_RSP.store(rsp, Ordering::SeqCst);
    PENDING_PML4.store(pml4, Ordering::SeqCst);
    PENDING_SWITCH.store((new_pid << 32) | old_pid, Ordering::SeqCst);
}

// ===== IRQ Handlers =====

extern "x86-interrupt" fn timer_interrupt_handler(stack_frame: InterruptStackFrame) {
    // KPTI: restore kernel CR3 — timer IRQ may fire while user PML4 is active
    unsafe {
        let kcr3 = crate::arch::x86_64::syscall::get_kernel_cr3();
        if kcr3 != 0 {
            let cr3: u64;
            core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack));
            if cr3 != kcr3 {
                core::arch::asm!("mov cr3, {}", in(reg) kcr3, options(nostack));
            }
        }
    }
    // Jalon 69: Preemptive timer tick with safe context save.
    //
    // CRITICAL FIX: Save the current process's FULL context (RIP, RSP, RFLAGS)
    // from the interrupt stack frame BEFORE calling tick_preemptive().
    // This ensures that if the scheduler decides to switch, the current
    // process's state is already saved and can be restored later.
    //
    // The interrupt stack frame contains the CPU-saved state:
    //   [RSP+0]  = RIP  (instruction that was interrupted)
    //   [RSP+8]  = CS   
    //   [RSP+16] = RFLAGS
    //   [RSP+24] = RSP  (user stack pointer)
    //   [RSP+32] = SS

    let current_pid = crate::scheduler::current_pid();
    
    // Only save preempt state if we're interrupting a real userspace process
    if current_pid != 0 {
        let irq_rip = stack_frame.instruction_pointer.as_u64();
        let irq_rsp = stack_frame.stack_pointer.as_u64();
        let irq_rflags = stack_frame.cpu_flags;
        let cs = stack_frame.code_segment;
        
        // Only save if this was a Ring 3 interrupt (CS RPL == 3)
        // Jalon 109: Accept both AetherionOS ELF range and Linux ABI range
        if (cs & 0x3) == 3 && is_valid_user_rip(irq_rip) {
            crate::process::save_preempt_state(current_pid, irq_rip, irq_rsp, irq_rflags);
        }
    }

    // Let the scheduler decide if a switch is needed
    // Context switches happen cooperatively via sys_yield().
    // The timer ISR saves preempt state so that when a process calls yield,
    // the scheduler knows which process to switch to. True preemptive
    // switching from ISR context requires modifying the IRET frame directly
    // (Phase D future work).
    if let Some((old_pid, new_pid, new_rip, new_rsp, _new_rflags, new_pml4)) =
        crate::scheduler::tick_preemptive()
    {
        if new_pid != old_pid && new_pml4 != 0 && new_rip != 0 {
            set_pending_switch(old_pid, new_pid, new_rip, new_rsp, new_pml4);
        }
    }

    unsafe {
        super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET);
    }
}

/// Check for and execute a pending context switch from timer interrupt.
/// This should be called from syscall exit path or kernel idle loop.
/// Returns true if a switch was performed (does not return to caller).
/// 
/// SAFETY: This function never returns if a switch is performed!
#[inline(never)]
pub fn check_pending_switch() -> bool {
    if let Some((_old_pid, _new_pid, new_rip, new_rsp, new_pml4)) = take_pending_switch() {
        if new_pml4 != 0 && new_rip != 0 {
            // Jalon 109c: FORCE explicit GPR allocation (r8=CR3, r9=RSP, r10=RIP)
            // Prevents LLVM from coalescing PML4 and RIP into the same register.
            let f_rip = unsafe { core::ptr::read_volatile(&new_rip) };
            let f_rsp = unsafe { core::ptr::read_volatile(&new_rsp) };
            let f_cr3 = unsafe { core::ptr::read_volatile(&new_pml4) };
            // Perform the actual context switch via IRETQ.
            // Jalon 134b: DO NOT swapgs before iretq.
            // At this point in kernel context, GS_BASE=0 (reset_gs_bases'd) and
            // KERNEL_GS_BASE=&PER_CPU. On entering Ring 3, GS_BASE remains 0,
            // KERNEL_GS_BASE stays &PER_CPU. The next SYSCALL entry will
            // swapgs once, correctly making GS=&PER_CPU for kernel access.
            // A swapgs HERE would invert the roles, leaving GS=&PER_CPU in
            // Ring 3 and then the next syscall would swapgs again, setting
            // GS=0 in kernel mode → every gs:[N] access faults with CR2=N.
            // This was the root cause of the [PF-NULLPTR] addr=0xC/0x18 bug.
            unsafe {
                core::arch::asm!(
                    "cli",
                    "mov cr3, r8",              // Switch page tables (r8 = PML4)
                    "push 0x1B",                // SS (Ring 3 data)
                    "push r9",                  // RSP (user stack, guaranteed in r9)
                    "push 0x202",               // RFLAGS (IF=1)
                    "push 0x23",                // CS (Ring 3 code)
                    "push r10",                 // RIP (entry point, guaranteed in r10)
                    "iretq",                    // IRETQ to new process
                    in("r8") f_cr3,
                    in("r9") f_rsp,
                    in("r10") f_rip,
                    options(noreturn)
                );
            }
        }
    }
    false
}

/// Keyboard IRQ1 handler — Scancode Set 1 (with 8042 translation ON)
/// JALON 72: Ultra-robust handler without Shift (for now)
extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;
    
    // Read the scancode
    let scancode: u8 = unsafe { Port::new(0x60).read() };

    // Handle E0 prefix (extended keys: arrows, etc.)
    if scancode == 0xE0 {
        E0_PREFIX.store(true, Ordering::Relaxed);
        unsafe { super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1); }
        return;
    }

    let is_e0 = E0_PREFIX.load(Ordering::Relaxed);
    if is_e0 {
        E0_PREFIX.store(false, Ordering::Relaxed);
        // E0 extended key — only handle make codes (bit 7 clear)
        if scancode & 0x80 == 0 {
            let special = match scancode {
                0x48 => 0x01u8, // Up arrow    → SOH
                0x50 => 0x02u8, // Down arrow  → STX
                0x4B => 0x03u8, // Left arrow  → ETX
                0x4D => 0x04u8, // Right arrow → EOT
                _ => 0u8,
            };
            if special != 0 {
                crate::process::kbd_push_byte(special);
            }
        }
        // E0 release codes are silently dropped
        unsafe { super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1); }
        return;
    }

    // Handle Shift press (make codes)
    if scancode == 0x2A || scancode == 0x36 {
        SHIFT_PRESSED.store(true, Ordering::Relaxed);
        unsafe { super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1); }
        return;
    }
    // Handle Shift release (break codes)
    if scancode == 0xAA || scancode == 0xB6 {
        SHIFT_PRESSED.store(false, Ordering::Relaxed);
        unsafe { super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1); }
        return;
    }

    // Jalon 131: Track Ctrl and Alt modifier keys
    // Ctrl make/break (0x1D / 0x9D)
    if scancode == 0x1D || scancode == 0x9D {
        crate::drivers::mouse::update_modifier(0x1D, scancode & 0x80 != 0);
        crate::drivers::mouse::push_key_event(scancode & 0x7F, scancode & 0x80 != 0);
        unsafe { super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1); }
        return;
    }
    // Alt make/break (0x38 / 0xB8)
    if scancode == 0x38 || scancode == 0xB8 {
        crate::drivers::mouse::update_modifier(0x38, scancode & 0x80 != 0);
        crate::drivers::mouse::push_key_event(scancode & 0x7F, scancode & 0x80 != 0);
        unsafe { super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1); }
        return;
    }

    // Ignore all other release codes (bit 7 set) — no logging
    if scancode & 0x80 != 0 {
        // Push release event to HID ring for WM
        crate::drivers::mouse::push_key_event(scancode & 0x7F, true);
        unsafe { super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1); }
        return;
    }

    // Jalon 131: Intercept Ctrl+key shortcuts before ASCII conversion
    let (ctrl, alt, _) = crate::drivers::mouse::get_modifiers();
    if ctrl {
        match scancode {
            0x14 => { /* Ctrl+T: push special shortcut to HID ring */
                crate::drivers::mouse::push_key_event(scancode, false);
                crate::process::kbd_push_byte(0x14); // Ctrl+T
            }
            0x2E => { /* Ctrl+C: interrupt */
                crate::process::kbd_push_byte(0x03); // ETX = Ctrl+C
            }
            0x26 => { /* Ctrl+L: clear */
                crate::process::kbd_push_byte(0x0C); // FF = Ctrl+L
            }
            0x2D => { /* Ctrl+X: kill */
                crate::process::kbd_push_byte(0x18); // CAN = Ctrl+X
            }
            _ => {
                // Generic Ctrl+key: push ASCII code 1-26 (Ctrl+A=1, Ctrl+Z=26)
                let ascii = scancode_set1_to_ascii(scancode);
                if ascii >= b'a' && ascii <= b'z' {
                    crate::process::kbd_push_byte(ascii - b'a' + 1);
                }
            }
        }
        crate::drivers::mouse::push_key_event(scancode, false);
        unsafe { super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1); }
        return;
    }

    if alt {
        // Alt+Tab = 0x0F (Tab scancode)
        if scancode == 0x0F {
            crate::process::kbd_push_byte(0x1B); // ESC for Alt+Tab (window switch)
        }
        crate::drivers::mouse::push_key_event(scancode, false);
        unsafe { super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1); }
        return;
    }

    // Normal make code — convert to ASCII
    let ascii = scancode_set1_to_ascii(scancode);
    if ascii != 0 {
        crate::process::kbd_push_byte(ascii);
    }
    // Also push raw scancode to HID ring for WM/sys_poll_hid
    crate::drivers::mouse::push_key_event(scancode, false);

    // EOI always sent
    unsafe { super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1); }
}

/// PS/2 Mouse IRQ 12 handler (Jalon 38)
extern "x86-interrupt" fn mouse_interrupt_handler(_stack_frame: InterruptStackFrame) {
    crate::drivers::mouse::handle_irq();

    // EOI for IRQ 12 (slave PIC vector 44)
    unsafe {
        super::interrupts::end_of_interrupt(super::interrupts::PIC2_OFFSET + 4);
    }
}

/// Convert PS/2 Scancode Set 1 to ASCII (AZERTY layout, avec Shift)
fn scancode_set1_to_ascii(scancode: u8) -> u8 {
    let shifted = SHIFT_PRESSED.load(Ordering::Relaxed);
    match (scancode, shifted) {
        // Rangée chiffres AZERTY — sans Shift: symboles, avec Shift: chiffres
        (0x02, false) => b'&',  (0x02, true) => b'1',
        (0x03, false) => b'2',  (0x03, true) => b'2', // é → 2
        (0x04, false) => b'"',  (0x04, true) => b'3',
        (0x05, false) => b'\'', (0x05, true) => b'4',
        (0x06, false) => b'(',  (0x06, true) => b'5',
        (0x07, false) => b'-',  (0x07, true) => b'6',
        (0x08, false) => b'7',  (0x08, true) => b'7', // è → 7
        (0x09, false) => b'_',  (0x09, true) => b'8',
        (0x0A, false) => b'9',  (0x0A, true) => b'9', // ç → 9
        (0x0B, false) => b'0',  (0x0B, true) => b'0', // à → 0
        (0x0C, false) => b')',  (0x0C, true) => b'-',
        (0x0D, false) => b'=',  (0x0D, true) => b'+',
        (0x0E, _) => 0x08, // Backspace
        (0x0F, _) => b'\t',
        // Rangée AZERTY: AZERTYUIOP
        (0x10, false) => b'a', (0x10, true) => b'A',
        (0x11, false) => b'z', (0x11, true) => b'Z',
        (0x12, false) => b'e', (0x12, true) => b'E',
        (0x13, false) => b'r', (0x13, true) => b'R',
        (0x14, false) => b't', (0x14, true) => b'T',
        (0x15, false) => b'y', (0x15, true) => b'Y',
        (0x16, false) => b'u', (0x16, true) => b'U',
        (0x17, false) => b'i', (0x17, true) => b'I',
        (0x18, false) => b'o', (0x18, true) => b'O',
        (0x19, false) => b'p', (0x19, true) => b'P',
        (0x1C, _) => b'\n', // Entrée
        // Rangée AZERTY: QSDFGHJKLM
        (0x1E, false) => b'q', (0x1E, true) => b'Q',
        (0x1F, false) => b's', (0x1F, true) => b'S',
        (0x20, false) => b'd', (0x20, true) => b'D',
        (0x21, false) => b'f', (0x21, true) => b'F',
        (0x22, false) => b'g', (0x22, true) => b'G',
        (0x23, false) => b'h', (0x23, true) => b'H',
        (0x24, false) => b'j', (0x24, true) => b'J',
        (0x25, false) => b'k', (0x25, true) => b'K',
        (0x26, false) => b'l', (0x26, true) => b'L',
        (0x27, false) => b'm', (0x27, true) => b'M',
        // Rangée AZERTY: WXCVBN
        (0x2C, false) => b'w', (0x2C, true) => b'W',
        (0x2D, false) => b'x', (0x2D, true) => b'X',
        (0x2E, false) => b'c', (0x2E, true) => b'C',
        (0x2F, false) => b'v', (0x2F, true) => b'V',
        (0x30, false) => b'b', (0x30, true) => b'B',
        (0x31, false) => b'n', (0x31, true) => b'N',
        (0x32, false) => b',', (0x32, true) => b'?',
        (0x33, false) => b';', (0x33, true) => b'.',
        (0x34, false) => b':', (0x34, true) => b'/',
        (0x35, false) => b'!', (0x35, true) => b'!',
        (0x39, _) => b' ', // Espace
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_idt_init() {
        init();
    }

    #[test_case]
    fn test_idt_handlers_present() {
        let _idt = idt_ref();
    }
}
