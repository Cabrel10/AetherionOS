// kernel/src/net/ethernet.rs - Ethernet Frame Handling
//
// IEEE 802.3 Ethernet II frame format:
//   [6] Destination MAC
//   [6] Source MAC
//   [2] EtherType
//   [46-1500] Payload
//
// EtherTypes:
//   0x0800 = IPv4
//   0x0806 = ARP

use alloc::vec::Vec;

/// Maximum Transmission Unit for Ethernet
pub const MTU: usize = 1500;

/// Ethernet header size (no VLAN tag)
pub const HEADER_LEN: usize = 14;

/// EtherType constants
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;

/// MAC address (6 bytes)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacAddress(pub [u8; 6]);

impl MacAddress {
    pub const ZERO: MacAddress = MacAddress([0; 6]);
    pub const BROADCAST: MacAddress = MacAddress([0xFF; 6]);

    pub fn is_broadcast(&self) -> bool {
        self.0 == [0xFF; 6]
    }
}

impl core::fmt::Display for MacAddress {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5])
    }
}

/// Parsed Ethernet frame header
#[derive(Debug)]
pub struct EthernetFrame<'a> {
    pub dst_mac: MacAddress,
    pub src_mac: MacAddress,
    pub ethertype: u16,
    pub payload: &'a [u8],
}

impl<'a> EthernetFrame<'a> {
    /// Parse an Ethernet frame from raw bytes
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < HEADER_LEN {
            return None;
        }
        let mut dst = [0u8; 6];
        let mut src = [0u8; 6];
        dst.copy_from_slice(&data[0..6]);
        src.copy_from_slice(&data[6..12]);
        let ethertype = u16::from_be_bytes([data[12], data[13]]);

        Some(EthernetFrame {
            dst_mac: MacAddress(dst),
            src_mac: MacAddress(src),
            ethertype,
            payload: &data[HEADER_LEN..],
        })
    }
}

/// Build an Ethernet frame
pub fn build_frame(dst: MacAddress, src: MacAddress, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(&dst.0);
    frame.extend_from_slice(&src.0);
    frame.extend_from_slice(&ethertype.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}
