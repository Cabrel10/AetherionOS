// kernel/src/net/ipv4.rs - IPv4 Packet Handling
//
// RFC 791: Internet Protocol
//
// IPv4 Header (20 bytes minimum):
//   [1] Version(4) + IHL(4)
//   [1] DSCP + ECN
//   [2] Total Length
//   [2] Identification
//   [2] Flags(3) + Fragment Offset(13)
//   [1] TTL
//   [1] Protocol
//   [2] Header Checksum
//   [4] Source IP
//   [4] Destination IP

use alloc::vec::Vec;

pub const HEADER_LEN: usize = 20;

/// IP Protocol numbers
pub const PROTO_ICMP: u8 = 1;
pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;

/// IPv4 address
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ipv4Addr(pub [u8; 4]);

impl Ipv4Addr {
    pub const ZERO: Ipv4Addr = Ipv4Addr([0, 0, 0, 0]);
    pub const LOOPBACK: Ipv4Addr = Ipv4Addr([127, 0, 0, 1]);
    pub const BROADCAST: Ipv4Addr = Ipv4Addr([255, 255, 255, 255]);

    pub fn new(a: u8, b: u8, c: u8, d: u8) -> Self {
        Ipv4Addr([a, b, c, d])
    }

    pub fn as_u32(&self) -> u32 {
        u32::from_be_bytes(self.0)
    }

    /// Check if same subnet
    pub fn same_subnet(&self, other: &Ipv4Addr, mask: &Ipv4Addr) -> bool {
        (self.as_u32() & mask.as_u32()) == (other.as_u32() & mask.as_u32())
    }
}

impl core::fmt::Display for Ipv4Addr {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "{}.{}.{}.{}", self.0[0], self.0[1], self.0[2], self.0[3])
    }
}

/// Parsed IPv4 packet header
#[derive(Debug)]
pub struct Ipv4Packet<'a> {
    pub version: u8,
    pub ihl: u8,
    pub total_length: u16,
    pub identification: u16,
    pub ttl: u8,
    pub protocol: u8,
    pub checksum: u16,
    pub src_ip: Ipv4Addr,
    pub dst_ip: Ipv4Addr,
    pub payload: &'a [u8],
}

impl<'a> Ipv4Packet<'a> {
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < HEADER_LEN {
            return None;
        }

        let version = (data[0] >> 4) & 0xF;
        let ihl = data[0] & 0xF;
        if version != 4 || ihl < 5 {
            return None;
        }

        let header_len = (ihl as usize) * 4;
        if data.len() < header_len {
            return None;
        }

        let total_length = u16::from_be_bytes([data[2], data[3]]);
        let identification = u16::from_be_bytes([data[4], data[5]]);
        let ttl = data[8];
        let protocol = data[9];
        let checksum = u16::from_be_bytes([data[10], data[11]]);

        let mut src = [0u8; 4];
        let mut dst = [0u8; 4];
        src.copy_from_slice(&data[12..16]);
        dst.copy_from_slice(&data[16..20]);

        let payload_end = core::cmp::min(total_length as usize, data.len());
        let payload = if header_len < payload_end {
            &data[header_len..payload_end]
        } else {
            &[]
        };

        Some(Ipv4Packet {
            version,
            ihl,
            total_length,
            identification,
            ttl,
            protocol,
            checksum,
            src_ip: Ipv4Addr(src),
            dst_ip: Ipv4Addr(dst),
            payload,
        })
    }
}

/// Compute Internet checksum (RFC 1071)
pub fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Build an IPv4 packet
pub fn build_packet(
    src: Ipv4Addr,
    dst: Ipv4Addr,
    protocol: u8,
    id: u16,
    ttl: u8,
    payload: &[u8],
) -> Vec<u8> {
    let total_len = (HEADER_LEN + payload.len()) as u16;
    let mut packet = Vec::with_capacity(total_len as usize);

    // Version(4) + IHL(5) = 0x45
    packet.push(0x45);
    // DSCP + ECN
    packet.push(0x00);
    // Total length
    packet.extend_from_slice(&total_len.to_be_bytes());
    // Identification
    packet.extend_from_slice(&id.to_be_bytes());
    // Flags (DF) + Fragment offset
    packet.extend_from_slice(&[0x40, 0x00]); // Don't Fragment
    // TTL
    packet.push(ttl);
    // Protocol
    packet.push(protocol);
    // Checksum placeholder (will fill after)
    packet.extend_from_slice(&[0x00, 0x00]);
    // Source IP
    packet.extend_from_slice(&src.0);
    // Destination IP
    packet.extend_from_slice(&dst.0);

    // Compute header checksum
    let cksum = checksum(&packet[..HEADER_LEN]);
    packet[10] = (cksum >> 8) as u8;
    packet[11] = (cksum & 0xFF) as u8;

    // Payload
    packet.extend_from_slice(payload);
    packet
}
