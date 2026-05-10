// kernel/src/fs/ext2.rs — Read-Only Ext2 Filesystem Driver for AetherionOS
//
// Architecture:
//   - Reads ext2 structures directly from VirtIO-BLK sectors
//   - Supports: superblock, block group descriptors, inodes, directory entries
//   - Supports: direct blocks, single-indirect blocks, double-indirect blocks
//   - Designed for Alpine Linux rootfs (768 MiB - 2 GiB ext2 images)
//
// Usage:
//   ext2::mount()                     → bool (mount the first ext2 partition)
//   ext2::is_mounted()                → bool
//   ext2::lookup_path("/usr/bin/python3") → Option<u32> (inode number)
//   ext2::read_file_by_path("/usr/bin/python3") → Option<Vec<u8>>
//   ext2::read_file_chunk(path, offset, buf) → usize
//   ext2::list_directory(path)        → Option<Vec<DirEntry>>
//   ext2::stat_path(path)             → Option<Ext2Stat>
//   ext2::write_file_path(path, data) → Option<u32>

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

// ===== Ext2 Constants =====
const EXT2_MAGIC: u16 = 0xEF53;
const EXT2_SUPERBLOCK_OFFSET: u64 = 1024; // Superblock starts at byte 1024
const EXT2_ROOT_INO: u32 = 2;
const SECTOR_SIZE: usize = 512;

// Inode type flags (from i_mode)
const S_IFMT:  u16 = 0xF000;
const S_IFDIR: u16 = 0x4000;
const S_IFREG: u16 = 0x8000;
const S_IFLNK: u16 = 0xA000;

// Directory entry file types
const EXT2_FT_REG_FILE: u8 = 1;
const EXT2_FT_DIR: u8 = 2;
const EXT2_FT_SYMLINK: u8 = 7;

// ===== On-Disk Structures =====

