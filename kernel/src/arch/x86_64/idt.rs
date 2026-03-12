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
        idt.page_fault.set_handler_fn(page_fault_handler);

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
}

/// Retourne une reference statique a l'IDT (pour tests)
pub fn idt_ref() -> &'static InterruptDescriptorTable {
    &IDT
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
    crate::serial_println!("[EXCEPTION] #UD Invalid opcode at {:?}", stack_frame.instruction_pointer);
    panic!("Invalid opcode");
}

extern "x86-interrupt" fn device_not_available_handler(stack_frame: InterruptStackFrame) {
    crate::serial_println!("[EXCEPTION] #NM Device not available at {:?}", stack_frame.instruction_pointer);
}

extern "x86-interrupt" fn double_fault_handler(stack_frame: InterruptStackFrame, _error_code: u64) -> ! {
    crate::serial_println!("[EXCEPTION] #DF DOUBLE FAULT at {:?}", stack_frame.instruction_pointer);
    crate::serial_println!("[EXCEPTION] Stack frame: {:?}", stack_frame);
    panic!("Double fault - possible stack overflow");
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
const USER_STACK_DEMAND_HIGH: u64 = 0x7FFF_FFFF_F000;
/// User heap demand-paging lower bound (sys_brk region)
const USER_HEAP_DEMAND_LOW: u64  = 0x0000_3000_0000_0000;
/// User heap demand-paging upper bound (8 GiB — Jalon 68)
/// Expanded from 256 MiB to 8 GiB to support Mistral 7B model loading.
const USER_HEAP_DEMAND_HIGH: u64 = 0x0000_3002_0000_0000;

/// Helper: try to demand-map a user page at `page_addr`.
/// Returns true if the page was successfully mapped.
fn try_demand_map_user_page(page_addr: u64) -> bool {
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
            // flags: PRESENT | WRITABLE | USER | NX
            let flags: u64 = 0x01 | 0x02 | 0x04 | (1u64 << 63);
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

/// Helper: kill current Ring 3 process and IRETQ to next ready one.
/// Never returns if a next process is found.
///
/// JALON 69 FIX: Properly terminates the process, then uses the scheduler's
/// yield_to_next() which correctly restores CR3, RIP, RSP and performs IRETQ.
/// Validates that the next process's saved_rip is in valid userspace range.
fn kill_user_and_switch(current_pid: u64, addr_raw: u64) {
    let _ = crate::process::set_state(current_pid, crate::process::ProcessState::Terminated);
    crate::serial_println!("[SIGSEGV] PID {} terminated (addr 0x{:X})", current_pid, addr_raw);

    // Use scheduler's yield_to_next which handles re-queuing properly
    let next = crate::scheduler::yield_to_next(current_pid);
    
    if next != 0 && next != current_pid {
        // Get the next process's saved state
        let (rip, rsp, rfl, cr3) =
            if let Some((saved_rip, saved_rsp, saved_rfl, saved_pml4)) =
                crate::process::get_preempt_state(next)
            {
                // Validate saved_rip is in userspace range (0x8000000000 - 0x9000000000)
                if saved_rip >= 0x8000000000 && saved_rip < 0x9000000000 {
                    (saved_rip, saved_rsp, saved_rfl, saved_pml4)
                } else if let Some((entry, stack, pml4)) = crate::process::get_entry_state(next) {
                    // Invalid saved_rip → start from entry_point (first-run)
                    crate::serial_println!(
                        "[SIGSEGV] PID {} saved_rip=0x{:X} invalid, using entry=0x{:X}",
                        next, saved_rip, entry
                    );
                    (entry, stack, 0x202u64, pml4)
                } else {
                    // No valid state at all
                    crate::serial_println!("[SIGSEGV] PID {} has no valid state, idle", next);
                    crate::scheduler::set_current_pid(0);
                    loop { x86_64::instructions::hlt(); }
                }
            } else if let Some((entry, stack, pml4)) = crate::process::get_entry_state(next) {
                // No preempt state → first-run
                (entry, stack, 0x202u64, pml4)
            } else {
                crate::serial_println!("[SIGSEGV] PID {} has no entry state, idle", next);
                crate::scheduler::set_current_pid(0);
                loop { x86_64::instructions::hlt(); }
            };

        if cr3 == 0 || rip == 0 {
            crate::serial_println!("[SIGSEGV] PID {} invalid cr3/rip, idle", next);
            crate::scheduler::set_current_pid(0);
            loop { x86_64::instructions::hlt(); }
        }

        let _ = crate::process::set_state(next, crate::process::ProcessState::Running);
        crate::scheduler::set_current_pid(next);
        
        if let Some(name) = crate::process::with_process(next, |p| p.name.clone()) {
            crate::serial_println!(
                "[SIGSEGV] -> PID {} ({}) rip=0x{:X} rsp=0x{:X}",
                next, name, rip, rsp
            );
        }

        // Reset GS bases before IRETQ to Ring 3
        crate::arch::x86_64::syscall::reset_gs_bases();

        // IRETQ to the next process — never returns
        unsafe {
            core::arch::asm!(
                "cli",
                "mov cr3, {cr3_val}",
                "push 0x1B",           // SS (Ring 3 data)
                "push {rsp_val}",      // RSP
                "push {rfl_val}",      // RFLAGS
                "push 0x23",           // CS (Ring 3 code)
                "push {rip_val}",      // RIP
                "iretq",
                cr3_val = in(reg) cr3,
                rsp_val = in(reg) rsp,
                rfl_val = in(reg) rfl,
                rip_val = in(reg) rip,
                options(noreturn),
            );
        }
    }

    // No other Ready process — enter kernel idle loop
    crate::serial_println!("[SIGSEGV] No other process ready, idle");
    crate::scheduler::set_current_pid(0);
    loop { x86_64::instructions::hlt(); }
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    let accessed_address = Cr2::read();
    let addr_raw = accessed_address.as_u64();
    let is_user_mode = error_code.contains(PageFaultErrorCode::USER_MODE);
    let page_addr = addr_raw & !0xFFF;

    // --- Demand paging for user stack ---
    if is_user_mode
        && addr_raw >= USER_STACK_DEMAND_LOW
        && addr_raw < USER_STACK_DEMAND_HIGH
    {
        if try_demand_map_user_page(page_addr) {
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
            if try_demand_map_user_page(page_addr) {
                return; // Resume
            }
        }
        // Fall through to SIGSEGV
    }

    // --- Demand paging for VMA-backed regions (Jalon 68: Zero-Copy Model Loading) ---
    // Check if the faulting address is in a file-backed VMA.
    // If so, allocate a frame, read the file data into it, and map it.
    if is_user_mode {
        let current_pid = crate::scheduler::current_pid();
        if current_pid != 0 {
            if let Some((file_path, file_offset, writable)) = crate::process::find_vma(current_pid, addr_raw) {
                // Allocate a physical frame
                let frame_phys = unsafe { crate::elf::alloc_demand_frame() };
                if let Some(phys) = frame_phys {
                    let phys_offset = crate::elf::phys_offset();
                    let buf_ptr = (phys + phys_offset) as *mut u8;
                    
                    // Zero the frame first
                    unsafe { core::ptr::write_bytes(buf_ptr, 0, 4096); }
                    
                    // Read 4 KB from the file at the correct offset
                    let mut page_buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr, 4096) };
                    
                    // Try exFAT first, then FAT32
                    let bytes_read = if crate::fs::exfat::is_mounted() {
                        // Strip /disk/ prefix for exFAT
                        let name = if file_path.starts_with("/disk/") {
                            &file_path[6..]
                        } else {
                            &file_path
                        };
                        crate::fs::exfat::read_file(name, file_offset, page_buf)
                    } else {
                        // FAT32 fallback: read via VFS
                        crate::fs::fat32::read_file_at_offset(&file_path, file_offset, page_buf)
                            .unwrap_or(0)
                    };
                    
                    // Map the page
                    let cr3: u64;
                    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)); }
                    let pml4_phys = cr3 & !0xFFF;
                    
                    // flags: PRESENT | USER | NX, optionally WRITABLE
                    let mut flags: u64 = 0x01 | 0x04 | (1u64 << 63);
                    if writable { flags |= 0x02; }
                    
                    match unsafe { crate::elf::demand_map_user_page(pml4_phys, page_addr, phys, flags) } {
                        Ok(()) => {
                            // Flush TLB for this page
                            unsafe {
                                core::arch::asm!("invlpg [{}]", in(reg) page_addr, options(nostack));
                            }
                            return; // Resume execution
                        }
                        Err(_) => {
                            crate::serial_println!("[VMA-PF] Map failed for 0x{:X}", page_addr);
                        }
                    }
                }
                // Fall through to SIGSEGV if allocation or mapping failed
            }
        }
    }

    // --- Non-recoverable page fault ---
    if is_user_mode {
        crate::serial_println!(
            "[SIGSEGV] PF addr=0x{:X} rip={:?} code={:?}",
            addr_raw, stack_frame.instruction_pointer, error_code
        );
        let current_pid = crate::scheduler::current_pid();
        if current_pid != 0 {
            kill_user_and_switch(current_pid, addr_raw);
            // kill_user_and_switch never returns if another process exists
        }
    }

    // Kernel-mode page fault — fatal
    crate::serial_println!("[EXCEPTION] #PF at {:?} addr=0x{:X}", stack_frame.instruction_pointer, addr_raw);
    panic!("Kernel page fault at 0x{:X}", addr_raw);
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
        if (cs & 0x3) == 3 && irq_rip >= 0x8000000000 && irq_rip < 0x9000000000 {
            crate::process::save_preempt_state(current_pid, irq_rip, irq_rsp, irq_rflags);
        }
    }

    // Let the scheduler decide if a switch is needed
    // But DON'T actually perform the switch from the timer ISR —
    // context switches happen cooperatively via sys_yield() or via
    // check_pending_switch() in the idle loop.
    if let Some((old_pid, new_pid, new_rip, new_rsp, _new_rflags, new_pml4)) =
        crate::scheduler::tick_preemptive()
    {
        // Store the pending switch for later execution
        // The actual switch happens in sys_yield or the idle loop
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
            // Perform the actual context switch via IRETQ
            // This switches to the new process and never returns to caller
            unsafe {
                core::arch::asm!(
                    "cli",
                    "mov cr3, {cr3}",           // Switch page tables
                    "push 0x1B",                // SS (Ring 3 data)
                    "push {rsp}",               // RSP
                    "push 0x202",               // RFLAGS (IF=1)
                    "push 0x23",                // CS (Ring 3 code)
                    "push {rip}",               // RIP
                    "iretq",                    // IRETQ to new process
                    cr3 = in(reg) new_pml4,
                    rsp = in(reg) new_rsp,
                    rip = in(reg) new_rip,
                    options(noreturn)
                );
            }
        }
    }
    false
}

