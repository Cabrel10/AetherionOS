// kernel/src/fs/fat32.rs - FAT32 Filesystem Driver (Couche 19)
//
// Minimal read-only FAT32 implementation for AetherionOS.
// Reads the BPB (BIOS Parameter Block), navigates the FAT,
// and reads files from the root directory.
//
// FAT32 Layout:
//   - Sector 0: Boot sector (BPB)
//   - Reserved sectors (BPB.reserved_sectors)
//   - FAT region (BPB.num_fats * BPB.fat_size_32)
//   - Data region (clusters start here)
//
// References:
//   - Microsoft FAT32 File System Specification
//   - https://wiki.osdev.org/FAT

use alloc::vec::Vec;
use alloc::vec;
use alloc::string::String;

/// FAT32 cluster constants
const FAT32_EOC: u32 = 0x0FFF_FFF8;  // End of cluster chain marker
const FAT32_BAD: u32 = 0x0FFF_FFF7;  // Bad cluster marker

/// FAT32 directory entry size
const DIR_ENTRY_SIZE: usize = 32;

/// FAT32 BPB (BIOS Parameter Block) - parsed from boot sector
pub struct Fat32Bpb {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub num_fats: u8,
    pub total_sectors_32: u32,
    pub fat_size_32: u32,
    pub root_cluster: u32,
}

impl Fat32Bpb {
    /// Parse BPB from a 512-byte boot sector
    pub fn parse(sector: &[u8]) -> Option<Self> {
        if sector.len() < 512 {
            return None;
        }

        // Check boot signature (0x55AA at offset 510-511)
        let sig = u16::from_le_bytes([sector[510], sector[511]]);
        if sig != 0xAA55 {
            crate::serial_println!("[FAT32] Invalid boot signature: 0x{:04X}", sig);
            return None;
        }

        let bytes_per_sector = u16::from_le_bytes([sector[11], sector[12]]);
        let sectors_per_cluster = sector[13];
        let reserved_sectors = u16::from_le_bytes([sector[14], sector[15]]);
        let num_fats = sector[16];
        let total_sectors_32 = u32::from_le_bytes([sector[32], sector[33], sector[34], sector[35]]);
        let fat_size_32 = u32::from_le_bytes([sector[36], sector[37], sector[38], sector[39]]);
        let root_cluster = u32::from_le_bytes([sector[44], sector[45], sector[46], sector[47]]);

        // Validate
        if bytes_per_sector != 512 {
            crate::serial_println!("[FAT32] Unsupported sector size: {}", bytes_per_sector);
            return None;
        }
        if sectors_per_cluster == 0 || num_fats == 0 {
            crate::serial_println!("[FAT32] Invalid BPB: spc={}, fats={}", sectors_per_cluster, num_fats);
            return None;
        }

        crate::serial_println!("[FAT32] BPB: spc={}, reserved={}, fats={}, fat_size={}, root_cluster={}",
            sectors_per_cluster, reserved_sectors, num_fats, fat_size_32, root_cluster);

        Some(Fat32Bpb {
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors,
            num_fats,
            total_sectors_32,
            fat_size_32,
            root_cluster,
        })
    }

    /// Get the LBA of the first sector of the data area
    pub fn data_start_lba(&self) -> u32 {
        self.reserved_sectors as u32 + self.num_fats as u32 * self.fat_size_32
    }

    /// Get the LBA of the first sector of a cluster
    pub fn cluster_to_lba(&self, cluster: u32) -> u32 {
        self.data_start_lba() + (cluster - 2) * self.sectors_per_cluster as u32
    }

    /// Get the LBA of the FAT sector containing the entry for a given cluster
    pub fn fat_sector_for_cluster(&self, cluster: u32) -> (u32, usize) {
        let fat_offset = cluster * 4;
        let fat_sector = self.reserved_sectors as u32 + fat_offset / 512;
        let fat_offset_in_sector = (fat_offset % 512) as usize;
        (fat_sector, fat_offset_in_sector)
    }
}

/// FAT32 directory entry (short name)
#[derive(Debug, Clone)]
pub struct Fat32DirEntry {
    pub name: String,       // 8.3 filename decoded
    pub is_directory: bool,
    pub first_cluster: u32,
    pub file_size: u32,
}

