// kernel/src/fs/ext2.rs - Read/Write ext2 filesystem driver for AetherionOS
//
// Implements the ext2 filesystem (Linux Second Extended FS) on VirtIO-Block.
// This enables persistent storage for APK packages, /var, /usr, etc.
//
// ext2 on-disk layout:
//   Block 0: Boot block (1024 bytes, unused)
//   Block 1: Superblock (1024 bytes at offset 1024)
//   Block 2: Block Group Descriptor Table
//   Block N: Block/Inode bitmaps, inode table, data blocks
//
// Key structures:
//   Superblock: filesystem metadata (block size, inode count, etc.)
//   Block Group Descriptor: per-group metadata (bitmap locations, free counts)
//   Inode: file metadata (size, permissions, data block pointers)
//   Directory Entry: name -> inode mapping
//
// References:
//   - ext2 specification: https://www.nongnu.org/ext2-doc/ext2.html
//   - Linux kernel fs/ext2/

use alloc::string::String;
use alloc::vec::Vec;
use alloc::vec;

/// ext2 magic number
const EXT2_MAGIC: u16 = 0xEF53;

/// Inode numbers
const EXT2_ROOT_INO: u32 = 2;

/// File type in inode mode
const S_IFMT: u16 = 0xF000;
const S_IFREG: u16 = 0x8000;
const S_IFDIR: u16 = 0x4000;
const S_IFLNK: u16 = 0xA000;

/// Directory entry file types
const EXT2_FT_UNKNOWN: u8 = 0;
const EXT2_FT_REG_FILE: u8 = 1;
const EXT2_FT_DIR: u8 = 2;
const EXT2_FT_SYMLINK: u8 = 7;

/// ext2 Superblock (located at byte offset 1024, size 1024 bytes)
#[repr(C)]
#[derive(Debug, Clone)]
pub struct Ext2Superblock {
    pub s_inodes_count: u32,        // Total number of inodes
    pub s_blocks_count: u32,        // Total number of blocks
    pub s_r_blocks_count: u32,      // Reserved blocks
    pub s_free_blocks_count: u32,   // Free blocks
    pub s_free_inodes_count: u32,   // Free inodes
    pub s_first_data_block: u32,    // First data block (0 for 4K blocks, 1 for 1K)
    pub s_log_block_size: u32,      // Block size = 1024 << s_log_block_size
    pub s_log_frag_size: u32,       // Fragment size
    pub s_blocks_per_group: u32,    // Blocks per group
    pub s_frags_per_group: u32,     // Fragments per group
    pub s_inodes_per_group: u32,    // Inodes per group
    pub s_mtime: u32,               // Last mount time
    pub s_wtime: u32,               // Last write time
    pub s_mnt_count: u16,           // Mount count
    pub s_max_mnt_count: u16,       // Max mount count
    pub s_magic: u16,               // Magic signature (0xEF53)
    pub s_state: u16,               // Filesystem state
    pub s_errors: u16,              // Error handling
    pub s_minor_rev_level: u16,     // Minor revision level
    pub s_lastcheck: u32,           // Time of last check
    pub s_checkinterval: u32,       // Max interval between checks
    pub s_creator_os: u32,          // Creator OS
    pub s_rev_level: u32,           // Revision level
    pub s_def_resuid: u16,          // Default UID for reserved blocks
    pub s_def_resgid: u16,          // Default GID for reserved blocks
    // Extended superblock fields (rev >= 1)
    pub s_first_ino: u32,           // First non-reserved inode
    pub s_inode_size: u16,          // Inode size
    pub s_block_group_nr: u16,      // Block group of this superblock
    pub s_feature_compat: u32,      // Compatible features
    pub s_feature_incompat: u32,    // Incompatible features
    pub s_feature_ro_compat: u32,   // Read-only compatible features
}

/// Block Group Descriptor (32 bytes)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ext2GroupDesc {
    pub bg_block_bitmap: u32,       // Block bitmap block
    pub bg_inode_bitmap: u32,       // Inode bitmap block
    pub bg_inode_table: u32,        // Inode table start block
    pub bg_free_blocks_count: u16,  // Free blocks in group
    pub bg_free_inodes_count: u16,  // Free inodes in group
    pub bg_used_dirs_count: u16,    // Directories in group
    pub bg_pad: u16,
    pub bg_reserved: [u32; 3],
}

/// ext2 Inode (128 bytes for rev 0, s_inode_size for rev >= 1)
#[repr(C)]
#[derive(Debug, Clone)]
pub struct Ext2Inode {
    pub i_mode: u16,                // File mode
    pub i_uid: u16,                 // Owner UID
    pub i_size: u32,                // Size in bytes (lower 32 bits)
    pub i_atime: u32,               // Access time
    pub i_ctime: u32,               // Creation time
    pub i_mtime: u32,               // Modification time
    pub i_dtime: u32,               // Deletion time
    pub i_gid: u16,                 // Group ID
    pub i_links_count: u16,         // Hard links count
    pub i_blocks: u32,              // 512-byte blocks count
    pub i_flags: u32,               // Inode flags
    pub i_osd1: u32,                // OS-dependent value 1
    pub i_block: [u32; 15],         // Block pointers (0-11: direct, 12: indirect, 13: double, 14: triple)
    pub i_generation: u32,          // File version (for NFS)
    pub i_file_acl: u32,            // File ACL
    pub i_dir_acl: u32,             // Directory ACL (or file size upper 32 bits)
    pub i_faddr: u32,               // Fragment address
    pub i_osd2: [u8; 12],           // OS-dependent value 2
}

/// ext2 Directory Entry
#[repr(C)]
pub struct Ext2DirEntry {
    pub inode: u32,                 // Inode number
    pub rec_len: u16,               // Record length
    pub name_len: u8,               // Name length
    pub file_type: u8,              // File type
    // Followed by name[name_len] bytes
}

/// ext2 filesystem state
pub struct Ext2Fs {
    pub block_size: u32,
    pub inodes_per_group: u32,
    pub blocks_per_group: u32,
    pub inode_size: u16,
    pub first_data_block: u32,
    pub groups_count: u32,
    pub group_descs: Vec<Ext2GroupDesc>,
}

/// Global ext2 filesystem state
static mut EXT2_FS: Option<Ext2Fs> = None;
static EXT2_INITIALIZED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Read raw bytes from the block device at a byte offset
fn read_bytes(offset: u64, buf: &mut [u8]) -> bool {
    let sector_start = offset / 512;
    let byte_offset = (offset % 512) as usize;

    // Read enough sectors to cover the request
    let total_bytes = byte_offset + buf.len();
    let sectors_needed = (total_bytes + 511) / 512;

    let mut sector_buf = vec![0u8; sectors_needed * 512];
    if !crate::drivers::virtio_blk::read_sectors(sector_start, sectors_needed, &mut sector_buf) {
        return false;
    }

    buf.copy_from_slice(&sector_buf[byte_offset..byte_offset + buf.len()]);
    true
}