/// Keyboard IRQ1 handler — Scancode Set 1 (with 8042 translation ON)
///
/// JALON 68 FIX: The PS/2 controller now has Translation (Bit 6) enabled,
/// which converts Set 2 → Set 1 in hardware. This means:
///   - Make codes: 0x01-0x58 (key press)
///   - Break codes: make | 0x80 (key release, bit 7 set)
///   - Extended prefix: 0xE0 (multi-byte keys like arrows)
///
/// We ONLY process make codes (bit 7 = 0) and ignore releases (bit 7 = 1).
/// This is the standard approach for PS/2 Set 1 on real hardware / KVM.
extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    let scancode: u8 = unsafe { Port::new(0x60).read() };

    // Log every scancode for boot diagnostics (helps trace keyboard issues)
    // Only log the first few to avoid flooding serial
    {
        static mut KBD_LOG_COUNT: u64 = 0;
        unsafe {
            KBD_LOG_COUNT += 1;
            if KBD_LOG_COUNT <= 20 {
                crate::serial_println!(
                    "[KBD] IRQ1 scancode=0x{:02X} ({}), total={}",
                    scancode,
                    if scancode & 0x80 != 0 { "release" } else { "press" },
                    KBD_LOG_COUNT
                );
            }
        }
    }

    // Skip extended prefix (0xE0) — multi-byte sequences (arrows, etc.)
    if scancode == 0xE0 {
        unsafe { super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1); }
        return;
    }
    // Also skip 0xE1 (Pause key prefix)
    if scancode == 0xE1 {
        unsafe { super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1); }
        return;
    }

    // Check if this is a KEY RELEASE (bit 7 set in Set 1)
    let is_release = (scancode & 0x80) != 0;
    
    // For releases, just send EOI and return (don't process)
    if is_release {
        unsafe {
            super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1);
        }
        return;
    }

    if !is_release {
        // KEY PRESS — convert make code to ASCII
        let ascii = scancode_set1_to_ascii(scancode);
        if ascii != 0 {
            crate::process::kbd_push_byte(ascii);
        }
    }
    // Key releases already handled above

    // Send EOI for keyboard IRQ
    unsafe {
        super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1);
    }
}

