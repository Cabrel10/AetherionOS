// Virtual File System
use alloc::string::String;
use alloc::vec::Vec;

pub enum VfsNode {
    File { name: String, size: usize },
    Directory { name: String, children: Vec<VfsNode> },
}

pub trait FileSystem {
    fn open(&self, path: &str) -> Result<usize, &'static str>;
    fn read(&self, fd: usize, buffer: &mut [u8]) -> Result<usize, &'static str>;
    fn write(&self, fd: usize, data: &[u8]) -> Result<usize, &'static str>;
    fn close(&self, fd: usize) -> Result<(), &'static str>;
}

pub fn init() {
    // VFS initialization
}