/// Write raw bytes to the block device at a byte offset
fn write_bytes(offset: u64, data: &[u8]) -> bool {
    let sector_start = offset / 512;
    let byte_offset = (offset % 512) as usize;
    let total_bytes = byte_offset + data.len();
    let sectors_needed = (total_bytes + 511) / 512;

    // Read-modify-write for partial sectors
    let mut sector_buf = vec![0u8; sectors_needed * 512];
    if !crate::drivers::virtio_blk::read_sectors(sector_start, sectors_needed, &mut sector_buf) {
        return false;
    }

    sector_buf[byte_offset..byte_offset + data.len()].copy_from_slice(data);

    crate::drivers::virtio_blk::write_sectors(sector_start, sectors_needed, &sector_buf)
}

/// Read a block from the filesystem
fn read_block(block_num: u32, buf: &mut [u8]) -> bool {
    let fs = match unsafe { &*core::ptr::addr_of!(EXT2_FS) } {
        Some(f) => f,
        None => return false,
    };
    let offset = block_num as u64 * fs.block_size as u64;
    read_bytes(offset, buf)
}

/// Write a block to the filesystem
fn write_block(block_num: u32, data: &[u8]) -> bool {
    let fs = match unsafe { &*core::ptr::addr_of!(EXT2_FS) } {
        Some(f) => f,
        None => return false,
    };
    let offset = block_num as u64 * fs.block_size as u64;
    write_bytes(offset, data)
}

/// Initialize the ext2 filesystem from the VirtIO block device
pub fn init() -> bool {
    if !crate::drivers::virtio_blk::is_available() {
        crate::serial_println!("[EXT2] No block device available");
        return false;
    }

    // Read superblock (at byte offset 1024)
    let mut sb_buf = [0u8; 1024];
    if !read_bytes(1024, &mut sb_buf) {
        crate::serial_println!("[EXT2] Failed to read superblock");
        return false;
    }

    // Parse superblock
    let sb = unsafe { &*(sb_buf.as_ptr() as *const Ext2Superblock) };

    // Verify magic
    if sb.s_magic != EXT2_MAGIC {
        crate::serial_println!("[EXT2] Bad magic: 0x{:04X} (expected 0xEF53)", sb.s_magic);
        return false;
    }

    let block_size = 1024u32 << sb.s_log_block_size;
    let groups_count = (sb.s_blocks_count + sb.s_blocks_per_group - 1) / sb.s_blocks_per_group;
    let inode_size = if sb.s_rev_level >= 1 { sb.s_inode_size } else { 128 };

    crate::serial_println!("[EXT2] Superblock OK:");
    crate::serial_println!("[EXT2]   Block size: {} bytes", block_size);
    crate::serial_println!("[EXT2]   Blocks: {} total, {} free", sb.s_blocks_count, sb.s_free_blocks_count);
    crate::serial_println!("[EXT2]   Inodes: {} total, {} free", sb.s_inodes_count, sb.s_free_inodes_count);
    crate::serial_println!("[EXT2]   Groups: {}", groups_count);
    crate::serial_println!("[EXT2]   Inode size: {} bytes", inode_size);
    crate::serial_println!("[EXT2]   Inodes/group: {}", sb.s_inodes_per_group);

    // Read Block Group Descriptor Table
    // Located in the block after the superblock
    let gdt_block = if block_size == 1024 { 2 } else { 1 };
    let gdt_size = groups_count as usize * 32; // Each descriptor is 32 bytes
    let gdt_sectors = (gdt_size + 511) / 512;
    let gdt_offset = gdt_block as u64 * block_size as u64;

    let mut gdt_buf = vec![0u8; gdt_sectors * 512];
    if !read_bytes(gdt_offset, &mut gdt_buf[..gdt_size]) {
        crate::serial_println!("[EXT2] Failed to read group descriptor table");
        return false;
    }

    let mut group_descs = Vec::with_capacity(groups_count as usize);
    for i in 0..groups_count as usize {
        let offset = i * 32;
        let gd = unsafe { &*(gdt_buf[offset..].as_ptr() as *const Ext2GroupDesc) };
        group_descs.push(*gd);
    }

    crate::serial_println!("[EXT2]   Group 0: inode_table=block {}, block_bitmap={}, inode_bitmap={}",
        group_descs[0].bg_inode_table, group_descs[0].bg_block_bitmap, group_descs[0].bg_inode_bitmap);

    let fs = Ext2Fs {
        block_size,
        inodes_per_group: sb.s_inodes_per_group,
        blocks_per_group: sb.s_blocks_per_group,
        inode_size,
        first_data_block: sb.s_first_data_block,
        groups_count,
        group_descs,
    };

    unsafe { *core::ptr::addr_of_mut!(EXT2_FS) = Some(fs); }
    EXT2_INITIALIZED.store(true, core::sync::atomic::Ordering::SeqCst);

    crate::serial_println!("[EXT2] Filesystem mounted successfully");
    true
}

/// Check if ext2 is initialized
pub fn is_mounted() -> bool {
    EXT2_INITIALIZED.load(core::sync::atomic::Ordering::SeqCst)
}

/// Read an inode from the filesystem
pub fn read_inode(ino: u32) -> Option<Ext2Inode> {
    let fs = unsafe { (*core::ptr::addr_of!(EXT2_FS)).as_ref()? };

    let group = ((ino - 1) / fs.inodes_per_group) as usize;
    let index = ((ino - 1) % fs.inodes_per_group) as usize;

    if group >= fs.group_descs.len() {
        return None;
    }

    let inode_table_block = fs.group_descs[group].bg_inode_table;
    let byte_offset = inode_table_block as u64 * fs.block_size as u64
        + index as u64 * fs.inode_size as u64;

    let mut inode_buf = [0u8; 128]; // Read at least 128 bytes
    if !read_bytes(byte_offset, &mut inode_buf) {
        return None;
    }

    let inode = unsafe { &*(inode_buf.as_ptr() as *const Ext2Inode) };
    Some(inode.clone())
}

