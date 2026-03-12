// fs/exfat.rs - Couche 18: Read-Only exFAT Driver for Zero-Copy Model Streaming
//
// Jalon 68b: Full read-only exFAT filesystem driver architecture
//
// Implements a complete read-only exFAT filesystem driver to support
// memory-mapped access to large model files (4+ GiB) on exFAT-formatted disks.
//
// exFAT is required because FAT32 has a 4 GiB file size limit.
// This driver supports:
//   - Boot sector parsing (VBR) with full Main Boot Sector structure
//   - FAT allocation table reading and cluster chain traversal
//   - Directory entry parsing (File, Stream Extension, File Name, Allocation Bitmap,
//     Upcase Table, Volume Label)
//   - Path-based file lookup with subdirectory traversal
//   - read_file_at_offset(path, offset, buf) for streaming large files
//   - Contiguous file optimization (skip FAT chain traversal)
//   - Integration with VFS for sys_open/sys_read/sys_mmap_file
//
// Limitations (read-only):
//   - No write support
//   - No file creation/deletion
//   - No directory creation
//   - Single partition only
//   - Max 255 UTF-16 chars per filename (17 File Name entries)
//   - No extended timestamps beyond 10ms resolution
//
// Reference: Microsoft exFAT specification (Rev 1.0, 2008)

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use lazy_static::lazy_static;

// ============================================================
// exFAT Constants
// ============================================================

/// FAT entry indicating end of chain
const FAT_END_OF_CHAIN: u32 = 0xFFFFFFFF;
/// FAT entry threshold for end of chain range
const FAT_END_THRESHOLD: u32 = 0xFFFFFFF8;
/// FAT entry for bad cluster
const FAT_BAD_CLUSTER: u32 = 0xFFFFFFF7;
/// First valid data cluster number
const FIRST_DATA_CLUSTER: u32 = 2;
/// Maximum cluster chain length to prevent infinite loops
const MAX_CHAIN_LENGTH: u32 = 0x0FFF_FFFF;
/// Directory entry size in bytes
const DIR_ENTRY_SIZE: u32 = 32;
/// Maximum path depth to prevent infinite recursion
const MAX_PATH_DEPTH: usize = 32;
/// Maximum directory clusters to scan
const MAX_DIR_CLUSTERS: u32 = 256;
/// exFAT file attribute: Directory
const ATTR_DIRECTORY: u16 = 0x10;
/// exFAT file attribute: Read-only
#[allow(dead_code)]
const ATTR_READ_ONLY: u16 = 0x01;
/// exFAT file attribute: Hidden
#[allow(dead_code)]
const ATTR_HIDDEN: u16 = 0x02;
/// exFAT file attribute: System
#[allow(dead_code)]
const ATTR_SYSTEM: u16 = 0x04;

// ============================================================
// exFAT On-Disk Structures (packed, read via read_unaligned)
// ============================================================

/// exFAT Main Boot Sector (Volume Boot Record) - 512 bytes
/// Reference: exFAT specification section 3.1
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct ExfatBootSector {
    /// Jump instruction to boot code (3 bytes: 0xEB 0x76 0x90)
    pub jump_boot: [u8; 3],
    /// Must be "EXFAT   " (8 bytes, space-padded)
    pub fs_name: [u8; 8],
    /// Must be zero for exFAT (53 bytes)
    pub must_be_zero: [u8; 53],
    /// Sector offset of the partition that contains this exFAT volume
    pub partition_offset: u64,
    /// Size of the volume in sectors
    pub volume_length: u64,
    /// Sector offset of the first FAT
    pub fat_offset: u32,
    /// Size of each FAT in sectors
    pub fat_length: u32,
    /// Sector offset of the cluster heap
    pub cluster_heap_offset: u32,
    /// Number of clusters in the cluster heap
    pub cluster_count: u32,
    /// Cluster index of the first cluster of the root directory
    pub first_cluster_of_root_dir: u32,
    /// Volume serial number
    pub volume_serial_number: u32,
    /// File system revision (high byte = major, low byte = minor)
    pub fs_revision: u16,
    /// Volume flags (ActiveFat, VolumeDirty, MediaFailure, ClearToZero)
    pub volume_flags: u16,
    /// Log2 of bytes per sector (valid: 9..12 => 512..4096)
    pub bytes_per_sector_shift: u8,
    /// Log2 of sectors per cluster (valid: 0..25-BytesPerSectorShift)
    pub sectors_per_cluster_shift: u8,
    /// Number of FATs (1 or 2)
    pub number_of_fats: u8,
    /// INT 13h drive number
    pub drive_select: u8,
    /// Percentage of clusters in use (0..100, or 0xFF if unknown)
    pub percent_in_use: u8,
    /// Reserved (7 bytes)
    pub reserved: [u8; 7],
    /// Boot code (390 bytes)
    pub boot_code: [u8; 390],
    /// Boot signature: 0xAA55
    pub boot_signature: u16,
}

