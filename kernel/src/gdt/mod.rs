// Aetherion OS - Global Descriptor Table (GDT)
// Phase 2.1: Segmentation setup for x86_64

use core::mem::size_of;

/// GDT Entry
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct GdtEntry {
    limit_low: u16,
    base_low: u16,
    base_middle: u8,
    access: u8,
    granularity: u8,
    base_high: u8,
}

impl GdtEntry {
    const fn null() -> Self {
        GdtEntry {
            limit_low: 0,
            base_low: 0,
            base_middle: 0,
            access: 0,
            granularity: 0,
            base_high: 0,
        }
    }

    const fn new(base: u32, limit: u32, access: u8, flags: u8) -> Self {
        GdtEntry {
            limit_low: (limit & 0xFFFF) as u16,
            base_low: (base & 0xFFFF) as u16,
            base_middle: ((base >> 16) & 0xFF) as u8,
            access,
            granularity: (((limit >> 16) & 0x0F) | (flags & 0xF0)) as u8,
            base_high: ((base >> 24) & 0xFF) as u8,
        }
    }

    const fn code_segment() -> Self {
        // Code segment: Executable, readable, ring 0
        // Access: Present (1) + DPL (00) + Type (1010) = 10011010 = 0x9A
        // Flags: Granularity (1) + Size (1) + Long mode (1) = 1010 = 0xA0
        Self::new(0, 0xFFFFF, 0x9A, 0xA0)
    }

    const fn data_segment() -> Self {
        // Data segment: Writable, ring 0
        // Access: Present (1) + DPL (00) + Type (0010) = 10010010 = 0x92
        Self::new(0, 0xFFFFF, 0x92, 0xC0)
    }

    const fn user_code_segment() -> Self {
        // User code: Executable, readable, ring 3
        // Access: Present (1) + DPL (11) + Type (1010) = 11111010 = 0xFA
        Self::new(0, 0xFFFFF, 0xFA, 0xA0)
    }

    const fn user_data_segment() -> Self {
        // User data: Writable, ring 3
        // Access: Present (1) + DPL (11) + Type (0010) = 11110010 = 0xF2
        Self::new(0, 0xFFFFF, 0xF2, 0xC0)
    }
}

/// GDT Descriptor
#[repr(C, packed)]
struct GdtDescriptor {
    limit: u16,
    base: u64,
}

/// Global Descriptor Table
pub struct Gdt {
    entries: [GdtEntry; 5],
}

impl Gdt {
    pub const fn new() -> Self {
        Gdt {
            entries: [
                GdtEntry::null(),              // 0x00: Null descriptor
                GdtEntry::code_segment(),      // 0x08: Kernel code
                GdtEntry::data_segment(),      // 0x10: Kernel data
                GdtEntry::user_data_segment(), // 0x18: User data
                GdtEntry::user_code_segment(), // 0x20: User code
            ],
        }
    }

    pub fn load(&'static self) {
        let descriptor = GdtDescriptor {
            limit: (size_of::<Self>() - 1) as u16,
            base: self as *const _ as u64,
        };

        unsafe {
            load_gdt(&descriptor);
        }
    }
}

/// Load GDT and update segment registers
unsafe fn load_gdt(descriptor: &GdtDescriptor) {
    core::arch::asm!(
        "lgdt [{}]",
        "push 0x08",           // Code segment selector
        "lea {tmp}, [rip + 2f]",
        "push {tmp}",
        "retfq",               // Far return to reload CS
        "2:",
        "mov ax, 0x10",        // Data segment selector
        "mov ds, ax",
        "mov es, ax",
        "mov fs, ax",
        "mov gs, ax",
        "mov ss, ax",
        in(reg) descriptor,
        tmp = lateout(reg) _,
        options(preserves_flags)
    );
}

/// Initialize and load GDT
pub fn init() {
    static mut GDT: Gdt = Gdt::new();
    
    unsafe {
        GDT.load();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gdt_creation() {
        let gdt = Gdt::new();
        assert_eq!(gdt.entries.len(), 5);
    }
}