/// Read all data blocks of an inode into a Vec
pub fn read_file(ino: u32) -> Option<Vec<u8>> {
    let inode = read_inode(ino)?;
    let fs = unsafe { (*core::ptr::addr_of!(EXT2_FS)).as_ref()? };
    let file_size = inode.i_size as usize;

    if file_size == 0 {
        return Some(Vec::new());
    }

    let blocks_needed = (file_size + fs.block_size as usize - 1) / fs.block_size as usize;
    let mut data = Vec::with_capacity(file_size);
    let mut block_buf = vec![0u8; fs.block_size as usize];

    for block_idx in 0..blocks_needed {
        let block_num = get_block_num(&inode, block_idx as u32, fs)?;
        if block_num == 0 {
            // Sparse file: zero-filled block
            data.extend_from_slice(&vec![0u8; fs.block_size as usize]);
        } else {
            if !read_block(block_num, &mut block_buf) {
                return None;
            }
            let remaining = file_size - data.len();
            let to_copy = core::cmp::min(remaining, fs.block_size as usize);
            data.extend_from_slice(&block_buf[..to_copy]);
        }
    }

    data.truncate(file_size);
    Some(data)
}

/// Get the disk block number for a given logical block index in an inode
fn get_block_num(inode: &Ext2Inode, logical_block: u32, fs: &Ext2Fs) -> Option<u32> {
    let ptrs_per_block = fs.block_size / 4; // u32 pointers per block

    if logical_block < 12 {
        // Direct blocks
        return Some(inode.i_block[logical_block as usize]);
    }

    let logical_block = logical_block - 12;
    if logical_block < ptrs_per_block {
        // Single indirect
        let indirect_block = inode.i_block[12];
        if indirect_block == 0 { return Some(0); }
        return read_indirect_block(indirect_block, logical_block, fs);
    }

    let logical_block = logical_block - ptrs_per_block;
    if logical_block < ptrs_per_block * ptrs_per_block {
        // Double indirect
        let dind_block = inode.i_block[13];
        if dind_block == 0 { return Some(0); }
        let ind_idx = logical_block / ptrs_per_block;
        let ind_off = logical_block % ptrs_per_block;
        let ind_block = read_indirect_block(dind_block, ind_idx, fs)?;
        if ind_block == 0 { return Some(0); }
        return read_indirect_block(ind_block, ind_off, fs);
    }

    let logical_block = logical_block - ptrs_per_block * ptrs_per_block;
    if logical_block < ptrs_per_block * ptrs_per_block * ptrs_per_block {
        // Triple indirect
        let tind_block = inode.i_block[14];
        if tind_block == 0 { return Some(0); }
        let dind_idx = logical_block / (ptrs_per_block * ptrs_per_block);
        let remainder = logical_block % (ptrs_per_block * ptrs_per_block);
        let dind_block = read_indirect_block(tind_block, dind_idx, fs)?;
        if dind_block == 0 { return Some(0); }
        let ind_idx = remainder / ptrs_per_block;
        let ind_off = remainder % ptrs_per_block;
        let ind_block = read_indirect_block(dind_block, ind_idx, fs)?;
        if ind_block == 0 { return Some(0); }
        return read_indirect_block(ind_block, ind_off, fs);
    }

    None // Block index too large
}

/// Read a u32 block pointer from an indirect block
fn read_indirect_block(block_num: u32, index: u32, fs: &Ext2Fs) -> Option<u32> {
    let offset = block_num as u64 * fs.block_size as u64 + index as u64 * 4;
    let mut buf = [0u8; 4];
    if !read_bytes(offset, &mut buf) {
        return None;
    }
    Some(u32::from_le_bytes(buf))
}

/// List directory entries of an inode
pub fn read_dir(ino: u32) -> Option<Vec<(String, u32, u8)>> {
    let data = read_file(ino)?;
    let mut entries = Vec::new();
    let mut offset = 0;

    while offset + 8 <= data.len() {
        let inode_num = u32::from_le_bytes([data[offset], data[offset+1], data[offset+2], data[offset+3]]);
        let rec_len = u16::from_le_bytes([data[offset+4], data[offset+5]]) as usize;
        let name_len = data[offset+6] as usize;
        let file_type = data[offset+7];

        if rec_len == 0 || rec_len < 8 {
            break; // Prevent infinite loop
        }

        if inode_num != 0 && name_len > 0 && offset + 8 + name_len <= data.len() {
            let name = core::str::from_utf8(&data[offset+8..offset+8+name_len])
                .unwrap_or("?");
            entries.push((String::from(name), inode_num, file_type));
        }

        offset += rec_len;
    }

    Some(entries)
}

/// Lookup a name in a directory inode, return the child inode number
fn dir_lookup(dir_ino: u32, name: &str) -> Option<u32> {
    let entries = read_dir(dir_ino)?;
    for (entry_name, ino, _ft) in entries {
        if entry_name == name {
            return Some(ino);
        }
    }
    None
}

/// Resolve a path to an inode number (starting from root inode 2)
pub fn lookup_path(path: &str) -> Option<u32> {
    lookup_path_follow(path, 0)
}

/// Internal path lookup that follows symlinks (up to max_depth=8 to prevent loops)
fn lookup_path_follow(path: &str, depth: u32) -> Option<u32> {
    if depth > 8 { return None; } // Symlink loop protection

    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return Some(EXT2_ROOT_INO);
    }

    let mut current_ino = EXT2_ROOT_INO;
    let components: Vec<&str> = path.split('/').filter(|c| !c.is_empty() && *c != ".").collect();

    for (idx, component) in components.iter().enumerate() {
        if *component == ".." {
            // Parent directory: simplified — stay at root for now
            // A proper implementation would track parent inodes
            continue;
        }
        current_ino = dir_lookup(current_ino, component)?;

        // Check if this inode is a symlink
        if let Some(true) = is_symlink(current_ino) {
            if let Some(target) = read_symlink(current_ino) {
                // Build the resolved path:
                // For absolute symlinks (starting with /): use target directly
                // For relative symlinks: prepend the parent directory path
                let resolved = if target.starts_with('/') {
                    // Absolute symlink
                    if idx + 1 < components.len() {
                        let rest: Vec<&str> = components[idx+1..].to_vec();
                        alloc::format!("{}/{}", target, rest.join("/"))
                    } else {
                        target
                    }
                } else {
                    // Relative symlink: resolve relative to parent directory
                    let parent_path: String = if idx > 0 {
                        components[..idx].join("/")
                    } else {
                        alloc::string::String::new()
                    };
                    if idx + 1 < components.len() {
                        let rest: Vec<&str> = components[idx+1..].to_vec();
                        if parent_path.is_empty() {
                            alloc::format!("{}/{}", target, rest.join("/"))
                        } else {
                            alloc::format!("{}/{}/{}", parent_path, target, rest.join("/"))
                        }
                    } else {
                        if parent_path.is_empty() {
                            target
                        } else {
                            alloc::format!("{}/{}", parent_path, target)
                        }
                    }
                };
                // Recursively resolve
                return lookup_path_follow(&resolved, depth + 1);
            }
            return None; // Broken symlink
        }
    }

    Some(current_ino)
}