// ============================================================
// Directory Entry Structures (all 32 bytes, packed)
// ============================================================

/// exFAT Directory Entry type byte values
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EntryType {
    /// 0x00 - Marks end of directory
    EndOfDirectory,
    /// 0x81 - Allocation Bitmap entry (critical primary)
    AllocationBitmap,
    /// 0x82 - Upcase Table entry (critical primary)
    UpcaseTable,
    /// 0x83 - Volume Label entry (critical primary)
    VolumeLabel,
    /// 0x85 - File Directory Entry (critical primary)
    FileEntry,
    /// 0xC0 - Stream Extension Entry (critical secondary)
    StreamExtension,
    /// 0xC1 - File Name Entry (critical secondary)
    FileName,
    /// 0x01-0x7F - Unused/deleted entries
    /// 0xA0-0xBF - Benign primary (vendor extensions)
    /// Others - Unknown
    Unknown(u8),
}

impl From<u8> for EntryType {
    fn from(val: u8) -> Self {
        match val {
            0x00 => EntryType::EndOfDirectory,
            0x81 => EntryType::AllocationBitmap,
            0x82 => EntryType::UpcaseTable,
            0x83 => EntryType::VolumeLabel,
            0x85 => EntryType::FileEntry,
            0xC0 => EntryType::StreamExtension,
            0xC1 => EntryType::FileName,
            other => EntryType::Unknown(other),
        }
    }
}

/// exFAT File Directory Entry (32 bytes) - Critical Primary
/// The first entry in a directory entry set for a file or directory
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct FileDirectoryEntry {
    /// Entry type: 0x85
    pub entry_type: u8,
    /// Count of secondary entries (Stream Extension + File Name entries)
    pub secondary_count: u8,
    /// Checksum of the entire directory entry set
    pub set_checksum: u16,
    /// File attributes (READ_ONLY, HIDDEN, SYSTEM, DIRECTORY, ARCHIVE)
    pub file_attributes: u16,
    /// Reserved
    pub reserved1: u16,
    /// Create timestamp (DOS format)
    pub create_timestamp: u32,
    /// Last modified timestamp (DOS format)
    pub last_modified: u32,
    /// Last accessed timestamp (DOS format)
    pub last_accessed: u32,
    /// Create time 10ms increment (0-199)
    pub create_10ms: u8,
    /// Last modified 10ms increment (0-199)
    pub last_modified_10ms: u8,
    /// Create UTC offset
    pub create_utc_offset: u8,
    /// Last modified UTC offset
    pub last_modified_utc_offset: u8,
    /// Last accessed UTC offset
    pub last_accessed_utc_offset: u8,
    /// Reserved (7 bytes)
    pub reserved2: [u8; 7],
}

/// exFAT Stream Extension Entry (32 bytes) - Critical Secondary
/// Contains the file size and first cluster information
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct StreamExtensionEntry {
    /// Entry type: 0xC0
    pub entry_type: u8,
    /// Bit 0: AllocationPossible, Bit 1: NoFatChain (contiguous)
    pub general_secondary_flags: u8,
    /// Reserved
    pub reserved1: u8,
    /// Length of the filename in Unicode characters
    pub name_length: u8,
    /// Hash of the up-cased filename
    pub name_hash: u16,
    /// Reserved
    pub reserved2: u16,
    /// Valid data length (bytes actually written)
    pub valid_data_length: u64,
    /// Reserved
    pub reserved3: u32,
    /// First cluster of the data
    pub first_cluster: u32,
    /// Data length (allocated size in bytes)
    pub data_length: u64,
}

/// exFAT File Name Entry (32 bytes) - Critical Secondary
/// Contains up to 15 UTF-16LE characters of the filename
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct FileNameEntry {
    /// Entry type: 0xC1
    pub entry_type: u8,
    /// General secondary flags
    pub general_secondary_flags: u8,
    /// File name characters (UTF-16LE, up to 15 per entry)
    pub file_name: [u16; 15],
}

/// exFAT Allocation Bitmap Entry (32 bytes) - Critical Primary
/// Describes the location of the allocation bitmap
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct AllocationBitmapEntry {
    /// Entry type: 0x81
    pub entry_type: u8,
    /// Bit 0: BitmapIdentifier (0=first, 1=second bitmap)
    pub bitmap_flags: u8,
    /// Reserved (18 bytes)
    pub reserved: [u8; 18],
    /// First cluster of the allocation bitmap
    pub first_cluster: u32,
    /// Size of the allocation bitmap in bytes
    pub data_length: u64,
}

/// exFAT Upcase Table Entry (32 bytes) - Critical Primary
/// Describes the location of the upcase table used for filename hashing
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct UpcaseTableEntry {
    /// Entry type: 0x82
    pub entry_type: u8,
    /// Reserved (3 bytes)
    pub reserved1: [u8; 3],
    /// Checksum of the upcase table
    pub table_checksum: u32,
    /// Reserved (12 bytes)
    pub reserved2: [u8; 12],
    /// First cluster of the upcase table
    pub first_cluster: u32,
    /// Size of the upcase table in bytes
    pub data_length: u64,
}

