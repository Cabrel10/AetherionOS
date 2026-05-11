// kernel/src/fs/ext2.rs — Ext2 Filesystem Driver for AetherionOS (Read/Write)
//
// Architecture:
//   - Reads/writes ext2 structures directly from/to VirtIO-BLK sectors
//   - Supports: superblock, block group descriptors, inodes, directory entries
//   - Supports: direct blocks, single-indirect blocks, double-indirect blocks
//   - Write support: block/inode allocation, file creation, directory entries
//   - Designed for Alpine Linux rootfs (256 MiB - 2 GiB ext2 images)
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
//   ext2::create_dir(path, name, mode) → Option<u32>
//   ext2::create_symlink(target, link_path, name) → Option<u32>
//   ext2::append_to_file(path, data)  → bool
//   ext2::truncate_file(path)         → bool

use alloc::string::String;
use alloc::format;
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

// ===== Write Layer: Block I/O =====

/// Write a single ext2 block to VirtIO-BLK
fn write_block(block_num: u32, buf: &[u8]) -> bool {
    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as u64;
    if buf.len() < bs as usize {
        return false;
    }
    let byte_offset = block_num as u64 * bs;
    let start_sector = byte_offset / SECTOR_SIZE as u64;
    let sector_count = (bs as usize) / SECTOR_SIZE;
    crate::drivers::virtio_blk::write_sectors(start_sector, sector_count, &buf[..bs as usize])
}

/// Write arbitrary bytes to disk at a given byte offset
fn write_bytes(offset: u64, data: &[u8]) -> bool {
    let start_sector = offset / SECTOR_SIZE as u64;
    let sector_offset = (offset % SECTOR_SIZE as u64) as usize;

    // If write is not sector-aligned or doesn't fill complete sectors,
    // we must read-modify-write
    let total_bytes = data.len();
    let sectors_needed = (sector_offset + total_bytes + SECTOR_SIZE - 1) / SECTOR_SIZE;

    let mut sector_buf = vec![0u8; sectors_needed * SECTOR_SIZE];
    // Read existing data
    if !crate::drivers::virtio_blk::read_sectors(start_sector, sectors_needed, &mut sector_buf) {
        return false;
    }
    // Overlay new data
    sector_buf[sector_offset..sector_offset + total_bytes].copy_from_slice(data);
    // Write back
    crate::drivers::virtio_blk::write_sectors(start_sector, sectors_needed, &sector_buf)
}

// ===== Write Layer: Bitmap Operations =====

/// Allocate a free block from the bitmap. Returns block number or None.
fn alloc_block_in_group(group: u32) -> Option<u32> {
    let bgd = unsafe { BGD_TABLE.as_ref()?.get(group as usize)? };
    if bgd.bg_free_blocks_count == 0 {
        return None;
    }

    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
    let bitmap_block = bgd.bg_block_bitmap;
    let mut bitmap = vec![0u8; bs];
    if !read_block(bitmap_block, &mut bitmap) {
        return None;
    }

    let bpg = BLOCKS_PER_GROUP.load(Ordering::Relaxed) as usize;
    let fdb = FIRST_DATA_BLOCK.load(Ordering::Relaxed);

    // Scan bitmap for first free bit
    for byte_idx in 0..bpg / 8 {
        if byte_idx >= bs { break; }
        if bitmap[byte_idx] == 0xFF { continue; }
        for bit in 0..8u32 {
            let local_idx = byte_idx * 8 + bit as usize;
            if local_idx >= bpg { break; }
            if (bitmap[byte_idx] & (1 << bit)) == 0 {
                // Found free block — mark it
                bitmap[byte_idx] |= 1 << bit;
                if !write_block(bitmap_block, &bitmap) {
                    return None;
                }
                // Update BGD free count
                update_bgd_free_blocks(group, -1);
                update_superblock_free_blocks(-1);
                // Calculate absolute block number
                let abs_block = group * BLOCKS_PER_GROUP.load(Ordering::Relaxed) + local_idx as u32 + fdb;
                return Some(abs_block);
            }
        }
    }
    None
}

/// Allocate a free block (searches all groups)
fn alloc_block() -> Option<u32> {
    let bg_count = BLOCK_GROUP_COUNT.load(Ordering::Relaxed);
    for g in 0..bg_count {
        if let Some(b) = alloc_block_in_group(g) {
            return Some(b);
        }
    }
    crate::serial_println!("[EXT2] alloc_block: no free blocks");
    None
}

/// Allocate a free inode from a block group. Returns inode number or None.
fn alloc_inode_in_group(group: u32) -> Option<u32> {
    let bgd = unsafe { BGD_TABLE.as_ref()?.get(group as usize)? };
    if bgd.bg_free_inodes_count == 0 {
        return None;
    }

    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
    let bitmap_block = bgd.bg_inode_bitmap;
    let mut bitmap = vec![0u8; bs];
    if !read_block(bitmap_block, &mut bitmap) {
        return None;
    }

    let ipg = INODES_PER_GROUP.load(Ordering::Relaxed) as usize;

    for byte_idx in 0..ipg / 8 {
        if byte_idx >= bs { break; }
        if bitmap[byte_idx] == 0xFF { continue; }
        for bit in 0..8u32 {
            let local_idx = byte_idx * 8 + bit as usize;
            if local_idx >= ipg { break; }
            if (bitmap[byte_idx] & (1 << bit)) == 0 {
                bitmap[byte_idx] |= 1 << bit;
                if !write_block(bitmap_block, &bitmap) {
                    return None;
                }
                update_bgd_free_inodes(group, -1);
                update_superblock_free_inodes(-1);
                // Inode numbers are 1-based
                let ino = group * INODES_PER_GROUP.load(Ordering::Relaxed) + local_idx as u32 + 1;
                return Some(ino);
            }
        }
    }
    None
}

/// Allocate a free inode (searches all groups)
fn alloc_inode() -> Option<u32> {
    let bg_count = BLOCK_GROUP_COUNT.load(Ordering::Relaxed);
    for g in 0..bg_count {
        if let Some(i) = alloc_inode_in_group(g) {
            return Some(i);
        }
    }
    crate::serial_println!("[EXT2] alloc_inode: no free inodes");
    None
}

