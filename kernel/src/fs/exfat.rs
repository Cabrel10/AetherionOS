// fs/exfat.rs - Couche 18: Read-Only exFAT Driver for Zero-Copy Model Streaming
//
// Jalon 68: Implements a minimal read-only exFAT filesystem driver to support
// memory-mapped access to large model files (4+ GiB) on exFAT-formatted disks.
//
// exFAT is required because FAT32 has a 4 GiB file size limit.
// This driver supports:
//   - Boot sector parsing (VBR)
//   - Cluster chain traversal via FAT
//   - Directory entry parsing (File, Stream Extension, File Name)
//   - Sequential cluster reading for contiguous files
//   - Integration with VFS for sys_open/sys_read
//
// Limitations (read-only):
//   - No write support
//   - No file creation/deletion
//   - No directory creation
//   - Single partition only
//   - No long directory entry chains (max 255 char filename)
//
// Reference: Microsoft exFAT specification (Rev 1.0, 2008)

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;
use lazy_static::lazy_static;

/// exFAT Boot Sector (Volume Boot Record)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct ExfatBootSector {
    pub jump_boot: [u8; 3],
    pub fs_name: [u8; 8],        // "EXFAT   "
    pub must_be_zero: [u8; 53],  // Reserved (must be 0 for exFAT)
    pub partition_offset: u64,
    pub volume_length: u64,
    pub fat_offset: u32,
    pub fat_length: u32,
    pub cluster_heap_offset: u32,
    pub cluster_count: u32,
    pub first_cluster_of_root_dir: u32,
    pub volume_serial_number: u32,
    pub fs_revision: u16,
    pub volume_flags: u16,
    pub bytes_per_sector_shift: u8,
    pub sectors_per_cluster_shift: u8,
    pub number_of_fats: u8,
    pub drive_select: u8,
    pub percent_in_use: u8,
    pub reserved: [u8; 7],
    pub boot_code: [u8; 390],
    pub boot_signature: u16,    // 0xAA55
}

/// exFAT Directory Entry types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EntryType {
    EndOfDirectory,
    AllocationBitmap,
    UpcaseTable,
    VolumeLabel,
    FileEntry,
    StreamExtension,
    FileName,
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

/// exFAT File Directory Entry (32 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct FileDirectoryEntry {
    pub entry_type: u8,          // 0x85
    pub secondary_count: u8,
    pub set_checksum: u16,
    pub file_attributes: u16,
    pub reserved1: u16,
    pub create_timestamp: u32,
    pub last_modified: u32,
    pub last_accessed: u32,
    pub create_10ms: u8,
    pub last_modified_10ms: u8,
    pub create_utc_offset: u8,
    pub last_modified_utc_offset: u8,
    pub last_accessed_utc_offset: u8,
    pub reserved2: [u8; 7],
}

/// exFAT Stream Extension Entry (32 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct StreamExtensionEntry {
    pub entry_type: u8,          // 0xC0
    pub general_secondary_flags: u8,
    pub reserved1: u8,
    pub name_length: u8,
    pub name_hash: u16,
    pub reserved2: u16,
    pub valid_data_length: u64,
    pub reserved3: u32,
    pub first_cluster: u32,
    pub data_length: u64,
}

/// exFAT File Name Entry (32 bytes)
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct FileNameEntry {
    pub entry_type: u8,          // 0xC1
    pub general_secondary_flags: u8,
    pub file_name: [u16; 15],    // UTF-16LE, up to 15 characters per entry
}

/// A resolved exFAT file
#[derive(Debug, Clone)]
pub struct ExfatFile {
    pub name: String,
    pub size: u64,
    pub first_cluster: u32,
    pub is_contiguous: bool,
}