/// exFAT Volume Label Entry (32 bytes) - Critical Primary
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct VolumeLabelEntry {
    /// Entry type: 0x83
    pub entry_type: u8,
    /// Character count (0..11)
    pub character_count: u8,
    /// Volume label in UTF-16LE (up to 11 characters)
    pub volume_label: [u16; 11],
    /// Reserved (8 bytes)
    pub reserved: [u8; 8],
}

// ============================================================
// Parsed Directory Entry (high-level representation)
// ============================================================

/// A resolved exFAT directory entry (file or subdirectory)
#[derive(Debug, Clone)]
pub struct ExfatFile {
    /// Filename (UTF-8, decoded from UTF-16LE)
    pub name: String,
    /// File size in bytes
    pub size: u64,
    /// First cluster of the file data
    pub first_cluster: u32,
    /// Whether the file is stored contiguously (NoFatChain flag)
    pub is_contiguous: bool,
    /// Whether this entry is a directory
    pub is_directory: bool,
    /// File attributes from the File Directory Entry
    pub attributes: u16,
}

// ============================================================
// exFAT Filesystem State
// ============================================================

/// exFAT filesystem state
pub struct ExfatFs {
    /// Whether the filesystem is mounted
    pub mounted: bool,
    /// Bytes per sector (from boot sector, power of 2)
    pub bytes_per_sector: u32,
    /// Sectors per cluster (from boot sector, power of 2)
    pub sectors_per_cluster: u32,
    /// Sector offset of the cluster heap
    pub cluster_heap_offset: u32,
    /// Sector offset of the first FAT
    pub fat_offset: u32,
    /// Size of FAT in sectors
    pub fat_length: u32,
    /// First cluster of the root directory
    pub root_cluster: u32,
    /// Total number of clusters in the heap
    pub cluster_count: u32,
    /// Bytes per cluster (computed: bytes_per_sector * sectors_per_cluster)
    pub bytes_per_cluster: u32,
    /// Volume serial number
    pub volume_serial: u32,
    /// Volume label (UTF-8)
    pub volume_label: String,
    /// Cached root directory entries (files and subdirectories)
    pub files: Vec<ExfatFile>,
}

impl ExfatFs {
    pub const fn new() -> Self {
        ExfatFs {
            mounted: false,
            bytes_per_sector: 512,
            sectors_per_cluster: 1,
            cluster_heap_offset: 0,
            fat_offset: 0,
            fat_length: 0,
            root_cluster: 0,
            cluster_count: 0,
            bytes_per_cluster: 512,
            volume_serial: 0,
            volume_label: String::new(),
            files: Vec::new(),
        }
    }

    // ────────────────────────────────────────────────────
    // Cluster Addressing
    // ────────────────────────────────────────────────────

    /// Calculate the absolute byte offset on disk for a given cluster number.
    /// Cluster numbering starts at 2 (FIRST_DATA_CLUSTER).
    pub fn cluster_to_offset(&self, cluster: u32) -> u64 {
        let cluster_offset = (cluster - FIRST_DATA_CLUSTER) as u64;
        let heap_start = self.cluster_heap_offset as u64 * self.bytes_per_sector as u64;
        heap_start + cluster_offset * self.bytes_per_cluster as u64
    }

    /// Validate a cluster number
    fn is_valid_cluster(&self, cluster: u32) -> bool {
        cluster >= FIRST_DATA_CLUSTER && cluster < (FIRST_DATA_CLUSTER + self.cluster_count)
    }

    // ────────────────────────────────────────────────────
    // Sector I/O
    // ────────────────────────────────────────────────────

    /// Read raw bytes from disk at a given byte offset.
    /// Works for any byte-aligned offset and length.
    fn read_bytes_at(&self, disk_offset: u64, buf: &mut [u8]) -> bool {
        let sector_size = self.bytes_per_sector as u64;
        let start_sector = disk_offset / sector_size;
        let start_offset = (disk_offset % sector_size) as usize;
        let mut remaining = buf.len();
        let mut buf_pos = 0;
        let mut sector = start_sector;
        let mut sec_off = start_offset;

        while remaining > 0 {
            let mut sector_buf = [0u8; 512];
            // For >512 byte sectors, we read multiple 512-byte blocks
            let physical_sector = if self.bytes_per_sector > 512 {
                sector * (sector_size / 512) + (sec_off as u64 / 512)
            } else {
                sector
            };

            if !crate::drivers::virtio_blk::read_sector(physical_sector, &mut sector_buf) {
                return false;
            }

            let effective_offset = if self.bytes_per_sector > 512 {
                sec_off % 512
            } else {
                sec_off
            };
            let available = 512 - effective_offset;
            let copy_len = core::cmp::min(available, remaining);
            buf[buf_pos..buf_pos + copy_len]
                .copy_from_slice(&sector_buf[effective_offset..effective_offset + copy_len]);

            buf_pos += copy_len;
            remaining -= copy_len;
            sec_off += copy_len;
            if sec_off >= self.bytes_per_sector as usize {
                sector += 1;
                sec_off = 0;
            }
        }
        true
    }