/// Read only the first `max_bytes` of a file (avoids loading huge files into heap)
pub fn read_file_head(ino: u32, max_bytes: usize) -> Option<Vec<u8>> {
    let inode = read_inode(ino)?;
    let fs = unsafe { (*core::ptr::addr_of!(EXT2_FS)).as_ref()? };
    let file_size = core::cmp::min(inode.i_size as usize, max_bytes);

    if file_size == 0 {
        return Some(Vec::new());
    }

    let blocks_needed = (file_size + fs.block_size as usize - 1) / fs.block_size as usize;
    let mut data = Vec::with_capacity(file_size);
    let mut block_buf = vec![0u8; fs.block_size as usize];

    for block_idx in 0..blocks_needed {
        let block_num = get_block_num(&inode, block_idx as u32, fs)?;
        if block_num == 0 {
            data.extend_from_slice(&vec![0u8; fs.block_size as usize]);
        } else {
            if !read_block(block_num, &mut block_buf) {
                return None;
            }
            let remaining = file_size - data.len();
            let to_copy = core::cmp::min(remaining, fs.block_size as usize);
            data.extend_from_slice(&block_buf[..to_copy]);
        }
    }

    data.truncate(file_size);
    Some(data)
}

/// Read only the first `max_bytes` of a file by path
pub fn read_file_head_path(path: &str, max_bytes: usize) -> Option<Vec<u8>> {
    let ino = lookup_path(path)?;
    read_file_head(ino, max_bytes)
}

/// Read a file by path, returns the file contents
pub fn read_file_path(path: &str) -> Option<Vec<u8>> {
    let ino = lookup_path(path)?;
    read_file(ino)
}

/// List a directory by path
pub fn list_dir(path: &str) -> Option<Vec<(String, u32, u8)>> {
    let ino = lookup_path(path)?;
    read_dir(ino)
}

/// Get file size by inode
pub fn file_size(ino: u32) -> Option<u64> {
    let inode = read_inode(ino)?;
    // For regular files in rev >= 1, i_dir_acl holds upper 32 bits of size
    let size = inode.i_size as u64;
    // For large files, check if S_IFREG and add upper bits
    if inode.i_mode & S_IFMT == S_IFREG {
        let upper = inode.i_dir_acl as u64;
        Some(size | (upper << 32))
    } else {
        Some(size)
    }
}

/// Check if an inode is a directory
pub fn is_dir(ino: u32) -> Option<bool> {
    let inode = read_inode(ino)?;
    Some(inode.i_mode & S_IFMT == S_IFDIR)
}

/// Check if an inode is a regular file
pub fn is_file(ino: u32) -> Option<bool> {
    let inode = read_inode(ino)?;
    Some(inode.i_mode & S_IFMT == S_IFREG)
}

/// Check if an inode is a symlink
pub fn is_symlink(ino: u32) -> Option<bool> {
    let inode = read_inode(ino)?;
    Some(inode.i_mode & S_IFMT == S_IFLNK)
}

/// Read symlink target (for short symlinks, target is stored in i_block directly)
pub fn read_symlink(ino: u32) -> Option<String> {
    let inode = read_inode(ino)?;
    if inode.i_mode & S_IFMT != S_IFLNK {
        return None;
    }
    let size = inode.i_size as usize;
    if size <= 60 {
        // Fast symlink: target stored in i_block array (60 bytes max)
        let bytes = unsafe {
            core::slice::from_raw_parts(inode.i_block.as_ptr() as *const u8, 60)
        };
        let target = core::str::from_utf8(&bytes[..size]).ok()?;
        Some(String::from(target))
    } else {
        // Slow symlink: target stored in data blocks
        let data = read_file(ino)?;
        let target = core::str::from_utf8(&data[..size]).ok()?;
        Some(String::from(target))
    }
}

/// Read a chunk of a file by path at a given offset.
/// Returns up to `max_len` bytes starting at `offset`.
/// This avoids loading the entire file into memory.
pub fn read_file_chunk(path: &str, offset: u64, max_len: u64) -> Option<Vec<u8>> {
    let ino = lookup_path(path)?;
    let inode = read_inode(ino)?;
    let fs = unsafe { (*core::ptr::addr_of!(EXT2_FS)).as_ref()? };

    // Calculate total file size
    let total_size = if inode.i_mode & S_IFMT == S_IFREG {
        (inode.i_size as u64) | ((inode.i_dir_acl as u64) << 32)
    } else {
        inode.i_size as u64
    };

    if offset >= total_size {
        return Some(Vec::new()); // EOF
    }

    let available = total_size - offset;
    let to_read = core::cmp::min(available, max_len) as usize;
    let block_size = fs.block_size as usize;

    let mut data = Vec::with_capacity(to_read);
    let mut remaining = to_read;
    let mut current_offset = offset as usize;

    while remaining > 0 {
        let block_idx = current_offset / block_size;
        let offset_in_block = current_offset % block_size;
        let can_read = core::cmp::min(remaining, block_size - offset_in_block);

        let block_num = get_block_num(&inode, block_idx as u32, fs)?;
        if block_num == 0 {
            // Sparse block — fill with zeros
            data.extend(core::iter::repeat(0u8).take(can_read));
        } else {
            let mut block_buf = vec![0u8; block_size];
            if !read_block(block_num, &mut block_buf) {
                return None;
            }
            data.extend_from_slice(&block_buf[offset_in_block..offset_in_block + can_read]);
        }

        current_offset += can_read;
        remaining -= can_read;
    }

    Some(data)
}

/// Check if a path exists on ext2 (without reading the file data).
/// Returns the file size if found, None if not found.
pub fn file_exists(path: &str) -> Option<u64> {
    let ino = lookup_path(path)?;
    file_size(ino)
}

// ═══════════════════════════════════════════════════════════════
// WRITE SUPPORT: Block allocation, inode allocation, file creation
// ═══════════════════════════════════════════════════════════════

/// Read the superblock from disk
fn read_superblock() -> Option<Ext2Superblock> {
    let mut sb_buf = [0u8; 1024];
    if !read_bytes(1024, &mut sb_buf) {
        return None;
    }
    let sb = unsafe { &*(sb_buf.as_ptr() as *const Ext2Superblock) };
    Some(sb.clone())
}

