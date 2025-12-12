// Aetherion OS - Memory Management Module
// Phase 1: Complete memory management (Physical + Virtual)

pub mod bitmap;
pub mod frame_allocator;
pub mod page_table;
pub mod paging;

// Re-exports for convenience
pub use page_table::{PageTable, PageTableEntry, PageTableFlags};
pub use paging::{PageMapper, MapperError};

/// Page and frame size (4 KB standard)
pub const PAGE_SIZE: usize = 4096;
pub const FRAME_SIZE: usize = 4096;

/// Physical address wrapper
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysicalAddress(pub usize);

impl PhysicalAddress {
    /// Create a new physical address
    pub const fn new(addr: usize) -> Self {
        PhysicalAddress(addr)
    }
    
    /// Get the raw address value
    pub const fn as_usize(&self) -> usize {
        self.0
    }
    
    /// Align address down to the nearest boundary
    pub fn align_down(&self, align: usize) -> Self {
        PhysicalAddress(self.0 & !(align - 1))
    }
    
    /// Align address up to the nearest boundary
    pub fn align_up(&self, align: usize) -> Self {
        PhysicalAddress((self.0 + align - 1) & !(align - 1))
    }
    
    /// Check if address is aligned to boundary
    pub fn is_aligned(&self, align: usize) -> bool {
        self.0 % align == 0
    }
    
    /// Add offset to address
    pub fn offset(&self, offset: usize) -> Self {
        PhysicalAddress(self.0 + offset)
    }
}

/// Virtual address wrapper (for future paging)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct VirtualAddress(pub usize);

impl VirtualAddress {
    /// Create a new virtual address
    pub const fn new(addr: usize) -> Self {
        VirtualAddress(addr)
    }
    
    /// Get the raw address value
    pub const fn as_usize(&self) -> usize {
        self.0
    }
    
    /// Extract PML4 table index (bits 39-47)
    pub fn pml4_index(&self) -> usize {
        (self.0 >> 39) & 0x1FF
    }
    
    /// Extract PDPT index (bits 30-38)
    pub fn pdpt_index(&self) -> usize {
        (self.0 >> 30) & 0x1FF
    }
    
    /// Extract Page Directory index (bits 21-29)
    pub fn pd_index(&self) -> usize {
        (self.0 >> 21) & 0x1FF
    }
    
    /// Extract Page Table index (bits 12-20)
    pub fn pt_index(&self) -> usize {
        (self.0 >> 12) & 0x1FF
    }
    
    /// Extract page offset (bits 0-11)
    pub fn page_offset(&self) -> usize {
        self.0 & 0xFFF
    }
    
    /// Align address down to page boundary
    pub fn align_down(&self, align: usize) -> Self {
        VirtualAddress(self.0 & !(align - 1))
    }
    
    /// Align address up to page boundary
    pub fn align_up(&self, align: usize) -> Self {
        VirtualAddress((self.0 + align - 1) & !(align - 1))
    }
    
    /// Check if address is aligned to boundary
    pub fn is_aligned(&self, align: usize) -> bool {
        self.0 % align == 0
    }
    
    /// Add offset to address
    pub fn offset(&self, offset: usize) -> Self {
        VirtualAddress(self.0 + offset)
    }
}

/// Initialize a new PML4 table at the given physical address
/// 
/// # Safety
/// The physical address must point to valid, writable memory
pub unsafe fn init_pml4_table(pml4_addr: PhysicalAddress) -> &'static mut PageTable {
    let table = &mut *(pml4_addr.as_usize() as *mut PageTable);
    table.zero();
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_physical_address_alignment() {
        let addr = PhysicalAddress::new(0x1234);
        assert_eq!(addr.align_down(0x1000).as_usize(), 0x1000);
        assert_eq!(addr.align_up(0x1000).as_usize(), 0x2000);
    }
    
    #[test]
    fn test_virtual_address_indices() {
        // Address: 0xFFFF_8000_0040_1234
        // PML4 = 256, PDPT = 0, PD = 2, PT = 1, Offset = 0x234
        let addr = VirtualAddress::new(0xFFFF_8000_0040_1234);
        assert_eq!(addr.pml4_index(), 256);
        assert_eq!(addr.pdpt_index(), 0);
        assert_eq!(addr.pd_index(), 2);
        assert_eq!(addr.pt_index(), 1);
        assert_eq!(addr.page_offset(), 0x234);
    }
}
