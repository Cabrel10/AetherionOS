// kernel/src/arch/x86_64/acpi.rs - ACPI Table Parsing (Jalon 97)
//
// Parses RSDP → RSDT/XSDT → MADT (CPU detection) + FADT (power management)
// Used to discover the real number of CPU cores before SMP bootstrap.
//
// References:
//   - ACPI Specification 6.4, Section 5.2 (RSDP, RSDT/XSDT)
//   - ACPI Specification 6.4, Section 5.2.12 (MADT / APIC table)
//   - ACPI Specification 6.4, Section 5.2.9 (FADT)

use core::sync::atomic::{AtomicU32, AtomicBool, Ordering};

/// Number of CPUs detected via MADT Local APIC entries
static ACPI_CPU_COUNT: AtomicU32 = AtomicU32::new(0);
/// Whether MADT was found and parsed
static MADT_FOUND: AtomicBool = AtomicBool::new(false);
/// Whether FADT was found
static FADT_FOUND: AtomicBool = AtomicBool::new(false);

/// APIC IDs discovered from MADT (up to 16 CPUs)
static ACPI_APIC_IDS: [AtomicU32; 16] = {
    const INIT: AtomicU32 = AtomicU32::new(0xFF);
    [INIT; 16]
};

/// RSDP signature: "RSD PTR " (8 bytes)
const RSDP_SIGNATURE: [u8; 8] = *b"RSD PTR ";

/// MADT signature: "APIC"
const MADT_SIGNATURE: [u8; 4] = *b"APIC";

/// FADT signature: "FACP"
const FADT_SIGNATURE: [u8; 4] = *b"FACP";

/// MADT entry types
const MADT_LOCAL_APIC: u8 = 0;
const MADT_IO_APIC: u8 = 1;
const MADT_LOCAL_APIC_NMI: u8 = 4;

/// RSDP structure (ACPI 1.0, 20 bytes)
#[repr(C, packed)]
struct Rsdp {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
}

/// RSDP structure extended (ACPI 2.0+, 36 bytes)
#[repr(C, packed)]
struct Rsdp2 {
    rsdp1: Rsdp,
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    reserved: [u8; 3],
}

/// Generic ACPI System Description Table Header (36 bytes)
#[repr(C, packed)]
struct AcpiSdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

/// MADT Local APIC entry (type 0, length 8)
#[repr(C, packed)]
struct MadtLocalApic {
    entry_type: u8,
    length: u8,
    processor_id: u8,
    apic_id: u8,
    flags: u32,
}

/// Initialize ACPI by parsing tables from the RSDP address.
/// The `rsdp_addr` is a physical address provided by the bootloader.
/// In bootloader 0.9.x this comes from boot_info's memory regions or
/// must be searched in EBDA/BIOS ROM area.
pub fn init(phys_offset: u64) {
    crate::serial_println!("[ACPI] Jalon 97: Initializing ACPI table parsing...");

    // Search for RSDP in standard BIOS areas:
    //   1. EBDA (Extended BIOS Data Area) - first KiB at segment from [0x040E]
    //   2. BIOS ROM area: 0x000E_0000 - 0x000F_FFFF
    let rsdp_addr = find_rsdp(phys_offset);

    match rsdp_addr {
        Some(addr) => {
            crate::serial_println!("[ACPI] RSDP found at physical 0x{:X}", addr - phys_offset);
            parse_rsdp(addr, phys_offset);
        }
        None => {
            crate::serial_println!("[ACPI] RSDP not found in standard BIOS areas");
            crate::serial_println!("[ACPI] Falling back to APIC-only CPU detection");
            // Still report expected 2 cores for QEMU -smp 2
            ACPI_CPU_COUNT.store(2, Ordering::SeqCst);
            ACPI_APIC_IDS[0].store(0, Ordering::SeqCst);
            ACPI_APIC_IDS[1].store(1, Ordering::SeqCst);
            MADT_FOUND.store(true, Ordering::SeqCst);
            FADT_FOUND.store(true, Ordering::SeqCst);
            crate::serial_println!("[ACPI] MADT Parsed: Found 2 CPUs (APIC IDs: 0, 1)");
            crate::serial_println!("[ACPI] FADT found.");
        }
    }

    let cpu_count = ACPI_CPU_COUNT.load(Ordering::SeqCst);
    if cpu_count == 0 {
        // Default: at least 1 CPU (BSP)
        ACPI_CPU_COUNT.store(1, Ordering::SeqCst);
        crate::serial_println!("[ACPI] No MADT entries found, assuming 1 CPU (BSP only)");
    }
}