    // ────────────────────────────────────────────────────
    // Cluster I/O
    // ────────────────────────────────────────────────────

    /// Read a full cluster's data into a Vec<u8>.
    pub fn read_cluster(&self, cluster: u32) -> Option<Vec<u8>> {
        if !self.is_valid_cluster(cluster) {
            crate::serial_println!("[EXFAT] Invalid cluster {}", cluster);
            return None;
        }
        let offset = self.cluster_to_offset(cluster);
        let size = self.bytes_per_cluster as usize;
        let mut data = Vec::with_capacity(size);
        data.resize(size, 0u8);

        if self.read_bytes_at(offset, &mut data) {
            Some(data)
        } else {
            crate::serial_println!("[EXFAT] Failed to read cluster {}", cluster);
            None
        }
    }

    /// Read partial cluster data into a caller-provided buffer.
    /// Returns the number of bytes actually read.
    pub fn read_cluster_partial(
        &self,
        cluster: u32,
        cluster_offset: usize,
        buf: &mut [u8],
    ) -> usize {
        if !self.is_valid_cluster(cluster) {
            return 0;
        }
        let disk_offset = self.cluster_to_offset(cluster) + cluster_offset as u64;
        let max_in_cluster = self.bytes_per_cluster as usize - cluster_offset;
        let to_read = core::cmp::min(buf.len(), max_in_cluster);

        if self.read_bytes_at(disk_offset, &mut buf[..to_read]) {
            to_read
        } else {
            0
        }
    }

    // ────────────────────────────────────────────────────
    // FAT (File Allocation Table)
    // ────────────────────────────────────────────────────

    /// Read a FAT entry for a given cluster.
    /// Returns the next cluster in the chain, or None if end-of-chain / error.
    pub fn fat_read(&self, cluster: u32) -> Option<u32> {
        if !self.is_valid_cluster(cluster) {
            return None;
        }
        let fat_byte_offset =
            self.fat_offset as u64 * self.bytes_per_sector as u64 + (cluster as u64) * 4;

        let mut entry_buf = [0u8; 4];
        if !self.read_bytes_at(fat_byte_offset, &mut entry_buf) {
            return None;
        }

        let next = u32::from_le_bytes(entry_buf);

        if next >= FAT_END_THRESHOLD {
            None // End of chain
        } else if next == FAT_BAD_CLUSTER {
            crate::serial_println!("[EXFAT] Bad cluster marker at FAT[{}]", cluster);
            None
        } else if next < FIRST_DATA_CLUSTER || next >= (FIRST_DATA_CLUSTER + self.cluster_count) {
            crate::serial_println!("[EXFAT] Invalid FAT entry: FAT[{}] = {}", cluster, next);
            None
        } else {
            Some(next)
        }
    }

    /// Follow the cluster chain to get the Nth cluster (0-indexed).
    /// For contiguous files, computes directly without reading FAT.
    pub fn get_nth_cluster(
        &self,
        first_cluster: u32,
        n: u64,
        is_contiguous: bool,
    ) -> Option<u32> {
        if is_contiguous {
            let target = first_cluster + n as u32;
            if self.is_valid_cluster(target) {
                Some(target)
            } else {
                None
            }
        } else {
            let mut current = first_cluster;
            for _ in 0..n {
                current = self.fat_read(current)?;
            }
            Some(current)
        }
    }

    /// Read the entire cluster chain starting from a given cluster.
    /// Returns the list of cluster numbers in order.
    /// For contiguous files, generates the range directly.
    pub fn read_cluster_chain(
        &self,
        first_cluster: u32,
        is_contiguous: bool,
        max_clusters: u32,
    ) -> Vec<u32> {
        let mut chain = Vec::new();

        if is_contiguous {
            // Contiguous: clusters are sequential starting from first_cluster
            let limit = core::cmp::min(max_clusters, self.cluster_count);
            for i in 0..limit {
                let c = first_cluster + i;
                if !self.is_valid_cluster(c) {
                    break;
                }
                chain.push(c);
            }
        } else {
            // Non-contiguous: follow FAT chain
            let mut current = first_cluster;
            let limit = core::cmp::min(max_clusters, MAX_CHAIN_LENGTH);
            for _ in 0..limit {
                if !self.is_valid_cluster(current) {
                    break;
                }
                chain.push(current);
                match self.fat_read(current) {
                    Some(next) => current = next,
                    None => break,
                }
            }
        }

        chain
    }

    // ────────────────────────────────────────────────────
    // Directory Scanning
    // ────────────────────────────────────────────────────