/// Write the superblock back to disk
fn write_superblock(sb: &Ext2Superblock) -> bool {
    let sb_bytes = unsafe {
        core::slice::from_raw_parts(
            sb as *const Ext2Superblock as *const u8,
            core::mem::size_of::<Ext2Superblock>(),
        )
    };
    let mut buf = [0u8; 1024];
    if !read_bytes(1024, &mut buf) {
        return false;
    }
    buf[..sb_bytes.len()].copy_from_slice(sb_bytes);
    write_bytes(1024, &buf)
}

/// Write a group descriptor back to disk
fn write_group_desc(group: usize, gd: &Ext2GroupDesc) -> bool {
    let fs = match unsafe { &*core::ptr::addr_of!(EXT2_FS) } {
        Some(f) => f,
        None => return false,
    };
    let gdt_block = if fs.block_size == 1024 { 2 } else { 1 };
    let gdt_offset = gdt_block as u64 * fs.block_size as u64 + (group as u64 * 32);
    let gd_bytes = unsafe {
        core::slice::from_raw_parts(gd as *const Ext2GroupDesc as *const u8, 32)
    };
    write_bytes(gdt_offset, gd_bytes)
}

/// Allocate a free block from the filesystem. Returns block number or None.
pub fn alloc_block() -> Option<u32> {
    let fs = unsafe { (*core::ptr::addr_of!(EXT2_FS)).as_ref()? };
    let block_size = fs.block_size as usize;

    for group_idx in 0..fs.groups_count as usize {
        let gd = &fs.group_descs[group_idx];
        if gd.bg_free_blocks_count == 0 {
            continue;
        }

        // Read block bitmap
        let mut bitmap = vec![0u8; block_size];
        if !read_block(gd.bg_block_bitmap, &mut bitmap) {
            continue;
        }

        // Find first free bit
        let bits_to_check = fs.blocks_per_group as usize;
        for byte_idx in 0..core::cmp::min(block_size, (bits_to_check + 7) / 8) {
            if bitmap[byte_idx] == 0xFF {
                continue;
            }
            for bit in 0..8u32 {
                if bitmap[byte_idx] & (1 << bit) == 0 {
                    let block_num = group_idx as u32 * fs.blocks_per_group
                        + byte_idx as u32 * 8 + bit + fs.first_data_block;
                    // Mark as used
                    bitmap[byte_idx] |= 1 << bit;
                    write_block(gd.bg_block_bitmap, &bitmap);

                    // Update group descriptor free count
                    let mut new_gd = *gd;
                    new_gd.bg_free_blocks_count -= 1;
                    write_group_desc(group_idx, &new_gd);

                    // Update superblock free count
                    if let Some(mut sb) = read_superblock() {
                        sb.s_free_blocks_count -= 1;
                        write_superblock(&sb);
                    }

                    // Update in-memory state
                    let fs_mut = unsafe { (*core::ptr::addr_of_mut!(EXT2_FS)).as_mut().unwrap() };
                    fs_mut.group_descs[group_idx].bg_free_blocks_count -= 1;

                    // Zero the allocated block
                    let zeros = vec![0u8; block_size];
                    write_block(block_num, &zeros);

                    return Some(block_num);
                }
            }
        }
    }
    None
}

/// Allocate a free inode. Returns inode number (1-based) or None.
pub fn alloc_inode() -> Option<u32> {
    let fs = unsafe { (*core::ptr::addr_of!(EXT2_FS)).as_ref()? };
    let block_size = fs.block_size as usize;

    for group_idx in 0..fs.groups_count as usize {
        let gd = &fs.group_descs[group_idx];
        if gd.bg_free_inodes_count == 0 {
            continue;
        }

        let mut bitmap = vec![0u8; block_size];
        if !read_block(gd.bg_inode_bitmap, &mut bitmap) {
            continue;
        }

        let inodes_in_group = fs.inodes_per_group as usize;
        for byte_idx in 0..core::cmp::min(block_size, (inodes_in_group + 7) / 8) {
            if bitmap[byte_idx] == 0xFF {
                continue;
            }
            for bit in 0..8u32 {
                if bitmap[byte_idx] & (1 << bit) == 0 {
                    let local_idx = byte_idx as u32 * 8 + bit;
                    if local_idx >= fs.inodes_per_group {
                        break;
                    }
                    let ino = group_idx as u32 * fs.inodes_per_group + local_idx + 1;

                    // Mark used
                    bitmap[byte_idx] |= 1 << bit;
                    write_block(gd.bg_inode_bitmap, &bitmap);

                    let mut new_gd = *gd;
                    new_gd.bg_free_inodes_count -= 1;
                    write_group_desc(group_idx, &new_gd);

                    if let Some(mut sb) = read_superblock() {
                        sb.s_free_inodes_count -= 1;
                        write_superblock(&sb);
                    }

                    let fs_mut = unsafe { (*core::ptr::addr_of_mut!(EXT2_FS)).as_mut().unwrap() };
                    fs_mut.group_descs[group_idx].bg_free_inodes_count -= 1;

                    return Some(ino);
                }
            }
        }
    }
    None
}

/// Write an inode back to disk
pub fn write_inode(ino: u32, inode: &Ext2Inode) -> bool {
    let fs = match unsafe { &*core::ptr::addr_of!(EXT2_FS) } {
        Some(f) => f,
        None => return false,
    };

    let group = ((ino - 1) / fs.inodes_per_group) as usize;
    let index = ((ino - 1) % fs.inodes_per_group) as usize;

    if group >= fs.group_descs.len() {
        return false;
    }

    let inode_table_block = fs.group_descs[group].bg_inode_table;
    let byte_offset = inode_table_block as u64 * fs.block_size as u64
        + index as u64 * fs.inode_size as u64;

    let inode_bytes = unsafe {
        core::slice::from_raw_parts(
            inode as *const Ext2Inode as *const u8,
            core::cmp::min(128, fs.inode_size as usize),
        )
    };

    write_bytes(byte_offset, inode_bytes)
}

