// FAT32 filesystem implementation
use super::FileSystem;

pub struct Fat32Fs {
    cluster_size: usize,
}

impl Fat32Fs {
    pub fn new() -> Self {
        Fat32Fs { cluster_size: 4096 }
    }
}

impl FileSystem for Fat32Fs {
    fn open(&self, _path: &str) -> Result<usize, &'static str> {
        Ok(0)
    }
    
    fn read(&self, _fd: usize, _buffer: &mut [u8]) -> Result<usize, &'static str> {
        Ok(0)
    }
    
    fn write(&self, _fd: usize, _data: &[u8]) -> Result<usize, &'static str> {
        Ok(0)
    }
    
    fn close(&self, _fd: usize) -> Result<(), &'static str> {
        Ok(())
    }
}