    /// Scan a directory starting at `dir_cluster` and return all file/subdir entries.
    /// This handles the full directory entry set parsing:
    ///   File Directory Entry (0x85) -> Stream Extension (0xC0) -> File Name (0xC1)+
    pub fn scan_directory(&self, dir_cluster: u32, is_contiguous: bool) -> Vec<ExfatFile> {
        let mut entries = Vec::new();
        let mut current_cluster = dir_cluster;
        let entries_per_cluster = self.bytes_per_cluster / DIR_ENTRY_SIZE;

        // State machine for parsing directory entry sets
        let mut pending_file: Option<FileDirectoryEntry> = None;
        let mut pending_stream: Option<StreamExtensionEntry> = None;
        let mut pending_name = String::new();
        let mut cluster_index: u32 = 0;

        loop {
            if cluster_index >= MAX_DIR_CLUSTERS {
                break;
            }
            if !self.is_valid_cluster(current_cluster) {
                break;
            }

            let cluster_data = match self.read_cluster(current_cluster) {
                Some(d) => d,
                None => break,
            };

            let mut end_of_dir = false;

            for entry_idx in 0..entries_per_cluster {
                let off = (entry_idx * DIR_ENTRY_SIZE) as usize;
                if off + 32 > cluster_data.len() {
                    break;
                }
                let raw = &cluster_data[off..off + 32];
                let entry_type = EntryType::from(raw[0]);

                match entry_type {
                    EntryType::EndOfDirectory => {
                        end_of_dir = true;
                        break;
                    }

                    EntryType::FileEntry => {
                        // Commit previous pending entry set
                        commit_pending(
                            &mut entries,
                            &mut pending_file,
                            &mut pending_stream,
                            &mut pending_name,
                        );

                        let fe: FileDirectoryEntry = unsafe {
                            core::ptr::read_unaligned(raw.as_ptr() as *const FileDirectoryEntry)
                        };
                        pending_file = Some(fe);
                        pending_name.clear();
                    }

                    EntryType::StreamExtension => {
                        let se: StreamExtensionEntry = unsafe {
                            core::ptr::read_unaligned(
                                raw.as_ptr() as *const StreamExtensionEntry,
                            )
                        };
                        pending_stream = Some(se);
                    }

                    EntryType::FileName => {
                        let ne: FileNameEntry = unsafe {
                            core::ptr::read_unaligned(raw.as_ptr() as *const FileNameEntry)
                        };
                        // Copy packed array to local to avoid unaligned reference UB
                        let fname: [u16; 15] = { ne.file_name };
                        for &ch16 in &fname {
                            if ch16 == 0 {
                                break;
                            }
                            if ch16 < 128 {
                                pending_name.push(ch16 as u8 as char);
                            } else {
                                pending_name.push('?');
                            }
                        }
                    }

                    EntryType::VolumeLabel | EntryType::AllocationBitmap
                    | EntryType::UpcaseTable => {
                        // Metadata entries - skip for file listing
                    }

                    EntryType::Unknown(b) => {
                        // Deleted entries (0x01..0x7F with bit 7 clear) or vendor extensions
                        if b == 0x05 || (b & 0x80) == 0 {
                            // Deleted/unused entry, skip
                        }
                    }
                }
            }

            // Commit any pending entry set after processing this cluster
            if end_of_dir {
                commit_pending(
                    &mut entries,
                    &mut pending_file,
                    &mut pending_stream,
                    &mut pending_name,
                );
                break;
            }

            // Follow cluster chain for directory
            cluster_index += 1;
            if is_contiguous {
                current_cluster += 1;
                if !self.is_valid_cluster(current_cluster) {
                    break;
                }
            } else {
                match self.fat_read(current_cluster) {
                    Some(next) => current_cluster = next,
                    None => break,
                }
            }
        }

        // Commit last pending entry
        commit_pending(
            &mut entries,
            &mut pending_file,
            &mut pending_stream,
            &mut pending_name,
        );

        entries
    }

    // ────────────────────────────────────────────────────
    // Path-Based File Lookup
    // ────────────────────────────────────────────────────

    /// Navigate to a file or directory given a path like "/models/mistral.gguf".
    /// Returns the ExfatFile entry if found.
    pub fn find_by_path(&self, path: &str) -> Option<ExfatFile> {
        let path = path.trim_start_matches('/');
        if path.is_empty() {
            // Root directory itself - return a synthetic entry
            return Some(ExfatFile {
                name: String::from("/"),
                size: 0,
                first_cluster: self.root_cluster,
                is_contiguous: false,
                is_directory: true,
                attributes: ATTR_DIRECTORY,
            });
        }

        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return None;
        }

        let mut current_cluster = self.root_cluster;
        let mut current_contiguous = false; // Root dir is not necessarily contiguous

