// Ethernet layer
pub struct EthernetFrame {
    dest_mac: [u8; 6],
    src_mac: [u8; 6],
    ethertype: u16,
    payload: [u8; 1500],
    payload_len: usize,
}

impl EthernetFrame {
    pub fn new() -> Self {
        EthernetFrame {
            dest_mac: [0; 6],
            src_mac: [0; 6],
            ethertype: 0x0800,
            payload: [0; 1500],
            payload_len: 0,
        }
    }
    
    pub fn send(&self) -> Result<(), &'static str> {
        Ok(())
    }
}

pub fn init() {}
