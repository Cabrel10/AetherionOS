// Aetherion OS - Page Table Structures
// Phase 1.2: 4-level paging implementation (PML4 → PDPT → PD → PT)

use core::ops::{Index, IndexMut};
use super::{PhysicalAddress, VirtualAddress};

/// Page Table Entry Flags (x86_64 standard)
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageTableFlags {
    PRESENT         = 1 << 0,  // Page is present in memory
    WRITABLE        = 1 << 1,  // Page is writable
    USER_ACCESSIBLE = 1 << 2,  // Page is accessible from user mode
    WRITE_THROUGH   = 1 << 3,  // Write-through caching
    NO_CACHE        = 1 << 4,  // Disable cache for this page
    ACCESSED        = 1 << 5,  // Set by CPU when page is accessed
    DIRTY           = 1 << 6,  // Set by CPU when page is written to
    HUGE_PAGE       = 1 << 7,  // 2MB/1GB huge page (depends on level)
    GLOBAL          = 1 << 8,  // Page isn't flushed from cache on CR3 switch
    NO_EXECUTE      = 1 << 63, // Prevent code execution (NXE bit)
}

/// Page Table Entry (64-bit)
/// 
/// Structure:
/// - Bits 0-11:  Flags (Present, Writable, User, etc.)
/// - Bits 12-51: Physical address (4KB aligned, 40 bits = 1TB)
/// - Bits 52-62: Available for OS use
/// - Bit 63:     No Execute (NX)
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct PageTableEntry {
    entry: u64,
}

impl PageTableEntry {
    /// Create a new empty (non-present) page table entry
    pub const fn new() -> Self {
        PageTableEntry { entry: 0 }
    }

    /// Create entry with physical address and flags
    pub fn new_with_address(addr: PhysicalAddress, flags: u64) -> Self {
        let mut entry = PageTableEntry::new();
        entry.set_address(addr, flags);
        entry
    }

    /// Set the physical address and flags for this entry
    /// 
    /// # Arguments
    /// * `addr` - Physical address (must be 4KB aligned)
    /// * `flags` - Combination of PageTableFlags
    pub fn set_address(&mut self, addr: PhysicalAddress, flags: u64) {
        // Mask to keep only bits 12-51 (physical address)
        let addr_mask = 0x000F_FFFF_FFFF_F000;
        self.entry = (addr.as_usize() as u64 & addr_mask) | flags;
    }

    /// Get the physical address from this entry
    pub fn physical_address(&self) -> PhysicalAddress {
        PhysicalAddress::new((self.entry & 0x000F_FFFF_FFFF_F000) as usize)
    }

    /// Check if PRESENT flag is set
    pub fn is_present(&self) -> bool {
        (self.entry & PageTableFlags::PRESENT as u64) != 0
    }

    /// Check if WRITABLE flag is set
    pub fn is_writable(&self) -> bool {
        (self.entry & PageTableFlags::WRITABLE as u64) != 0
    }

    /// Check if USER_ACCESSIBLE flag is set
    pub fn is_user_accessible(&self) -> bool {
        (self.entry & PageTableFlags::USER_ACCESSIBLE as u64) != 0
    }

    /// Check if HUGE_PAGE flag is set
    pub fn is_huge_page(&self) -> bool {
        (self.entry & PageTableFlags::HUGE_PAGE as u64) != 0
    }

    /// Check if NO_EXECUTE flag is set
    pub fn is_no_execute(&self) -> bool {
        (self.entry & PageTableFlags::NO_EXECUTE as u64) != 0
    }

    /// Set the PRESENT flag
    pub fn set_present(&mut self, present: bool) {
        if present {
            self.entry |= PageTableFlags::PRESENT as u64;
        } else {
            self.entry &= !(PageTableFlags::PRESENT as u64);
        }
    }

    /// Set the WRITABLE flag
    pub fn set_writable(&mut self, writable: bool) {
        if writable {
            self.entry |= PageTableFlags::WRITABLE as u64;
        } else {
            self.entry &= !(PageTableFlags::WRITABLE as u64);
        }
    }

    /// Get all flags as u64
    pub fn flags(&self) -> u64 {
        self.entry & 0xFFF
    }

    /// Set entry to zero (mark as not present)
    pub fn clear(&mut self) {
        self.entry = 0;
    }

    /// Get raw entry value
    pub fn raw(&self) -> u64 {
        self.entry
    }
}