/// Set a block pointer in an inode (handles direct, single indirect)
fn set_block_num(inode: &mut Ext2Inode, logical_block: u32, block_num: u32) -> bool {
    let fs = match unsafe { &*core::ptr::addr_of!(EXT2_FS) } {
        Some(f) => f,
        None => return false,
    };
    let ptrs_per_block = fs.block_size / 4;

    if logical_block < 12 {
        inode.i_block[logical_block as usize] = block_num;
        return true;
    }

    let logical_block = logical_block - 12;
    if logical_block < ptrs_per_block {
        // Single indirect
        if inode.i_block[12] == 0 {
            if let Some(ind_blk) = alloc_block() {
                inode.i_block[12] = ind_blk;
            } else {
                return false;
            }
        }
        let offset = inode.i_block[12] as u64 * fs.block_size as u64 + logical_block as u64 * 4;
        let bytes = block_num.to_le_bytes();
        return write_bytes(offset, &bytes);
    }

    let logical_block = logical_block - ptrs_per_block;
    if logical_block < ptrs_per_block * ptrs_per_block {
        // Double indirect
        if inode.i_block[13] == 0 {
            if let Some(dind_blk) = alloc_block() {
                inode.i_block[13] = dind_blk;
            } else {
                return false;
            }
        }
        let ind_idx = logical_block / ptrs_per_block;
        let ind_off = logical_block % ptrs_per_block;

        // Read/create indirect block pointer
        let dind_offset = inode.i_block[13] as u64 * fs.block_size as u64 + ind_idx as u64 * 4;
        let mut ptr_buf = [0u8; 4];
        if !read_bytes(dind_offset, &mut ptr_buf) {
            return false;
        }
        let mut ind_blk = u32::from_le_bytes(ptr_buf);
        if ind_blk == 0 {
            if let Some(new_blk) = alloc_block() {
                ind_blk = new_blk;
                write_bytes(dind_offset, &new_blk.to_le_bytes());
            } else {
                return false;
            }
        }

        let offset = ind_blk as u64 * fs.block_size as u64 + ind_off as u64 * 4;
        let bytes = block_num.to_le_bytes();
        return write_bytes(offset, &bytes);
    }

    false // Triple indirect not yet implemented
}

/// Add a directory entry to a directory inode
fn add_dir_entry(dir_ino: u32, name: &str, child_ino: u32, file_type: u8) -> bool {
    let fs = match unsafe { &*core::ptr::addr_of!(EXT2_FS) } {
        Some(f) => f,
        None => return false,
    };
    let block_size = fs.block_size as usize;

    let mut dir_inode = match read_inode(dir_ino) {
        Some(i) => i,
        None => return false,
    };

    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len();
    // Entry size: 8 bytes header + name_len, rounded up to 4 bytes
    let entry_size = ((8 + name_len + 3) / 4) * 4;

    // Try to find space in existing blocks
    let blocks_used = (dir_inode.i_size as usize + block_size - 1) / block_size;
    for blk_idx in 0..blocks_used {
        let blk_num = match get_block_num(&dir_inode, blk_idx as u32, fs) {
            Some(b) if b != 0 => b,
            _ => continue,
        };

        let mut blk_data = vec![0u8; block_size];
        if !read_block(blk_num, &mut blk_data) {
            continue;
        }

        // Scan existing entries looking for one with extra space
        let mut offset = 0;
        while offset + 8 <= block_size {
            let rec_len = u16::from_le_bytes([blk_data[offset + 4], blk_data[offset + 5]]) as usize;
            if rec_len == 0 || rec_len < 8 {
                break;
            }
            let entry_name_len = blk_data[offset + 6] as usize;
            let actual_size = ((8 + entry_name_len + 3) / 4) * 4;

            if offset + rec_len >= block_size {
                // Last entry in the block: check if we can split it
                if rec_len >= actual_size + entry_size {
                    let new_rec_len = rec_len - actual_size;
                    // Shrink existing entry
                    blk_data[offset + 4] = (actual_size & 0xFF) as u8;
                    blk_data[offset + 5] = ((actual_size >> 8) & 0xFF) as u8;

                    // Write new entry after it
                    let new_offset = offset + actual_size;
                    blk_data[new_offset..new_offset + 4].copy_from_slice(&child_ino.to_le_bytes());
                    blk_data[new_offset + 4] = (new_rec_len & 0xFF) as u8;
                    blk_data[new_offset + 5] = ((new_rec_len >> 8) & 0xFF) as u8;
                    blk_data[new_offset + 6] = name_len as u8;
                    blk_data[new_offset + 7] = file_type;
                    blk_data[new_offset + 8..new_offset + 8 + name_len].copy_from_slice(name_bytes);

                    write_block(blk_num, &blk_data);
                    return true;
                }
            }
            offset += rec_len;
        }
    }

    // Need a new block for directory data
    let new_blk = match alloc_block() {
        Some(b) => b,
        None => return false,
    };
    let blk_idx = blocks_used as u32;
    set_block_num(&mut dir_inode, blk_idx, new_blk);

    let mut blk_data = vec![0u8; block_size];
    blk_data[0..4].copy_from_slice(&child_ino.to_le_bytes());
    // rec_len = entire block (this is the only entry)
    blk_data[4] = (block_size & 0xFF) as u8;
    blk_data[5] = ((block_size >> 8) & 0xFF) as u8;
    blk_data[6] = name_len as u8;
    blk_data[7] = file_type;
    blk_data[8..8 + name_len].copy_from_slice(name_bytes);
    write_block(new_blk, &blk_data);

    dir_inode.i_size += block_size as u32;
    dir_inode.i_blocks += (block_size as u32) / 512;
    write_inode(dir_ino, &dir_inode);
    true
}

/// Create a new file in the filesystem. Returns the inode number.
pub fn create_file(parent_dir: &str, name: &str, mode: u16) -> Option<u32> {
    let parent_ino = lookup_path(parent_dir)?;

    // Check if name already exists
    if dir_lookup(parent_ino, name).is_some() {
        crate::serial_println!("[EXT2] create_file: '{}' already exists in '{}'", name, parent_dir);
        return dir_lookup(parent_ino, name);
    }

    let ino = alloc_inode()?;
    let inode = Ext2Inode {
        i_mode: mode | S_IFREG,
        i_uid: 0,
        i_size: 0,
        i_atime: 0,
        i_ctime: 0,
        i_mtime: 0,
        i_dtime: 0,
        i_gid: 0,
        i_links_count: 1,
        i_blocks: 0,
        i_flags: 0,
        i_osd1: 0,
        i_block: [0; 15],
        i_generation: 0,
        i_file_acl: 0,
        i_dir_acl: 0,
        i_faddr: 0,
        i_osd2: [0; 12],
    };

    write_inode(ino, &inode);
    add_dir_entry(parent_ino, name, ino, EXT2_FT_REG_FILE);

    crate::serial_println!("[EXT2] Created file: {}/{} -> inode {}", parent_dir, name, ino);
    Some(ino)
}