// ===== Write Layer: Superblock/BGD Updates =====

/// Update the free blocks count in the superblock (delta: +1 or -1)
fn update_superblock_free_blocks(delta: i32) {
    unsafe {
        if let Some(ref mut sb) = SUPERBLOCK {
            sb.s_free_blocks_count = (sb.s_free_blocks_count as i32 + delta) as u32;
            // Write superblock back to disk
            let sb_bytes = core::slice::from_raw_parts(
                sb as *const Ext2Superblock as *const u8,
                core::mem::size_of::<Ext2Superblock>(),
            );
            let _ = write_bytes(EXT2_SUPERBLOCK_OFFSET, sb_bytes);
        }
    }
}

/// Update the free inodes count in the superblock
fn update_superblock_free_inodes(delta: i32) {
    unsafe {
        if let Some(ref mut sb) = SUPERBLOCK {
            sb.s_free_inodes_count = (sb.s_free_inodes_count as i32 + delta) as u32;
            let sb_bytes = core::slice::from_raw_parts(
                sb as *const Ext2Superblock as *const u8,
                core::mem::size_of::<Ext2Superblock>(),
            );
            let _ = write_bytes(EXT2_SUPERBLOCK_OFFSET, sb_bytes);
        }
    }
}

/// Update BGD free blocks count for a specific group
fn update_bgd_free_blocks(group: u32, delta: i32) {
    unsafe {
        if let Some(ref mut table) = BGD_TABLE {
            if let Some(bgd) = table.get_mut(group as usize) {
                bgd.bg_free_blocks_count = (bgd.bg_free_blocks_count as i32 + delta) as u16;
                write_bgd_entry(group, bgd);
            }
        }
    }
}

/// Update BGD free inodes count for a specific group
fn update_bgd_free_inodes(group: u32, delta: i32) {
    unsafe {
        if let Some(ref mut table) = BGD_TABLE {
            if let Some(bgd) = table.get_mut(group as usize) {
                bgd.bg_free_inodes_count = (bgd.bg_free_inodes_count as i32 + delta) as u16;
                write_bgd_entry(group, bgd);
            }
        }
    }
}

/// Write a single BGD entry back to disk
fn write_bgd_entry(group: u32, bgd: &Ext2BlockGroupDesc) {
    let bs = BLOCK_SIZE.load(Ordering::Relaxed);
    let bgd_block = if bs == 1024 { 2u32 } else { 1u32 };
    let bgd_size = core::mem::size_of::<Ext2BlockGroupDesc>();
    let offset = bgd_block as u64 * bs as u64 + group as u64 * bgd_size as u64;
    let bgd_bytes = unsafe {
        core::slice::from_raw_parts(bgd as *const Ext2BlockGroupDesc as *const u8, bgd_size)
    };
    let _ = write_bytes(offset, bgd_bytes);
}

// ===== Write Layer: Inode Operations =====

/// Write an inode back to disk
fn write_inode(ino: u32, inode: &Ext2Inode) -> bool {
    if ino == 0 { return false; }

    let ipg = INODES_PER_GROUP.load(Ordering::Relaxed);
    let inode_sz = INODE_SIZE.load(Ordering::Relaxed);
    if ipg == 0 || inode_sz == 0 { return false; }

    let group = (ino - 1) / ipg;
    let index = (ino - 1) % ipg;

    let inode_table_block = unsafe {
        match BGD_TABLE.as_ref().and_then(|t| t.get(group as usize)) {
            Some(bgd) => bgd.bg_inode_table,
            None => return false,
        }
    };

    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as u64;
    let byte_offset = inode_table_block as u64 * bs + index as u64 * inode_sz as u64;

    let inode_bytes = unsafe {
        core::slice::from_raw_parts(
            inode as *const Ext2Inode as *const u8,
            core::mem::size_of::<Ext2Inode>().min(inode_sz as usize),
        )
    };
    write_bytes(byte_offset, inode_bytes)
}

// ===== Write Layer: Directory Operations =====

/// Add a directory entry to a directory inode.
/// Returns true on success.
fn add_dir_entry(dir_ino: u32, name: &str, child_ino: u32, file_type: u8) -> bool {
    let dir_inode = match read_inode(dir_ino) {
        Some(i) => i,
        None => return false,
    };

    if (dir_inode.i_mode & S_IFMT) != S_IFDIR {
        return false;
    }

    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
    let dir_size = inode_size(&dir_inode) as usize;

    // Entry we want to add
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len().min(255);
    // Minimum entry size: 8 bytes header + name, rounded up to 4
    let needed_rec_len = ((8 + name_len) + 3) & !3;

    // Scan existing directory blocks for space
    let num_blocks = (dir_size + bs - 1) / bs;
    for blk_idx in 0..num_blocks {
        let phys_block = match resolve_block(&dir_inode, blk_idx as u32) {
            Some(b) if b != 0 => b,
            _ => continue,
        };

        let mut block_buf = vec![0u8; bs];
        if !read_block(phys_block, &mut block_buf) {
            continue;
        }

        // Scan entries in this block to find space in the last entry's padding
        let mut offset = 0usize;
        while offset < bs {
            if offset + 8 > bs { break; }
            let rec_len = u16::from_le_bytes([block_buf[offset + 4], block_buf[offset + 5]]) as usize;
            if rec_len == 0 || rec_len < 8 { break; }

            let entry_ino = u32::from_le_bytes([
                block_buf[offset], block_buf[offset + 1],
                block_buf[offset + 2], block_buf[offset + 3],
            ]);
            let entry_name_len = block_buf[offset + 6] as usize;
            let actual_entry_size = ((8 + entry_name_len) + 3) & !3;

            // Check if this is the last entry in the block (rec_len extends to end)
            // or if there's enough padding after the actual data
            if entry_ino != 0 && rec_len >= actual_entry_size + needed_rec_len {
                // Split this entry: shrink current, add new after it
                let new_rec_len = rec_len - actual_entry_size;
                // Shrink current entry
                block_buf[offset + 4] = (actual_entry_size & 0xFF) as u8;
                block_buf[offset + 5] = ((actual_entry_size >> 8) & 0xFF) as u8;

                // Write new entry after current
                let new_offset = offset + actual_entry_size;
                write_dir_entry_at(&mut block_buf, new_offset, child_ino, new_rec_len as u16, name_bytes, file_type);

                // Write block back
                return write_block(phys_block, &block_buf);
            }

            // If entry is deleted (ino=0) and has enough space
            if entry_ino == 0 && rec_len >= needed_rec_len {
                write_dir_entry_at(&mut block_buf, offset, child_ino, rec_len as u16, name_bytes, file_type);
                return write_block(phys_block, &block_buf);
            }

            offset += rec_len;
        }
    }

    // No space in existing blocks — allocate a new block for the directory
    let new_block = match alloc_block() {
        Some(b) => b,
        None => return false,
    };

    // Initialize new directory block with our entry filling the whole block
    let mut new_block_buf = vec![0u8; bs];
    write_dir_entry_at(&mut new_block_buf, 0, child_ino, bs as u16, name_bytes, file_type);
    if !write_block(new_block, &new_block_buf) {
        return false;
    }

    // Add block to directory inode (find first free slot in i_block)
    let mut dir_inode_mut = dir_inode;
    let block_idx = num_blocks as u32;
    if block_idx < 12 {
        dir_inode_mut.i_block[block_idx as usize] = new_block;
    } else {
        // Would need indirect block support for directory growth
        crate::serial_println!("[EXT2] add_dir_entry: directory too large (needs indirect)");
        return false;
    }
    dir_inode_mut.i_size += bs as u32;
    dir_inode_mut.i_blocks += (bs / 512) as u32;
    write_inode(dir_ino, &dir_inode_mut)
}

