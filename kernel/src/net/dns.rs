// kernel/src/net/dns.rs - DNS Resolver (RFC 1035)
//
// Minimal DNS client for AetherionOS.
// Sends A record queries via UDP to the configured DNS server (10.0.2.3 in QEMU).
//
// DNS Message Format (simplified):
//   [2] Transaction ID
//   [2] Flags (QR, Opcode, AA, TC, RD, RA, Z, RCODE)
//   [2] Questions count
//   [2] Answers count
//   [2] Authority count
//   [2] Additional count
//   [?] Questions section
//   [?] Answers section
//
// SAFETY: All unsafe blocks are for accessing global network state and
// reading user-space string buffers.

use alloc::vec::Vec;
use alloc::string::String;
use alloc::collections::BTreeMap;
use spin::Mutex;
use lazy_static::lazy_static;

use super::ipv4::Ipv4Addr;

/// DNS port
pub const DNS_PORT: u16 = 53;

/// DNS record types
pub const TYPE_A: u16 = 1;     // IPv4 address
pub const CLASS_IN: u16 = 1;   // Internet class

/// DNS header flags
const FLAG_QR_RESPONSE: u16 = 0x8000;
const FLAG_RD: u16 = 0x0100;   // Recursion Desired
const FLAG_RA: u16 = 0x0080;   // Recursion Available

/// DNS cache entry
struct DnsCacheEntry {
    pub ip: Ipv4Addr,
    pub _ttl: u32,
}

lazy_static! {
    static ref DNS_CACHE: Mutex<BTreeMap<String, DnsCacheEntry>> = Mutex::new(BTreeMap::new());
    static ref DNS_PENDING: Mutex<BTreeMap<u16, Option<Ipv4Addr>>> = Mutex::new(BTreeMap::new());
}

/// Transaction ID counter
static DNS_TX_ID: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(0xAE00);

fn next_tx_id() -> u16 {
    DNS_TX_ID.fetch_add(1, core::sync::atomic::Ordering::Relaxed)
}

/// Encode a domain name in DNS wire format
/// e.g. "example.com" -> [7, 'e','x','a','m','p','l','e', 3, 'c','o','m', 0]
fn encode_domain(domain: &str) -> Vec<u8> {
    let mut result = Vec::with_capacity(domain.len() + 2);
    for label in domain.split('.') {
        let len = label.len();
        if len > 63 { return result; } // Label too long
        result.push(len as u8);
        result.extend_from_slice(label.as_bytes());
    }
    result.push(0); // Root label
    result
}

/// Build a DNS query packet for an A record
pub fn build_query(domain: &str) -> (u16, Vec<u8>) {
    let tx_id = next_tx_id();
    let mut packet = Vec::with_capacity(64);

    // Header
    packet.extend_from_slice(&tx_id.to_be_bytes());      // Transaction ID
    packet.extend_from_slice(&FLAG_RD.to_be_bytes());     // Flags: RD=1
    packet.extend_from_slice(&1u16.to_be_bytes());        // QDCOUNT = 1
    packet.extend_from_slice(&0u16.to_be_bytes());        // ANCOUNT = 0
    packet.extend_from_slice(&0u16.to_be_bytes());        // NSCOUNT = 0
    packet.extend_from_slice(&0u16.to_be_bytes());        // ARCOUNT = 0

    // Question section
    packet.extend_from_slice(&encode_domain(domain));
    packet.extend_from_slice(&TYPE_A.to_be_bytes());      // QTYPE = A
    packet.extend_from_slice(&CLASS_IN.to_be_bytes());    // QCLASS = IN

    (tx_id, packet)
}

