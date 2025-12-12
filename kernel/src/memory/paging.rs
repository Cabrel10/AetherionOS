// Aetherion OS - Page Mapper
// Phase 1.2: Virtual memory mapping with 4-level paging

use super::{PhysicalAddress, VirtualAddress, PAGE_SIZE};
use super::page_table::{PageTable, PageTableEntry, PageTableFlags};
use super::frame_allocator::FrameAllocator;

/// Errors that can occur during page mapping
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapperError {
    /// Out of physical memory frames
    OutOfMemory,
    /// Page is already mapped
    PageAlreadyMapped,
    /// Invalid address (not aligned, out of range, etc.)
    InvalidAddress,
    /// Page table creation failed
    TableCreationFailed,
}

/// Page Mapper - Manages virtual to physical address translation
/// 
/// Uses 4-level paging: PML4 → PDPT → PD → PT
pub struct PageMapper {
    pml4_address: PhysicalAddress,
}

impl PageMapper {
    /// Create a new PageMapper with existing PML4 table
    /// 
    /// # Arguments
    /// * `pml4_addr` - Physical address of the PML4 table
    /// 
    /// # Safety
    /// The PML4 table must be valid and properly initialized
    pub unsafe fn new(pml4_addr: PhysicalAddress) -> Self {
        PageMapper {
            pml4_address: pml4_addr,
        }
    }

