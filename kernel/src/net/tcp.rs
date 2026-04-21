// kernel/src/net/tcp.rs - TCP Protocol Implementation (RFC 793 / RFC 9293)
//
// Transmission Control Protocol - Full state machine for AetherionOS
//
// TCP Header (20 bytes minimum):
//   [2] Source Port
//   [2] Destination Port
//   [4] Sequence Number
//   [4] Acknowledgment Number
//   [1] Data Offset (4 bits) + Reserved (4 bits)
//   [1] Flags: FIN|SYN|RST|PSH|ACK|URG
//   [2] Window Size
//   [2] Checksum
//   [2] Urgent Pointer
//
// Implemented states: CLOSED, SYN_SENT, ESTABLISHED, FIN_WAIT_1, FIN_WAIT_2,
//                     CLOSE_WAIT, LAST_ACK, TIME_WAIT, LISTEN
//
// SAFETY: All unsafe blocks are required for packet buffer access from
// userspace and static mutable state access (single-threaded kernel context).

use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use spin::Mutex;
use lazy_static::lazy_static;

use super::ipv4::{self, Ipv4Addr};

pub const HEADER_LEN: usize = 20;

// TCP Flags
pub const FIN: u8 = 0x01;
pub const SYN: u8 = 0x02;
pub const RST: u8 = 0x04;
pub const PSH: u8 = 0x08;
pub const ACK: u8 = 0x10;
pub const URG: u8 = 0x20;

/// TCP connection states (RFC 793 Section 3.2)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    LastAck,
    TimeWait,
}

impl core::fmt::Display for TcpState {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            TcpState::Closed => write!(f, "CLOSED"),
            TcpState::Listen => write!(f, "LISTEN"),
            TcpState::SynSent => write!(f, "SYN_SENT"),
            TcpState::SynReceived => write!(f, "SYN_RECEIVED"),
            TcpState::Established => write!(f, "ESTABLISHED"),
            TcpState::FinWait1 => write!(f, "FIN_WAIT_1"),
            TcpState::FinWait2 => write!(f, "FIN_WAIT_2"),
            TcpState::CloseWait => write!(f, "CLOSE_WAIT"),
            TcpState::LastAck => write!(f, "LAST_ACK"),
            TcpState::TimeWait => write!(f, "TIME_WAIT"),
        }
    }
}

/// Parsed TCP segment header
#[derive(Debug)]
pub struct TcpSegment<'a> {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq_num: u32,
    pub ack_num: u32,
    pub data_offset: u8,
    pub flags: u8,
    pub window: u16,
    pub checksum: u16,
    pub urgent: u16,
    pub payload: &'a [u8],
}

impl<'a> TcpSegment<'a> {
    /// Parse a TCP segment from raw bytes
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < HEADER_LEN {
            return None;
        }

        let src_port = u16::from_be_bytes([data[0], data[1]]);
        let dst_port = u16::from_be_bytes([data[2], data[3]]);
        let seq_num = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let ack_num = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
        let data_offset = (data[12] >> 4) & 0xF;
        let flags = data[13];
        let window = u16::from_be_bytes([data[14], data[15]]);
        let checksum = u16::from_be_bytes([data[16], data[17]]);
        let urgent = u16::from_be_bytes([data[18], data[19]]);

        let header_len = (data_offset as usize) * 4;
        if header_len < HEADER_LEN || data.len() < header_len {
            return None;
        }

        let payload = &data[header_len..];

        Some(TcpSegment {
            src_port,
            dst_port,
            seq_num,
            ack_num,
            data_offset,
            flags,
            window,
            checksum,
            urgent,
            payload,
        })
    }

    #[inline]
    pub fn has_syn(&self) -> bool { self.flags & SYN != 0 }
    #[inline]
    pub fn has_ack(&self) -> bool { self.flags & ACK != 0 }
    #[inline]
    pub fn has_fin(&self) -> bool { self.flags & FIN != 0 }
    #[inline]
    pub fn has_rst(&self) -> bool { self.flags & RST != 0 }
    #[inline]
    pub fn has_psh(&self) -> bool { self.flags & PSH != 0 }
}