/// exFAT filesystem state
pub struct ExfatFs {
    pub mounted: bool,
    pub bytes_per_sector: u32,
    pub sectors_per_cluster: u32,
    pub cluster_heap_offset: u32,
    pub fat_offset: u32,
    pub fat_length: u32,
    pub root_cluster: u32,
    pub cluster_count: u32,
    pub bytes_per_cluster: u32,
    /// Cached root directory files
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
            files: Vec::new(),
        }
    }
    
    /// Calculate the byte offset for a given cluster number
    pub fn cluster_to_offset(&self, cluster: u32) -> u64 {
        let cluster_offset = (cluster - 2) as u64;
        let heap_start = self.cluster_heap_offset as u64 * self.bytes_per_sector as u64;
        heap_start + cluster_offset * self.bytes_per_cluster as u64
    }
    
    /// Read a cluster's worth of data via VirtIO block device
    /// Returns the data as a Vec<u8>
    pub fn read_cluster(&self, cluster: u32) -> Option<Vec<u8>> {
        let offset = self.cluster_to_offset(cluster);
        let size = self.bytes_per_cluster as usize;
        
        // Read from virtio_blk
        let sector = offset / self.bytes_per_sector as u64;
        let num_sectors = (size as u64 + self.bytes_per_sector as u64 - 1) / self.bytes_per_sector as u64;
        
        let mut data = Vec::with_capacity(size);
        data.resize(size, 0u8);
        
        for i in 0..num_sectors {
            let mut sector_buf = [0u8; 512];
            let current_sector = sector + i;
            
            if !crate::drivers::virtio_blk::read_sector(current_sector, &mut sector_buf) {
                crate::serial_println!("[EXFAT] Failed to read sector {}", current_sector);
                return None;
            }
            
            let buf_offset = (i as usize) * 512;
            let copy_len = core::cmp::min(512, size - buf_offset);
            data[buf_offset..buf_offset + copy_len].copy_from_slice(&sector_buf[..copy_len]);
        }
        
        Some(data)
    }
    
    /// Read the next cluster number from the FAT
    pub fn next_cluster(&self, cluster: u32) -> Option<u32> {
        let fat_byte_offset = self.fat_offset as u64 * self.bytes_per_sector as u64 
            + (cluster as u64) * 4;
        let fat_sector = fat_byte_offset / self.bytes_per_sector as u64;
        let fat_entry_offset = (fat_byte_offset % self.bytes_per_sector as u64) as usize;
        
        let mut sector_buf = [0u8; 512];
        if !crate::drivers::virtio_blk::read_sector(fat_sector, &mut sector_buf) {
            return None;
        }
        
        let next = u32::from_le_bytes([
            sector_buf[fat_entry_offset],
            sector_buf[fat_entry_offset + 1],
            sector_buf[fat_entry_offset + 2],
            sector_buf[fat_entry_offset + 3],
        ]);
        
        // 0xFFFFFFF8..=0xFFFFFFFF = end of chain
        if next >= 0xFFFFFFF8 {
            None
        } else if next < 2 || next > self.cluster_count + 1 {
            None
        } else {
            Some(next)
        }
    }
    
    /// Read sequential bytes from a file starting at a given offset
    pub fn read_file_bytes(&self, file: &ExfatFile, offset: u64, buf: &mut [u8]) -> usize {
        if offset >= file.size {
            return 0;
        }
        
        let to_read = core::cmp::min(buf.len() as u64, file.size - offset) as usize;
        let cluster_size = self.bytes_per_cluster as u64;
        
        // Find the starting cluster
        let skip_clusters = offset / cluster_size;
        let offset_in_cluster = (offset % cluster_size) as usize;
        
        let mut current_cluster = file.first_cluster;
        
        // If contiguous, calculate directly
        if file.is_contiguous {
            let start_cluster = file.first_cluster + skip_clusters as u32;
            let mut bytes_read = 0;
            let mut cluster = start_cluster;
            let mut cluster_offset = offset_in_cluster;
            
            while bytes_read < to_read {
                if let Some(data) = self.read_cluster(cluster) {
                    let available = data.len() - cluster_offset;
                    let copy_len = core::cmp::min(available, to_read - bytes_read);
                    buf[bytes_read..bytes_read + copy_len]
                        .copy_from_slice(&data[cluster_offset..cluster_offset + copy_len]);
                    bytes_read += copy_len;
                    cluster_offset = 0;
                    cluster += 1;
                } else {
                    break;
                }
            }
            return bytes_read;
        }
        
        // Non-contiguous: follow FAT chain
        for _ in 0..skip_clusters {
            current_cluster = match self.next_cluster(current_cluster) {
                Some(c) => c,
                None => return 0,
            };
        }
        
        let mut bytes_read = 0;
        let mut cluster_offset = offset_in_cluster;
        
        while bytes_read < to_read {
            if let Some(data) = self.read_cluster(current_cluster) {
                let available = data.len() - cluster_offset;
                let copy_len = core::cmp::min(available, to_read - bytes_read);
                buf[bytes_read..bytes_read + copy_len]
                    .copy_from_slice(&data[cluster_offset..cluster_offset + copy_len]);
                bytes_read += copy_len;
                cluster_offset = 0;
                
                if bytes_read < to_read {
                    current_cluster = match self.next_cluster(current_cluster) {
                        Some(c) => c,
                        None => break,
                    };
                }
            } else {
                break;
            }
        }
        
        bytes_read
    }
    
    /// Find a file by name in the root directory
    pub fn find_file(&self, name: &str) -> Option<&ExfatFile> {
        self.files.iter().find(|f| f.name == name)
    }
}