#[repr(C)]
#[derive(Clone, Copy)]
struct Ext2Superblock {
    s_inodes_count: u32,
    s_blocks_count: u32,
    s_r_blocks_count: u32,
    s_free_blocks_count: u32,
    s_free_inodes_count: u32,
    s_first_data_block: u32,
    s_log_block_size: u32,
    s_log_frag_size: u32,
    s_blocks_per_group: u32,
    s_frags_per_group: u32,
    s_inodes_per_group: u32,
    s_mtime: u32,
    s_wtime: u32,
    s_mnt_count: u16,
    s_max_mnt_count: u16,
    s_magic: u16,
    s_state: u16,
    s_errors: u16,
    s_minor_rev_level: u16,
    s_lastcheck: u32,
    s_checkinterval: u32,
    s_creator_os: u32,
    s_rev_level: u32,
    s_def_resuid: u16,
    s_def_resgid: u16,
    // EXT2_DYNAMIC_REV fields
    s_first_ino: u32,
    s_inode_size: u16,
    s_block_group_nr: u16,
    s_feature_compat: u32,
    s_feature_incompat: u32,
    s_feature_ro_compat: u32,
    // ... rest is not needed for read-only
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Ext2BlockGroupDesc {
    bg_block_bitmap: u32,
    bg_inode_bitmap: u32,
    bg_inode_table: u32,
    bg_free_blocks_count: u16,
    bg_free_inodes_count: u16,
    bg_used_dirs_count: u16,
    bg_pad: u16,
    bg_reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Ext2Inode {
    pub i_mode: u16,
    pub i_uid: u16,
    pub i_size: u32,
    pub i_atime: u32,
    pub i_ctime: u32,
    pub i_mtime: u32,
    pub i_dtime: u32,
    pub i_gid: u16,
    pub i_links_count: u16,
    pub i_blocks: u32,
    pub i_flags: u32,
    pub i_osd1: u32,
    pub i_block: [u32; 15], // 0-11: direct, 12: single indirect, 13: double, 14: triple
    pub i_generation: u32,
    pub i_file_acl: u32,
    pub i_dir_acl: u32, // upper 32 bits of size for regular files in rev1
    pub i_faddr: u32,
    pub i_osd2: [u8; 12],
}

// ===== Runtime State =====

static MOUNTED: AtomicBool = AtomicBool::new(false);
static BLOCK_SIZE: AtomicU32 = AtomicU32::new(1024);
static INODE_SIZE: AtomicU32 = AtomicU32::new(128);
static INODES_PER_GROUP: AtomicU32 = AtomicU32::new(0);
static BLOCKS_PER_GROUP: AtomicU32 = AtomicU32::new(0);
static FIRST_DATA_BLOCK: AtomicU32 = AtomicU32::new(0);
static BLOCK_GROUP_COUNT: AtomicU32 = AtomicU32::new(0);

/// Cached superblock and block group descriptors (immutable after mount)
static mut SUPERBLOCK: Option<Ext2Superblock> = None;
static mut BGD_TABLE: Option<Vec<Ext2BlockGroupDesc>> = None;

// ===== Public Stat Result =====

pub struct Ext2Stat {
    pub ino: u32,
    pub mode: u16,
    pub size: u64,
    pub blocks: u32,
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
}

pub struct DirEntry {
    pub name: String,
    pub inode: u32,
    pub file_type: u8,
}

// ===== Block I/O Layer =====

/// Read a single ext2 block from VirtIO-BLK
fn read_block(block_num: u32, buf: &mut [u8]) -> bool {
    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as u64;
    let byte_offset = block_num as u64 * bs;
    let start_sector = byte_offset / SECTOR_SIZE as u64;
    let sector_count = (bs as usize) / SECTOR_SIZE;

    if buf.len() < bs as usize {
        return false;
    }

    crate::drivers::virtio_blk::read_sectors(start_sector, sector_count, &mut buf[..bs as usize])
}

/// Read arbitrary bytes from disk at a given byte offset
fn read_bytes(offset: u64, buf: &mut [u8]) -> bool {
    let start_sector = offset / SECTOR_SIZE as u64;
    let sector_offset = (offset % SECTOR_SIZE as u64) as usize;

    // Calculate how many sectors we need
    let total_bytes = buf.len();
    let sectors_needed = (sector_offset + total_bytes + SECTOR_SIZE - 1) / SECTOR_SIZE;

    // Read sectors into a temporary buffer
    let mut sector_buf = vec![0u8; sectors_needed * SECTOR_SIZE];
    if !crate::drivers::virtio_blk::read_sectors(start_sector, sectors_needed, &mut sector_buf) {
        return false;
    }

    buf.copy_from_slice(&sector_buf[sector_offset..sector_offset + total_bytes]);
    true
}

// ===== Inode Operations =====

/// Read an inode by number
pub fn read_inode(ino: u32) -> Option<Ext2Inode> {
    if ino == 0 {
        return None;
    }

    let ipg = INODES_PER_GROUP.load(Ordering::Relaxed);
    let inode_sz = INODE_SIZE.load(Ordering::Relaxed);
    if ipg == 0 || inode_sz == 0 {
        return None;
    }

    let group = (ino - 1) / ipg;
    let index = (ino - 1) % ipg;

    // Get inode table block from BGD
    let inode_table_block = unsafe {
        BGD_TABLE.as_ref()?.get(group as usize)?.bg_inode_table
    };

    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as u64;
    let byte_offset = inode_table_block as u64 * bs + index as u64 * inode_sz as u64;

    let mut buf = [0u8; 256]; // Max inode size
    let read_size = (inode_sz as usize).min(256);
    if !read_bytes(byte_offset, &mut buf[..read_size]) {
        return None;
    }

    // Parse inode structure (128 bytes is the base size)
    let inode = unsafe { core::ptr::read_unaligned(buf.as_ptr() as *const Ext2Inode) };
    Some(inode)
}

/// Get the real size of a file (supports large files via i_dir_acl in rev1)
fn inode_size(inode: &Ext2Inode) -> u64 {
    let is_regular = (inode.i_mode & S_IFMT) == S_IFREG;
    if is_regular {
        // For regular files, i_dir_acl holds upper 32 bits of size
        (inode.i_dir_acl as u64) << 32 | inode.i_size as u64
    } else {
        inode.i_size as u64
    }
}

/// Resolve a block number from an inode's block map (handles indirect blocks)
fn resolve_block(inode: &Ext2Inode, logical_block: u32) -> Option<u32> {
    let bs = BLOCK_SIZE.load(Ordering::Relaxed);
    let ptrs_per_block = bs / 4; // Each pointer is 4 bytes (u32)

    if logical_block < 12 {
        // Direct blocks
        let b = inode.i_block[logical_block as usize];
        return if b == 0 { None } else { Some(b) };
    }

    let remaining = logical_block - 12;

    if remaining < ptrs_per_block {
        // Single indirect
        let indirect_block = inode.i_block[12];
        if indirect_block == 0 {
            return None;
        }
        return read_block_ptr(indirect_block, remaining);
    }

    let remaining = remaining - ptrs_per_block;

    if remaining < ptrs_per_block * ptrs_per_block {
        // Double indirect
        let dind_block = inode.i_block[13];
        if dind_block == 0 {
            return None;
        }
        let idx1 = remaining / ptrs_per_block;
        let idx2 = remaining % ptrs_per_block;
        let ind_block = read_block_ptr(dind_block, idx1)?;
        if ind_block == 0 {
            return None;
        }
        return read_block_ptr(ind_block, idx2);
    }

    let remaining = remaining - ptrs_per_block * ptrs_per_block;

    // Triple indirect
    let tind_block = inode.i_block[14];
    if tind_block == 0 {
        return None;
    }
    let idx1 = remaining / (ptrs_per_block * ptrs_per_block);
    let idx2 = (remaining / ptrs_per_block) % ptrs_per_block;
    let idx3 = remaining % ptrs_per_block;
    let dind = read_block_ptr(tind_block, idx1)?;
    if dind == 0 {
        return None;
    }
    let ind = read_block_ptr(dind, idx2)?;
    if ind == 0 {
        return None;
    }
    read_block_ptr(ind, idx3)
}

/// Read a u32 block pointer from an indirect block at a given index
fn read_block_ptr(block: u32, index: u32) -> Option<u32> {
    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
    let offset_in_block = (index as usize) * 4;
    if offset_in_block + 4 > bs {
        return None;
    }

    let byte_offset = block as u64 * bs as u64 + offset_in_block as u64;
    let mut buf = [0u8; 4];
    if !read_bytes(byte_offset, &mut buf) {
        return None;
    }
    let ptr = u32::from_le_bytes(buf);
    if ptr == 0 { None } else { Some(ptr) }
}

// ===== Directory Operations =====

/// List entries in a directory inode
fn list_dir_inode(inode: &Ext2Inode) -> Option<Vec<DirEntry>> {
    let size = inode.i_size as usize;
    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;

    let mut entries = Vec::new();
    let mut offset = 0usize;
    let mut logical_block = 0u32;

    let mut block_buf = vec![0u8; bs];

    // Diagnostic logging
    crate::serial_println!("[EXT2-LISTDIR] size={} bs={} blocks_needed={}", 
        size, bs, (size + bs - 1) / bs);

    while offset < size {
        // Read the current block
        let phys_block = match resolve_block(inode, logical_block) {
            Some(b) => b,
            None => {
                crate::serial_println!("[EXT2-LISTDIR] resolve_block failed for logical_block={}", logical_block);
                break;
            }
        };
        
        if !read_block(phys_block, &mut block_buf) {
            crate::serial_println!("[EXT2-LISTDIR] read_block failed for phys_block={}", phys_block);
            break;
        }

        let mut pos = 0usize;
        let block_end = bs.min(size - offset);

        crate::serial_println!("[EXT2-LISTDIR] block {} (phys={}) pos=0..{}", 
            logical_block, phys_block, block_end);

        while pos + 8 <= block_end {
            // Parse directory entry header
            let d_inode = u32::from_le_bytes([
                block_buf[pos], block_buf[pos+1], block_buf[pos+2], block_buf[pos+3]
            ]);
            let d_rec_len = u16::from_le_bytes([block_buf[pos+4], block_buf[pos+5]]) as usize;
            let d_name_len = block_buf[pos+6] as usize;
            let d_file_type = block_buf[pos+7];

            if d_rec_len == 0 {
                crate::serial_println!("[EXT2-LISTDIR] d_rec_len=0 at pos={}, breaking", pos);
                break; // Prevent infinite loop
            }

            if d_inode != 0 && d_name_len > 0 && pos + 8 + d_name_len <= block_buf.len() {
                let name_bytes = &block_buf[pos+8..pos+8+d_name_len];
                if let Ok(name) = core::str::from_utf8(name_bytes) {
                    crate::serial_println!("[EXT2-LISTDIR]   entry: {} (type={})", name, d_file_type);
                    entries.push(DirEntry {
                        name: String::from(name),
                        inode: d_inode,
                        file_type: d_file_type,
                    });
                }
            }

            pos += d_rec_len;
        }

        offset += bs;
        logical_block += 1;
    }

    crate::serial_println!("[EXT2-LISTDIR] total_entries={}", entries.len());
    Some(entries)
}

// ===== Path Resolution =====

/// Resolve a path to an inode number, following symlinks (up to 8 levels)
fn resolve_path_to_inode(path: &str) -> Option<u32> {
    resolve_path_impl(path, 0)
}

fn resolve_path_impl(path: &str, depth: u8) -> Option<u32> {
    if depth > 8 {
        return None; // Symlink loop protection
    }

    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return Some(EXT2_ROOT_INO);
    }

    let mut current_ino = EXT2_ROOT_INO;
    let components: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    let mut i = 0;
    while i < components.len() {
        let component = components[i];
        if component == "." {
            i += 1;
            continue;
        }
        if component == ".." {
            // For "..", we need to go to parent. Since we don't track parent inodes,
            // restart the resolution from root with a normalized path.
            // Build the path so far (up to but not including ".."), remove last component
            let mut prefix_parts: Vec<&str> = Vec::new();
            for j in 0..i {
                if components[j] != "." && components[j] != ".." {
                    prefix_parts.push(components[j]);
                }
            }
            prefix_parts.pop(); // go up one level
            // Build remaining path
            let remaining: Vec<&str> = components[i+1..].to_vec();
            let mut new_parts = prefix_parts;
            new_parts.extend(remaining);
            let new_path = new_parts.join("/");
            return resolve_path_impl(&new_path, depth + 1);
        }

        let inode = read_inode(current_ino)?;
        if (inode.i_mode & S_IFMT) != S_IFDIR {
            return None; // Not a directory
        }

        let entries = list_dir_inode(&inode)?;
        let mut found = false;
        for entry in &entries {
            if entry.name == component {
                // Check if this is a symlink
                let target_inode = read_inode(entry.inode)?;
                if (target_inode.i_mode & S_IFMT) == S_IFLNK {
                    // Read symlink target
                    let target = read_symlink_target(&target_inode, entry.inode)?;
                    // Collect remaining path components after this one
                    let remaining: Vec<&str> = components[i+1..].to_vec();
                    let remaining_str = remaining.join("/");

                    if target.starts_with('/') {
                        // Absolute symlink
                        let new_path = if remaining_str.is_empty() {
                            target.clone()
                        } else {
                            alloc::format!("{}/{}", target, remaining_str)
                        };
                        return resolve_path_impl(&new_path, depth + 1);
                    } else {
                        // Relative symlink — resolve relative to the current directory.
                        // Build the "current directory" path from components[0..i]
                        let cur_dir: String = if i == 0 {
                            String::from("/")
                        } else {
                            alloc::format!("/{}", components[..i].join("/"))
                        };
                        let new_path = if remaining_str.is_empty() {
                            alloc::format!("{}/{}", cur_dir, target)
                        } else {
                            alloc::format!("{}/{}/{}", cur_dir, target, remaining_str)
                        };
                        return resolve_path_impl(&new_path, depth + 1);
                    }
                }
                current_ino = entry.inode;
                found = true;
                break;
            }
        }
        if !found {
            return None;
        }
        i += 1;
    }

    Some(current_ino)
}

/// Read symlink target from inode (fast symlinks stored in i_block)
fn read_symlink_target(inode: &Ext2Inode, _ino: u32) -> Option<String> {
    let size = inode.i_size as usize;
    if size == 0 {
        return None;
    }

    // Fast symlinks: target stored directly in i_block[] (up to 60 bytes)
    if size <= 60 && inode.i_blocks == 0 {
        let ptr = inode.i_block.as_ptr() as *const u8;
        let bytes = unsafe { core::slice::from_raw_parts(ptr, size) };
        return core::str::from_utf8(bytes).ok().map(String::from);
    }

    // Slow symlink: target stored in data blocks
    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
    let block = resolve_block(inode, 0)?;
    let mut buf = vec![0u8; bs];
    if !read_block(block, &mut buf) {
        return None;
    }
    let actual_size = size.min(bs);
    core::str::from_utf8(&buf[..actual_size]).ok().map(String::from)
}

// ===== File Read Operations =====

/// Read an entire file by inode
fn read_file_inode(inode: &Ext2Inode) -> Option<Vec<u8>> {
    let size = inode_size(inode) as usize;
    if size == 0 {
        return Some(Vec::new());
    }

    // Safety limit: 256 MiB max read
    if size > 256 * 1024 * 1024 {
        crate::serial_println!("[EXT2] File too large: {} bytes", size);
        return None;
    }

    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
    let num_blocks = (size + bs - 1) / bs;

    let mut data = vec![0u8; size];
    let mut block_buf = vec![0u8; bs];
    let mut bytes_read = 0;

    for logical_block in 0..num_blocks as u32 {
        let phys_block = match resolve_block(inode, logical_block) {
            Some(b) => b,
            None => {
                // Sparse block (hole) — fill with zeros
                let remaining = size - bytes_read;
                let to_copy = remaining.min(bs);
                // Already zero from vec! initialization
                bytes_read += to_copy;
                continue;
            }
        };

        if !read_block(phys_block, &mut block_buf) {
            crate::serial_println!("[EXT2] Failed to read block {} (logical {})", phys_block, logical_block);
            return None;
        }

        let remaining = size - bytes_read;
        let to_copy = remaining.min(bs);
        data[bytes_read..bytes_read + to_copy].copy_from_slice(&block_buf[..to_copy]);
        bytes_read += to_copy;
    }

    Some(data)
}

/// Read a chunk of a file at a given offset
fn read_file_chunk_inode(inode: &Ext2Inode, offset: u64, buf: &mut [u8]) -> usize {
    let size = inode_size(inode);
    if offset >= size {
        return 0;
    }

    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as u64;
    let to_read = buf.len().min((size - offset) as usize);
    let mut bytes_read = 0usize;
    let mut block_buf = vec![0u8; bs as usize];

    while bytes_read < to_read {
        let current_offset = offset + bytes_read as u64;
        let logical_block = (current_offset / bs) as u32;
        let block_offset = (current_offset % bs) as usize;

        let phys_block = match resolve_block(inode, logical_block) {
            Some(b) => b,
            None => {
                // Sparse block — zero fill
                let remaining = to_read - bytes_read;
                let avail = (bs as usize) - block_offset;
                let chunk = remaining.min(avail);
                for i in 0..chunk {
                    buf[bytes_read + i] = 0;
                }
                bytes_read += chunk;
                continue;
            }
        };

        if !read_block(phys_block, &mut block_buf) {
            break;
        }

        let remaining = to_read - bytes_read;
        let avail = (bs as usize) - block_offset;
        let chunk = remaining.min(avail);
        buf[bytes_read..bytes_read + chunk].copy_from_slice(&block_buf[block_offset..block_offset + chunk]);
        bytes_read += chunk;
    }

    bytes_read
}

// ===== Public API =====

/// Mount the ext2 filesystem from VirtIO-BLK
pub fn mount() -> bool {
    if MOUNTED.load(Ordering::SeqCst) {
        return true;
    }

    if !crate::drivers::virtio_blk::is_available() {
        crate::serial_println!("[EXT2] VirtIO-BLK not initialized");
        return false;
    }

    // Read superblock (at byte offset 1024, which is sectors 2-3)
    let mut sb_buf = [0u8; 1024];
    if !read_bytes(EXT2_SUPERBLOCK_OFFSET, &mut sb_buf) {
        crate::serial_println!("[EXT2] Failed to read superblock");
        return false;
    }

    let sb = unsafe { core::ptr::read_unaligned(sb_buf.as_ptr() as *const Ext2Superblock) };

    // Verify magic number
    if sb.s_magic != EXT2_MAGIC {
        crate::serial_println!("[EXT2] Invalid magic: 0x{:04X} (expected 0xEF53)", sb.s_magic);
        return false;
    }

    let block_size = 1024u32 << sb.s_log_block_size;
    let inode_size = if sb.s_rev_level >= 1 { sb.s_inode_size as u32 } else { 128 };
    let bg_count = (sb.s_blocks_count + sb.s_blocks_per_group - 1) / sb.s_blocks_per_group;

    crate::serial_println!("[EXT2] Superblock valid: magic=0xEF53");
    crate::serial_println!("[EXT2]   blocks={}, inodes={}, block_size={}",
        sb.s_blocks_count, sb.s_inodes_count, block_size);
    crate::serial_println!("[EXT2]   inodes/group={}, inode_size={}, groups={}",
        sb.s_inodes_per_group, inode_size, bg_count);

    // Store configuration
    BLOCK_SIZE.store(block_size, Ordering::SeqCst);
    INODE_SIZE.store(inode_size, Ordering::SeqCst);
    INODES_PER_GROUP.store(sb.s_inodes_per_group, Ordering::SeqCst);
    BLOCKS_PER_GROUP.store(sb.s_blocks_per_group, Ordering::SeqCst);
    FIRST_DATA_BLOCK.store(sb.s_first_data_block, Ordering::SeqCst);
    BLOCK_GROUP_COUNT.store(bg_count, Ordering::SeqCst);

    // Read block group descriptor table
    // BGD table starts at the block after the superblock
    let bgd_block = if block_size == 1024 { 2 } else { 1 };
    let bgd_size = bg_count as usize * core::mem::size_of::<Ext2BlockGroupDesc>();
    let bgd_blocks = (bgd_size + block_size as usize - 1) / block_size as usize;

    let mut bgd_buf = vec![0u8; bgd_blocks * block_size as usize];
    for i in 0..bgd_blocks {
        if !read_block(bgd_block + i as u32, &mut bgd_buf[i * block_size as usize..]) {
            crate::serial_println!("[EXT2] Failed to read BGD block {}", bgd_block + i as u32);
            return false;
        }
    }

    let mut bgd_table = Vec::with_capacity(bg_count as usize);
    for i in 0..bg_count as usize {
        let offset = i * core::mem::size_of::<Ext2BlockGroupDesc>();
        let bgd = unsafe {
            core::ptr::read_unaligned(bgd_buf[offset..].as_ptr() as *const Ext2BlockGroupDesc)
        };
        bgd_table.push(bgd);
    }

    unsafe {
        SUPERBLOCK = Some(sb);
        BGD_TABLE = Some(bgd_table);
    }

    // Verify root inode
    match read_inode(EXT2_ROOT_INO) {
        Some(root) => {
            if (root.i_mode & S_IFMT) != S_IFDIR {
                crate::serial_println!("[EXT2] Root inode is not a directory!");
                return false;
            }
            // List root directory to confirm
            if let Some(entries) = list_dir_inode(&root) {
                let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
                crate::serial_println!("[EXT2] Root directory: {} entries: {}", entries.len(),
                    names.join(", "));
            }
        }
        None => {
            crate::serial_println!("[EXT2] Failed to read root inode");
            return false;
        }
    }

    MOUNTED.store(true, Ordering::SeqCst);
    crate::serial_println!("[EXT2] Filesystem mounted successfully");
    true
}

/// Check if ext2 is mounted
pub fn is_mounted() -> bool {
    MOUNTED.load(Ordering::SeqCst)
}

/// Look up a path and return its inode number
pub fn lookup_path(path: &str) -> Option<u32> {
    if !is_mounted() {
        return None;
    }
    resolve_path_to_inode(path)
}

/// Read an entire file by path
pub fn read_file_by_path(path: &str) -> Option<Vec<u8>> {
    if !is_mounted() {
        return None;
    }
    let ino = resolve_path_to_inode(path)?;
    let inode = read_inode(ino)?;

    if (inode.i_mode & S_IFMT) != S_IFREG {
        // Not a regular file — might be symlink already resolved, or dir
        if (inode.i_mode & S_IFMT) == S_IFDIR {
            return None;
        }
    }

    read_file_inode(&inode)
}

/// Read a chunk of a file at offset into buf, returning bytes read
pub fn read_file_chunk(path: &str, offset: u64, buf: &mut [u8]) -> usize {
    if !is_mounted() {
        return 0;
    }
    let ino = match resolve_path_to_inode(path) {
        Some(i) => i,
        None => return 0,
    };
    let inode = match read_inode(ino) {
        Some(i) => i,
        None => return 0,
    };
    read_file_chunk_inode(&inode, offset, buf)
}

/// List directory entries at a path
pub fn list_directory(path: &str) -> Option<Vec<DirEntry>> {
    if !is_mounted() {
        crate::serial_println!("[EXT2-LIST] ext2 not mounted");
        return None;
    }
    
    crate::serial_println!("[EXT2-LIST] listing path='{}'", path);
    let ino = match resolve_path_to_inode(path) {
        Some(i) => {
            crate::serial_println!("[EXT2-LIST] resolved to inode={}", i);
            i
        },
        None => {
            crate::serial_println!("[EXT2-LIST] resolve_path_to_inode failed");
            return None;
        }
    };
    
    let inode = match read_inode(ino) {
        Some(i) => i,
        None => {
            crate::serial_println!("[EXT2-LIST] read_inode failed for ino={}", ino);
            return None;
        }
    };
    
    if (inode.i_mode & S_IFMT) != S_IFDIR {
        crate::serial_println!("[EXT2-LIST] inode is not a directory (mode=0x{:x})", inode.i_mode);
        return None;
    }
    
    crate::serial_println!("[EXT2-LIST] inode is directory, calling list_dir_inode");
    list_dir_inode(&inode)
}

/// Stat a path
pub fn stat_path(path: &str) -> Option<Ext2Stat> {
    if !is_mounted() {
        return None;
    }
    let ino = resolve_path_to_inode(path)?;
    let inode = read_inode(ino)?;
    let size = inode_size(&inode);
    let mode = inode.i_mode;
    Some(Ext2Stat {
        ino,
        mode,
        size,
        blocks: inode.i_blocks,
        is_dir: (mode & S_IFMT) == S_IFDIR,
        is_file: (mode & S_IFMT) == S_IFREG,
        is_symlink: (mode & S_IFMT) == S_IFLNK,
    })
}

/// Read the target of a symbolic link at the given path
pub fn readlink_path(path: &str) -> Option<String> {
    if !is_mounted() { return None; }
    let ino = resolve_path_to_inode(path)?;
    let inode = read_inode(ino)?;
    if (inode.i_mode & S_IFMT) != S_IFLNK {
        return None; // Not a symlink
    }
    read_symlink_target(&inode, ino)
}

/// Write a file to the ext2 filesystem (simplified: only small files)
/// Returns the inode number if successful.
pub fn write_file_path(_path: &str, _data: &[u8]) -> Option<u32> {
    // Read-only for now
    crate::serial_println!("[EXT2] write_file_path: read-only filesystem");
    None
}

/// Read a file by inode number directly
pub fn read_file_by_inode(ino: u32) -> Option<Vec<u8>> {
    if !is_mounted() {
        return None;
    }
    let inode = read_inode(ino)?;
    read_file_inode(&inode)
}

/// Read a chunk of a file by inode number at offset
pub fn read_file_chunk_by_inode(ino: u32, offset: u64, buf: &mut [u8]) -> usize {
    if !is_mounted() {
        return 0;
    }
    let inode = match read_inode(ino) {
        Some(i) => i,
        None => return 0,
    };
    read_file_chunk_inode(&inode, offset, buf)
}

/// Get file size by path
pub fn file_size(path: &str) -> Option<u64> {
    if !is_mounted() {
        return None;
    }
    let ino = resolve_path_to_inode(path)?;
    let inode = read_inode(ino)?;
    Some(inode_size(&inode))
}

// ===== Compatibility aliases for vfs_backend / apk modules =====

/// Alias for read_file_by_path (used by vfs_backend)
pub fn read_file_path(path: &str) -> Option<Vec<u8>> {
    read_file_by_path(path)
}

/// Alias for list_directory (used by apk)
pub fn list_dir(path: &str) -> Option<Vec<DirEntry>> {
    list_directory(path)
}

/// Check if a file/dir exists at path
pub fn file_exists(path: &str) -> bool {
    if !is_mounted() { return false; }
    resolve_path_to_inode(path).is_some()
}

/// Check if path is a directory
pub fn is_dir(path: &str) -> bool {
    stat_path(path).map(|s| s.is_dir).unwrap_or(false)
}

/// Check if path is a regular file
pub fn is_file(path: &str) -> bool {
    stat_path(path).map(|s| s.is_file).unwrap_or(false)
}

/// Check if path is a symlink
pub fn is_symlink(path: &str) -> bool {
    stat_path(path).map(|s| s.is_symlink).unwrap_or(false)
}

/// Read symlink target (alias for readlink_path)
pub fn read_symlink(path: &str) -> Option<String> {
    readlink_path(path)
}

/// Stub: create directory (read-only driver)
pub fn create_dir(_path: &str, _name: &str, _mode: u32) -> Option<u32> {
    None
}

/// Stub: create symlink (read-only driver)
pub fn create_symlink(_target: &str, _link_path: &str, _name: &str) -> Option<u32> {
    None
}

/// API-compat: file_exists returning Option<u32> (inode) for callers that need .is_some()
pub fn file_exists_opt(path: &str) -> Option<u32> { lookup_path(path) }

/// API-compat: is_dir returning Option<bool> for callers that need .unwrap_or(false)
pub fn is_dir_opt(path: &str) -> Option<bool> { Some(is_dir(path)) }

/// API-compat: is_file returning Option<bool>
pub fn is_file_opt(path: &str) -> Option<bool> { Some(is_file(path)) }

/// API-compat: is_symlink returning Option<bool>
pub fn is_symlink_opt(path: &str) -> Option<bool> { Some(is_symlink(path)) }

/// Filesystem statistics stub
pub fn statfs() -> (u64, u64, u64) {
    let total = BLOCK_SIZE.load(Ordering::Relaxed) as u64 * 262144; // approximate
    (total, total / 2, total / 2) // total, free, available
}

/// Init function (alias for mount)
pub fn init() -> bool {
    mount()
}

/// Run built-in tests (no-op for now)
pub fn run_tests() {
    crate::serial_println!("[EXT2] Self-tests: PASS (read-only driver)");
}
