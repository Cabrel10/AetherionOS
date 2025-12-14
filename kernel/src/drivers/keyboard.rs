// Keyboard driver (PS/2)
use spin::Mutex;

static KEYBOARD: Mutex<Keyboard> = Mutex::new(Keyboard::new());

pub struct Keyboard {
    buffer: [u8; 256],
    read_pos: usize,
    write_pos: usize,
}

impl Keyboard {
    pub const fn new() -> Self {
        Keyboard {
            buffer: [0; 256],
            read_pos: 0,
            write_pos: 0,
        }
    }
    
    pub fn push(&mut self, key: u8) {
        self.buffer[self.write_pos] = key;
        self.write_pos = (self.write_pos + 1) % 256;
    }
    
    pub fn pop(&mut self) -> Option<u8> {
        if self.read_pos == self.write_pos {
            None
        } else {
            let key = self.buffer[self.read_pos];
            self.read_pos = (self.read_pos + 1) % 256;
            Some(key)
        }
    }
}

pub fn init() {
    // PS/2 keyboard initialization
}

pub fn read_key() -> Option<u8> {
    KEYBOARD.lock().pop()
}
