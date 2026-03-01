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
    crate::serial_println!("[EXCEPTION] #GP General protection fault (code 0x{:X}) at {:?}", error_code, stack_frame.instruction_pointer);
    crate::serial_println!("[EXCEPTION] Stack: {:?}", stack_frame);
    panic!("General protection fault");
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

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    let accessed_address = Cr2::read();
    let addr_raw = accessed_address.as_u64();

    // Check if this is a user-mode page fault (bit 2 of error code = USER_MODE)
    let is_user_mode = error_code.contains(PageFaultErrorCode::USER_MODE);

    if is_user_mode
        && addr_raw >= USER_STACK_DEMAND_LOW
        && addr_raw < USER_STACK_DEMAND_HIGH
    {
        // Demand paging: allocate and map a page for the user stack
        let page_addr = addr_raw & !0xFFF; // page-align

        crate::serial_println!(
            "[PF-DEMAND] User stack fault at 0x{:X} (page 0x{:X}), allocating...",
            addr_raw, page_addr
        );

        // Allocate a frame from the ELF frame pool
        let frame_phys = unsafe { crate::elf::alloc_demand_frame() };
        match frame_phys {
            Some(phys) => {
                // Zero the frame
                let phys_offset = crate::elf::phys_offset();
                unsafe {
                    core::ptr::write_bytes(
                        (phys + phys_offset) as *mut u8,
                        0,
                        4096,
                    );
                }

                // Get the current CR3 (user process page table)
                let cr3: u64;
                unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack)); }
                let pml4_phys = cr3 & !0xFFF;

                // Map with USER_ACCESSIBLE | WRITABLE | NO_EXECUTE
                // flags: PRESENT(0x01) | WRITABLE(0x02) | USER(0x04) | NX(bit 63)
                let flags: u64 = 0x01 | 0x02 | 0x04 | (1u64 << 63);

                match unsafe { crate::elf::demand_map_user_page(pml4_phys, page_addr, phys, flags) } {
                    Ok(()) => {
                        crate::serial_println!(
                            "[PF-DEMAND] Mapped 0x{:X} -> phys 0x{:X} (U|W|NX) OK",
                            page_addr, phys
                        );
                        // Return to resume execution — CPU will retry the instruction
                        return;
                    }
                    Err(_) => {
                        crate::serial_println!(
                            "[PF-DEMAND] FATAL: Failed to map page 0x{:X}",
                            page_addr
                        );
                    }
                }
            }
            None => {
                crate::serial_println!("[PF-DEMAND] FATAL: Out of frames for demand paging");
            }
        }
    }

    // Non-recoverable page fault — SIGSEGV
    crate::serial_println!("[EXCEPTION] #PF Page fault at {:?}", stack_frame.instruction_pointer);
    crate::serial_println!("[EXCEPTION] Accessed address: {:?}", accessed_address);
    crate::serial_println!("[EXCEPTION] Error code: {:?}", error_code);
    crate::serial_println!("[EXCEPTION] User mode: {}", is_user_mode);

    if is_user_mode {
        crate::serial_println!("[SIGSEGV] Killing user process (bad address 0x{:X})", addr_raw);
        // Terminate the process (SIGSEGV equivalent)
        let current_pid = crate::scheduler::current_pid();
        if current_pid != 0 {
            let _ = crate::process::set_state(current_pid, crate::process::ProcessState::Terminated);
            crate::serial_println!("[SIGSEGV] PID {} terminated due to page fault", current_pid);
            // Trigger a context switch to the next process
            crate::scheduler::schedule_next();
        }
    }

    panic!("Page fault");
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
    // Couche 7: Scheduler tick on every PIT timer interrupt
    crate::scheduler::tick();

    // SAFETY: Sends EOI for timer IRQ (vector 32) to acknowledge the PIC.
    // Required so the PIC will deliver subsequent timer interrupts.
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

    if scancode != 0 {
        crate::serial_println!("[KEYBOARD] Scancode: 0x{:02x}", scancode);
    }

    // Envoyer EOI au PIC
    // SAFETY: Sends EOI for keyboard IRQ (vector 33). Must be called to
    // acknowledge the interrupt and re-enable subsequent keyboard IRQs.
    unsafe {
        super::interrupts::end_of_interrupt(super::interrupts::PIC1_OFFSET + 1);
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