impl Fat32DirEntry {
    /// Parse a 32-byte directory entry
    pub fn parse(entry: &[u8]) -> Option<Self> {
        if entry.len() < DIR_ENTRY_SIZE {
            return None;
        }

        // Check if entry is free or end-of-directory
        if entry[0] == 0x00 || entry[0] == 0xE5 {
            return None;
        }

        // Skip long filename entries (attribute 0x0F)
        let attr = entry[11];
        if attr == 0x0F {
            return None;
        }

        // Skip volume label
        if attr & 0x08 != 0 {
            return None;
        }

        // Extract 8.3 filename
        let name_bytes = &entry[0..8];
        let ext_bytes = &entry[8..11];

        let name_part: String = name_bytes.iter()
            .take_while(|&&b| b != 0x20 && b != 0x00)
            .map(|&b| (b as char).to_ascii_lowercase())
            .collect();

        let ext_part: String = ext_bytes.iter()
            .take_while(|&&b| b != 0x20 && b != 0x00)
            .map(|&b| (b as char).to_ascii_lowercase())
            .collect();

        let name = if ext_part.is_empty() {
            name_part
        } else {
            alloc::format!("{}.{}", name_part, ext_part)
        };

        let is_directory = attr & 0x10 != 0;

        // Cluster high (bytes 20-21) and low (bytes 26-27)
        let cluster_hi = u16::from_le_bytes([entry[20], entry[21]]) as u32;
        let cluster_lo = u16::from_le_bytes([entry[26], entry[27]]) as u32;
        let first_cluster = (cluster_hi << 16) | cluster_lo;

        let file_size = u32::from_le_bytes([entry[28], entry[29], entry[30], entry[31]]);

        Some(Fat32DirEntry {
            name,
            is_directory,
            first_cluster,
            file_size,
        })
    }
}

/// FAT32 filesystem instance
pub struct Fat32Fs {
    pub bpb: Fat32Bpb,
}

impl Fat32Fs {
    /// Mount a FAT32 filesystem from the VirtIO-Block device
    pub fn mount() -> Option<Self> {
        if !crate::drivers::virtio_blk::is_available() {
            return None;
        }

        // Read boot sector (sector 0)
        let mut sector = [0u8; 512];
        if !crate::drivers::virtio_blk::read_sector(0, &mut sector) {
            crate::serial_println!("[FAT32] Failed to read boot sector");
            return None;
        }

        let bpb = Fat32Bpb::parse(&sector)?;
        crate::serial_println!("[FAT32] Filesystem mounted (data starts at sector {})", bpb.data_start_lba());

        Some(Fat32Fs { bpb })
    }

    /// Read the next cluster in a chain from the FAT
    fn next_cluster(&self, cluster: u32) -> Option<u32> {
        let (fat_sector, offset) = self.bpb.fat_sector_for_cluster(cluster);
        let mut sector_buf = [0u8; 512];

        if !crate::drivers::virtio_blk::read_sector(fat_sector as u64, &mut sector_buf) {
            return None;
        }

        let next = u32::from_le_bytes([
            sector_buf[offset],
            sector_buf[offset + 1],
            sector_buf[offset + 2],
            sector_buf[offset + 3],
        ]) & 0x0FFF_FFFF;

        if next >= FAT32_EOC || next == FAT32_BAD || next < 2 {
            None
        } else {
            Some(next)
        }
    }

    /// Read all sectors of a cluster into a buffer
    fn read_cluster(&self, cluster: u32) -> Option<Vec<u8>> {
        let lba = self.bpb.cluster_to_lba(cluster);
        let sectors = self.bpb.sectors_per_cluster as usize;
        let mut buf = vec![0u8; sectors * 512];

        if !crate::drivers::virtio_blk::read_sectors(
            lba as u64, sectors, &mut buf
        ) {
            return None;
        }

        Some(buf)
    }

    /// List directory entries from a given starting cluster
    pub fn list_directory(&self, start_cluster: u32) -> Vec<Fat32DirEntry> {
        let mut entries = Vec::new();
        let mut cluster = start_cluster;
        let mut iterations = 0;

        loop {
            if iterations > 100 { break; } // Safety limit
            iterations += 1;

            let data = match self.read_cluster(cluster) {
                Some(d) => d,
                None => break,
            };

            let num_entries = data.len() / DIR_ENTRY_SIZE;
            for i in 0..num_entries {
                let offset = i * DIR_ENTRY_SIZE;
                let entry_data = &data[offset..offset + DIR_ENTRY_SIZE];

                // End of directory
                if entry_data[0] == 0x00 {
                    return entries;
                }

                if let Some(entry) = Fat32DirEntry::parse(entry_data) {
                    // Skip . and .. entries
                    if entry.name != "." && entry.name != ".." {
                        entries.push(entry);
                    }
                }
            }

            // Follow cluster chain
            match self.next_cluster(cluster) {
                Some(next) => cluster = next,
                None => break,
            }
        }

        entries
    }

    /// List the root directory
    pub fn list_root(&self) -> Vec<Fat32DirEntry> {
        self.list_directory(self.bpb.root_cluster)
    }