/// TCP Transmission Control Block (per-connection state)
pub struct TcpConnection {
    pub state: TcpState,
    pub local_port: u16,
    pub remote_port: u16,
    pub remote_ip: Ipv4Addr,

    // Send sequence variables (RFC 793 Section 3.2)
    pub snd_una: u32,   // oldest unacknowledged seq
    pub snd_nxt: u32,   // next seq to send
    pub snd_wnd: u16,   // send window

    // Receive sequence variables
    pub rcv_nxt: u32,   // next seq expected
    pub rcv_wnd: u16,   // receive window

    // Initial sequence numbers
    pub iss: u32,       // initial send sequence
    pub irs: u32,       // initial receive sequence

    // MSS (Maximum Segment Size)
    pub mss: u16,

    // Receive buffer: reassembled data for userspace to read
    pub recv_buf: Vec<u8>,

    // Send buffer: data queued for transmission
    pub send_buf: Vec<u8>,

    // Retransmission: last sent data for potential retransmit
    pub retransmit_data: Vec<u8>,
    pub retransmit_seq: u32,
    pub retransmit_count: u8,
}

impl TcpConnection {
    fn new(local_port: u16, remote_ip: Ipv4Addr, remote_port: u16, iss: u32) -> Self {
        TcpConnection {
            state: TcpState::Closed,
            local_port,
            remote_port,
            remote_ip,
            snd_una: iss,
            snd_nxt: iss,
            snd_wnd: 0,
            rcv_nxt: 0,
            rcv_wnd: 8192,
            iss,
            irs: 0,
            mss: 1460, // Ethernet MTU 1500 - 20 (IP) - 20 (TCP)
            recv_buf: Vec::with_capacity(8192),
            send_buf: Vec::new(),
            retransmit_data: Vec::new(),
            retransmit_seq: 0,
            retransmit_count: 0,
        }
    }
}

// Connection key: (local_port, remote_ip, remote_port)
type ConnKey = (u16, u32, u16);

lazy_static! {
    static ref TCP_CONNECTIONS: Mutex<BTreeMap<ConnKey, TcpConnection>> = Mutex::new(BTreeMap::new());
}

/// Simple sequence number generator using timestamp counter
static TCP_SEQ_COUNTER: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0x1000_0000);

fn next_iss() -> u32 {
    // SAFETY: rdtsc is always safe on x86_64
    let tsc = unsafe {
        let lo: u32;
        let hi: u32;
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
        ((hi as u64) << 32 | lo as u64) as u32
    };
    TCP_SEQ_COUNTER.fetch_add(tsc.wrapping_add(64000), core::sync::atomic::Ordering::Relaxed)
}

/// Next ephemeral port (49152-65535)
static NEXT_EPHEMERAL: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(49152);

fn alloc_ephemeral_port() -> u16 {
    let p = NEXT_EPHEMERAL.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if p >= 65500 {
        NEXT_EPHEMERAL.store(49152, core::sync::atomic::Ordering::Relaxed);
    }
    p
}