/// Create a directory. Returns the inode number.
pub fn create_dir(parent_dir: &str, name: &str, mode: u16) -> Option<u32> {
    let parent_ino = lookup_path(parent_dir)?;

    if dir_lookup(parent_ino, name).is_some() {
        crate::serial_println!("[EXT2] mkdir: '{}' already exists in '{}'", name, parent_dir);
        return dir_lookup(parent_ino, name);
    }

    let ino = alloc_inode()?;
    let fs = unsafe { (*core::ptr::addr_of!(EXT2_FS)).as_ref()? };
    let block_size = fs.block_size as usize;

    // Allocate first data block for the directory
    let first_blk = alloc_block()?;

    let mut inode = Ext2Inode {
        i_mode: mode | S_IFDIR,
        i_uid: 0,
        i_size: block_size as u32,
        i_atime: 0,
        i_ctime: 0,
        i_mtime: 0,
        i_dtime: 0,
        i_gid: 0,
        i_links_count: 2, // . and parent's link
        i_blocks: (block_size as u32) / 512,
        i_flags: 0,
        i_osd1: 0,
        i_block: [0; 15],
        i_generation: 0,
        i_file_acl: 0,
        i_dir_acl: 0,
        i_faddr: 0,
        i_osd2: [0; 12],
    };
    inode.i_block[0] = first_blk;

    // Write . and .. entries
    let mut blk_data = vec![0u8; block_size];
    // "." entry
    blk_data[0..4].copy_from_slice(&ino.to_le_bytes());
    blk_data[4] = 12; blk_data[5] = 0; // rec_len = 12
    blk_data[6] = 1;  // name_len = 1
    blk_data[7] = EXT2_FT_DIR;
    blk_data[8] = b'.';
    // ".." entry
    let dotdot_rec_len = block_size - 12;
    blk_data[12..16].copy_from_slice(&parent_ino.to_le_bytes());
    blk_data[16] = (dotdot_rec_len & 0xFF) as u8;
    blk_data[17] = ((dotdot_rec_len >> 8) & 0xFF) as u8;
    blk_data[18] = 2; // name_len = 2
    blk_data[19] = EXT2_FT_DIR;
    blk_data[20] = b'.';
    blk_data[21] = b'.';

    write_block(first_blk, &blk_data);
    write_inode(ino, &inode);

    // Add entry in parent directory
    add_dir_entry(parent_ino, name, ino, EXT2_FT_DIR);

    // Increment parent's link count
    if let Some(mut parent_inode) = read_inode(parent_ino) {
        parent_inode.i_links_count += 1;
        write_inode(parent_ino, &parent_inode);
    }

    crate::serial_println!("[EXT2] Created dir: {}/{} -> inode {}", parent_dir, name, ino);
    Some(ino)
}

/// Write data to a file (overwrite). Sets the file size and allocates blocks as needed.
pub fn write_file_data(ino: u32, data: &[u8]) -> bool {
    let fs = match unsafe { &*core::ptr::addr_of!(EXT2_FS) } {
        Some(f) => f,
        None => return false,
    };
    let block_size = fs.block_size as usize;

    let mut inode = match read_inode(ino) {
        Some(i) => i,
        None => return false,
    };

    let blocks_needed = (data.len() + block_size - 1) / block_size;
    let mut block_buf = vec![0u8; block_size];

    for blk_idx in 0..blocks_needed {
        // Check if block already allocated
        let existing = get_block_num(&inode, blk_idx as u32, fs).unwrap_or(0);
        let blk_num = if existing != 0 {
            existing
        } else {
            match alloc_block() {
                Some(b) => {
                    set_block_num(&mut inode, blk_idx as u32, b);
                    b
                }
                None => {
                    crate::serial_println!("[EXT2] write_file_data: out of blocks at idx {}", blk_idx);
                    return false;
                }
            }
        };

        // Prepare block data
        let start = blk_idx * block_size;
        let end = core::cmp::min(start + block_size, data.len());
        let chunk = &data[start..end];

        for b in block_buf.iter_mut() { *b = 0; }
        block_buf[..chunk.len()].copy_from_slice(chunk);

        if !write_block(blk_num, &block_buf) {
            return false;
        }
    }

    inode.i_size = data.len() as u32;
    inode.i_blocks = (blocks_needed as u32) * (block_size as u32 / 512);
    write_inode(ino, &inode);

    true
}

/// Create and write a file by path. Returns inode number.
pub fn write_file_path(path: &str, data: &[u8]) -> Option<u32> {
    // Split into parent dir and filename
    let path = path.trim_start_matches('/');
    let (parent, name) = if let Some(idx) = path.rfind('/') {
        let parent = alloc::format!("/{}", &path[..idx]);
        let name = &path[idx + 1..];
        (parent, name)
    } else {
        (String::from("/"), path)
    };

    // Ensure parent directories exist
    ensure_dirs(&parent);

    // Create or get existing inode
    let ino = if let Some(existing) = lookup_path(&alloc::format!("/{}", path)) {
        existing
    } else {
        create_file(&parent, name, 0o644)?
    };

    if write_file_data(ino, data) {
        Some(ino)
    } else {
        None
    }
}

/// Recursively ensure all directories in a path exist
fn ensure_dirs(path: &str) {
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return;
    }

    let mut current = String::from("/");
    for component in path.split('/') {
        if component.is_empty() {
            continue;
        }
        let check_path = if current == "/" {
            alloc::format!("/{}", component)
        } else {
            alloc::format!("{}/{}", current, component)
        };

        if lookup_path(&check_path).is_none() {
            let parent = if current.is_empty() { "/" } else { &current };
            create_dir(parent, component, 0o755);
        }

        current = check_path;
    }
}

/// Create a symlink
pub fn create_symlink(parent_dir: &str, name: &str, target: &str) -> Option<u32> {
    let parent_ino = lookup_path(parent_dir)?;

    if dir_lookup(parent_ino, name).is_some() {
        return dir_lookup(parent_ino, name);
    }

    let ino = alloc_inode()?;
    let target_bytes = target.as_bytes();
    let mut inode = Ext2Inode {
        i_mode: 0o777 | S_IFLNK,
        i_uid: 0,
        i_size: target_bytes.len() as u32,
        i_atime: 0,
        i_ctime: 0,
        i_mtime: 0,
        i_dtime: 0,
        i_gid: 0,
        i_links_count: 1,
        i_blocks: 0,
        i_flags: 0,
        i_osd1: 0,
        i_block: [0; 15],
        i_generation: 0,
        i_file_acl: 0,
        i_dir_acl: 0,
        i_faddr: 0,
        i_osd2: [0; 12],
    };

    // Fast symlink: store in i_block if <= 60 bytes
    if target_bytes.len() <= 60 {
        let block_bytes = unsafe {
            core::slice::from_raw_parts_mut(
                inode.i_block.as_mut_ptr() as *mut u8,
                60,
            )
        };
        block_bytes[..target_bytes.len()].copy_from_slice(target_bytes);
    } else {
        // Slow symlink: store in data block
        write_inode(ino, &inode);
        if !write_file_data(ino, target_bytes) {
            return None;
        }
        // Re-read the inode after write_file_data modified it
        inode = read_inode(ino)?;
    }

    write_inode(ino, &inode);
    add_dir_entry(parent_ino, name, ino, EXT2_FT_SYMLINK);

    Some(ino)
}