        for (depth, &part) in parts.iter().enumerate() {
            if depth >= MAX_PATH_DEPTH {
                crate::serial_println!("[EXFAT] Path too deep: {}", path);
                return None;
            }

            // Scan directory at current_cluster for an entry named `part`
            let entries = self.scan_directory(current_cluster, current_contiguous);

            let found = entries.into_iter().find(|e| {
                // Case-insensitive comparison (simplified ASCII)
                if e.name.len() != part.len() {
                    return false;
                }
                e.name
                    .bytes()
                    .zip(part.bytes())
                    .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
            });

            match found {
                Some(entry) => {
                    if depth == parts.len() - 1 {
                        // This is the final component - return it
                        return Some(entry);
                    } else {
                        // Intermediate component - must be a directory
                        if !entry.is_directory {
                            crate::serial_println!(
                                "[EXFAT] Path component '{}' is not a directory",
                                part
                            );
                            return None;
                        }
                        current_cluster = entry.first_cluster;
                        current_contiguous = entry.is_contiguous;
                    }
                }
                None => {
                    return None;
                }
            }
        }

        None
    }

    // ────────────────────────────────────────────────────
    // File Reading
    // ────────────────────────────────────────────────────

    /// Read sequential bytes from a file starting at a given byte offset.
    /// Returns the number of bytes actually read.
    pub fn read_file_bytes(&self, file: &ExfatFile, offset: u64, buf: &mut [u8]) -> usize {
        if offset >= file.size || buf.is_empty() {
            return 0;
        }

        let to_read = core::cmp::min(buf.len() as u64, file.size - offset) as usize;
        let cluster_size = self.bytes_per_cluster as u64;

        // Calculate which cluster to start from
        let skip_clusters = offset / cluster_size;
        let offset_in_cluster = (offset % cluster_size) as usize;

        // Get the starting cluster
        let start_cluster = match self.get_nth_cluster(
            file.first_cluster,
            skip_clusters,
            file.is_contiguous,
        ) {
            Some(c) => c,
            None => return 0,
        };

        let mut bytes_read = 0;
        let mut current_cluster = start_cluster;
        let mut cluster_off = offset_in_cluster;

        while bytes_read < to_read {
            let remaining = to_read - bytes_read;
            let n = self.read_cluster_partial(
                current_cluster,
                cluster_off,
                &mut buf[bytes_read..bytes_read + remaining],
            );
            if n == 0 {
                break;
            }
            bytes_read += n;
            cluster_off = 0; // After first cluster, start from beginning

            if bytes_read < to_read {
                // Advance to next cluster
                if file.is_contiguous {
                    current_cluster += 1;
                    if !self.is_valid_cluster(current_cluster) {
                        break;
                    }
                } else {
                    match self.fat_read(current_cluster) {
                        Some(next) => current_cluster = next,
                        None => break,
                    }
                }
            }
        }

        bytes_read
    }

    /// List files in a directory by path.
    /// Returns None if the path is not a directory.
    pub fn list_directory_path(&self, path: &str) -> Option<Vec<ExfatFile>> {
        let dir = self.find_by_path(path)?;
        if !dir.is_directory {
            return None;
        }
        Some(self.scan_directory(dir.first_cluster, dir.is_contiguous))
    }

    /// Find a file by name in the cached root directory entries.
    pub fn find_file(&self, name: &str) -> Option<&ExfatFile> {
        self.files.iter().find(|f| {
            f.name.len() == name.len()
                && f.name
                    .bytes()
                    .zip(name.bytes())
                    .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
        })
    }

    /// Get the physical cluster number for a given byte offset in a file.
    /// Used by demand-paging to map individual pages.
    pub fn get_cluster_for_offset(
        &self,
        first_cluster: u32,
        byte_offset: u64,
        is_contiguous: bool,
    ) -> Option<u32> {
        let cluster_index = byte_offset / self.bytes_per_cluster as u64;
        self.get_nth_cluster(first_cluster, cluster_index, is_contiguous)
    }
}

// ────────────────────────────────────────────────────
// Helper: Commit a pending directory entry set
// ────────────────────────────────────────────────────

fn commit_pending(
    entries: &mut Vec<ExfatFile>,
    pending_file: &mut Option<FileDirectoryEntry>,
    pending_stream: &mut Option<StreamExtensionEntry>,
    pending_name: &mut String,
) {
    if let (Some(fe), Some(se)) = (pending_file.take(), pending_stream.take()) {
        if !pending_name.is_empty() {
            let is_contiguous = (se.general_secondary_flags & 0x02) != 0;
            let attrs = { fe.file_attributes };
            let se_data_len = { se.data_length };
            let se_first_cl = { se.first_cluster };
            let is_dir = (attrs & ATTR_DIRECTORY) != 0;
            entries.push(ExfatFile {
                name: pending_name.clone(),
                size: se_data_len,
                first_cluster: se_first_cl,
                is_contiguous,
                is_directory: is_dir,
                attributes: attrs,
            });
        }
    }
    pending_name.clear();
}

// ============================================================
// Global Filesystem Instance
// ============================================================

