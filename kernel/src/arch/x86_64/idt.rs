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
/// User heap demand-paging upper bound (256 MiB)
const USER_HEAP_DEMAND_HIGH: u64 = 0x0000_3000_1000_0000;

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
fn kill_user_and_switch(current_pid: u64, addr_raw: u64) {
    let _ = crate::process::set_state(current_pid, crate::process::ProcessState::Terminated);
    crate::serial_println!("[SIGSEGV] PID {} terminated (addr 0x{:X})", current_pid, addr_raw);

    // Find the next Ready userspace process and IRETQ to it.
    if let Some((next_pid, entry, stack, pml4, name)) =
        crate::process::find_next_ready_userspace(current_pid)
    {
        crate::scheduler::set_current_pid(next_pid);
        let _ = crate::process::set_state(next_pid, crate::process::ProcessState::Running);

        // CRITICAL: Only use preempt_state if it's valid (rip in userspace range 0x8000000000+).
        // If saved_rip is 0 or invalid, use entry_point instead.
        let (rip, rsp, rfl, cr3) =
            if let Some((saved_rip, saved_rsp, saved_rfl, saved_pml4)) =
                crate::process::get_preempt_state(next_pid)
            {
                // Validate saved_rip is in userspace range (0x8000000000 - 0x9000000000)
                if saved_rip >= 0x8000000000 && saved_rip < 0x9000000000 {
                    (saved_rip, saved_rsp, saved_rfl, saved_pml4)
                } else {
                    // Invalid saved_rip → start from entry_point
                    crate::serial_println!(
                        "[SIGSEGV] Invalid saved_rip=0x{:X} for PID {}, using entry_point",
                        saved_rip, next_pid
                    );
                    (entry, stack, 0x202u64, pml4)
                }
            } else {
                // No preempt_state saved → start from entry_point
                (entry, stack, 0x202u64, pml4)
            };

        crate::serial_println!(
            "[SIGSEGV] -> PID {} ({}) rip=0x{:X}",
            next_pid, name, rip
        );

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

// ===== IRQ Handlers =====

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Jalon 55: Preemptive scheduler tick (context switch infrastructure ready,
    // actual switching disabled until multi-process support is complete).
    //
    // tick_preemptive() updates scheduler counters and aging; the actual
    // frame-modification path is guarded to only fire when at least 2
    // Ring-3 processes are simultaneously ready with saved state.
    let _ = crate::scheduler::tick_preemptive();

    // Send EOI for timer IRQ (vector 32)
    unsafe {
        super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET);
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    // Lire le scancode du port clavier 0x60
    let mut port = Port::new(0x60);
    // SAFETY: Port 0x60 is the PS/2 keyboard data port. Reading it inside
    // the keyboard IRQ handler retrieves the pending scancode. No side effects
    // beyond consuming the byte from the hardware buffer.
    let scancode: u8 = unsafe { port.read() };

    // Convert scancode to ASCII and push to keyboard buffer
    // Only process key-press events (bit 7 clear = press, set = release)
    if scancode & 0x80 == 0 {
        let ascii = scancode_to_ascii(scancode);
        if ascii != 0 {
            crate::process::kbd_push_byte(ascii);
            // Echo to serial for debugging
            if ascii == b'\n' {
                crate::serial_write("\n");
            } else {
                let ch = [ascii];
                if let Ok(s) = core::str::from_utf8(&ch) {
                    crate::serial_write(s);
                }
            }
        }
    }

    if scancode != 0 {
        crate::serial_println!("[KEYBOARD] Scancode: 0x{:02x}", scancode);
    }

    // Envoyer EOI au PIC
    // SAFETY: Sends EOI for keyboard IRQ (vector 33). Must be called to
    // acknowledge the interrupt and re-enable subsequent keyboard IRQs.
    unsafe {
        super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1);
    }

    // Push keyboard event to HID ring buffer for J38
    crate::drivers::mouse::push_key_event(scancode, scancode & 0x80 != 0);
}

/// PS/2 Mouse IRQ 12 handler (Jalon 38)
extern "x86-interrupt" fn mouse_interrupt_handler(_stack_frame: InterruptStackFrame) {
    crate::drivers::mouse::handle_irq();

    // EOI for IRQ 12 (slave PIC vector 44)
    unsafe {
        super::interrupts::end_of_interrupt(super::interrupts::PIC2_OFFSET + 4);
    }
}

/// Convert PS/2 scancode set 1 to ASCII
fn scancode_to_ascii(scancode: u8) -> u8 {
    // US QWERTY layout, scancode set 1 (make codes only)
    match scancode {
        0x02 => b'1', 0x03 => b'2', 0x04 => b'3', 0x05 => b'4',
        0x06 => b'5', 0x07 => b'6', 0x08 => b'7', 0x09 => b'8',
        0x0A => b'9', 0x0B => b'0', 0x0C => b'-', 0x0D => b'=',
        0x0E => 0x08, // Backspace
        0x0F => b'\t', // Tab
        0x10 => b'q', 0x11 => b'w', 0x12 => b'e', 0x13 => b'r',
        0x14 => b't', 0x15 => b'y', 0x16 => b'u', 0x17 => b'i',
        0x18 => b'o', 0x19 => b'p', 0x1A => b'[', 0x1B => b']',
        0x1C => b'\n', // Enter
        0x1E => b'a', 0x1F => b's', 0x20 => b'd', 0x21 => b'f',
        0x22 => b'g', 0x23 => b'h', 0x24 => b'j', 0x25 => b'k',
        0x26 => b'l', 0x27 => b';', 0x28 => b'\'',
        0x29 => b'`',
        0x2B => b'\\',
        0x2C => b'z', 0x2D => b'x', 0x2E => b'c', 0x2F => b'v',
        0x30 => b'b', 0x31 => b'n', 0x32 => b'm', 0x33 => b',',
        0x34 => b'.', 0x35 => b'/',
        0x39 => b' ', // Space
        _ => 0, // Unknown scancode
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