impl core::fmt::Debug for PageTableEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(
            f,
            "PTE[addr: {:?}, P:{}, W:{}, U:{}, H:{}]",
            self.physical_address(),
            self.is_present(),
            self.is_writable(),
            self.is_user_accessible(),
            self.is_huge_page()
        )
    }
}

/// Page Table
/// 
/// Contains 512 entries (each 8 bytes = 4KB total)
/// Must be 4KB aligned in memory
#[repr(align(4096))]
pub struct PageTable {
    entries: [PageTableEntry; 512],
}

impl PageTable {
    /// Create a new empty page table (all entries non-present)
    pub const fn new() -> Self {
        PageTable {
            entries: [PageTableEntry::new(); 512],
        }
    }

    /// Zero out all entries
    pub fn zero(&mut self) {
        for entry in self.entries.iter_mut() {
            entry.clear();
        }
    }

    /// Get mutable reference to entry at index
    pub fn entry_mut(&mut self, index: usize) -> &mut PageTableEntry {
        &mut self.entries[index]
    }

    /// Get reference to entry at index
    pub fn entry(&self, index: usize) -> &PageTableEntry {
        &self.entries[index]
    }

    /// Get number of present entries
    pub fn present_count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_present()).count()
    }

    /// Clear all entries (mark as not present)
    pub fn clear_all(&mut self) {
        for entry in self.entries.iter_mut() {
            entry.clear();
        }
    }
}

impl Index<usize> for PageTable {
    type Output = PageTableEntry;

    fn index(&self, index: usize) -> &Self::Output {
        &self.entries[index]
    }
}

impl IndexMut<usize> for PageTable {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.entries[index]
    }
}

impl core::fmt::Debug for PageTable {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let present = self.present_count();
        write!(f, "PageTable[{}/512 present]", present)
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_table_entry_flags() {
        let mut entry = PageTableEntry::new();
        assert!(!entry.is_present());

        entry.set_present(true);
        assert!(entry.is_present());

        entry.set_writable(true);
        assert!(entry.is_writable());
        assert!(entry.is_present());
    }

    #[test]
    fn test_page_table_entry_address() {
        let addr = PhysicalAddress::new(0x1000);
        let flags = PageTableFlags::PRESENT as u64 | PageTableFlags::WRITABLE as u64;
        
        let entry = PageTableEntry::new_with_address(addr, flags);
        
        assert_eq!(entry.physical_address().as_usize(), 0x1000);
        assert!(entry.is_present());
        assert!(entry.is_writable());
    }

    #[test]
    fn test_page_table_alignment() {
        let table = PageTable::new();
        let addr = &table as *const _ as usize;
        
        // Must be 4KB aligned
        assert_eq!(addr % 4096, 0);
    }

    #[test]
    fn test_page_table_size() {
        use core::mem::size_of;
        
        // Page table must be exactly 4096 bytes
        assert_eq!(size_of::<PageTable>(), 4096);
        assert_eq!(size_of::<PageTableEntry>(), 8);
    }

    #[test]
    fn test_page_table_indexing() {
        let mut table = PageTable::new();
        
        let addr = PhysicalAddress::new(0x5000);
        let flags = PageTableFlags::PRESENT as u64;
        
        table[10].set_address(addr, flags);
        
        assert!(table[10].is_present());
        assert_eq!(table[10].physical_address().as_usize(), 0x5000);
        assert!(!table[11].is_present());
    }

    #[test]
    fn test_page_table_clear_all() {
        let mut table = PageTable::new();
        
        // Set some entries
        for i in 0..10 {
            let addr = PhysicalAddress::new(0x1000 * i);
            table[i].set_address(addr, PageTableFlags::PRESENT as u64);
        }
        
        assert_eq!(table.present_count(), 10);
        
        table.clear_all();
        assert_eq!(table.present_count(), 0);
    }

    #[test]
    fn test_huge_page_flag() {
        let mut entry = PageTableEntry::new();
        let addr = PhysicalAddress::new(0x200000); // 2MB aligned
        
        let flags = PageTableFlags::PRESENT as u64 
                  | PageTableFlags::HUGE_PAGE as u64;
        
        entry.set_address(addr, flags);
        
        assert!(entry.is_present());
        assert!(entry.is_huge_page());
    }

    #[test]
    fn test_no_execute_flag() {
        let mut entry = PageTableEntry::new();
        let addr = PhysicalAddress::new(0x1000);
        
        let flags = PageTableFlags::PRESENT as u64 
                  | PageTableFlags::NO_EXECUTE as u64;
        
        entry.set_address(addr, flags);
        
        assert!(entry.is_present());
        assert!(entry.is_no_execute());
    }
}
