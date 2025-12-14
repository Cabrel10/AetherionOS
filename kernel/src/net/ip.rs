// IP layer
pub struct IpPacket {
    version: u8,
    src_addr: [u8; 4],
    dest_addr: [u8; 4],
    protocol: u8,
    payload: [u8; 1480],
    payload_len: usize,
}

impl IpPacket {
    pub fn new(src: [u8; 4], dest: [u8; 4]) -> Self {
        IpPacket {
            version: 4,
            src_addr: src,
            dest_addr: dest,
            protocol: 6,
            payload: [0; 1480],
            payload_len: 0,
        }
    }
}