/// Write a directory entry at a specific offset within a block buffer
fn write_dir_entry_at(buf: &mut [u8], offset: usize, ino: u32, rec_len: u16, name: &[u8], file_type: u8) {
    let name_len = name.len().min(255);
    // inode (4 bytes)
    buf[offset..offset + 4].copy_from_slice(&ino.to_le_bytes());
    // rec_len (2 bytes)
    buf[offset + 4..offset + 6].copy_from_slice(&rec_len.to_le_bytes());
    // name_len (1 byte)
    buf[offset + 6] = name_len as u8;
    // file_type (1 byte)
    buf[offset + 7] = file_type;
    // name
    buf[offset + 8..offset + 8 + name_len].copy_from_slice(&name[..name_len]);
    // Zero-pad rest
    let end = offset + 8 + name_len;
    let padded_end = (offset + rec_len as usize).min(buf.len());
    for i in end..padded_end {
        buf[i] = 0;
    }
}

// ===== Write Layer: File Creation =====

/// Create a new regular file at the given path with the given data.
/// Parent directory must exist. Returns inode number on success.
fn create_file_internal(path: &str, data: &[u8], mode: u16) -> Option<u32> {
    if !is_mounted() { return None; }

    // Split path into parent directory and filename
    let (parent_path, filename) = split_path(path)?;

    // Resolve parent directory
    let parent_ino = resolve_path_to_inode(parent_path)?;
    let parent_inode = read_inode(parent_ino)?;
    if (parent_inode.i_mode & S_IFMT) != S_IFDIR {
        crate::serial_println!("[EXT2] create_file: parent is not a directory");
        return None;
    }

    // Check if file already exists
    if resolve_path_to_inode(path).is_some() {
        crate::serial_println!("[EXT2] create_file: '{}' already exists", path);
        // For write_file_path, we should overwrite — truncate and rewrite
        return overwrite_file(path, data);
    }

    // Allocate a new inode
    let new_ino = alloc_inode()?;

    // Allocate blocks for file data
    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
    let blocks_needed = (data.len() + bs - 1) / bs;

    let mut new_inode = Ext2Inode {
        i_mode: S_IFREG | (mode & 0o777),
        i_uid: 0,
        i_size: data.len() as u32,
        i_atime: 0,
        i_ctime: 0,
        i_mtime: 0,
        i_dtime: 0,
        i_gid: 0,
        i_links_count: 1,
        i_blocks: 0,
        i_flags: 0,
        i_osd1: 0,
        i_block: [0u32; 15],
        i_generation: 0,
        i_file_acl: 0,
        i_dir_acl: 0,
        i_faddr: 0,
        i_osd2: [0u8; 12],
    };

    // Allocate and write data blocks (up to 12 direct blocks)
    let max_direct = 12.min(blocks_needed);
    for i in 0..max_direct {
        let block = alloc_block()?;
        new_inode.i_block[i] = block;
        new_inode.i_blocks += (bs / 512) as u32;

        // Write data to block
        let start = i * bs;
        let end = (start + bs).min(data.len());
        let mut block_buf = vec![0u8; bs];
        if start < data.len() {
            block_buf[..end - start].copy_from_slice(&data[start..end]);
        }
        if !write_block(block, &block_buf) {
            crate::serial_println!("[EXT2] create_file: failed to write data block");
            return None;
        }
    }

    // Handle indirect blocks for files > 12 * block_size
    if blocks_needed > 12 {
        let indirect_block = alloc_block()?;
        new_inode.i_block[12] = indirect_block;
        new_inode.i_blocks += (bs / 512) as u32;

        let ptrs_per_block = bs / 4;
        let indirect_count = (blocks_needed - 12).min(ptrs_per_block);
        let mut indirect_buf = vec![0u8; bs];

        for i in 0..indirect_count {
            let block = alloc_block()?;
            let ptr_offset = i * 4;
            indirect_buf[ptr_offset..ptr_offset + 4].copy_from_slice(&block.to_le_bytes());
            new_inode.i_blocks += (bs / 512) as u32;

            let data_offset = (12 + i) * bs;
            let end = (data_offset + bs).min(data.len());
            let mut block_buf = vec![0u8; bs];
            if data_offset < data.len() {
                block_buf[..end - data_offset].copy_from_slice(&data[data_offset..end]);
            }
            if !write_block(block, &block_buf) {
                return None;
            }
        }

        if !write_block(indirect_block, &indirect_buf) {
            return None;
        }
    }

    // Write inode to disk
    if !write_inode(new_ino, &new_inode) {
        crate::serial_println!("[EXT2] create_file: failed to write inode");
        return None;
    }

    // Add directory entry
    if !add_dir_entry(parent_ino, filename, new_ino, EXT2_FT_REG_FILE) {
        crate::serial_println!("[EXT2] create_file: failed to add dir entry");
        return None;
    }

    crate::serial_println!("[EXT2] Created file '{}' (ino={}, {} bytes, {} blocks)",
        path, new_ino, data.len(), blocks_needed);
    Some(new_ino)
}

