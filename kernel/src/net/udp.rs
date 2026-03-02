// kernel/src/net/udp.rs - UDP Protocol (RFC 768)
//
// UDP Header (8 bytes):
//   [2] Source Port
//   [2] Destination Port
//   [2] Length (header + data)
//   [2] Checksum (optional for IPv4)

use alloc::vec::Vec;
use super::ipv4::Ipv4Packet;

pub const HEADER_LEN: usize = 8;

/// Parsed UDP packet
#[derive(Debug)]
pub struct UdpPacket<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub length: u16,
    pub checksum: u16,
    pub data: &'a [u8],
}

impl<'a> UdpPacket<'a> {
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < HEADER_LEN {
            return None;
        }

        let src_port = u16::from_be_bytes([data[0], data[1]]);
        let dst_port = u16::from_be_bytes([data[2], data[3]]);
        let length = u16::from_be_bytes([data[4], data[5]]);
        let checksum = u16::from_be_bytes([data[6], data[7]]);

        let data_end = core::cmp::min(length as usize, data.len());
        let payload = if HEADER_LEN < data_end {
            &data[HEADER_LEN..data_end]
        } else {
            &[]
        };

        Some(UdpPacket {
            src_port,
            dst_port,
            length,
            checksum,
            data: payload,
        })
    }
}

/// Build a UDP packet
pub fn build_packet(src_port: u16, dst_port: u16, data: &[u8]) -> Vec<u8> {
    let length = (HEADER_LEN + data.len()) as u16;
    let mut packet = Vec::with_capacity(length as usize);

    packet.extend_from_slice(&src_port.to_be_bytes());
    packet.extend_from_slice(&dst_port.to_be_bytes());
    packet.extend_from_slice(&length.to_be_bytes());
    // Checksum = 0 (optional for IPv4 UDP)
    packet.extend_from_slice(&[0x00, 0x00]);
    packet.extend_from_slice(data);

    packet
}

/// Process incoming UDP packet
pub fn process_udp(ip_pkt: &Ipv4Packet) {
    let udp_pkt = match UdpPacket::parse(ip_pkt.payload) {
        Some(p) => p,
        None => return,
    };

    crate::serial_println!(
        "[UDP] {}:{} -> {}:{} ({} bytes)",
        ip_pkt.src_ip, udp_pkt.src_port,
        ip_pkt.dst_ip, udp_pkt.dst_port,
        udp_pkt.data.len()
    );

    // Deliver to socket layer
    super::socket::deliver_udp(ip_pkt.src_ip, udp_pkt.src_port, udp_pkt.dst_port, udp_pkt.data);
}