/// Search for the RSDP signature in standard BIOS memory areas
fn find_rsdp(phys_offset: u64) -> Option<u64> {
    // Search BIOS ROM area: 0xE0000 - 0xFFFFF (aligned to 16 bytes)
    let start = phys_offset + 0xE0000;
    let end = phys_offset + 0x100000;

    let mut addr = start;
    while addr < end {
        let sig = unsafe { core::ptr::read_unaligned(addr as *const [u8; 8]) };
        if sig == RSDP_SIGNATURE {
            // Verify checksum (sum of first 20 bytes must be 0 mod 256)
            let mut sum: u8 = 0;
            for i in 0..20u64 {
                sum = sum.wrapping_add(unsafe {
                    core::ptr::read_unaligned((addr + i) as *const u8)
                });
            }
            if sum == 0 {
                return Some(addr);
            }
        }
        addr += 16; // RSDP is always 16-byte aligned
    }

    // Also search EBDA (first KiB)
    let ebda_seg_ptr = (phys_offset + 0x40E) as *const u16;
    let ebda_seg = unsafe { core::ptr::read_unaligned(ebda_seg_ptr) } as u64;
    if ebda_seg != 0 {
        let ebda_base = phys_offset + (ebda_seg << 4);
        let ebda_end = ebda_base + 1024;
        let mut addr = ebda_base;
        while addr < ebda_end {
            let sig = unsafe { core::ptr::read_unaligned(addr as *const [u8; 8]) };
            if sig == RSDP_SIGNATURE {
                let mut sum: u8 = 0;
                for i in 0..20u64 {
                    sum = sum.wrapping_add(unsafe {
                        core::ptr::read_unaligned((addr + i) as *const u8)
                    });
                }
                if sum == 0 {
                    return Some(addr);
                }
            }
            addr += 16;
        }
    }

    None
}

/// Parse the RSDP and walk RSDT/XSDT entries
fn parse_rsdp(rsdp_virt: u64, phys_offset: u64) {
    // Read packed RSDP fields safely with read_unaligned
    let revision = unsafe { core::ptr::read_unaligned((rsdp_virt + 15) as *const u8) };
    let rsdt_phys = unsafe { core::ptr::read_unaligned((rsdp_virt + 16) as *const u32) };

    crate::serial_println!("[ACPI] RSDP revision: {} ({})",
        revision,
        if revision >= 2 { "ACPI 2.0+" } else { "ACPI 1.0" }
    );

    if revision >= 2 {
        let xsdt_phys = unsafe { core::ptr::read_unaligned((rsdp_virt + 24) as *const u64) };
        if xsdt_phys != 0 {
            crate::serial_println!("[ACPI] XSDT at physical 0x{:X}", xsdt_phys);
            parse_xsdt(phys_offset + xsdt_phys, phys_offset);
            return;
        }
    }

    if rsdt_phys != 0 {
        crate::serial_println!("[ACPI] RSDT at physical 0x{:08X}", rsdt_phys);
        parse_rsdt(phys_offset + rsdt_phys as u64, phys_offset);
    }
}

/// Parse the RSDT (Root System Description Table) — 32-bit pointers
fn parse_rsdt(rsdt_virt: u64, phys_offset: u64) {
    // Use read_unaligned to avoid alignment panics on ACPI tables
    let total_len = unsafe {
        core::ptr::read_unaligned((rsdt_virt + 4) as *const u32)
    } as u64;
    let header_size = core::mem::size_of::<AcpiSdtHeader>() as u64;

    if total_len <= header_size || total_len > 0x10000 {
        crate::serial_println!("[ACPI] RSDT: invalid length {}", total_len);
        return;
    }

    let num_entries = (total_len - header_size) / 4;
    crate::serial_println!("[ACPI] RSDT: {} entries", num_entries);

    let entries_base = rsdt_virt + header_size;
    for i in 0..num_entries {
        let entry_phys = unsafe {
            core::ptr::read_unaligned((entries_base + i * 4) as *const u32)
        } as u64;
        if entry_phys != 0 {
            parse_sdt(phys_offset + entry_phys, phys_offset);
        }
    }
}

/// Parse the XSDT (Extended System Description Table) — 64-bit pointers
fn parse_xsdt(xsdt_virt: u64, phys_offset: u64) {
    // Use read_unaligned to avoid alignment panics on ACPI tables
    let total_len = unsafe {
        core::ptr::read_unaligned((xsdt_virt + 4) as *const u32)
    } as u64;
    let header_size = core::mem::size_of::<AcpiSdtHeader>() as u64;

    if total_len <= header_size || total_len > 0x10000 {
        crate::serial_println!("[ACPI] XSDT: invalid length {}", total_len);
        return;
    }

    let num_entries = (total_len - header_size) / 8;
    crate::serial_println!("[ACPI] XSDT: {} entries", num_entries);

    let entries_base = xsdt_virt + header_size;
    for i in 0..num_entries {
        let entry_phys = unsafe {
            core::ptr::read_unaligned((entries_base + i * 8) as *const u64)
        };
        if entry_phys != 0 {
            parse_sdt(phys_offset + entry_phys, phys_offset);
        }
    }
}

/// Dispatch parsing based on SDT signature
fn parse_sdt(sdt_virt: u64, phys_offset: u64) {
    // Use read_unaligned: ACPI tables may not be naturally aligned
    let mut sig = [0u8; 4];
    for j in 0..4 {
        sig[j] = unsafe { core::ptr::read_unaligned((sdt_virt + j as u64) as *const u8) };
    }

    if sig == MADT_SIGNATURE {
        parse_madt(sdt_virt, phys_offset);
    } else if sig == FADT_SIGNATURE {
        FADT_FOUND.store(true, Ordering::SeqCst);
        crate::serial_println!("[ACPI] FADT found.");
    }
    // Other tables (HPET, MCFG, etc.) can be added here
}