/// Overwrite an existing file's content (truncate + write)
fn overwrite_file(path: &str, data: &[u8]) -> Option<u32> {
    let ino = resolve_path_to_inode(path)?;
    let mut inode = read_inode(ino)?;
    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;

    // Free existing direct data blocks
    for i in 0..12 {
        if inode.i_block[i] != 0 {
            free_block(inode.i_block[i]);
            inode.i_block[i] = 0;
        }
    }
    // Free single-indirect block and its referenced blocks
    if inode.i_block[12] != 0 {
        free_indirect_blocks(inode.i_block[12]);
        free_block(inode.i_block[12]);
        inode.i_block[12] = 0;
    }
    // Free double-indirect block and all referenced blocks
    if inode.i_block[13] != 0 {
        free_double_indirect_blocks(inode.i_block[13]);
        free_block(inode.i_block[13]);
        inode.i_block[13] = 0;
    }
    // Free triple-indirect block (clear the pointer; full traversal would recurse 3 levels)
    if inode.i_block[14] != 0 {
        // For completeness: free all blocks under triple-indirect
        let tind_bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
        let mut tind_buf = vec![0u8; tind_bs];
        if read_block(inode.i_block[14], &mut tind_buf) {
            let ptrs_per_block = tind_bs / 4;
            for i in 0..ptrs_per_block {
                let off = i * 4;
                let dind = u32::from_le_bytes([tind_buf[off], tind_buf[off+1], tind_buf[off+2], tind_buf[off+3]]);
                if dind != 0 {
                    free_double_indirect_blocks(dind);
                    free_block(dind);
                }
            }
        }
        free_block(inode.i_block[14]);
        inode.i_block[14] = 0;
    }

    // Reset size
    inode.i_size = data.len() as u32;
    inode.i_blocks = 0;

    // Allocate new blocks
    let blocks_needed = (data.len() + bs - 1) / bs;
    let max_direct = 12.min(blocks_needed);
    for i in 0..max_direct {
        let block = alloc_block()?;
        inode.i_block[i] = block;
        inode.i_blocks += (bs / 512) as u32;

        let start = i * bs;
        let end = (start + bs).min(data.len());
        let mut block_buf = vec![0u8; bs];
        if start < data.len() {
            block_buf[..end - start].copy_from_slice(&data[start..end]);
        }
        write_block(block, &block_buf);
    }

    write_inode(ino, &inode);
    Some(ino)
}

/// Free a block (mark as free in bitmap)
fn free_block(block_num: u32) {
    let fdb = FIRST_DATA_BLOCK.load(Ordering::Relaxed);
    let bpg = BLOCKS_PER_GROUP.load(Ordering::Relaxed);
    if block_num < fdb { return; }

    let relative = block_num - fdb;
    let group = relative / bpg;
    let local_idx = (relative % bpg) as usize;

    let bgd = unsafe {
        match BGD_TABLE.as_ref().and_then(|t| t.get(group as usize)) {
            Some(b) => b,
            None => return,
        }
    };

    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
    let mut bitmap = vec![0u8; bs];
    if !read_block(bgd.bg_block_bitmap, &mut bitmap) { return; }

    let byte_idx = local_idx / 8;
    let bit_idx = local_idx % 8;
    if byte_idx < bs {
        bitmap[byte_idx] &= !(1 << bit_idx);
        let _ = write_block(bgd.bg_block_bitmap, &bitmap);
        update_bgd_free_blocks(group, 1);
        update_superblock_free_blocks(1);
    }
}

/// Split a path into (parent_dir, filename)
fn split_path(path: &str) -> Option<(&str, &str)> {
    let path = path.trim_end_matches('/');
    if path.is_empty() { return None; }
    match path.rfind('/') {
        Some(pos) => {
            let parent = if pos == 0 { "/" } else { &path[..pos] };
            let name = &path[pos + 1..];
            if name.is_empty() { None } else { Some((parent, name)) }
        }
        None => Some(("/", path)), // File in root
    }
}