/// Get filesystem statistics
pub fn statfs() -> Option<(u64, u64, u64, u64)> {
    let sb = read_superblock()?;
    Some((
        sb.s_blocks_count as u64,
        sb.s_free_blocks_count as u64,
        sb.s_inodes_count as u64,
        sb.s_free_inodes_count as u64,
    ))
}

/// Run ext2 self-tests
pub fn run_tests() {
    crate::serial_println!("\n========================================");
    crate::serial_println!("[EXT2 TESTS] Persistent Filesystem");
    crate::serial_println!("========================================\n");

    let mut passed = 0u32;
    let mut failed = 0u32;

    // Test 1: Mount
    crate::serial_write("  [TEST 1/8] ext2 mount... ");
    if is_mounted() {
        crate::serial_write("OK (already mounted)\n");
        passed += 1;
    } else if init() {
        crate::serial_write("OK\n");
        passed += 1;
    } else {
        crate::serial_write("SKIP (no ext2 filesystem)\n");
        crate::serial_println!("\n[EXT2 TESTS] Skipped (no ext2 disk)");
        return;
    }

    // Test 2: Read root inode
    crate::serial_write("  [TEST 2/8] Read root inode (inode 2)... ");
    match read_inode(EXT2_ROOT_INO) {
        Some(inode) => {
            if inode.i_mode & S_IFMT == S_IFDIR {
                crate::serial_println!("OK (dir, mode=0o{:o}, size={}, links={})",
                    inode.i_mode, inode.i_size, inode.i_links_count);
                passed += 1;
            } else {
                crate::serial_println!("FAIL (not a directory, mode=0x{:04X})", inode.i_mode);
                failed += 1;
            }
        }
        None => {
            crate::serial_write("FAIL (read error)\n");
            failed += 1;
        }
    }

    // Test 3: List root directory
    crate::serial_write("  [TEST 3/8] List root directory... ");
    match read_dir(EXT2_ROOT_INO) {
        Some(entries) => {
            let has_dot = entries.iter().any(|(n, _, _)| n == ".");
            let has_dotdot = entries.iter().any(|(n, _, _)| n == "..");
            if has_dot && has_dotdot {
                crate::serial_println!("OK ({} entries: {})",
                    entries.len(),
                    entries.iter().map(|(n, _, _)| n.as_str()).collect::<Vec<_>>().join(", "));
                passed += 1;
            } else {
                crate::serial_println!("FAIL (no . or .. entries)");
                failed += 1;
            }
        }
        None => {
            crate::serial_write("FAIL (read error)\n");
            failed += 1;
        }
    }

    // Test 4: Lookup /etc (common in Alpine rootfs)
    crate::serial_write("  [TEST 4/8] Lookup /etc... ");
    match lookup_path("/etc") {
        Some(ino) => {
            crate::serial_println!("OK (inode {})", ino);
            passed += 1;
        }
        None => {
            crate::serial_write("SKIP (no /etc)\n");
        }
    }

    // Test 5: Read a file
    crate::serial_write("  [TEST 5/8] Read /etc/hostname or /etc/os-release... ");
    let test_files = ["/etc/hostname", "/etc/os-release", "/etc/alpine-release"];
    let mut found_any = false;
    for path in &test_files {
        if let Some(data) = read_file_path(path) {
            let preview = core::str::from_utf8(&data[..core::cmp::min(data.len(), 64)])
                .unwrap_or("(binary)");
            crate::serial_println!("OK ({}: {} bytes, \"{}\")",
                path, data.len(), preview.trim());
            passed += 1;
            found_any = true;
            break;
        }
    }
    if !found_any {
        crate::serial_write("SKIP (no test files found)\n");
    }

    // Test 6: Check Alpine rootfs components (ld-musl, busybox)
    crate::serial_write("  [TEST 6/8] Alpine rootfs presence... ");
    let rootfs_files = ["/lib/ld-musl-x86_64.so.1", "/bin/busybox", "/bin/sh"];
    let mut rootfs_found = 0u32;
    for path in &rootfs_files {
        if lookup_path(path).is_some() {
            rootfs_found += 1;
        }
    }
    if rootfs_found > 0 {
        crate::serial_println!("OK ({}/{} rootfs binaries found)", rootfs_found, rootfs_files.len());
        passed += 1;
    } else {
        crate::serial_write("SKIP (no Alpine rootfs on disk)\n");
    }

    // Test 7: Write + read-back test
    crate::serial_write("  [TEST 7/8] Write and read-back... ");
    let test_data = b"AetherionOS ext2 write test OK\n";
    match write_file_path("/tmp/ext2_test.txt", test_data) {
        Some(ino) => {
            match read_file_path("/tmp/ext2_test.txt") {
                Some(data) if data == test_data => {
                    crate::serial_println!("OK (inode={}, {} bytes)", ino, data.len());
                    passed += 1;
                }
                Some(data) => {
                    crate::serial_println!("FAIL (readback mismatch: {} bytes)", data.len());
                    failed += 1;
                }
                None => {
                    crate::serial_write("FAIL (read-back returned None)\n");
                    failed += 1;
                }
            }
        }
        None => {
            crate::serial_write("FAIL (write returned None)\n");
            failed += 1;
        }
    }

    // Test 8: Symlink create + read
    crate::serial_write("  [TEST 8/8] Symlink create + read... ");
    match create_symlink("/tmp", "test_link", "/tmp/ext2_test.txt") {
        Some(ino) => {
            match read_symlink(ino) {
                Some(target) if target == "/tmp/ext2_test.txt" => {
                    crate::serial_println!("OK (inode={}, target='{}')", ino, target);
                    passed += 1;
                }
                Some(target) => {
                    crate::serial_println!("FAIL (wrong target: '{}')", target);
                    failed += 1;
                }
                None => {
                    crate::serial_write("FAIL (read_symlink returned None)\n");
                    failed += 1;
                }
            }
        }
        None => {
            crate::serial_write("FAIL (create_symlink returned None)\n");
            failed += 1;
        }
    }

    crate::serial_println!("\n========================================");
    crate::serial_println!("[EXT2 TESTS] {}/{} passed", passed, passed + failed);
    if failed == 0 && passed > 0 {
        crate::serial_write("[EXT2 TESTS] ALL TESTS PASSED!\n");
    }
    crate::serial_println!("========================================");
}