/// Parse a DNS response and extract the first A record
pub fn parse_response(data: &[u8]) -> Option<(u16, Ipv4Addr, u32)> {
    if data.len() < 12 {
        return None;
    }

    let tx_id = u16::from_be_bytes([data[0], data[1]]);
    let flags = u16::from_be_bytes([data[2], data[3]]);

    // Must be a response
    if flags & FLAG_QR_RESPONSE == 0 {
        return None;
    }

    // Check RCODE (bits 0-3) == 0 (no error)
    if flags & 0x000F != 0 {
        return None;
    }

    let _qdcount = u16::from_be_bytes([data[4], data[5]]);
    let ancount = u16::from_be_bytes([data[6], data[7]]);

    if ancount == 0 {
        return None;
    }

    // Skip header (12 bytes) and question section
    let mut pos = 12;

    // Skip question(s): name + type(2) + class(2)
    pos = skip_dns_name(data, pos)?;
    pos += 4; // QTYPE + QCLASS

    // Parse answers
    for _ in 0..ancount {
        if pos >= data.len() {
            break;
        }

        // Skip answer name (may be compressed pointer)
        pos = skip_dns_name(data, pos)?;

        if pos + 10 > data.len() {
            break;
        }

        let rtype = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let _rclass = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
        let ttl = u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]);
        let rdlength = u16::from_be_bytes([data[pos + 8], data[pos + 9]]);
        pos += 10;

        if rtype == TYPE_A && rdlength == 4 && pos + 4 <= data.len() {
            let ip = Ipv4Addr::new(data[pos], data[pos + 1], data[pos + 2], data[pos + 3]);
            return Some((tx_id, ip, ttl));
        }

        pos += rdlength as usize;
    }

    None
}

/// Skip a DNS name (handles compression pointers)
fn skip_dns_name(data: &[u8], mut pos: usize) -> Option<usize> {
    let mut jumps = 0;
    loop {
        if pos >= data.len() {
            return None;
        }
        let len = data[pos];
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xC0 == 0xC0 {
            // Compression pointer - 2 bytes, skip and done
            pos += 2;
            break;
        }
        if jumps > 64 {
            return None; // Infinite loop protection
        }
        pos += 1 + len as usize;
        jumps += 1;
    }
    Some(pos)
}

/// Resolve a domain name to an IPv4 address
/// Uses the DNS server configured for the network (10.0.2.3 in QEMU)
pub fn resolve(domain: &str) -> Result<Ipv4Addr, i64> {
    if !super::is_available() {
        return Err(-5); // EIO
    }

    // Check cache first
    {
        let cache = DNS_CACHE.lock();
        if let Some(entry) = cache.get(domain) {
            crate::serial_println!("[DNS] Cache hit: {} -> {}", domain, entry.ip);
            return Ok(entry.ip);
        }
    }

    // Get DNS server address
    // SAFETY: NET_CONFIG is initialized at boot
    let dns_server = unsafe {
        match &super::NET_CONFIG {
            Some(c) => c.dns,
            None => return Err(-5),
        }
    };

    // Build and send DNS query
    let (tx_id, query) = build_query(domain);
    crate::serial_println!("[DNS] Query: {} (txid=0x{:04X}) -> {}", domain, tx_id, dns_server);

    // Register pending query
    {
        let mut pending = DNS_PENDING.lock();
        pending.insert(tx_id, None);
    }

    // Send via UDP
    let src_port = 10053u16; // Our DNS client port
    super::send_udp(dns_server, src_port, DNS_PORT, &query);

    // Wait for response with timeout
    for attempt in 0..3_000_000u32 {
        super::poll();

        {
            let mut pending = DNS_PENDING.lock();
            if let Some(Some(ip)) = pending.get(&tx_id) {
                let ip = *ip;
                pending.remove(&tx_id);

                // Cache the result
                let mut cache = DNS_CACHE.lock();
                cache.insert(String::from(domain), DnsCacheEntry { ip, _ttl: 300 });

                crate::serial_println!("[DNS] Resolved: {} -> {} (cached)", domain, ip);
                return Ok(ip);
            }
        }

        // Retransmit after 1M cycles
        if attempt == 1_000_000 {
            super::send_udp(dns_server, src_port, DNS_PORT, &query);
            crate::serial_println!("[DNS] Retransmit query for {}", domain);
        }

        // SAFETY: Pause for busy-wait
        unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    }

    // Timeout
    {
        let mut pending = DNS_PENDING.lock();
        pending.remove(&tx_id);
    }
    crate::serial_println!("[DNS] Timeout resolving {}", domain);
    Err(-110) // ETIMEDOUT
}