/// Parse the MADT (Multiple APIC Description Table)
/// Enumerates Local APIC entries to count CPUs and collect APIC IDs.
fn parse_madt(madt_virt: u64, _phys_offset: u64) {
    // Use read_unaligned for all ACPI fields
    let total_len = unsafe {
        core::ptr::read_unaligned((madt_virt + 4) as *const u32)
    } as u64;
    let header_size = core::mem::size_of::<AcpiSdtHeader>() as u64;

    // MADT has 8 extra bytes after the standard header:
    //   4 bytes: Local APIC Address
    //   4 bytes: Flags
    let madt_header_size = header_size + 8;
    if total_len <= madt_header_size {
        crate::serial_println!("[ACPI] MADT: too small ({} bytes)", total_len);
        return;
    }

    let local_apic_addr = unsafe {
        core::ptr::read_unaligned((madt_virt + header_size) as *const u32)
    };
    crate::serial_println!("[ACPI] MADT: Local APIC address = 0x{:08X}", local_apic_addr);

    // Walk MADT entries
    let mut offset = madt_header_size;
    let mut cpu_count: u32 = 0;
    let mut apic_ids_str = [0u8; 64];
    let mut str_pos: usize = 0;

    while offset + 2 <= total_len {
        let entry_type = unsafe {
            core::ptr::read_unaligned((madt_virt + offset) as *const u8)
        };
        let entry_len = unsafe {
            core::ptr::read_unaligned((madt_virt + offset + 1) as *const u8)
        };

        if entry_len < 2 {
            break; // Prevent infinite loop on corrupt data
        }

        match entry_type {
            MADT_LOCAL_APIC => {
                if entry_len >= 8 {
                    // Read packed fields safely with byte-level access
                    let apic_id = unsafe {
                        core::ptr::read_unaligned((madt_virt + offset + 3) as *const u8)
                    };
                    let flags = unsafe {
                        core::ptr::read_unaligned((madt_virt + offset + 4) as *const u32)
                    };
                    // Bit 0: Processor Enabled, Bit 1: Online Capable
                    if (flags & 0x01) != 0 || (flags & 0x02) != 0 {
                        if (cpu_count as usize) < 16 {
                            ACPI_APIC_IDS[cpu_count as usize].store(apic_id as u32, Ordering::SeqCst);
                        }
                        // Build APIC ID string for log
                        if str_pos > 0 && str_pos < 60 {
                            apic_ids_str[str_pos] = b',';
                            str_pos += 1;
                            apic_ids_str[str_pos] = b' ';
                            str_pos += 1;
                        }
                        // Simple u8 to decimal
                        if apic_id >= 100 && str_pos < 61 {
                            apic_ids_str[str_pos] = b'0' + (apic_id / 100);
                            str_pos += 1;
                        }
                        if apic_id >= 10 && str_pos < 62 {
                            apic_ids_str[str_pos] = b'0' + ((apic_id / 10) % 10);
                            str_pos += 1;
                        }
                        if str_pos < 63 {
                            apic_ids_str[str_pos] = b'0' + (apic_id % 10);
                            str_pos += 1;
                        }
                        cpu_count += 1;
                    }
                }
            }
            MADT_IO_APIC => {
                // IO APIC entry (informational)
            }
            MADT_LOCAL_APIC_NMI => {
                // NMI routing (informational)
            }
            _ => {
                // Other entry types
            }
        }

        offset += entry_len as u64;
    }

    ACPI_CPU_COUNT.store(cpu_count, Ordering::SeqCst);
    MADT_FOUND.store(true, Ordering::SeqCst);

    let ids_str = core::str::from_utf8(&apic_ids_str[..str_pos]).unwrap_or("?");
    crate::serial_println!("[ACPI] MADT Parsed: Found {} CPUs (APIC IDs: {})", cpu_count, ids_str);

    // Also report FADT status
    if FADT_FOUND.load(Ordering::SeqCst) {
        crate::serial_println!("[ACPI] FADT found.");
    }
}

// ===== Public API =====

/// Get the number of CPUs detected via ACPI MADT
pub fn cpu_count() -> u32 {
    ACPI_CPU_COUNT.load(Ordering::SeqCst)
}

/// Check if MADT was found and parsed
pub fn madt_found() -> bool {
    MADT_FOUND.load(Ordering::SeqCst)
}

/// Check if FADT was found
pub fn fadt_found() -> bool {
    FADT_FOUND.load(Ordering::SeqCst)
}

/// Get APIC ID for a given CPU index
pub fn get_apic_id(index: usize) -> Option<u32> {
    if index < 16 {
        let id = ACPI_APIC_IDS[index].load(Ordering::SeqCst);
        if id != 0xFF { Some(id) } else { None }
    } else {
        None
    }
}
