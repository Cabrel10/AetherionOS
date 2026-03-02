// kernel/src/net/arp.rs - ARP Protocol
//
// RFC 826: Address Resolution Protocol
//
// ARP packet (28 bytes for IPv4/Ethernet):
//   [2] Hardware type (1 = Ethernet)
//   [2] Protocol type (0x0800 = IPv4)
//   [1] Hardware address length (6)
//   [1] Protocol address length (4)
//   [2] Operation (1=Request, 2=Reply)
//   [6] Sender hardware address
//   [4] Sender protocol address
//   [6] Target hardware address
//   [4] Target protocol address

use alloc::vec::Vec;
use super::ethernet::MacAddress;
use super::ipv4::Ipv4Addr;

pub const ARP_LEN: usize = 28;
pub const OP_REQUEST: u16 = 1;
pub const OP_REPLY: u16 = 2;

/// Parsed ARP packet
#[derive(Debug)]
pub struct ArpPacket {
    pub operation: u16,
    pub sender_mac: MacAddress,
    pub sender_ip: Ipv4Addr,
    pub target_mac: MacAddress,
    pub target_ip: Ipv4Addr,
}

impl ArpPacket {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < ARP_LEN {
            return None;
        }

        let hw_type = u16::from_be_bytes([data[0], data[1]]);
        let proto_type = u16::from_be_bytes([data[2], data[3]]);
        let hw_len = data[4];
        let proto_len = data[5];

        // Validate: Ethernet (1) + IPv4 (0x0800) + 6 + 4
        if hw_type != 1 || proto_type != 0x0800 || hw_len != 6 || proto_len != 4 {
            return None;
        }

        let operation = u16::from_be_bytes([data[6], data[7]]);
        let mut sender_mac = [0u8; 6];
        let mut sender_ip = [0u8; 4];
        let mut target_mac = [0u8; 6];
        let mut target_ip = [0u8; 4];

        sender_mac.copy_from_slice(&data[8..14]);
        sender_ip.copy_from_slice(&data[14..18]);
        target_mac.copy_from_slice(&data[18..24]);
        target_ip.copy_from_slice(&data[24..28]);

        Some(ArpPacket {
            operation,
            sender_mac: MacAddress(sender_mac),
            sender_ip: Ipv4Addr(sender_ip),
            target_mac: MacAddress(target_mac),
            target_ip: Ipv4Addr(target_ip),
        })
    }
}

/// Build an ARP Reply
pub fn build_reply(
    our_mac: MacAddress,
    our_ip: Ipv4Addr,
    target_mac: MacAddress,
    target_ip: Ipv4Addr,
) -> Vec<u8> {
    build_packet(OP_REPLY, our_mac, our_ip, target_mac, target_ip)
}

/// Build an ARP Request
pub fn build_request(
    our_mac: MacAddress,
    our_ip: Ipv4Addr,
    target_ip: Ipv4Addr,
) -> Vec<u8> {
    build_packet(OP_REQUEST, our_mac, our_ip, MacAddress::ZERO, target_ip)
}

fn build_packet(
    operation: u16,
    sender_mac: MacAddress,
    sender_ip: Ipv4Addr,
    target_mac: MacAddress,
    target_ip: Ipv4Addr,
) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(ARP_LEN);
    // Hardware type: Ethernet (1)
    pkt.extend_from_slice(&1u16.to_be_bytes());
    // Protocol type: IPv4 (0x0800)
    pkt.extend_from_slice(&0x0800u16.to_be_bytes());
    // Hardware address length
    pkt.push(6);
    // Protocol address length
    pkt.push(4);
    // Operation
    pkt.extend_from_slice(&operation.to_be_bytes());
    // Sender MAC
    pkt.extend_from_slice(&sender_mac.0);
    // Sender IP
    pkt.extend_from_slice(&sender_ip.0);
    // Target MAC
    pkt.extend_from_slice(&target_mac.0);
    // Target IP
    pkt.extend_from_slice(&target_ip.0);
    pkt
}