    /// Get reference to PML4 table
    unsafe fn pml4(&self) -> &'static PageTable {
        &*(self.pml4_address.as_usize() as *const PageTable)
    }

    /// Get mutable reference to PML4 table
    unsafe fn pml4_mut(&mut self) -> &'static mut PageTable {
        &mut *(self.pml4_address.as_usize() as *mut PageTable)
    }

    /// Map a virtual page to a physical frame
    /// 
    /// # Arguments
    /// * `virt_addr` - Virtual address to map
    /// * `phys_addr` - Physical address to map to
    /// * `flags` - Page table entry flags
    /// * `allocator` - Frame allocator for creating new page tables
    /// 
    /// # Returns
    /// Ok(()) if mapping succeeded, Err(MapperError) otherwise
    pub fn map_page(
        &mut self,
        virt_addr: VirtualAddress,
        phys_addr: PhysicalAddress,
        flags: u64,
        allocator: &mut FrameAllocator,
    ) -> Result<(), MapperError> {
        // Ensure addresses are page-aligned
        if !virt_addr.is_aligned(PAGE_SIZE) || !phys_addr.is_aligned(PAGE_SIZE) {
            return Err(MapperError::InvalidAddress);
        }

        unsafe {
            // Get or create PDPT (Page Directory Pointer Table)
            let pml4 = self.pml4_mut();
            let pml4_index = virt_addr.pml4_index();
            let pdpt = self.get_or_create_table(
                &mut pml4[pml4_index],
                allocator,
            )?;

            // Get or create PD (Page Directory)
            let pdpt_index = virt_addr.pdpt_index();
            let pd = self.get_or_create_table(
                &mut pdpt[pdpt_index],
                allocator,
            )?;

            // Get or create PT (Page Table)
            let pd_index = virt_addr.pd_index();
            let pt = self.get_or_create_table(
                &mut pd[pd_index],
                allocator,
            )?;

            // Map the page
            let pt_index = virt_addr.pt_index();
            let entry = &mut pt[pt_index];

            if entry.is_present() {
                return Err(MapperError::PageAlreadyMapped);
            }

            // Set the entry with physical address and flags
            entry.set_address(phys_addr, flags);

            // Invalidate TLB entry for this page
            Self::flush_tlb(virt_addr);

            Ok(())
        }
    }

    /// Unmap a virtual page
    /// 
    /// # Arguments
    /// * `virt_addr` - Virtual address to unmap
    /// 
    /// # Returns
    /// Ok(PhysicalAddress) of the unmapped frame, or Err(MapperError)
    pub fn unmap_page(
        &mut self,
        virt_addr: VirtualAddress,
    ) -> Result<PhysicalAddress, MapperError> {
        if !virt_addr.is_aligned(PAGE_SIZE) {
            return Err(MapperError::InvalidAddress);
        }

        unsafe {
            let pml4 = self.pml4_mut();
            let pml4_index = virt_addr.pml4_index();
            
            if !pml4[pml4_index].is_present() {
                return Err(MapperError::InvalidAddress);
            }

            let pdpt_addr = pml4[pml4_index].physical_address();
            let pdpt = &mut *(pdpt_addr.as_usize() as *mut PageTable);
            let pdpt_index = virt_addr.pdpt_index();

            if !pdpt[pdpt_index].is_present() {
                return Err(MapperError::InvalidAddress);
            }

            let pd_addr = pdpt[pdpt_index].physical_address();
            let pd = &mut *(pd_addr.as_usize() as *mut PageTable);
            let pd_index = virt_addr.pd_index();

            if !pd[pd_index].is_present() {
                return Err(MapperError::InvalidAddress);
            }

            let pt_addr = pd[pd_index].physical_address();
            let pt = &mut *(pt_addr.as_usize() as *mut PageTable);
            let pt_index = virt_addr.pt_index();

            let entry = &mut pt[pt_index];
            if !entry.is_present() {
                return Err(MapperError::InvalidAddress);
            }

            let phys_addr = entry.physical_address();
            entry.clear();

            // Invalidate TLB
            Self::flush_tlb(virt_addr);

            Ok(phys_addr)
        }
    }

    /// Translate a virtual address to physical address
    /// 
    /// # Returns
    /// Some(PhysicalAddress) if mapped, None if not mapped
    pub fn translate(&self, virt_addr: VirtualAddress) -> Option<PhysicalAddress> {
        unsafe {
            let pml4 = self.pml4();
            let pml4_entry = &pml4[virt_addr.pml4_index()];
            if !pml4_entry.is_present() {
                return None;
            }

            let pdpt = &*(pml4_entry.physical_address().as_usize() as *const PageTable);
            let pdpt_entry = &pdpt[virt_addr.pdpt_index()];
            if !pdpt_entry.is_present() {
                return None;
            }

            let pd = &*(pdpt_entry.physical_address().as_usize() as *const PageTable);
            let pd_entry = &pd[virt_addr.pd_index()];
            if !pd_entry.is_present() {
                return None;
            }

            let pt = &*(pd_entry.physical_address().as_usize() as *const PageTable);
            let pt_entry = &pt[virt_addr.pt_index()];
            if !pt_entry.is_present() {
                return None;
            }

            let frame_addr = pt_entry.physical_address();
            let offset = virt_addr.page_offset();
            Some(PhysicalAddress::new(frame_addr.as_usize() + offset))
        }
    }

    /// Identity map a range of pages (virtual address = physical address)
    /// 
    /// # Arguments
    /// * `start` - Start physical address
    /// * `size` - Size in bytes
    /// * `flags` - Page flags
    /// * `allocator` - Frame allocator
    pub fn identity_map_range(
        &mut self,
        start: PhysicalAddress,
        size: usize,
        flags: u64,
        allocator: &mut FrameAllocator,
    ) -> Result<(), MapperError> {
        let start_aligned = start.align_down(PAGE_SIZE);
        let end = PhysicalAddress::new(start.as_usize() + size);
        let end_aligned = end.align_up(PAGE_SIZE);

        let mut current = start_aligned;
        while current.as_usize() < end_aligned.as_usize() {
            let virt = VirtualAddress::new(current.as_usize());
            self.map_page(virt, current, flags, allocator)?;
            current = PhysicalAddress::new(current.as_usize() + PAGE_SIZE);
        }

        Ok(())
    }

    /// Get or create a page table at the given entry
    /// 
    /// If entry is present, return existing table
    /// If not, allocate new frame and create table
    unsafe fn get_or_create_table(
        &mut self,
        entry: &mut PageTableEntry,
        allocator: &mut FrameAllocator,
    ) -> Result<&'static mut PageTable, MapperError> {
        if entry.is_present() {
            // Table already exists
            let addr = entry.physical_address();
            Ok(&mut *(addr.as_usize() as *mut PageTable))
        } else {
            // Create new table
            let frame = allocator
                .allocate_frame()
                .ok_or(MapperError::OutOfMemory)?;

            // Set entry to point to new table
            let flags = PageTableFlags::PRESENT as u64 
                      | PageTableFlags::WRITABLE as u64;
            entry.set_address(frame, flags);

            // Zero out the new table
            let table = &mut *(frame.as_usize() as *mut PageTable);
            table.zero();

            Ok(table)
        }
    }

    /// Flush TLB entry for a virtual address
    fn flush_tlb(virt_addr: VirtualAddress) {
        unsafe {
            core::arch::asm!(
                "invlpg [{}]",
                in(reg) virt_addr.as_usize(),
                options(nostack, preserves_flags)
            );
        }
    }

    /// Flush entire TLB by reloading CR3
    pub fn flush_tlb_all(&self) {
        unsafe {
            core::arch::asm!(
                "mov rax, cr3",
                "mov cr3, rax",
                out("rax") _,
                options(nostack, preserves_flags)
            );
        }
    }

    /// Get PML4 physical address
    pub fn pml4_address(&self) -> PhysicalAddress {
        self.pml4_address
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Note: These tests require a working frame allocator
    // They should be run after frame allocator is initialized

    #[test]
    fn test_mapper_error_types() {
        assert_eq!(MapperError::OutOfMemory, MapperError::OutOfMemory);
        assert_ne!(MapperError::OutOfMemory, MapperError::PageAlreadyMapped);
    }

    #[test]
    fn test_pml4_address() {
        let addr = PhysicalAddress::new(0x1000);
        unsafe {
            let mapper = PageMapper::new(addr);
            assert_eq!(mapper.pml4_address().as_usize(), 0x1000);
        }
    }
}