lazy_static! {
    pub static ref EXFAT: Mutex<ExfatFs> = Mutex::new(ExfatFs::new());
}

/// Mount an exFAT filesystem from a VirtIO block device
/// sector_offset: the starting sector of the exFAT partition
pub fn mount(sector_offset: u64) -> Result<(), &'static str> {
    // Read the boot sector
    let mut boot_buf = [0u8; 512];
    if !crate::drivers::virtio_blk::read_sector(sector_offset, &mut boot_buf) {
        return Err("Failed to read exFAT boot sector");
    }
    
    // Validate signature
    if boot_buf[510] != 0x55 || boot_buf[511] != 0xAA {
        return Err("Invalid boot signature (not 0xAA55)");
    }
    
    // Check "EXFAT   " magic
    if &boot_buf[3..11] != b"EXFAT   " {
        return Err("Not an exFAT filesystem (missing EXFAT magic)");
    }
    
    let bs: ExfatBootSector = unsafe {
        core::ptr::read_unaligned(boot_buf.as_ptr() as *const ExfatBootSector)
    };
    
    let bytes_per_sector = 1u32 << bs.bytes_per_sector_shift;
    let sectors_per_cluster = 1u32 << bs.sectors_per_cluster_shift;
    let bytes_per_cluster = bytes_per_sector * sectors_per_cluster;
    
    // Copy packed fields to locals to avoid unaligned reference UB
    let fat_off = { bs.fat_offset };
    let heap_off = { bs.cluster_heap_offset };
    let root_cl = { bs.first_cluster_of_root_dir };
    let cl_count = { bs.cluster_count };
    let fat_len = { bs.fat_length };
    
    crate::serial_println!(
        "[EXFAT] Mount: BPS={}, SPC={}, BPC={}, FAT@{}, heap@{}, root_cluster={}, clusters={}",
        bytes_per_sector, sectors_per_cluster, bytes_per_cluster,
        fat_off, heap_off, root_cl, cl_count
    );
    
    let mut fs = EXFAT.lock();
    fs.bytes_per_sector = bytes_per_sector;
    fs.sectors_per_cluster = sectors_per_cluster;
    fs.cluster_heap_offset = heap_off;
    fs.fat_offset = fat_off;
    fs.fat_length = fat_len;
    fs.root_cluster = root_cl;
    fs.cluster_count = cl_count;
    fs.bytes_per_cluster = bytes_per_cluster;
    
    // Scan root directory for files
    scan_root_directory(&mut fs);
    
    fs.mounted = true;
    crate::serial_println!("[EXFAT] Mounted successfully, {} files found", fs.files.len());
    
    Ok(())
}