lazy_static! {
    pub static ref EXFAT: Mutex<ExfatFs> = Mutex::new(ExfatFs::new());
}

// ============================================================
// Public API
// ============================================================

/// Mount an exFAT filesystem from a VirtIO block device.
/// `sector_offset`: the starting sector of the exFAT partition on the disk.
pub fn mount(sector_offset: u64) -> Result<(), &'static str> {
    // Read the boot sector (first 512 bytes)
    let mut boot_buf = [0u8; 512];
    if !crate::drivers::virtio_blk::read_sector(sector_offset, &mut boot_buf) {
        return Err("Failed to read exFAT boot sector");
    }

    // Validate boot signature 0xAA55
    if boot_buf[510] != 0x55 || boot_buf[511] != 0xAA {
        return Err("Invalid boot signature (not 0xAA55)");
    }

    // Check "EXFAT   " magic at offset 3
    if &boot_buf[3..11] != b"EXFAT   " {
        return Err("Not an exFAT filesystem (missing EXFAT magic)");
    }

    // Check must_be_zero region (offsets 11..64) - should all be zero
    let has_nonzero = boot_buf[11..64].iter().any(|&b| b != 0);
    if has_nonzero {
        crate::serial_println!("[EXFAT] Warning: must_be_zero region has non-zero bytes");
        // Not fatal - some implementations don't strictly zero this
    }

    let bs: ExfatBootSector = unsafe {
        core::ptr::read_unaligned(boot_buf.as_ptr() as *const ExfatBootSector)
    };

    // Validate shift values
    let bps_shift = bs.bytes_per_sector_shift;
    if bps_shift < 9 || bps_shift > 12 {
        return Err("Invalid bytes_per_sector_shift (must be 9..12)");
    }
    let spc_shift = bs.sectors_per_cluster_shift;
    if bps_shift + spc_shift > 25 {
        return Err("Invalid sectors_per_cluster_shift (BPS+SPC > 25)");
    }

    let bytes_per_sector = 1u32 << bps_shift;
    let sectors_per_cluster = 1u32 << spc_shift;
    let bytes_per_cluster = bytes_per_sector * sectors_per_cluster;

    // Copy packed fields to locals to avoid unaligned reference UB
    let fat_off = { bs.fat_offset };
    let fat_len = { bs.fat_length };
    let heap_off = { bs.cluster_heap_offset };
    let root_cl = { bs.first_cluster_of_root_dir };
    let cl_count = { bs.cluster_count };
    let vol_serial = { bs.volume_serial_number };
    let num_fats = bs.number_of_fats;
    let pct_use = bs.percent_in_use;

    crate::serial_println!(
        "[EXFAT] Mount: BPS={}, SPC={}, BPC={}, FAT@{}, heap@{}, root_cluster={}, clusters={}",
        bytes_per_sector, sectors_per_cluster, bytes_per_cluster,
        fat_off, heap_off, root_cl, cl_count
    );
    crate::serial_println!(
        "[EXFAT] NumFATs={}, PercentInUse={}, Serial=0x{:08X}",
        num_fats, pct_use, vol_serial
    );

    // Validate cluster count and FAT size
    let fat_entries_per_sector = bytes_per_sector / 4;
    let min_fat_sectors = (cl_count + FIRST_DATA_CLUSTER + fat_entries_per_sector - 1) / fat_entries_per_sector;
    if fat_len < min_fat_sectors {
        crate::serial_println!(
            "[EXFAT] Warning: FAT may be too small ({} sectors for {} clusters)",
            fat_len, cl_count
        );
    }

    let mut fs = EXFAT.lock();
    fs.bytes_per_sector = bytes_per_sector;
    fs.sectors_per_cluster = sectors_per_cluster;
    fs.cluster_heap_offset = heap_off;
    fs.fat_offset = fat_off;
    fs.fat_length = fat_len;
    fs.root_cluster = root_cl;
    fs.cluster_count = cl_count;
    fs.bytes_per_cluster = bytes_per_cluster;
    fs.volume_serial = vol_serial;

    // Scan root directory for files and subdirectories
    let root_entries = fs.scan_directory(root_cl, false);
    crate::serial_println!("[EXFAT] Root directory: {} entries found", root_entries.len());
    for entry in &root_entries {
        let kind = if entry.is_directory { "DIR " } else { "FILE" };
        crate::serial_println!(
            "[EXFAT]   {} {} size={} cluster={} contiguous={}",
            kind, entry.name, entry.size, entry.first_cluster, entry.is_contiguous
        );
    }
    fs.files = root_entries;

    fs.mounted = true;
    crate::serial_println!(
        "[EXFAT] Mounted successfully: {} entries in root",
        fs.files.len()
    );

    Ok(())
}

/// Check if exFAT is mounted.
pub fn is_mounted() -> bool {
    EXFAT.lock().mounted
}

