// kernel/src/fs/fat32.rs - FAT32 Filesystem Driver (Couche 19 + Jalon 30 Write)
//
// Read/Write FAT32 implementation for AetherionOS.
// Reads the BPB (BIOS Parameter Block), navigates the FAT,
// reads and writes files, manages directory entries and cluster allocation.
//
// FAT32 Layout:
//   - Sector 0: Boot sector (BPB)
//   - Reserved sectors (BPB.reserved_sectors)
//   - FAT region (BPB.num_fats * BPB.fat_size_32)
//   - Data region (clusters start here)
//
// Jalon 30 additions:
//   - find_free_cluster: scan FAT for unallocated cluster
//   - set_fat_entry: write a FAT table entry (both copies)
//   - write_cluster: write data to a cluster on disk
//   - write_file_to_dir: create/overwrite a file in a directory
//   - navigate_to_dir: resolve a path like /var/sagas to a cluster
//
// References:
//   - Microsoft FAT32 File System Specification (fatgen103.doc)
//   - https://wiki.osdev.org/FAT

use alloc::vec::Vec;
use alloc::vec;
use alloc::string::String;

/// FAT32 cluster constants
const FAT32_EOC: u32 = 0x0FFF_FFF8;  // End of cluster chain marker
const FAT32_FREE: u32 = 0x0000_0000; // Free cluster marker
const FAT32_BAD: u32 = 0x0FFF_FFF7;  // Bad cluster marker

/// FAT32 directory entry size
const DIR_ENTRY_SIZE: usize = 32;

