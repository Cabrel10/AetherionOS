// Aetherion OS - IDT Implementation

use core::mem::size_of;

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    flags: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    pub const fn new() -> Self {
        IdtEntry {
            offset_low: 0,
            selector: 0,
            ist: 0,
            flags: 0,
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    pub fn set_handler(&mut self, handler: usize) {
        self.offset_low = (handler & 0xFFFF) as u16;
        self.offset_mid = ((handler >> 16) & 0xFFFF) as u16;
        self.offset_high = ((handler >> 32) & 0xFFFFFFFF) as u32;
        self.selector = 0x08; // Kernel code segment
        self.flags = 0x8E;    // Present, ring 0, interrupt gate
    }
}

#[repr(C, align(16))]
pub struct Idt {
    entries: [IdtEntry; 256],
}

impl Idt {
    pub const fn new() -> Self {
        Idt {
            entries: [IdtEntry::new(); 256],
        }
    }

    pub fn set_handler(&mut self, index: u8, handler: usize) {
        self.entries[index as usize].set_handler(handler);
    }

    pub fn load(&'static self) {
        let descriptor = IdtDescriptor {
            limit: (size_of::<Self>() - 1) as u16,
            base: self as *const _ as u64,
        };

        unsafe {
            core::arch::asm!("lidt [{}]", in(reg) &descriptor);
        }
    }
}

#[repr(C, packed)]
struct IdtDescriptor {
    limit: u16,
    base: u64,
}

pub type InterruptDescriptorTable = Idt;

pub fn init_idt() {
    static mut IDT: Idt = Idt::new();
    
    unsafe {
        // Register exception handlers
        IDT.set_handler(0, divide_by_zero_handler as usize);
        IDT.set_handler(13, general_protection_fault_handler as usize);
        IDT.set_handler(14, page_fault_handler as usize);
        
        IDT.load();
    }
}

// Exception stubs
extern "x86-interrupt" fn divide_by_zero_handler() {
    panic!("EXCEPTION: Divide by zero");
}

extern "x86-interrupt" fn general_protection_fault_handler() {
    panic!("EXCEPTION: General protection fault");
}

extern "x86-interrupt" fn page_fault_handler() {
    panic!("EXCEPTION: Page fault");
}