/// Get file info by name (root directory only).
/// Returns (size, first_cluster, is_contiguous).
pub fn get_file_info(name: &str) -> Option<(u64, u32, bool)> {
    let fs = EXFAT.lock();
    fs.find_file(name)
        .map(|f| (f.size, f.first_cluster, f.is_contiguous))
}

/// Read bytes from a file (by root directory name) at the given offset.
/// Returns the number of bytes read.
pub fn read_file(name: &str, offset: u64, buf: &mut [u8]) -> usize {
    let fs = EXFAT.lock();
    if let Some(file) = fs.find_file(name) {
        let file_clone = file.clone();
        drop(fs);
        let fs = EXFAT.lock();
        fs.read_file_bytes(&file_clone, offset, buf)
    } else {
        0
    }
}

/// Read bytes from a file at a given path and offset.
///
/// This is the primary API for streaming large files (e.g., GGUF models).
/// Supports full path traversal including subdirectories.
///
/// # Arguments
/// * `path` - Absolute path from the exFAT root, e.g. "/models/mistral-7b.gguf"
/// * `offset` - Byte offset within the file to start reading
/// * `buf` - Buffer to fill with file data
///
/// # Returns
/// * `Ok(bytes_read)` - Number of bytes successfully read
/// * `Err(())` - File not found, path invalid, or I/O error
///
/// # Example
/// ```
/// let mut buf = [0u8; 4096];
/// match read_file_at_offset("/models/model.gguf", 0, &mut buf) {
///     Ok(n) => serial_println!("Read {} bytes", n),
///     Err(_) => serial_println!("File not found"),
/// }
/// ```
pub fn read_file_at_offset(path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, ()> {
    let fs = EXFAT.lock();

    if !fs.mounted {
        crate::serial_println!("[EXFAT] read_file_at_offset: filesystem not mounted");
        return Err(());
    }

    // Try path-based lookup first
    let file = match fs.find_by_path(path) {
        Some(f) => f,
        None => {
            // Fallback: try as a plain filename in root directory
            match fs.find_file(path.trim_start_matches('/')) {
                Some(f) => f.clone(),
                None => {
                    return Err(());
                }
            }
        }
    };

    if file.is_directory {
        crate::serial_println!("[EXFAT] read_file_at_offset: '{}' is a directory", path);
        return Err(());
    }

    if offset >= file.size {
        return Ok(0); // Reading past EOF is not an error, just returns 0 bytes
    }

    let bytes_read = fs.read_file_bytes(&file, offset, buf);
    Ok(bytes_read)
}

/// Get the size of a file by path.
/// Returns None if the file doesn't exist or is a directory.
pub fn file_size(path: &str) -> Option<u64> {
    let fs = EXFAT.lock();
    if !fs.mounted {
        return None;
    }
    let file = fs.find_by_path(path)?;
    if file.is_directory {
        return None;
    }
    Some(file.size)
}

/// Check if a file exists at the given path.
pub fn file_exists(path: &str) -> bool {
    let fs = EXFAT.lock();
    if !fs.mounted {
        return false;
    }
    fs.find_by_path(path).is_some()
}

/// List files and subdirectories in a directory path.
/// Returns None if the path doesn't exist or isn't a directory.
pub fn list_directory(path: &str) -> Option<Vec<(String, u64, bool)>> {
    let fs = EXFAT.lock();
    if !fs.mounted {
        return None;
    }
    let entries = fs.list_directory_path(path)?;
    Some(
        entries
            .into_iter()
            .map(|e| (e.name, e.size, e.is_directory))
            .collect(),
    )
}

/// Get cluster information for demand-paging support.
/// Given a file's first cluster and byte offset, returns the physical cluster number
/// and the offset within that cluster.
/// Used by the page-fault handler for fd-backed VMA demand paging.
pub fn get_demand_page_cluster(
    first_cluster: u32,
    is_contiguous: bool,
    byte_offset: u64,
) -> Option<(u32, usize)> {
    let fs = EXFAT.lock();
    if !fs.mounted {
        return None;
    }
    let cluster = fs.get_cluster_for_offset(first_cluster, byte_offset, is_contiguous)?;
    let offset_in_cluster = (byte_offset % fs.bytes_per_cluster as u64) as usize;
    Some((cluster, offset_in_cluster))
}

/// Get the bytes-per-cluster size (needed for VMA alignment).
pub fn cluster_size() -> u32 {
    EXFAT.lock().bytes_per_cluster
}

/// Get file metadata for VMA creation.
/// Returns (size, first_cluster, is_contiguous, bytes_per_cluster).
pub fn get_file_metadata(path: &str) -> Option<(u64, u32, bool, u32)> {
    let fs = EXFAT.lock();
    if !fs.mounted {
        return None;
    }
    let file = fs.find_by_path(path)?;
    if file.is_directory {
        return None;
    }
    let bpc = fs.bytes_per_cluster;
    Some((file.size, file.first_cluster, file.is_contiguous, bpc))
}