/// PS/2 Mouse IRQ 12 handler (Jalon 38)
extern "x86-interrupt" fn mouse_interrupt_handler(_stack_frame: InterruptStackFrame) {
    crate::drivers::mouse::handle_irq();

    // EOI for IRQ 12 (slave PIC vector 44)
    unsafe {
        super::interrupts::end_of_interrupt(super::interrupts::PIC2_OFFSET + 4);
    }
}

/// Convert PS/2 Scancode Set 1 MAKE code to ASCII.
///
/// AZERTY French layout mapping for Set 1 scan codes.
/// Reference: https://wiki.osdev.org/PS/2_Keyboard#Scan_Code_Set_1
///
/// NOTE: This returns lowercase. Shift/Caps handling would require
/// tracking modifier state.
fn scancode_set1_to_ascii(scancode: u8) -> u8 {
    match scancode {
        // Number row (top)
        0x02 => b'1', 0x03 => b'2', 0x04 => b'3', 0x05 => b'4',
        0x06 => b'5', 0x07 => b'6', 0x08 => b'7', 0x09 => b'8',
        0x0A => b'9', 0x0B => b'0',
        0x0C => b'-', 0x0D => b'=',
        0x0E => 0x08, // Backspace
        0x0F => b'\t', // Tab

        // QWERTY row -> AZERTY mapping
        0x10 => b'a', 0x11 => b'z', 0x12 => b'e', 0x13 => b'r',
        0x14 => b't', 0x15 => b'y', 0x16 => b'u', 0x17 => b'i',
        0x18 => b'o', 0x19 => b'p',
        0x1A => b'[', 0x1B => b']',
        0x1C => b'\n', // Enter (main)

        // ASDF row (home row) - AZERTY
        0x1E => b'q', 0x1F => b's', 0x20 => b'd', 0x21 => b'f',
        0x22 => b'g', 0x23 => b'h', 0x24 => b'j', 0x25 => b'k',
        0x26 => b'l',
        0x27 => b';', 0x28 => b'\'',
        0x29 => b'`', // Backtick
        0x2B => b'\\', // Backslash

        // ZXCV row -> AZERTY bottom row
        0x2C => b'w', 0x2D => b'x', 0x2E => b'c', 0x2F => b'v',
        0x30 => b'b', 0x31 => b'n', 0x32 => b'm',
        0x33 => b',', 0x34 => b'.', 0x35 => b'/',

        // Right-side keys
        0x1D => 0, // Left Ctrl (modifier, ignore for now)
        0x2A => 0, // Left Shift (modifier)
        0x36 => 0, // Right Shift (modifier)
        0x38 => 0, // Left Alt (modifier)
        0x3A => 0, // Caps Lock (toggle)

        // Space bar
        0x39 => b' ',

        // Escape
        0x01 => 0x1B,

        // Numpad (when NumLock is ON)
        0x47 => b'7', 0x48 => b'8', 0x49 => b'9',
        0x4B => b'4', 0x4C => b'5', 0x4D => b'6',
        0x4F => b'1', 0x50 => b'2', 0x51 => b'3',
        0x52 => b'0', 0x53 => b'.',

        // Extended keys (arrows, etc) - handled separately via 0xE0 prefix
        // These are the non-extended codes that might slip through
        0x48 => 0, // Up arrow (without 0xE0 prefix - unlikely)
        0x50 => 0, // Down arrow
        0x4B => 0, // Left arrow
        0x4D => 0, // Right arrow

        // Function keys F1-F12
        0x3B..=0x44 => 0, // F1-F10
        0x57 => 0, // F11
        0x58 => 0, // F12

        // Everything else - ignore (modifiers, special keys)
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
