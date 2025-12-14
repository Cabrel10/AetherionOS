// TCP layer
pub struct TcpSegment {
    src_port: u16,
    dest_port: u16,
    seq_num: u32,
    ack_num: u32,
    flags: u8,
    window: u16,
    data: [u8; 1460],
    data_len: usize,
}

impl TcpSegment {
    pub fn new(src_port: u16, dest_port: u16) -> Self {
        TcpSegment {
            src_port,
            dest_port,
            seq_num: 0,
            ack_num: 0,
            flags: 0,
            window: 8192,
            data: [0; 1460],
            data_len: 0,
        }
    }
    
    pub fn connect(&mut self) -> Result<(), &'static str> {
        self.flags = 0x02; // SYN
        Ok(())
    }
}
