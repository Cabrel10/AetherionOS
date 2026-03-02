// kernel/src/net/icmp.rs - ICMP Protocol (Echo Request/Reply)
//
// RFC 792: Internet Control Message Protocol
//
// ICMP Header (8 bytes for Echo):
//   [1] Type (8=EchoRequest, 0=EchoReply)
//   [1] Code (0)
//   [2] Checksum
//   [2] Identifier
//   [2] Sequence Number
//   [...] Data

use alloc::vec::Vec;

/// ICMP Type constants
pub const TYPE_ECHO_REPLY: u8 = 0;
pub const TYPE_ECHO_REQUEST: u8 = 8;
pub const TYPE_DEST_UNREACHABLE: u8 = 3;

/// ICMP header size for Echo messages
pub const ECHO_HEADER_LEN: usize = 8;

/// Parsed ICMP message
#[derive(Debug)]
pub struct IcmpPacket<'a> {
    pub icmp_type: u8,
    pub code: u8,
    pub checksum: u16,
    pub identifier: u16,
    pub sequence: u16,
    pub data: &'a [u8],
}

impl<'a> IcmpPacket<'a> {
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < ECHO_HEADER_LEN {
            return None;
        }

        Some(IcmpPacket {
            icmp_type: data[0],
            code: data[1],
            checksum: u16::from_be_bytes([data[2], data[3]]),
            identifier: u16::from_be_bytes([data[4], data[5]]),
            sequence: u16::from_be_bytes([data[6], data[7]]),
            data: &data[ECHO_HEADER_LEN..],
        })
    }

    pub fn is_echo_request(&self) -> bool {
        self.icmp_type == TYPE_ECHO_REQUEST && self.code == 0
    }

    pub fn is_echo_reply(&self) -> bool {
        self.icmp_type == TYPE_ECHO_REPLY && self.code == 0
    }
}

/// Build an ICMP Echo Reply from a received Echo Request
pub fn build_echo_reply(identifier: u16, sequence: u16, data: &[u8]) -> Vec<u8> {
    build_echo(TYPE_ECHO_REPLY, identifier, sequence, data)
}

/// Build an ICMP Echo Request
pub fn build_echo_request(identifier: u16, sequence: u16, data: &[u8]) -> Vec<u8> {
    build_echo(TYPE_ECHO_REQUEST, identifier, sequence, data)
}

fn build_echo(icmp_type: u8, identifier: u16, sequence: u16, data: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(ECHO_HEADER_LEN + data.len());

    // Type
    packet.push(icmp_type);
    // Code
    packet.push(0);
    // Checksum placeholder
    packet.extend_from_slice(&[0x00, 0x00]);
    // Identifier
    packet.extend_from_slice(&identifier.to_be_bytes());
    // Sequence
    packet.extend_from_slice(&sequence.to_be_bytes());
    // Data
    packet.extend_from_slice(data);

    // Compute ICMP checksum over entire ICMP message
    let cksum = super::ipv4::checksum(&packet);
    packet[2] = (cksum >> 8) as u8;
    packet[3] = (cksum & 0xFF) as u8;

    packet
}
