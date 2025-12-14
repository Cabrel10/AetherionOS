// Aetherion OS - File System
// Phase 4: VFS and FAT32

pub mod vfs;
pub mod fat32;

pub use vfs::{FileSystem, VfsNode};

pub fn init() {
    vfs::init();
}
