// VGA driver
pub const VGA_WIDTH: usize = 80;
pub const VGA_HEIGHT: usize = 25;
pub const VGA_BUFFER: *mut u8 = 0xb8000 as *mut u8;

pub fn init() {
    clear();
}

pub fn clear() {
    unsafe {
        for i in 0..(VGA_WIDTH * VGA_HEIGHT * 2) {
            *VGA_BUFFER.offset(i as isize) = 0;
        }
    }
}

pub fn write_char(x: usize, y: usize, ch: u8, color: u8) {
    if x >= VGA_WIDTH || y >= VGA_HEIGHT {
        return;
    }
    unsafe {
        let offset = (y * VGA_WIDTH + x) * 2;
        *VGA_BUFFER.offset(offset as isize) = ch;
        *VGA_BUFFER.offset(offset as isize + 1) = color;
    }
}