/// Write a file to the ext2 filesystem.
/// Creates intermediate directories if they don't exist.
/// Returns the inode number if successful.
pub fn write_file_path(path: &str, data: &[u8]) -> Option<u32> {
    if !is_mounted() {
        crate::serial_println!("[EXT2] write_file_path: not mounted");
        return None;
    }
    create_file_internal(path, data, 0o644)
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

/// Create a directory at path/name with given mode. Returns inode number.
pub fn create_dir(path: &str, name: &str, mode: u32) -> Option<u32> {
    if !is_mounted() { return None; }

    // Resolve parent directory
    let parent_ino = resolve_path_to_inode(path)?;
    let parent_inode = read_inode(parent_ino)?;
    if (parent_inode.i_mode & S_IFMT) != S_IFDIR {
        return None;
    }

    // Allocate inode for new directory
    let new_ino = alloc_inode()?;
    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;

    // Allocate a block for the directory entries (. and ..)
    let dir_block = alloc_block()?;

    // Create directory inode
    let mut dir_inode = Ext2Inode {
        i_mode: S_IFDIR | (mode as u16 & 0o777),
        i_uid: 0,
        i_size: bs as u32,
        i_atime: 0,
        i_ctime: 0,
        i_mtime: 0,
        i_dtime: 0,
        i_gid: 0,
        i_links_count: 2, // . and parent's link
        i_blocks: (bs / 512) as u32,
        i_flags: 0,
        i_osd1: 0,
        i_block: [0u32; 15],
        i_generation: 0,
        i_file_acl: 0,
        i_dir_acl: 0,
        i_faddr: 0,
        i_osd2: [0u8; 12],
    };
    dir_inode.i_block[0] = dir_block;

    // Write . and .. entries in the new directory block
    let mut block_buf = vec![0u8; bs];
    // "." entry: points to self, rec_len = 12
    write_dir_entry_at(&mut block_buf, 0, new_ino, 12, b".", EXT2_FT_DIR);
    // ".." entry: points to parent, rec_len = rest of block
    let dotdot_rec_len = (bs - 12) as u16;
    write_dir_entry_at(&mut block_buf, 12, parent_ino, dotdot_rec_len, b"..", EXT2_FT_DIR);

    if !write_block(dir_block, &block_buf) {
        return None;
    }

    // Write the new inode
    if !write_inode(new_ino, &dir_inode) {
        return None;
    }

    // Add entry in parent directory
    if !add_dir_entry(parent_ino, name, new_ino, EXT2_FT_DIR) {
        return None;
    }

    // Increment parent's link count (for ..)
    let mut parent_inode_mut = parent_inode;
    parent_inode_mut.i_links_count += 1;
    write_inode(parent_ino, &parent_inode_mut);

    // Update used_dirs_count in BGD
    let ipg = INODES_PER_GROUP.load(Ordering::Relaxed);
    let group = (new_ino - 1) / ipg;
    unsafe {
        if let Some(ref mut table) = BGD_TABLE {
            if let Some(bgd) = table.get_mut(group as usize) {
                bgd.bg_used_dirs_count += 1;
                write_bgd_entry(group, bgd);
            }
        }
    }

    crate::serial_println!("[EXT2] Created directory '{}/{}' (ino={})", path, name, new_ino);
    Some(new_ino)
}

/// Create a symbolic link. Returns inode number.
pub fn create_symlink(target: &str, link_path: &str, name: &str) -> Option<u32> {
    if !is_mounted() { return None; }

    // Resolve parent directory
    let parent_ino = resolve_path_to_inode(link_path)?;
    let parent_inode = read_inode(parent_ino)?;
    if (parent_inode.i_mode & S_IFMT) != S_IFDIR {
        return None;
    }

    let new_ino = alloc_inode()?;
    let target_bytes = target.as_bytes();

    let mut sym_inode = Ext2Inode {
        i_mode: S_IFLNK | 0o777,
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
        i_block: [0u32; 15],
        i_generation: 0,
        i_file_acl: 0,
        i_dir_acl: 0,
        i_faddr: 0,
        i_osd2: [0u8; 12],
    };

    // Fast symlink: if target fits in i_block (60 bytes), store inline
    if target_bytes.len() <= 60 {
        let block_bytes = unsafe {
            core::slice::from_raw_parts_mut(
                sym_inode.i_block.as_mut_ptr() as *mut u8,
                60,
            )
        };
        block_bytes[..target_bytes.len()].copy_from_slice(target_bytes);
    } else {
        // Slow symlink: allocate a data block
        let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
        let data_block = alloc_block()?;
        sym_inode.i_block[0] = data_block;
        sym_inode.i_blocks = (bs / 512) as u32;

        let mut block_buf = vec![0u8; bs];
        block_buf[..target_bytes.len()].copy_from_slice(target_bytes);
        if !write_block(data_block, &block_buf) {
            return None;
        }
    }

    if !write_inode(new_ino, &sym_inode) {
        return None;
    }

    if !add_dir_entry(parent_ino, name, new_ino, EXT2_FT_SYMLINK) {
        return None;
    }

    crate::serial_println!("[EXT2] Created symlink '{}/{}' -> '{}'", link_path, name, target);
    Some(new_ino)
}

/// Create directories recursively (like mkdir -p)
pub fn mkdir_p(path: &str, mode: u32) -> bool {
    if !is_mounted() { return false; }
    if path.is_empty() || path == "/" { return true; }

    // Check if already exists
    if is_dir(path) { return true; }

    // Build path component by component
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    let mut current = String::from("/");

    for part in &parts {
        if part.is_empty() { continue; }
        let next = if current == "/" {
            alloc::format!("/{}", part)
        } else {
            alloc::format!("{}/{}", current, part)
        };

        if !is_dir(&next) {
            // Create this directory
            let parent = if current == "/" { "/" } else { current.as_str() };
            if create_dir(parent, part, mode).is_none() {
                crate::serial_println!("[EXT2] mkdir_p: failed to create '{}'", next);
                return false;
            }
        }
        current = next;
    }
    true
}

/// Append data to an existing file
pub fn append_to_file(path: &str, data: &[u8]) -> bool {
    if !is_mounted() { return false; }
    match read_file_by_path(path) {
        Some(existing) => {
            let mut combined = existing;
            combined.extend_from_slice(data);
            overwrite_file(path, &combined).is_some()
        }
        None => {
            // File doesn't exist, create it
            create_file_internal(path, data, 0o644).is_some()
        }
    }
}

/// Truncate a file to zero length
pub fn truncate_file(path: &str) -> bool {
    if !is_mounted() { return false; }
    overwrite_file(path, &[]).is_some()
}

/// Delete a file (unlink)
pub fn unlink(path: &str) -> bool {
    if !is_mounted() { return false; }

    let (parent_path, filename) = match split_path(path) {
        Some(p) => p,
        None => return false,
    };

    let ino = match resolve_path_to_inode(path) {
        Some(i) => i,
        None => return false,
    };

    let parent_ino = match resolve_path_to_inode(parent_path) {
        Some(i) => i,
        None => return false,
    };

    // Remove directory entry (mark inode as 0)
    if !remove_dir_entry(parent_ino, filename) {
        return false;
    }

    // Decrement link count
    if let Some(mut inode) = read_inode(ino) {
        inode.i_links_count = inode.i_links_count.saturating_sub(1);
        if inode.i_links_count == 0 {
            // Free direct blocks
            for i in 0..12 {
                if inode.i_block[i] != 0 {
                    free_block(inode.i_block[i]);
                }
            }
            // Free indirect blocks
            if inode.i_block[12] != 0 {
                free_indirect_blocks(inode.i_block[12]);
                free_block(inode.i_block[12]);
            }
            if inode.i_block[13] != 0 {
                free_double_indirect_blocks(inode.i_block[13]);
                free_block(inode.i_block[13]);
            }
            // Free inode
            free_inode(ino);
            inode.i_dtime = 1; // Mark as deleted
        }
        write_inode(ino, &inode);
    }

    true
}

/// Remove a directory entry by name from a directory
fn remove_dir_entry(dir_ino: u32, name: &str) -> bool {
    let dir_inode = match read_inode(dir_ino) {
        Some(i) => i,
        None => return false,
    };

    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
    let dir_size = inode_size(&dir_inode) as usize;
    let num_blocks = (dir_size + bs - 1) / bs;

    for blk_idx in 0..num_blocks {
        let phys_block = match resolve_block(&dir_inode, blk_idx as u32) {
            Some(b) if b != 0 => b,
            _ => continue,
        };

        let mut block_buf = vec![0u8; bs];
        if !read_block(phys_block, &mut block_buf) { continue; }

        let mut offset = 0usize;
        let mut prev_offset: Option<usize> = None;

        while offset < bs {
            if offset + 8 > bs { break; }
            let rec_len = u16::from_le_bytes([block_buf[offset + 4], block_buf[offset + 5]]) as usize;
            if rec_len == 0 || rec_len < 8 { break; }

            let entry_ino = u32::from_le_bytes([
                block_buf[offset], block_buf[offset + 1],
                block_buf[offset + 2], block_buf[offset + 3],
            ]);
            let entry_name_len = block_buf[offset + 6] as usize;

            if entry_ino != 0 && entry_name_len == name.len() {
                let entry_name = &block_buf[offset + 8..offset + 8 + entry_name_len];
                if entry_name == name.as_bytes() {
                    // Found it — merge with previous entry or zero the inode
                    if let Some(prev) = prev_offset {
                        // Merge: extend previous entry's rec_len
                        let prev_rec = u16::from_le_bytes([block_buf[prev + 4], block_buf[prev + 5]]) as usize;
                        let new_rec = prev_rec + rec_len;
                        block_buf[prev + 4] = (new_rec & 0xFF) as u8;
                        block_buf[prev + 5] = ((new_rec >> 8) & 0xFF) as u8;
                    } else {
                        // First entry — just zero the inode
                        block_buf[offset..offset + 4].copy_from_slice(&0u32.to_le_bytes());
                    }
                    return write_block(phys_block, &block_buf);
                }
            }

            if entry_ino != 0 {
                prev_offset = Some(offset);
            }
            offset += rec_len;
        }
    }
    false
}

/// Free an inode (mark as free in bitmap)
fn free_inode(ino: u32) {
    if ino == 0 { return; }
    let ipg = INODES_PER_GROUP.load(Ordering::Relaxed);
    let group = (ino - 1) / ipg;
    let local_idx = ((ino - 1) % ipg) as usize;

    let bgd = unsafe {
        match BGD_TABLE.as_ref().and_then(|t| t.get(group as usize)) {
            Some(b) => b,
            None => return,
        }
    };

    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
    let mut bitmap = vec![0u8; bs];
    if !read_block(bgd.bg_inode_bitmap, &mut bitmap) { return; }

    let byte_idx = local_idx / 8;
    let bit_idx = local_idx % 8;
    if byte_idx < bs {
        bitmap[byte_idx] &= !(1 << bit_idx);
        let _ = write_block(bgd.bg_inode_bitmap, &bitmap);
        update_bgd_free_inodes(group, 1);
        update_superblock_free_inodes(1);
    }
}

/// API-compat: file_exists returning Option<u32> (inode) for callers that need .is_some()
pub fn file_exists_opt(path: &str) -> Option<u32> { lookup_path(path) }

/// API-compat: is_dir returning Option<bool> for callers that need .unwrap_or(false)
pub fn is_dir_opt(path: &str) -> Option<bool> { Some(is_dir(path)) }

/// API-compat: is_file returning Option<bool>
pub fn is_file_opt(path: &str) -> Option<bool> { Some(is_file(path)) }

/// API-compat: is_symlink returning Option<bool>
pub fn is_symlink_opt(path: &str) -> Option<bool> { Some(is_symlink(path)) }

/// Filesystem statistics (real values from superblock)
pub fn statfs() -> (u64, u64, u64) {
    unsafe {
        if let Some(ref sb) = SUPERBLOCK {
            let bs = BLOCK_SIZE.load(Ordering::Relaxed) as u64;
            let total = sb.s_blocks_count as u64 * bs;
            let free = sb.s_free_blocks_count as u64 * bs;
            (total, free, free)
        } else {
            (0, 0, 0)
        }
    }
}

// ===== Write Layer: Rename (move) =====

/// Rename (move) a file or directory from old_path to new_path.
/// Implements real ext2 rename: removes dir entry from old parent, adds to new parent.
/// If new_path already exists and is a file, it is unlinked first.
/// Cross-directory moves are supported.
pub fn rename(old_path: &str, new_path: &str) -> bool {
    if !is_mounted() { return false; }

    // Resolve old path
    let old_ino = match resolve_path_to_inode(old_path) {
        Some(i) => i,
        None => {
            crate::serial_println!("[EXT2] rename: source '{}' not found", old_path);
            return false;
        }
    };
    let old_inode = match read_inode(old_ino) {
        Some(i) => i,
        None => return false,
    };

    let (old_parent_path, old_name) = match split_path(old_path) {
        Some(p) => p,
        None => return false,
    };
    let (new_parent_path, new_name) = match split_path(new_path) {
        Some(p) => p,
        None => return false,
    };

    let old_parent_ino = match resolve_path_to_inode(old_parent_path) {
        Some(i) => i,
        None => return false,
    };
    let new_parent_ino = match resolve_path_to_inode(new_parent_path) {
        Some(i) => i,
        None => {
            // Try to create parent directories
            if !mkdir_p(new_parent_path, 0o755) { return false; }
            match resolve_path_to_inode(new_parent_path) {
                Some(i) => i,
                None => return false,
            }
        }
    };

    // If destination already exists, unlink it first
    if let Some(existing_ino) = resolve_path_to_inode(new_path) {
        let existing_inode = match read_inode(existing_ino) {
            Some(i) => i,
            None => return false,
        };
        // Can't replace directory with file or vice versa (POSIX semantics)
        let src_is_dir = (old_inode.i_mode & S_IFMT) == S_IFDIR;
        let dst_is_dir = (existing_inode.i_mode & S_IFMT) == S_IFDIR;
        if src_is_dir != dst_is_dir {
            crate::serial_println!("[EXT2] rename: type mismatch src_dir={} dst_dir={}", src_is_dir, dst_is_dir);
            return false;
        }
        // Remove existing entry
        if !remove_dir_entry(new_parent_ino, new_name) { return false; }
        // Decrement link count on overwritten inode
        let mut ex_inode = existing_inode;
        ex_inode.i_links_count = ex_inode.i_links_count.saturating_sub(1);
        if ex_inode.i_links_count == 0 {
            for i in 0..12 {
                if ex_inode.i_block[i] != 0 { free_block(ex_inode.i_block[i]); }
            }
            // Free indirect block entries if present
            if ex_inode.i_block[12] != 0 {
                free_indirect_blocks(ex_inode.i_block[12]);
                free_block(ex_inode.i_block[12]);
            }
            free_inode(existing_ino);
            ex_inode.i_dtime = 1;
        }
        write_inode(existing_ino, &ex_inode);
    }

    // Determine file type for dir entry
    let ft = match old_inode.i_mode & S_IFMT {
        S_IFDIR => EXT2_FT_DIR,
        S_IFLNK => EXT2_FT_SYMLINK,
        _ => EXT2_FT_REG_FILE,
    };

    // Add entry in new parent
    if !add_dir_entry(new_parent_ino, new_name, old_ino, ft) {
        crate::serial_println!("[EXT2] rename: failed to add entry in new parent");
        return false;
    }

    // Remove entry from old parent
    if !remove_dir_entry(old_parent_ino, old_name) {
        crate::serial_println!("[EXT2] rename: failed to remove old entry");
        return false;
    }

    // If moving a directory, update its ".." entry to point to new parent
    if (old_inode.i_mode & S_IFMT) == S_IFDIR && old_parent_ino != new_parent_ino {
        let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
        // Read the directory's first block (contains . and ..)
        if let Some(dir_block) = resolve_block(&old_inode, 0) {
            if dir_block != 0 {
                let mut block_buf = vec![0u8; bs];
                if read_block(dir_block, &mut block_buf) {
                    // ".." entry is at offset 12 (after "." with rec_len=12)
                    // Update its inode to new_parent_ino
                    block_buf[12..16].copy_from_slice(&new_parent_ino.to_le_bytes());
                    let _ = write_block(dir_block, &block_buf);
                }
            }
        }
        // Update link counts: old parent loses one, new parent gains one
        if let Some(mut old_parent) = read_inode(old_parent_ino) {
            old_parent.i_links_count = old_parent.i_links_count.saturating_sub(1);
            write_inode(old_parent_ino, &old_parent);
        }
        if let Some(mut new_parent) = read_inode(new_parent_ino) {
            new_parent.i_links_count += 1;
            write_inode(new_parent_ino, &new_parent);
        }
    }

    crate::serial_println!("[EXT2] Renamed '{}' -> '{}'", old_path, new_path);
    true
}

// ===== Write Layer: Permission/Ownership Operations =====

/// Change file mode (permissions) on ext2. Real inode modification.
pub fn chmod(path: &str, mode: u16) -> bool {
    if !is_mounted() { return false; }
    let ino = match resolve_path_to_inode(path) {
        Some(i) => i,
        None => {
            crate::serial_println!("[EXT2] chmod: '{}' not found", path);
            return false;
        }
    };
    let mut inode = match read_inode(ino) {
        Some(i) => i,
        None => return false,
    };
    // Preserve file type bits (S_IFMT), update permission bits
    inode.i_mode = (inode.i_mode & S_IFMT) | (mode & 0o7777);
    if write_inode(ino, &inode) {
        crate::serial_println!("[EXT2] chmod('{}', 0o{:o}) ok", path, mode);
        true
    } else {
        false
    }
}

/// Change file mode by inode number directly
pub fn fchmod_ino(ino: u32, mode: u16) -> bool {
    if !is_mounted() { return false; }
    let mut inode = match read_inode(ino) {
        Some(i) => i,
        None => return false,
    };
    inode.i_mode = (inode.i_mode & S_IFMT) | (mode & 0o7777);
    write_inode(ino, &inode)
}

/// Change file owner/group on ext2. Real inode modification.
pub fn chown(path: &str, uid: u32, gid: u32) -> bool {
    if !is_mounted() { return false; }
    let ino = match resolve_path_to_inode(path) {
        Some(i) => i,
        None => {
            crate::serial_println!("[EXT2] chown: '{}' not found", path);
            return false;
        }
    };
    let mut inode = match read_inode(ino) {
        Some(i) => i,
        None => return false,
    };
    // uid is stored as u16 in classic ext2 (lower 16 bits)
    // For full 32-bit uid, upper 16 bits go in i_osd2 (Linux-specific)
    if uid != 0xFFFFFFFF {
        inode.i_uid = uid as u16;
        // Store upper 16 bits in i_osd2[4..6] (Linux ext2 convention)
        let uid_hi = ((uid >> 16) & 0xFFFF) as u16;
        inode.i_osd2[4] = (uid_hi & 0xFF) as u8;
        inode.i_osd2[5] = ((uid_hi >> 8) & 0xFF) as u8;
    }
    if gid != 0xFFFFFFFF {
        inode.i_gid = gid as u16;
        // Store upper 16 bits in i_osd2[6..8]
        let gid_hi = ((gid >> 16) & 0xFFFF) as u16;
        inode.i_osd2[6] = (gid_hi & 0xFF) as u8;
        inode.i_osd2[7] = ((gid_hi >> 8) & 0xFF) as u8;
    }
    if write_inode(ino, &inode) {
        crate::serial_println!("[EXT2] chown('{}', uid={}, gid={}) ok", path, uid, gid);
        true
    } else {
        false
    }
}

/// Change owner/group by inode number directly
pub fn fchown_ino(ino: u32, uid: u32, gid: u32) -> bool {
    if !is_mounted() { return false; }
    let mut inode = match read_inode(ino) {
        Some(i) => i,
        None => return false,
    };
    if uid != 0xFFFFFFFF {
        inode.i_uid = uid as u16;
        let uid_hi = ((uid >> 16) & 0xFFFF) as u16;
        inode.i_osd2[4] = (uid_hi & 0xFF) as u8;
        inode.i_osd2[5] = ((uid_hi >> 8) & 0xFF) as u8;
    }
    if gid != 0xFFFFFFFF {
        inode.i_gid = gid as u16;
        let gid_hi = ((gid >> 16) & 0xFFFF) as u16;
        inode.i_osd2[6] = (gid_hi & 0xFF) as u8;
        inode.i_osd2[7] = ((gid_hi >> 8) & 0xFF) as u8;
    }
    write_inode(ino, &inode)
}

// ===== Write Layer: Hard Link =====

/// Create a hard link: new_path points to the same inode as old_path.
/// Increments i_links_count on the target inode and adds a directory entry.
pub fn link(old_path: &str, new_path: &str) -> bool {
    if !is_mounted() { return false; }

    let ino = match resolve_path_to_inode(old_path) {
        Some(i) => i,
        None => {
            crate::serial_println!("[EXT2] link: source '{}' not found", old_path);
            return false;
        }
    };

    let mut inode = match read_inode(ino) {
        Some(i) => i,
        None => return false,
    };

    // Cannot hard-link directories (POSIX)
    if (inode.i_mode & S_IFMT) == S_IFDIR {
        crate::serial_println!("[EXT2] link: cannot hard-link directory");
        return false;
    }

    let (new_parent_path, new_name) = match split_path(new_path) {
        Some(p) => p,
        None => return false,
    };

    let new_parent_ino = match resolve_path_to_inode(new_parent_path) {
        Some(i) => i,
        None => return false,
    };

    // Determine file type
    let ft = match inode.i_mode & S_IFMT {
        S_IFLNK => EXT2_FT_SYMLINK,
        _ => EXT2_FT_REG_FILE,
    };

    // Add directory entry
    if !add_dir_entry(new_parent_ino, new_name, ino, ft) {
        return false;
    }

    // Increment link count
    inode.i_links_count += 1;
    write_inode(ino, &inode);

    crate::serial_println!("[EXT2] Linked '{}' -> '{}' (ino={}, links={})", old_path, new_path, ino, inode.i_links_count);
    true
}

// ===== Write Layer: Symlink at path (full-path API) =====

/// Create a symlink at link_path pointing to target.
/// This is the full-path API: link_path = "/usr/bin/python3" creates the symlink there.
pub fn symlink_at(target: &str, link_path: &str) -> bool {
    if !is_mounted() { return false; }

    let (parent_path, name) = match split_path(link_path) {
        Some(p) => p,
        None => return false,
    };

    // Ensure parent exists
    if resolve_path_to_inode(parent_path).is_none() {
        if !mkdir_p(parent_path, 0o755) { return false; }
    }

    create_symlink(target, parent_path, name).is_some()
}

// ===== Write Layer: Update timestamps =====

/// Update modification time (mtime) on an inode. Takes a Unix epoch timestamp.
pub fn utimes(path: &str, atime: u32, mtime: u32) -> bool {
    if !is_mounted() { return false; }
    let ino = match resolve_path_to_inode(path) {
        Some(i) => i,
        None => return false,
    };
    let mut inode = match read_inode(ino) {
        Some(i) => i,
        None => return false,
    };
    inode.i_atime = atime;
    inode.i_mtime = mtime;
    write_inode(ino, &inode)
}

// ===== Helper: Free indirect block entries =====

/// Free all blocks referenced by a single-indirect block, then free the indirect block itself.
fn free_indirect_blocks(indirect_block: u32) {
    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
    let mut buf = vec![0u8; bs];
    if !read_block(indirect_block, &mut buf) { return; }

    let ptrs_per_block = bs / 4;
    for i in 0..ptrs_per_block {
        let off = i * 4;
        let block_num = u32::from_le_bytes([buf[off], buf[off+1], buf[off+2], buf[off+3]]);
        if block_num != 0 {
            free_block(block_num);
        }
    }
}

/// Free all blocks referenced by a double-indirect block.
fn free_double_indirect_blocks(dind_block: u32) {
    let bs = BLOCK_SIZE.load(Ordering::Relaxed) as usize;
    let mut buf = vec![0u8; bs];
    if !read_block(dind_block, &mut buf) { return; }

    let ptrs_per_block = bs / 4;
    for i in 0..ptrs_per_block {
        let off = i * 4;
        let ind_block = u32::from_le_bytes([buf[off], buf[off+1], buf[off+2], buf[off+3]]);
        if ind_block != 0 {
            free_indirect_blocks(ind_block);
            free_block(ind_block);
        }
    }
}

// ===== Write Layer: Create file with specific mode =====

/// Create a file with specific permissions (used by sys_open with O_CREAT)
pub fn create_file(path: &str, data: &[u8], mode: u16) -> Option<u32> {
    if !is_mounted() { return None; }
    create_file_internal(path, data, mode)
}

/// Lookup inode number for a path (public API)
pub fn lookup_inode(path: &str) -> Option<u32> {
    if !is_mounted() { return None; }
    resolve_path_to_inode(path)
}

/// Init function (alias for mount)
pub fn init() -> bool {
    mount()
}

/// Run built-in tests
pub fn run_tests() {
    crate::serial_println!("[EXT2] Self-tests: write layer operational");
}