/// Process incoming DNS response (called from UDP handler)
pub fn process_dns_response(data: &[u8]) {
    if let Some((tx_id, ip, ttl)) = parse_response(data) {
        crate::serial_println!("[DNS] Response: txid=0x{:04X} -> {} (TTL={})", tx_id, ip, ttl);

        let mut pending = DNS_PENDING.lock();
        if let Some(entry) = pending.get_mut(&tx_id) {
            *entry = Some(ip);
        }
    }
}

/// Flush DNS cache
pub fn flush_cache() {
    let mut cache = DNS_CACHE.lock();
    cache.clear();
    crate::serial_println!("[DNS] Cache flushed");
}

/// Run DNS self-tests
pub fn run_tests() {
    crate::serial_write("  [DNS TEST 1/3] Domain encoding... ");
    {
        let encoded = encode_domain("example.com");
        // Should be: [7, e,x,a,m,p,l,e, 3, c,o,m, 0]
        if encoded.len() == 13
            && encoded[0] == 7
            && encoded[8] == 3
            && encoded[12] == 0
        {
            crate::serial_write("OK\n");
        } else {
            crate::serial_println!("FAIL (len={})", encoded.len());
        }
    }

    crate::serial_write("  [DNS TEST 2/3] Query build... ");
    {
        let (_tx_id, query) = build_query("test.local");
        // Header = 12 bytes, question = encoded_name + 4
        if query.len() > 12 && query[4] == 0 && query[5] == 1 { // QDCOUNT=1
            crate::serial_write("OK\n");
        } else {
            crate::serial_write("FAIL\n");
        }
    }

    crate::serial_write("  [DNS TEST 3/3] Response parsing... ");
    {
        // Build a fake DNS response: txid=0x1234, A record for 1.2.3.4
        let mut response = Vec::new();
        response.extend_from_slice(&[0x12, 0x34]); // tx_id
        response.extend_from_slice(&[0x81, 0x80]); // flags: QR=1, RD=1, RA=1
        response.extend_from_slice(&[0x00, 0x01]); // QDCOUNT=1
        response.extend_from_slice(&[0x00, 0x01]); // ANCOUNT=1
        response.extend_from_slice(&[0x00, 0x00]); // NSCOUNT=0
        response.extend_from_slice(&[0x00, 0x00]); // ARCOUNT=0
        // Question: test.local
        response.extend_from_slice(&encode_domain("test.local"));
        response.extend_from_slice(&[0x00, 0x01]); // QTYPE=A
        response.extend_from_slice(&[0x00, 0x01]); // QCLASS=IN
        // Answer: compressed pointer to name, then A record
        response.extend_from_slice(&[0xC0, 0x0C]); // Pointer to name at offset 12
        response.extend_from_slice(&[0x00, 0x01]); // TYPE=A
        response.extend_from_slice(&[0x00, 0x01]); // CLASS=IN
        response.extend_from_slice(&[0x00, 0x00, 0x01, 0x2C]); // TTL=300
        response.extend_from_slice(&[0x00, 0x04]); // RDLENGTH=4
        response.extend_from_slice(&[1, 2, 3, 4]); // RDATA=1.2.3.4

        match parse_response(&response) {
            Some((tx_id, ip, ttl)) => {
                if tx_id == 0x1234 && ip == Ipv4Addr::new(1, 2, 3, 4) && ttl == 300 {
                    crate::serial_write("OK\n");
                } else {
                    crate::serial_println!("FAIL (tx=0x{:04X}, ip={}, ttl={})", tx_id, ip, ttl);
                }
            }
            None => crate::serial_write("FAIL (parse returned None)\n"),
        }
    }
}