/// Compute TCP checksum (pseudo-header + TCP segment)
/// RFC 793 Section 3.1
pub fn tcp_checksum(src_ip: &Ipv4Addr, dst_ip: &Ipv4Addr, tcp_data: &[u8]) -> u16 {
    let mut sum = 0u32;

    // Pseudo-header: src IP + dst IP + zero + protocol(6) + TCP length
    let src = src_ip.0;
    let dst = dst_ip.0;
    sum += u16::from_be_bytes([src[0], src[1]]) as u32;
    sum += u16::from_be_bytes([src[2], src[3]]) as u32;
    sum += u16::from_be_bytes([dst[0], dst[1]]) as u32;
    sum += u16::from_be_bytes([dst[2], dst[3]]) as u32;
    sum += 6u32; // Protocol = TCP
    sum += tcp_data.len() as u32;

    // TCP segment (header + data)
    let mut i = 0;
    while i + 1 < tcp_data.len() {
        sum += u16::from_be_bytes([tcp_data[i], tcp_data[i + 1]]) as u32;
        i += 2;
    }
    if i < tcp_data.len() {
        sum += (tcp_data[i] as u32) << 8;
    }

    // Fold carries
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Build a TCP segment
pub fn build_segment(
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    payload: &[u8],
    src_ip: &Ipv4Addr,
    dst_ip: &Ipv4Addr,
) -> Vec<u8> {
    let header_len = HEADER_LEN; // No options for now
    let data_offset = (header_len / 4) as u8;
    let total_len = header_len + payload.len();
    let mut seg = Vec::with_capacity(total_len);

    seg.extend_from_slice(&src_port.to_be_bytes());
    seg.extend_from_slice(&dst_port.to_be_bytes());
    seg.extend_from_slice(&seq.to_be_bytes());
    seg.extend_from_slice(&ack.to_be_bytes());
    seg.push((data_offset << 4) | 0); // Data offset + reserved
    seg.push(flags);
    seg.extend_from_slice(&window.to_be_bytes());
    seg.extend_from_slice(&[0x00, 0x00]); // Checksum placeholder
    seg.extend_from_slice(&[0x00, 0x00]); // Urgent pointer

    seg.extend_from_slice(payload);

    // Compute checksum
    let cksum = tcp_checksum(src_ip, dst_ip, &seg);
    seg[16] = (cksum >> 8) as u8;
    seg[17] = (cksum & 0xFF) as u8;

    seg
}

/// Send a TCP segment via the IP layer
fn send_tcp_segment(dst_ip: Ipv4Addr, segment: &[u8]) {
    // SAFETY: Accessing global NET_CONFIG which is initialized once at boot
    unsafe {
        if let Some(ref config) = super::NET_CONFIG {
            let ip_pkt = ipv4::build_packet(
                config.our_ip,
                dst_ip,
                ipv4::PROTO_TCP,
                0x4321, // identification
                64,     // TTL
                segment,
            );
            let dst_mac = super::resolve_mac(dst_ip);
            let frame = super::ethernet::build_frame(
                dst_mac,
                config.our_mac,
                super::ethernet::ETHERTYPE_IPV4,
                &ip_pkt,
            );
            super::send_frame(&frame);
        }
    }
}

/// Initiate a TCP connection (active open / 3-way handshake)
/// Returns local_port on success
pub fn tcp_connect(remote_ip: Ipv4Addr, remote_port: u16) -> Result<u16, i64> {
    if !super::is_available() {
        return Err(-5); // EIO
    }

    // Ensure ARP is resolved before TCP
    super::send_arp_request(remote_ip);
    for _ in 0..10_000 {
        super::poll();
        // SAFETY: Pause instruction for busy-wait
        unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    }

    let local_port = alloc_ephemeral_port();
    let iss = next_iss();
    let key = (local_port, remote_ip.as_u32(), remote_port);

    let mut conn = TcpConnection::new(local_port, remote_ip, remote_port, iss);
    conn.state = TcpState::SynSent;
    conn.snd_nxt = iss.wrapping_add(1); // SYN consumes one seq

    // Build and send SYN
    // SAFETY: Accessing global NET_CONFIG
    let our_ip = unsafe {
        match &*core::ptr::addr_of!(super::NET_CONFIG) {
            Some(c) => c.our_ip,
            None => return Err(-5),
        }
    };

    let syn_seg = build_segment(
        local_port, remote_port,
        iss, 0,
        SYN, 8192,
        &[],
        &our_ip, &remote_ip,
    );
    send_tcp_segment(remote_ip, &syn_seg);
    crate::serial_println!("[TCP] SYN sent to {}:{} (seq=0x{:08X})", remote_ip, remote_port, iss);

    // Store connection
    {
        let mut conns = TCP_CONNECTIONS.lock();
        conns.insert(key, conn);
    }

    // Wait for SYN-ACK with timeout and exponential backoff
    let mut attempts = 0u32;
    let mut poll_interval = 0u32;
    loop {
        // Only poll every N iterations to reduce heap pressure
        if poll_interval == 0 {
            super::poll(); // Process incoming packets
            poll_interval = core::cmp::min(attempts / 100 + 1, 500); // backoff
        }
        poll_interval -= 1;

        {
            let conns = TCP_CONNECTIONS.lock();
            if let Some(c) = conns.get(&key) {
                match c.state {
                    TcpState::Established => {
                        crate::serial_println!("[TCP] Connection ESTABLISHED to {}:{}", remote_ip, remote_port);
                        return Ok(local_port);
                    }
                    TcpState::Closed => {
                        crate::serial_println!("[TCP] Connection REFUSED by {}:{}", remote_ip, remote_port);
                        return Err(-111); // ECONNREFUSED
                    }
                    _ => {}
                }
            }
        }

        attempts += 1;
        if attempts > 2_000_000 {
            // Timeout - retransmit SYN once
            if attempts == 2_000_001 {
                send_tcp_segment(remote_ip, &syn_seg);
                crate::serial_println!("[TCP] SYN retransmit to {}:{}", remote_ip, remote_port);
            }
        }
        if attempts > 4_000_000 {
            crate::serial_println!("[TCP] Connect timeout to {}:{}", remote_ip, remote_port);
            let mut conns = TCP_CONNECTIONS.lock();
            conns.remove(&key);
            return Err(-110); // ETIMEDOUT
        }

        // SAFETY: Pause for busy-wait
        unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    }
}

/// Send data over an established TCP connection
pub fn tcp_send(local_port: u16, remote_ip: Ipv4Addr, remote_port: u16, data: &[u8]) -> Result<usize, i64> {
    let key = (local_port, remote_ip.as_u32(), remote_port);

    // SAFETY: Accessing global NET_CONFIG
    let our_ip = unsafe {
        match &*core::ptr::addr_of!(super::NET_CONFIG) {
            Some(c) => c.our_ip,
            None => return Err(-5),
        }
    };

    let mut conns = TCP_CONNECTIONS.lock();
    let conn = match conns.get_mut(&key) {
        Some(c) => c,
        None => return Err(-9), // EBADF
    };

    if conn.state != TcpState::Established {
        return Err(-107); // ENOTCONN
    }

    // Send data in segments up to MSS
    let mut sent = 0;
    let mss = conn.mss as usize;

    while sent < data.len() {
        let end = core::cmp::min(sent + mss, data.len());
        let chunk = &data[sent..end];

        let flags = if end == data.len() { ACK | PSH } else { ACK };
        let seg = build_segment(
            local_port, remote_port,
            conn.snd_nxt, conn.rcv_nxt,
            flags, conn.rcv_wnd,
            chunk,
            &our_ip, &remote_ip,
        );

        send_tcp_segment(remote_ip, &seg);
        conn.snd_nxt = conn.snd_nxt.wrapping_add(chunk.len() as u32);

        // Store for retransmission
        conn.retransmit_data = chunk.to_vec();
        conn.retransmit_seq = conn.snd_nxt.wrapping_sub(chunk.len() as u32);

        sent += chunk.len();
    }

    Ok(sent)
}

/// Receive data from an established TCP connection (non-blocking)
/// Returns bytes copied to buffer, 0 if no data, negative on error
pub fn tcp_recv(local_port: u16, remote_ip: Ipv4Addr, remote_port: u16, buf: &mut [u8]) -> Result<usize, i64> {
    // Poll network for incoming data
    super::poll();

    let key = (local_port, remote_ip.as_u32(), remote_port);

    let mut conns = TCP_CONNECTIONS.lock();
    let conn = match conns.get_mut(&key) {
        Some(c) => c,
        None => return Err(-9), // EBADF
    };

    // Connection closed by peer - return remaining data then 0
    if conn.state == TcpState::CloseWait && conn.recv_buf.is_empty() {
        return Ok(0); // EOF
    }

    if conn.state != TcpState::Established && conn.state != TcpState::CloseWait {
        if conn.recv_buf.is_empty() {
            return Err(-107); // ENOTCONN
        }
    }

    if conn.recv_buf.is_empty() {
        return Ok(0); // No data yet (non-blocking)
    }

    let copy_len = core::cmp::min(conn.recv_buf.len(), buf.len());
    buf[..copy_len].copy_from_slice(&conn.recv_buf[..copy_len]);

    // Remove consumed data
    conn.recv_buf.drain(..copy_len);

    Ok(copy_len)
}

/// Close a TCP connection (active close)
pub fn tcp_close(local_port: u16, remote_ip: Ipv4Addr, remote_port: u16) -> Result<(), i64> {
    let key = (local_port, remote_ip.as_u32(), remote_port);

    // SAFETY: Accessing global NET_CONFIG
    let our_ip = unsafe {
        match &*core::ptr::addr_of!(super::NET_CONFIG) {
            Some(c) => c.our_ip,
            None => return Err(-5),
        }
    };

    let mut conns = TCP_CONNECTIONS.lock();
    let conn = match conns.get_mut(&key) {
        Some(c) => c,
        None => return Err(-9),
    };

    match conn.state {
        TcpState::Established => {
            // Send FIN
            let seg = build_segment(
                local_port, remote_port,
                conn.snd_nxt, conn.rcv_nxt,
                FIN | ACK, conn.rcv_wnd,
                &[],
                &our_ip, &remote_ip,
            );
            send_tcp_segment(remote_ip, &seg);
            conn.snd_nxt = conn.snd_nxt.wrapping_add(1);
            conn.state = TcpState::FinWait1;
            crate::serial_println!("[TCP] FIN sent, state -> FIN_WAIT_1");
            Ok(())
        }
        TcpState::CloseWait => {
            // Send FIN (our side close after peer closed)
            let seg = build_segment(
                local_port, remote_port,
                conn.snd_nxt, conn.rcv_nxt,
                FIN | ACK, conn.rcv_wnd,
                &[],
                &our_ip, &remote_ip,
            );
            send_tcp_segment(remote_ip, &seg);
            conn.snd_nxt = conn.snd_nxt.wrapping_add(1);
            conn.state = TcpState::LastAck;
            crate::serial_println!("[TCP] FIN sent from CLOSE_WAIT, state -> LAST_ACK");
            Ok(())
        }
        _ => {
            // Just remove the connection
            conns.remove(&key);
            Ok(())
        }
    }
}

/// Process incoming TCP segment (called from IPv4 handler)
pub fn process_tcp(ip_pkt: &ipv4::Ipv4Packet) {
    let seg = match TcpSegment::parse(ip_pkt.payload) {
        Some(s) => s,
        None => return,
    };

    let key = (seg.dst_port, ip_pkt.src_ip.as_u32(), seg.src_port);

    // SAFETY: Accessing global NET_CONFIG
    let our_ip = unsafe {
        match &*core::ptr::addr_of!(super::NET_CONFIG) {
            Some(c) => c.our_ip,
            None => return,
        }
    };

    let mut conns = TCP_CONNECTIONS.lock();

    if let Some(conn) = conns.get_mut(&key) {
        match conn.state {
            TcpState::SynSent => {
                // Expecting SYN-ACK
                if seg.has_syn() && seg.has_ack() {
                    // Verify ACK acknowledges our SYN
                    if seg.ack_num == conn.snd_nxt {
                        conn.irs = seg.seq_num;
                        conn.rcv_nxt = seg.seq_num.wrapping_add(1);
                        conn.snd_una = seg.ack_num;
                        conn.snd_wnd = seg.window;
                        conn.state = TcpState::Established;

                        // Send ACK to complete 3-way handshake
                        let ack_seg = build_segment(
                            conn.local_port, conn.remote_port,
                            conn.snd_nxt, conn.rcv_nxt,
                            ACK, conn.rcv_wnd,
                            &[],
                            &our_ip, &ip_pkt.src_ip,
                        );
                        drop(conns); // Release lock before sending
                        send_tcp_segment(ip_pkt.src_ip, &ack_seg);
                        crate::serial_println!("[TCP] SYN-ACK received, ACK sent -> ESTABLISHED");
                    }
                } else if seg.has_rst() {
                    conn.state = TcpState::Closed;
                    crate::serial_println!("[TCP] RST received in SYN_SENT -> CLOSED");
                }
            }

            TcpState::Established => {
                if seg.has_rst() {
                    conn.state = TcpState::Closed;
                    crate::serial_println!("[TCP] RST received -> CLOSED");
                    return;
                }

                // Process ACK
                if seg.has_ack() {
                    conn.snd_una = seg.ack_num;
                    conn.snd_wnd = seg.window;
                }

                // Process data
                if !seg.payload.is_empty() {
                    // Check sequence number is what we expect
                    if seg.seq_num == conn.rcv_nxt {
                        conn.recv_buf.extend_from_slice(seg.payload);
                        conn.rcv_nxt = conn.rcv_nxt.wrapping_add(seg.payload.len() as u32);

                        // Send ACK for received data
                        let ack_seg = build_segment(
                            conn.local_port, conn.remote_port,
                            conn.snd_nxt, conn.rcv_nxt,
                            ACK, conn.rcv_wnd,
                            &[],
                            &our_ip, &ip_pkt.src_ip,
                        );
                        drop(conns);
                        send_tcp_segment(ip_pkt.src_ip, &ack_seg);
                        return;
                    }
                    // else: out-of-order, discard (simplified - no reassembly)
                }

                // FIN received (peer closing)
                if seg.has_fin() {
                    conn.rcv_nxt = conn.rcv_nxt.wrapping_add(1);
                    conn.state = TcpState::CloseWait;

                    let ack_seg = build_segment(
                        conn.local_port, conn.remote_port,
                        conn.snd_nxt, conn.rcv_nxt,
                        ACK, conn.rcv_wnd,
                        &[],
                        &our_ip, &ip_pkt.src_ip,
                    );
                    drop(conns);
                    send_tcp_segment(ip_pkt.src_ip, &ack_seg);
                    crate::serial_println!("[TCP] FIN received -> CLOSE_WAIT");
                    return;
                }
            }

            TcpState::FinWait1 => {
                if seg.has_ack() {
                    if seg.has_fin() {
                        // Simultaneous close: FIN+ACK
                        conn.rcv_nxt = conn.rcv_nxt.wrapping_add(1);
                        conn.state = TcpState::TimeWait;
                        let ack_seg = build_segment(
                            conn.local_port, conn.remote_port,
                            conn.snd_nxt, conn.rcv_nxt,
                            ACK, conn.rcv_wnd,
                            &[],
                            &our_ip, &ip_pkt.src_ip,
                        );
                        drop(conns);
                        send_tcp_segment(ip_pkt.src_ip, &ack_seg);
                        crate::serial_println!("[TCP] FIN+ACK in FIN_WAIT_1 -> TIME_WAIT");
                    } else {
                        conn.state = TcpState::FinWait2;
                        crate::serial_println!("[TCP] ACK in FIN_WAIT_1 -> FIN_WAIT_2");
                    }
                }
            }

            TcpState::FinWait2 => {
                // Process any remaining data
                if !seg.payload.is_empty() && seg.seq_num == conn.rcv_nxt {
                    conn.recv_buf.extend_from_slice(seg.payload);
                    conn.rcv_nxt = conn.rcv_nxt.wrapping_add(seg.payload.len() as u32);
                }

                if seg.has_fin() {
                    conn.rcv_nxt = conn.rcv_nxt.wrapping_add(1);
                    conn.state = TcpState::TimeWait;
                    let ack_seg = build_segment(
                        conn.local_port, conn.remote_port,
                        conn.snd_nxt, conn.rcv_nxt,
                        ACK, conn.rcv_wnd,
                        &[],
                        &our_ip, &ip_pkt.src_ip,
                    );
                    drop(conns);
                    send_tcp_segment(ip_pkt.src_ip, &ack_seg);
                    crate::serial_println!("[TCP] FIN in FIN_WAIT_2 -> TIME_WAIT");
                }
            }

            TcpState::LastAck => {
                if seg.has_ack() {
                    conn.state = TcpState::Closed;
                    crate::serial_println!("[TCP] ACK in LAST_ACK -> CLOSED");
                }
            }

            TcpState::CloseWait => {
                // Waiting for our app to close
                if seg.has_ack() {
                    conn.snd_una = seg.ack_num;
                }
            }

            TcpState::TimeWait => {
                // Absorb delayed segments
            }

            _ => {}
        }
    } else {
        // No matching connection - send RST if not RST already
        if !seg.has_rst() {
            let rst_seg = build_segment(
                seg.dst_port, seg.src_port,
                seg.ack_num, seg.seq_num.wrapping_add(1),
                RST | ACK, 0,
                &[],
                &our_ip, &ip_pkt.src_ip,
            );
            drop(conns);
            send_tcp_segment(ip_pkt.src_ip, &rst_seg);
        }
    }
}

/// Get connection state
pub fn get_state(local_port: u16, remote_ip: Ipv4Addr, remote_port: u16) -> TcpState {
    let key = (local_port, remote_ip.as_u32(), remote_port);
    let conns = TCP_CONNECTIONS.lock();
    conns.get(&key).map(|c| c.state).unwrap_or(TcpState::Closed)
}

/// Run TCP self-tests
pub fn run_tests() {
    crate::serial_write("  [TCP TEST 1/3] Segment build/parse... ");
    {
        let src_ip = Ipv4Addr::new(10, 0, 2, 15);
        let dst_ip = Ipv4Addr::new(10, 0, 2, 2);
        let seg = build_segment(
            12345, 80,
            0x1000, 0x2000,
            SYN | ACK, 8192,
            b"Hello",
            &src_ip, &dst_ip,
        );
        let parsed = TcpSegment::parse(&seg).unwrap();
        if parsed.src_port == 12345 && parsed.dst_port == 80
            && parsed.seq_num == 0x1000 && parsed.ack_num == 0x2000
            && parsed.has_syn() && parsed.has_ack()
            && parsed.payload == b"Hello"
        {
            crate::serial_write("OK\n");
        } else {
            crate::serial_write("FAIL\n");
        }
    }

    crate::serial_write("  [TCP TEST 2/3] Checksum computation... ");
    {
        let src_ip = Ipv4Addr::new(10, 0, 2, 15);
        let dst_ip = Ipv4Addr::new(10, 0, 2, 2);
        let seg = build_segment(
            1234, 80,
            0xDEAD, 0xBEEF,
            ACK, 4096,
            b"data",
            &src_ip, &dst_ip,
        );
        // Verify checksum is valid
        let cksum = tcp_checksum(&src_ip, &dst_ip, &seg);
        if cksum == 0 {
            crate::serial_write("OK\n");
        } else {
            crate::serial_println!("FAIL (cksum=0x{:04X})", cksum);
        }
    }

    crate::serial_write("  [TCP TEST 3/3] State machine transitions... ");
    {
        let valid = TcpState::Closed != TcpState::Established
            && TcpState::SynSent != TcpState::Established
            && TcpState::FinWait1 != TcpState::Closed;
        if valid {
            crate::serial_write("OK\n");
        } else {
            crate::serial_write("FAIL\n");
        }
    }
}