/// Scan the root directory cluster chain for file entries
fn scan_root_directory(fs: &mut ExfatFs) {
    let mut current_cluster = fs.root_cluster;
    let mut pending_file: Option<FileDirectoryEntry> = None;
    let mut pending_stream: Option<StreamExtensionEntry> = None;
    let mut pending_name = String::new();
    
    for _ in 0..64 {  // Max 64 clusters of root directory
        let offset = fs.cluster_to_offset(current_cluster);
        let entries_per_cluster = fs.bytes_per_cluster / 32;
        
        for entry_idx in 0..entries_per_cluster {
            let entry_offset = offset + (entry_idx as u64) * 32;
            let sector = entry_offset / fs.bytes_per_sector as u64;
            let sector_offset = (entry_offset % fs.bytes_per_sector as u64) as usize;
            
            let mut sector_buf = [0u8; 512];
            if !crate::drivers::virtio_blk::read_sector(sector, &mut sector_buf) {
                return;
            }
            
            let entry_type = EntryType::from(sector_buf[sector_offset]);
            
            match entry_type {
                EntryType::EndOfDirectory => return,
                
                EntryType::FileEntry => {
                    // Commit any pending file
                    if let (Some(_fe), Some(se)) = (pending_file.take(), pending_stream.take()) {
                        if !pending_name.is_empty() {
                            let is_contiguous = (se.general_secondary_flags & 0x02) != 0;
                            let se_data_len = { se.data_length };
                            let se_first_cl = { se.first_cluster };
                            fs.files.push(ExfatFile {
                                name: pending_name.clone(),
                                size: se_data_len,
                                first_cluster: se_first_cl,
                                is_contiguous,
                            });
                            crate::serial_println!(
                                "[EXFAT] File: {} size={} cluster={} contiguous={}",
                                pending_name, se_data_len, se_first_cl, is_contiguous
                            );
                        }
                        pending_name.clear();
                    }
                    
                    let fe: FileDirectoryEntry = unsafe {
                        core::ptr::read_unaligned(
                            sector_buf[sector_offset..].as_ptr() as *const FileDirectoryEntry
                        )
                    };
                    pending_file = Some(fe);
                    pending_name.clear();
                }
                
                EntryType::StreamExtension => {
                    let se: StreamExtensionEntry = unsafe {
                        core::ptr::read_unaligned(
                            sector_buf[sector_offset..].as_ptr() as *const StreamExtensionEntry
                        )
                    };
                    pending_stream = Some(se);
                }
                
                EntryType::FileName => {
                    let ne: FileNameEntry = unsafe {
                        core::ptr::read_unaligned(
                            sector_buf[sector_offset..].as_ptr() as *const FileNameEntry
                        )
                    };
                    // Decode UTF-16LE to ASCII (simplified)
                    // Copy packed array to local to avoid unaligned reference
                    let fname: [u16; 15] = { ne.file_name };
                    for &ch16 in &fname {
                        if ch16 == 0 { break; }
                        if ch16 < 128 {
                            pending_name.push(ch16 as u8 as char);
                        } else {
                            pending_name.push('?');
                        }
                    }
                }
                
                _ => {} // Ignore other entry types
            }
        }
        
        // Follow cluster chain
        let fat_byte_offset = fs.fat_offset as u64 * fs.bytes_per_sector as u64
            + (current_cluster as u64) * 4;
        let fat_sector = fat_byte_offset / fs.bytes_per_sector as u64;
        let fat_entry_off = (fat_byte_offset % fs.bytes_per_sector as u64) as usize;
        
        let mut fat_buf = [0u8; 512];
        if !crate::drivers::virtio_blk::read_sector(fat_sector, &mut fat_buf) {
            break;
        }
        
        let next = u32::from_le_bytes([
            fat_buf[fat_entry_off],
            fat_buf[fat_entry_off + 1],
            fat_buf[fat_entry_off + 2],
            fat_buf[fat_entry_off + 3],
        ]);
        
        if next >= 0xFFFFFFF8 || next < 2 {
            break;
        }
        current_cluster = next;
    }
    
    // Commit last pending file
    if let (Some(_fe), Some(se)) = (pending_file, pending_stream) {
        if !pending_name.is_empty() {
            let is_contiguous = (se.general_secondary_flags & 0x02) != 0;
            let se_data_len = { se.data_length };
            let se_first_cl = { se.first_cluster };
            fs.files.push(ExfatFile {
                name: pending_name,
                size: se_data_len,
                first_cluster: se_first_cl,
                is_contiguous,
            });
        }
    }
}

/// Check if exFAT is mounted
pub fn is_mounted() -> bool {
    EXFAT.lock().mounted
}

/// Get file info by name
pub fn get_file_info(name: &str) -> Option<(u64, u32, bool)> {
    let fs = EXFAT.lock();
    fs.find_file(name).map(|f| (f.size, f.first_cluster, f.is_contiguous))
}

/// Read bytes from a file at the given offset
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