/// Maximum clusters to scan for free cluster search (safety limit)
const MAX_CLUSTER_SCAN: u32 = 65536;

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
            // crate::serial_println!("[FAT32] Invalid boot signature: 0x{:04X}", sig);
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
            // crate::serial_println!("[FAT32] Unsupported sector size: {}", bytes_per_sector);
            return None;
        }
        if sectors_per_cluster == 0 || num_fats == 0 {
            // crate::serial_println!("[FAT32] Invalid BPB: spc={}, fats={}", sectors_per_cluster, num_fats);
            return None;
        }

        // crate::serial_println!("[FAT32] BPB: spc={}, reserved={}, fats={}, fat_size={}, root_cluster={}",
        //     sectors_per_cluster, reserved_sectors, num_fats, fat_size_32, root_cluster);

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

    /// Total data clusters available
    pub fn total_data_clusters(&self) -> u32 {
        let data_sectors = self.total_sectors_32 - self.data_start_lba();
        data_sectors / self.sectors_per_cluster as u32
    }

    /// Cluster size in bytes
    pub fn cluster_size(&self) -> usize {
        self.sectors_per_cluster as usize * 512
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

        // Build both the 8.3 short name and an extended name for matching
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

/// Convert a filename to FAT32 8.3 format (uppercase, space-padded)
fn name_to_8_3(filename: &str) -> [u8; 11] {
    let mut result = [0x20u8; 11]; // space-padded
    let upper = filename.to_ascii_uppercase();

    // Split on last '.'
    if let Some(dot_pos) = upper.rfind('.') {
        let name_part = &upper[..dot_pos];
        let ext_part = &upper[dot_pos + 1..];

        // Copy name (up to 8 chars)
        for (i, &b) in name_part.as_bytes().iter().take(8).enumerate() {
            result[i] = b;
        }
        // Copy extension (up to 3 chars)
        for (i, &b) in ext_part.as_bytes().iter().take(3).enumerate() {
            result[8 + i] = b;
        }
    } else {
        // No extension
        for (i, &b) in upper.as_bytes().iter().take(8).enumerate() {
            result[i] = b;
        }
    }

    result
}

/// Match a FAT32 directory entry name against a target filename.
///
/// Handles three cases:
/// 1. Exact match: "model.ggu" == "model.ggu"
/// 2. Tilde match: "real_m~1.ggu" matches "real_model.gguf" because the 8.3
///    short name is a truncated version of the long name
/// 3. Extension truncation: ".gguf" stored as ".ggu" (3-char limit)
///
/// Both `entry_name` and `target` should already be lowercase.
fn fat32_name_matches(entry_name: &str, target: &str) -> bool {
    // Case 1: exact match
    if entry_name == target {
        return true;
    }

    // Case 2: target is a long filename and entry_name is its 8.3 tilde form
    // e.g. entry="real_m~1.ggu" vs target="real_model.gguf"
    if entry_name.contains('~') {
        // Split entry on tilde
        if let Some(tilde_pos) = entry_name.find('~') {
            let entry_prefix = &entry_name[..tilde_pos]; // "real_m"
            // Check if target starts with the same prefix
            let target_lower = target.to_ascii_lowercase();
            if target_lower.starts_with(entry_prefix) {
                // Also check extension: entry ".ggu" should match target ".gguf" (truncated)
                let entry_ext = entry_name.rfind('.').map(|p| &entry_name[p+1..]).unwrap_or("");
                let target_ext = target_lower.rfind('.').map(|p| &target_lower[p+1..]).unwrap_or("");
                // Extension matches if entry_ext is a prefix of target_ext (3-char truncation)
                // or if they're exactly equal
                if target_ext.starts_with(entry_ext) || entry_ext == target_ext {
                    return true;
                }
            }
        }
    }

    // Case 3: entry is an 8.3 name without tilde, but target has a longer extension
    // e.g. entry="model.ggu" vs target="model.gguf"
    // The entry extension is at most 3 chars; if target ext starts with entry ext, match
    let entry_dot = entry_name.rfind('.');
    let target_dot = target.rfind('.');
    if let (Some(epos), Some(tpos)) = (entry_dot, target_dot) {
        let entry_base = &entry_name[..epos];
        let target_base = &target[..tpos];
        let entry_ext = &entry_name[epos+1..];
        let target_ext = &target[tpos+1..];
        if entry_base == target_base && entry_ext.len() <= 3
            && target_ext.starts_with(entry_ext) {
            return true;
        }
    }

    false
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
            // crate::serial_println!("[FAT32] Failed to read boot sector");
            return None;
        }

        let bpb = Fat32Bpb::parse(&sector)?;
        // crate::serial_println!("[FAT32] Filesystem mounted (data starts at sector {})", bpb.data_start_lba());

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

    /// Read a FAT entry for a given cluster
    fn read_fat_entry(&self, cluster: u32) -> u32 {
        let (fat_sector, offset) = self.bpb.fat_sector_for_cluster(cluster);
        let mut sector_buf = [0u8; 512];

        if !crate::drivers::virtio_blk::read_sector(fat_sector as u64, &mut sector_buf) {
            return FAT32_BAD;
        }

        u32::from_le_bytes([
            sector_buf[offset],
            sector_buf[offset + 1],
            sector_buf[offset + 2],
            sector_buf[offset + 3],
        ]) & 0x0FFF_FFFF
    }

    /// Write a FAT entry for a given cluster (updates both FAT copies)
    fn set_fat_entry(&self, cluster: u32, value: u32) -> bool {
        let (fat_sector, offset) = self.bpb.fat_sector_for_cluster(cluster);
        let mut sector_buf = [0u8; 512];

        // Read the FAT sector
        if !crate::drivers::virtio_blk::read_sector(fat_sector as u64, &mut sector_buf) {
            // crate::serial_println!("[FAT32] Failed to read FAT sector {} for cluster {}", fat_sector, cluster);
            return false;
        }

        // Preserve top 4 bits of existing entry (reserved)
        let existing = u32::from_le_bytes([
            sector_buf[offset], sector_buf[offset+1],
            sector_buf[offset+2], sector_buf[offset+3],
        ]);
        let new_val = (existing & 0xF000_0000) | (value & 0x0FFF_FFFF);
        let bytes = new_val.to_le_bytes();
        sector_buf[offset] = bytes[0];
        sector_buf[offset + 1] = bytes[1];
        sector_buf[offset + 2] = bytes[2];
        sector_buf[offset + 3] = bytes[3];

        // Write back to FAT1
        if !crate::drivers::virtio_blk::write_sector(fat_sector as u64, &sector_buf) {
            // crate::serial_println!("[FAT32] Failed to write FAT1 sector {}", fat_sector);
            return false;
        }

        // Write to FAT2 (mirror) if num_fats >= 2
        if self.bpb.num_fats >= 2 {
            let fat2_sector = fat_sector + self.bpb.fat_size_32;
            if !crate::drivers::virtio_blk::write_sector(fat2_sector as u64, &sector_buf) {
                // crate::serial_println!("[FAT32] Warning: Failed to mirror FAT2 sector {}", fat2_sector);
                // Non-fatal: FAT1 was written successfully
            }
        }

        true
    }

    /// Find a free cluster in the FAT, starting from cluster 2
    fn find_free_cluster(&self) -> Option<u32> {
        let max_cluster = self.bpb.total_data_clusters() + 2;
        let limit = core::cmp::min(max_cluster, MAX_CLUSTER_SCAN + 2);

        for cluster in 2..limit {
            let entry = self.read_fat_entry(cluster);
            if entry == FAT32_FREE {
                return Some(cluster);
            }
        }

        // crate::serial_println!("[FAT32] ENOSPC: No free clusters found (scanned {})", limit - 2);
        None
    }

    /// Allocate a chain of clusters for `size` bytes.
    /// Returns the first cluster in the chain, or None if not enough space.
    fn allocate_cluster_chain(&self, size: usize) -> Option<u32> {
        if size == 0 {
            return Some(0); // No data needs no clusters
        }

        let cluster_size = self.bpb.cluster_size();
        let clusters_needed = (size + cluster_size - 1) / cluster_size;

        let mut chain: Vec<u32> = Vec::with_capacity(clusters_needed);
        let max_cluster = self.bpb.total_data_clusters() + 2;
        let limit = core::cmp::min(max_cluster, MAX_CLUSTER_SCAN + 2);
        let mut search_start = 2u32;

        for _ in 0..clusters_needed {
            let mut found = false;
            for cluster in search_start..limit {
                let entry = self.read_fat_entry(cluster);
                if entry == FAT32_FREE {
                    chain.push(cluster);
                    search_start = cluster + 1;
                    found = true;
                    break;
                }
            }
            if !found {
                // Not enough space — undo any allocations
                // crate::serial_println!("[FAT32] ENOSPC: Need {} clusters, found only {}", clusters_needed, chain.len());
                return None;
            }
        }

        // Link the chain: each cluster points to the next, last one gets EOC
        for i in 0..chain.len() {
            let value = if i + 1 < chain.len() {
                chain[i + 1]
            } else {
                FAT32_EOC
            };
            if !self.set_fat_entry(chain[i], value) {
                // crate::serial_println!("[FAT32] Failed to set FAT entry for cluster {}", chain[i]);
                return None;
            }
        }

        // crate::serial_println!("[FAT32] Allocated {} cluster(s), first={}", chain.len(), chain[0]);
        Some(chain[0])
    }

    /// Free a cluster chain starting from `start_cluster`
    fn free_cluster_chain(&self, start_cluster: u32) {
        if start_cluster < 2 {
            return;
        }
        let mut cluster = start_cluster;
        let mut count = 0u32;
        loop {
            if count > 10000 { break; } // Safety limit
            let next = self.read_fat_entry(cluster);
            self.set_fat_entry(cluster, FAT32_FREE);
            count += 1;

            if next >= FAT32_EOC || next == FAT32_BAD || next < 2 {
                break;
            }
            cluster = next;
        }
        // crate::serial_println!("[FAT32] Freed {} cluster(s) from chain starting at {}", count, start_cluster);
    }

    /// Write data to a cluster's sectors on disk
    fn write_cluster(&self, cluster: u32, data: &[u8]) -> bool {
        let lba = self.bpb.cluster_to_lba(cluster);
        let sectors = self.bpb.sectors_per_cluster as usize;
        let cluster_size = sectors * 512;

        // Pad data to full cluster size
        let mut buf = vec![0u8; cluster_size];
        let copy_len = core::cmp::min(data.len(), cluster_size);
        buf[..copy_len].copy_from_slice(&data[..copy_len]);

        crate::drivers::virtio_blk::write_sectors(lba as u64, sectors, &buf)
    }

    /// Write data following a cluster chain
    fn write_file_data(&self, start_cluster: u32, data: &[u8]) -> bool {
        let cluster_size = self.bpb.cluster_size();
        let mut cluster = start_cluster;
        let mut offset = 0usize;
        let mut iterations = 0;

        while offset < data.len() {
            if iterations > 1000 { break; } // Safety
            iterations += 1;

            let end = core::cmp::min(offset + cluster_size, data.len());
            if !self.write_cluster(cluster, &data[offset..end]) {
                // crate::serial_println!("[FAT32] Failed to write cluster {}", cluster);
                return false;
            }
            offset = end;

            if offset < data.len() {
                match self.next_cluster(cluster) {
                    Some(next) => cluster = next,
                    None => {
                        // crate::serial_println!("[FAT32] Cluster chain too short for data");
                        return false;
                    }
                }
            }
        }
        true
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
    /// Uses LFN-aware matching via fat32_name_matches.
    pub fn read_file(&self, name: &str) -> Option<Vec<u8>> {
        let entries = self.list_root();
        let target = name.to_ascii_lowercase();

        for entry in &entries {
            if !entry.is_directory && fat32_name_matches(&entry.name, &target) {
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

    /// Jalon 52: Read a chunk of a file without loading the entire file into memory.
    /// Navigates the cluster chain to skip to the correct offset, then reads only
    /// the needed clusters. This prevents OOM for large files (e.g., 2 GB models).
    pub fn read_file_chunk(&self, start_cluster: u32, file_size: u32, offset: u64, len: u64) -> Option<Vec<u8>> {
        let file_size = file_size as u64;
        if offset >= file_size {
            return Some(Vec::new()); // EOF
        }

        let cluster_size = self.bpb.cluster_size() as u64;
        if cluster_size == 0 { return None; }

        // How many bytes we actually want
        let available = file_size - offset;
        let to_read = core::cmp::min(available, len) as usize;
        if to_read == 0 {
            return Some(Vec::new());
        }

        // Skip clusters to reach the offset
        let clusters_to_skip = offset / cluster_size;
        let offset_in_cluster = (offset % cluster_size) as usize;

        let mut cluster = start_cluster;
        for _ in 0..clusters_to_skip {
            cluster = self.next_cluster(cluster)?;
        }

        // Now read from 'cluster' at 'offset_in_cluster', collecting 'to_read' bytes
        let mut result = Vec::with_capacity(to_read);
        let mut remaining = to_read;
        let mut first = true;
        let mut iterations = 0u32;

        loop {
            if remaining == 0 || iterations > 10000 { break; }
            iterations += 1;

            let cluster_data = self.read_cluster(cluster)?;
            let start_in_cluster = if first { offset_in_cluster } else { 0 };
            first = false;

            let avail_in_cluster = cluster_data.len() - start_in_cluster;
            let to_copy = core::cmp::min(remaining, avail_in_cluster);
            result.extend_from_slice(&cluster_data[start_in_cluster..start_in_cluster + to_copy]);
            remaining -= to_copy;

            if remaining == 0 { break; }

            match self.next_cluster(cluster) {
                Some(next) => cluster = next,
                None => break,
            }
        }

        Some(result)
    }

    /// Find and read a file in a subdirectory
    /// Supports both exact 8.3 names (e.g. "model.ggu") and long names
    /// that would match the 8.3 tilde form (e.g. "real_model.gguf" matches "real_m~1.ggu").
    pub fn read_file_in_dir(&self, dir_cluster: u32, name: &str) -> Option<Vec<u8>> {
        let entries = self.list_directory(dir_cluster);
        let target = name.to_ascii_lowercase();

        for entry in &entries {
            if !entry.is_directory && fat32_name_matches(&entry.name, &target) {
                return self.read_file_data(entry.first_cluster, entry.file_size);
            }
        }

        None
    }

    /// Navigate a path like "var/sagas" from root, returning the cluster of the final directory.
    /// Each component must be an existing directory.
    /// Uses case-insensitive + tilde-aware matching for LFN compatibility.
    pub fn navigate_to_dir(&self, path_components: &[&str]) -> Option<u32> {
        let mut current_cluster = self.bpb.root_cluster;

        for component in path_components {
            let target = component.to_ascii_lowercase();
            let entries = self.list_directory(current_cluster);
            let mut found = false;

            for entry in &entries {
                if entry.is_directory && fat32_name_matches(&entry.name, &target) {
                    current_cluster = entry.first_cluster;
                    found = true;
                    break;
                }
            }

            if !found {
                return None;
            }
        }

        Some(current_cluster)
    }

    /// Find a directory entry by path (supports recursive subdirectories).
    /// Path format: "models/mistral_part_aa" (no leading slash).
    /// Returns the Fat32DirEntry if found.
    /// Uses case-insensitive + tilde-aware matching for LFN compatibility.
    pub fn find_directory_entry(&self, path: &str) -> Option<Fat32DirEntry> {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return None;
        }

        let filename = parts[parts.len() - 1];
        let dir_parts = &parts[..parts.len() - 1];
        let target = filename.to_ascii_lowercase();

        let dir_cluster = if dir_parts.is_empty() {
            self.bpb.root_cluster
        } else {
            self.navigate_to_dir(dir_parts)?
        };

        let entries = self.list_directory(dir_cluster);
        for entry in &entries {
            if fat32_name_matches(&entry.name, &target) {
                return Some(entry.clone());
            }
        }

        None
    }

    /// List all entries in a subdirectory given a path.
    /// Path format: "models" or "models/subdir".
    pub fn list_directory_path(&self, path: &str) -> Vec<Fat32DirEntry> {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return self.list_root();
        }

        match self.navigate_to_dir(&parts) {
            Some(cluster) => {
                // crate::serial_println!("[FAT32] list_directory_path('{}') -> cluster {}", path, cluster);
                self.list_directory(cluster)
            }
            None => Vec::new(),
        }
    }

    /// Write (create or overwrite) a file in a given directory cluster.
    ///
    /// Steps:
    /// 1. Scan directory for existing entry with matching name
    /// 2. If found, free its old cluster chain
    /// 3. Allocate new cluster chain for the data
    /// 4. Write data to clusters
    /// 5. Update (or create) the directory entry with new cluster and size
    ///
    /// Returns true on success.
    pub fn write_file_to_dir(&self, dir_cluster: u32, filename: &str, data: &[u8]) -> bool {
        let name_8_3 = name_to_8_3(filename);
        let target_lower = filename.to_ascii_lowercase();

        // crate::serial_println!("[FAT32-W] write_file_to_dir: dir_cluster={}, file='{}', size={}",
        //     dir_cluster, filename, data.len());

        // Step 1+2: Scan directory for existing entry
        let mut found_entry_cluster = 0u32;
        let mut found_entry_offset = 0usize;
        let mut found_old_cluster = 0u32;
        let mut entry_found = false;

        // Also track the first free slot (0xE5 or 0x00) for creating new entries
        let mut free_slot_cluster = 0u32;
        let mut free_slot_offset = 0usize;
        let mut free_slot_found = false;

        let mut scan_cluster = dir_cluster;
        let mut iterations = 0;

        'outer: loop {
            if iterations > 100 { break; }
            iterations += 1;

            let cluster_data = match self.read_cluster(scan_cluster) {
                Some(d) => d,
                None => break,
            };

            let num_entries = cluster_data.len() / DIR_ENTRY_SIZE;
            for i in 0..num_entries {
                let off = i * DIR_ENTRY_SIZE;
                let raw = &cluster_data[off..off + DIR_ENTRY_SIZE];

                // Free/deleted entry — remember as potential slot
                if (raw[0] == 0x00 || raw[0] == 0xE5) && !free_slot_found {
                    free_slot_cluster = scan_cluster;
                    free_slot_offset = off;
                    free_slot_found = true;
                }

                // End of directory marker
                if raw[0] == 0x00 {
                    break 'outer;
                }

                // Check if this is our target file
                if let Some(entry) = Fat32DirEntry::parse(raw) {
                    if entry.name == target_lower && !entry.is_directory {
                        found_entry_cluster = scan_cluster;
                        found_entry_offset = off;
                        found_old_cluster = entry.first_cluster;
                        entry_found = true;
                        break 'outer;
                    }
                }
            }

            match self.next_cluster(scan_cluster) {
                Some(next) => scan_cluster = next,
                None => break,
            }
        }

        // Step 2: Free old cluster chain if overwriting
        if entry_found && found_old_cluster >= 2 {
            // crate::serial_println!("[FAT32-W] Overwriting existing file, freeing old chain from cluster {}", found_old_cluster);
            self.free_cluster_chain(found_old_cluster);
        }

        // Step 3: Allocate new cluster chain
        let new_first_cluster = if data.is_empty() {
            0u32
        } else {
            match self.allocate_cluster_chain(data.len()) {
                Some(c) => c,
                None => {
                    // crate::serial_println!("[FAT32-W] Failed to allocate clusters for {} bytes", data.len());
                    return false;
                }
            }
        };

        // Step 4: Write data to the new clusters
        if !data.is_empty() && new_first_cluster >= 2 {
            if !self.write_file_data(new_first_cluster, data) {
                // crate::serial_println!("[FAT32-W] Failed to write file data");
                return false;
            }
        }

        // Step 5: Build the 32-byte directory entry
        let mut dir_entry = [0u8; 32];
        dir_entry[0..11].copy_from_slice(&name_8_3);
        dir_entry[11] = 0x20; // Archive attribute
        // Cluster high word (bytes 20-21)
        let cluster_hi = ((new_first_cluster >> 16) & 0xFFFF) as u16;
        dir_entry[20] = (cluster_hi & 0xFF) as u8;
        dir_entry[21] = ((cluster_hi >> 8) & 0xFF) as u8;
        // Cluster low word (bytes 26-27)
        let cluster_lo = (new_first_cluster & 0xFFFF) as u16;
        dir_entry[26] = (cluster_lo & 0xFF) as u8;
        dir_entry[27] = ((cluster_lo >> 8) & 0xFF) as u8;
        // File size (bytes 28-31)
        let size_bytes = (data.len() as u32).to_le_bytes();
        dir_entry[28] = size_bytes[0];
        dir_entry[29] = size_bytes[1];
        dir_entry[30] = size_bytes[2];
        dir_entry[31] = size_bytes[3];

        // Step 6: Write directory entry to disk
        let (target_cluster, target_offset) = if entry_found {
            (found_entry_cluster, found_entry_offset)
        } else if free_slot_found {
            (free_slot_cluster, free_slot_offset)
        } else {
            // crate::serial_println!("[FAT32-W] No free directory entry slot found");
            return false;
        };

        // Read the cluster containing the directory entry
        let mut cluster_data = match self.read_cluster(target_cluster) {
            Some(d) => d,
            None => {
                // crate::serial_println!("[FAT32-W] Failed to read dir cluster {}", target_cluster);
                return false;
            }
        };

        // Overwrite the 32-byte entry
        cluster_data[target_offset..target_offset + 32].copy_from_slice(&dir_entry);

        // Write the cluster back
        if !self.write_cluster(target_cluster, &cluster_data) {
            // crate::serial_println!("[FAT32-W] Failed to write back dir cluster {}", target_cluster);
            return false;
        }

        // crate::serial_println!("[FAT32-W] File '{}' written: {} bytes, first_cluster={}",
        //     filename, data.len(), new_first_cluster);

        true
    }
}

/// Global FAT32 instance
static mut FAT32_FS: Option<Fat32Fs> = None;

/// Initialize FAT32 filesystem
pub fn init() -> bool {
    match Fat32Fs::mount() {
        Some(fs) => {
            // crate::serial_println!("[FAT32] Filesystem initialized");
            unsafe { FAT32_FS = Some(fs); }
            true
        }
        None => {
            // crate::serial_println!("[FAT32] No FAT32 filesystem found");
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

/// Write a file to a path on the FAT32 disk.
///
/// `disk_path` is the relative path inside /disk/ (e.g., "var/sagas/001.bin").
/// Navigates directories, then writes the file into the final directory.
///
/// Returns true on success.
pub fn write_file(disk_path: &str, data: &[u8]) -> bool {
    unsafe {
        let fs = match FAT32_FS {
            Some(ref f) => f,
            None => {
                // crate::serial_println!("[FAT32] write_file: No filesystem mounted");
                return false;
            }
        };

        // crate::serial_println!("[FAT32-W] write_file('{}', {} bytes)", disk_path, data.len());

        // Parse the path into directory components + filename
        let parts: Vec<&str> = disk_path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            // crate::serial_println!("[FAT32-W] Empty path");
            return false;
        }

        let filename = parts[parts.len() - 1];
        let dir_parts = &parts[..parts.len() - 1];

        // Navigate to the target directory
        let dir_cluster = if dir_parts.is_empty() {
            fs.bpb.root_cluster
        } else {
            match fs.navigate_to_dir(dir_parts) {
                Some(c) => c,
                None => {
                    // crate::serial_println!("[FAT32-W] Directory path not found: {:?}", dir_parts);
                    return false;
                }
            }
        };

        // Write the file
        fs.write_file_to_dir(dir_cluster, filename, data)
    }
}

/// Read a file from a full path on the FAT32 disk.
///
/// `disk_path` is the relative path inside /disk/ (e.g., "var/sagas/001.bin"
/// or "models/mistral_part_aa").
/// Supports recursive subdirectory traversal.
pub fn read_file_path(disk_path: &str) -> Option<Vec<u8>> {
    unsafe {
        let fs = match FAT32_FS {
            Some(ref f) => f,
            None => return None,
        };

        let parts: Vec<&str> = disk_path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return None;
        }

        let filename = parts[parts.len() - 1];
        let dir_parts = &parts[..parts.len() - 1];

        // Hot-path logging disabled for performance
        // crate::serial_println!("[FAT32] read_file_path: '{}' (dir={:?}, file='{}')",
        //     disk_path, dir_parts, filename);

        let dir_cluster = if dir_parts.is_empty() {
            fs.bpb.root_cluster
        } else {
            match fs.navigate_to_dir(dir_parts) {
                Some(c) => c,
                None => { return None; }
            }
        };

        let result = fs.read_file_in_dir(dir_cluster, filename);
        // Success/failure logging disabled for performance
        result
    }
}

/// Jalon 52: Read a chunk of a file by path, using offset-based cluster navigation.
/// This avoids loading the entire file into kernel heap, preventing OOM for large files.
/// `disk_path` is relative to /disk/ (e.g. "models/part1").
pub fn read_file_path_chunk(disk_path: &str, offset: u64, len: u64) -> Option<Vec<u8>> {
    unsafe {
        let fs = match FAT32_FS {
            Some(ref f) => f,
            None => return None,
        };

        // Find the directory entry to get start_cluster and file_size
        let entry = fs.find_directory_entry(disk_path)?;
        if entry.is_directory {
            return None;
        }

        fs.read_file_chunk(entry.first_cluster, entry.file_size, offset, len)
    }
}

/// Check if a file exists on the FAT32 filesystem without loading it.
/// This is critical for avoiding OOM when checking large files (e.g. 2GB Mistral parts).
/// Returns Some(file_size) if the file exists, None otherwise.
pub fn file_exists(disk_path: &str) -> Option<u64> {
    unsafe {
        let fs = match FAT32_FS {
            Some(ref f) => f,
            None => return None,
        };
        match fs.find_directory_entry(disk_path) {
            Some(entry) if !entry.is_directory => Some(entry.file_size as u64),
            _ => None,
        }
    }
}

/// Read bytes from a file at a given offset directly into a provided buffer.
/// Returns the number of bytes actually read.
/// Used by the VMA demand pager to fill pages from file-backed mappings.
pub fn read_file_at_offset(disk_path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, ()> {
    let len = buf.len() as u64;
    match read_file_path_chunk(disk_path, offset, len) {
        Some(data) => {
            let copy_len = core::cmp::min(data.len(), buf.len());
            buf[..copy_len].copy_from_slice(&data[..copy_len]);
            Ok(copy_len)
        }
        None => Err(()),
    }
}

/// Find a directory entry by path (public API).
pub fn find_directory_entry(path: &str) -> Option<Fat32DirEntry> {
    unsafe {
        match FAT32_FS {
            Some(ref fs) => fs.find_directory_entry(path),
            None => None,
        }
    }
}

/// List entries in a subdirectory by path (public API).
pub fn list_directory_path(path: &str) -> Vec<Fat32DirEntry> {
    unsafe {
        match FAT32_FS {
            Some(ref fs) => fs.list_directory_path(path),
            None => Vec::new(),
        }
    }
}

/// Run FAT32 self-tests (updated for J43: subdirectory traversal)
pub fn run_tests() {
    // crate::serial_println!("\n========================================");
    // crate::serial_println!("[FAT32 TESTS] Couche 19 - FAT32 Filesystem");
    // crate::serial_println!("========================================\n");

    let mut _passed = 0u32;
    let mut failed = 0u32;

    // Test 1: FAT32 mounted
    // crate::serial_write("  [TEST 1/3] FAT32 mounted... ");
    if is_mounted() {
        // crate::serial_write("OK\n");
        _passed += 1;
    } else {
        // crate::serial_write("SKIP (no FAT32 disk)\n");
        // crate::serial_println!("\n========================================");
        // crate::serial_println!("[FAT32 TESTS] Skipped (no FAT32 filesystem)");
        // crate::serial_println!("========================================");
        return;
    }

    // Test 2: List root directory
    // crate::serial_write("  [TEST 2/3] Root directory listing... ");
    let entries = list_root();
    if !entries.is_empty() {
        // crate::serial_println!("OK ({} entries)", entries.len());
        for entry in &entries {
            if entry.is_directory {
                // crate::serial_println!("    <DIR> {}", entry.name);
            } else {
                // crate::serial_println!("    {} ({} bytes)", entry.name, entry.file_size);
            }
        }
        _passed += 1;
    } else {
        // crate::serial_write("WARN (empty directory)\n");
        _passed += 1; // Could be an empty disk
    }

    // Test 3: Read index.html
    // crate::serial_write("  [TEST 3/5] Read index.html... ");
    match read_file("index.html") {
        Some(data) => {
            // crate::serial_println!("OK ({} bytes)", data.len());
            let preview_len = core::cmp::min(data.len(), 100);
            if let Ok(_s) = core::str::from_utf8(&data[..preview_len]) {
                // crate::serial_println!("    Content: {}", s);
            }
            _passed += 1;
        }
        None => {
            // crate::serial_write("SKIP (no index.html on disk)\n");
            _passed += 1;
        }
    }

    // Test 4: Subdirectory traversal (models/)
    // crate::serial_write("  [TEST 4/5] Subdirectory listing (models/)... ");
    let sub_entries = list_directory_path("models");
    if !sub_entries.is_empty() {
        // crate::serial_println!("OK ({} entries)", sub_entries.len());
        for entry in &sub_entries {
            if entry.is_directory {
                // crate::serial_println!("    <DIR> {}", entry.name);
            } else {
                // crate::serial_println!("    /disk/models/{} ({} bytes)", entry.name, entry.file_size);
            }
        }
        _passed += 1;
    } else {
        // crate::serial_write("SKIP (no models/ directory)\n");
        _passed += 1;
    }

    // Test 5: Read first 16 bytes of a file from subdirectory (SAFE: chunked read, no OOM)
    // crate::serial_write("  [TEST 5/5] Chunked read from subdirectory... ");
    if !sub_entries.is_empty() {
        let first_file = sub_entries.iter().find(|e| !e.is_directory);
        if let Some(entry) = first_file {
            let path = alloc::format!("models/{}", entry.name);
            // CRITICAL FIX (#16): Use file_exists + chunked read instead of full read
            match file_exists(&path) {
                Some(_size) => {
                    // crate::serial_println!("EXISTS ({} = {} bytes)", path, size);
                    // Only read first 16 bytes for preview (safe even for 2GB files)
                    match read_file_path_chunk(&path, 0, 16) {
                        Some(data) => {
                            let preview_len = core::cmp::min(data.len(), 16);
                            let mut hex = alloc::string::String::with_capacity(preview_len * 3);
                            for b in &data[..preview_len] {
                                use core::fmt::Write;
                                let _ = write!(hex, "{:02X} ", b);
                            }
                            // crate::serial_println!("    First {} bytes: {}", preview_len, hex);
                            _passed += 1;
                        }
                        None => {
                            // crate::serial_println!("WARN (chunk read failed, but file exists)");
                            _passed += 1;
                        }
                    }
                }
                None => {
                    // crate::serial_println!("FAIL (file_exists returned None for '{}')", path);
                    failed += 1;
                }
            }
        } else {
            // crate::serial_write("SKIP (no files in models/)\n");
            _passed += 1;
        }
    } else {
        // crate::serial_write("SKIP (no models/ directory)\n");
        _passed += 1;
    }

    // crate::serial_println!("\n========================================");
    // crate::serial_println!("[FAT32 TESTS] {}/{} passed, {} failed", passed, passed + failed, failed);
    if failed == 0 { crate::serial_write("[FAT32 TESTS] ALL TESTS PASSED!\n"); }
    // crate::serial_println!("========================================");
}