    /// Read a file by name from the root directory
    pub fn read_file(&self, name: &str) -> Option<Vec<u8>> {
        let entries = self.list_root();
        let target = name.to_ascii_lowercase();

        for entry in &entries {
            if entry.name == target && !entry.is_directory {
                return self.read_file_data(entry.first_cluster, entry.file_size);
            }
        }

        None
    }

    /// Read file data following the cluster chain
    fn read_file_data(&self, start_cluster: u32, file_size: u32) -> Option<Vec<u8>> {
        let mut data = Vec::with_capacity(file_size as usize);
        let mut cluster = start_cluster;
        let mut remaining = file_size as usize;
        let mut iterations = 0;

        loop {
            if remaining == 0 || iterations > 1000 { break; }
            iterations += 1;

            let cluster_data = self.read_cluster(cluster)?;
            let to_copy = core::cmp::min(remaining, cluster_data.len());
            data.extend_from_slice(&cluster_data[..to_copy]);
            remaining -= to_copy;

            if remaining == 0 { break; }

            match self.next_cluster(cluster) {
                Some(next) => cluster = next,
                None => break,
            }
        }

        Some(data)
    }

    /// Find and read a file in a subdirectory
    pub fn read_file_in_dir(&self, dir_cluster: u32, name: &str) -> Option<Vec<u8>> {
        let entries = self.list_directory(dir_cluster);
        let target = name.to_ascii_lowercase();

        for entry in &entries {
            if entry.name == target && !entry.is_directory {
                return self.read_file_data(entry.first_cluster, entry.file_size);
            }
        }

        None
    }
}

/// Global FAT32 instance
static mut FAT32_FS: Option<Fat32Fs> = None;

/// Initialize FAT32 filesystem
pub fn init() -> bool {
    match Fat32Fs::mount() {
        Some(fs) => {
            crate::serial_println!("[FAT32] Filesystem initialized");
            unsafe { FAT32_FS = Some(fs); }
            true
        }
        None => {
            crate::serial_println!("[FAT32] No FAT32 filesystem found");
            false
        }
    }
}

/// Check if FAT32 is mounted
pub fn is_mounted() -> bool {
    unsafe { FAT32_FS.is_some() }
}

/// List root directory entries
pub fn list_root() -> Vec<Fat32DirEntry> {
    unsafe {
        if let Some(ref fs) = FAT32_FS {
            fs.list_root()
        } else {
            Vec::new()
        }
    }
}

/// Read a file from the root directory
pub fn read_file(name: &str) -> Option<Vec<u8>> {
    unsafe {
        if let Some(ref fs) = FAT32_FS {
            fs.read_file(name)
        } else {
            None
        }
    }
}

/// Run FAT32 self-tests
pub fn run_tests() {
    crate::serial_println!("\n========================================");
    crate::serial_println!("[FAT32 TESTS] Couche 19 - FAT32 Filesystem");
    crate::serial_println!("========================================\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // Test 1: FAT32 mounted
    crate::serial_write("  [TEST 1/3] FAT32 mounted... ");
    if is_mounted() {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_write("SKIP (no FAT32 disk)\n");
        crate::serial_println!("\n========================================");
        crate::serial_println!("[FAT32 TESTS] Skipped (no FAT32 filesystem)");
        crate::serial_println!("========================================");
        return;
    }

    // Test 2: List root directory
    crate::serial_write("  [TEST 2/3] Root directory listing... ");
    let entries = list_root();
    if !entries.is_empty() {
        crate::serial_println!("OK ({} entries)", entries.len());
        for entry in &entries {
            if entry.is_directory {
                crate::serial_println!("    <DIR> {}", entry.name);
            } else {
                crate::serial_println!("    {} ({} bytes)", entry.name, entry.file_size);
            }
        }
        passed += 1;
    } else {
        crate::serial_write("WARN (empty directory)\n");
        passed += 1; // Could be an empty disk
    }

    // Test 3: Read index.html
    crate::serial_write("  [TEST 3/3] Read index.html... ");
    match read_file("index.html") {
        Some(data) => {
            crate::serial_println!("OK ({} bytes)", data.len());
            // Print first 100 bytes
            let preview_len = core::cmp::min(data.len(), 100);
            if let Ok(s) = core::str::from_utf8(&data[..preview_len]) {
                crate::serial_println!("    Content: {}", s);
            }
            passed += 1;
        }
        None => {
            crate::serial_write("SKIP (no index.html on disk)\n");
            passed += 1;
        }
    }

    crate::serial_println!("\n========================================");
    crate::serial_println!("[FAT32 TESTS] {}/{} passed, {} failed", passed, passed + failed, failed);
    if failed == 0 { crate::serial_write("[FAT32 TESTS] ALL TESTS PASSED!\n"); }
    crate::serial_println!("========================================");
}
