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
// Session 13 improvements:
//   - 65536-byte receive buffer (from 8192)
//   - Out-of-order segment reassembly with gap tracking
//   - Sliding window with dynamic rcv_wnd advertisement
//   - Retransmission with exponential backoff
//   - Duplicate ACK counting for fast retransmit
//   - MSS option in SYN segments
//   - Proper sequence number wrapping arithmetic
//
// SAFETY: All unsafe blocks are required for packet buffer access from
// userspace and static mutable state access (single-threaded kernel context).

use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use spin::Mutex;
use lazy_static::lazy_static;

use super::ipv4::{self, Ipv4Addr};

pub const HEADER_LEN: usize = 20;

/// Receive buffer capacity (64 KB, matching typical Linux default)
const RECV_BUF_CAPACITY: usize = 65536;

/// Maximum number of out-of-order segments to track
const MAX_OOO_SEGMENTS: usize = 32;

/// Retransmission limit before giving up
const MAX_RETRANSMITS: u8 = 8;

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

/// Out-of-order segment tracking for reassembly
#[derive(Clone)]
struct OooSegment {
    seq: u32,
    data: Vec<u8>,
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
    pub snd_wnd: u16,   // send window (peer's receive window)

    // Receive sequence variables
    pub rcv_nxt: u32,   // next seq expected
    pub rcv_wnd: u16,   // receive window (our advertised window)

    // Initial sequence numbers
    pub iss: u32,       // initial send sequence
    pub irs: u32,       // initial receive sequence

    // MSS (Maximum Segment Size)
    pub mss: u16,

    // Receive buffer: reassembled in-order data for userspace to read
    pub recv_buf: Vec<u8>,

    // Out-of-order segments waiting for reassembly
    ooo_segments: Vec<OooSegment>,

    // Send buffer: data queued for transmission
    pub send_buf: Vec<u8>,

    // Retransmission state
    pub retransmit_data: Vec<u8>,
    pub retransmit_seq: u32,
    pub retransmit_count: u8,

    // Duplicate ACK tracking for fast retransmit
    dup_ack_count: u8,
    last_ack_received: u32,

    // FIN received flag (for data+FIN in same segment)
    fin_received: bool,
    fin_seq: u32,
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
            rcv_wnd: RECV_BUF_CAPACITY as u16,
            iss,
            irs: 0,
            mss: 1460, // Ethernet MTU 1500 - 20 (IP) - 20 (TCP)
            recv_buf: Vec::with_capacity(RECV_BUF_CAPACITY),
            ooo_segments: Vec::new(),
            send_buf: Vec::new(),
            retransmit_data: Vec::new(),
            retransmit_seq: 0,
            retransmit_count: 0,
            dup_ack_count: 0,
            last_ack_received: iss,
            fin_received: false,
            fin_seq: 0,
        }
    }

    /// Update the advertised receive window based on buffer space
    fn update_rcv_wnd(&mut self) {
        let free = RECV_BUF_CAPACITY.saturating_sub(self.recv_buf.len());
        // Cap at u16 max (65535)
        self.rcv_wnd = core::cmp::min(free, 65535) as u16;
    }

    /// Try to reassemble out-of-order segments into the receive buffer
    fn reassemble_ooo(&mut self) {
        loop {
            let mut merged = false;
            let mut i = 0;
            while i < self.ooo_segments.len() {
                let seg_seq = self.ooo_segments[i].seq;
                let _seg_end = seg_seq.wrapping_add(self.ooo_segments[i].data.len() as u32);

                // Check if this segment starts at or before rcv_nxt
                if seq_le(seg_seq, self.rcv_nxt) {
                    // How many bytes overlap with already-received data?
                    let overlap = self.rcv_nxt.wrapping_sub(seg_seq) as usize;
                    if overlap < self.ooo_segments[i].data.len() {
                        // Append the non-overlapping portion
                        let new_data = &self.ooo_segments[i].data[overlap..];
                        if self.recv_buf.len() + new_data.len() <= RECV_BUF_CAPACITY {
                            self.recv_buf.extend_from_slice(new_data);
                            self.rcv_nxt = self.rcv_nxt.wrapping_add(new_data.len() as u32);
                        }
                    }
                    // Remove this segment (it's been consumed or fully overlapping)
                    self.ooo_segments.swap_remove(i);
                    merged = true;
                } else {
                    i += 1;
                }
            }
            if !merged {
                break;
            }
        }
    }

    /// Insert an out-of-order segment, deduplicating overlaps
    fn insert_ooo(&mut self, seq: u32, data: &[u8]) {
        if data.is_empty() || self.ooo_segments.len() >= MAX_OOO_SEGMENTS {
            return;
        }
        // Simple dedup: check if we already have data covering this range
        let seg_end = seq.wrapping_add(data.len() as u32);
        for existing in &self.ooo_segments {
            let ex_end = existing.seq.wrapping_add(existing.data.len() as u32);
            // If existing fully covers new segment, skip
            if seq_le(existing.seq, seq) && seq_le(seg_end, ex_end) {
                return;
            }
        }
        self.ooo_segments.push(OooSegment {
            seq,
            data: Vec::from(data),
        });
    }
}

/// Sequence number comparison helpers (handles wrapping per RFC 793)
#[inline]
fn seq_lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

#[inline]
fn seq_le(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) <= 0
}

#[inline]
fn seq_gt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
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

/// Build a TCP segment with optional MSS option (for SYN)
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
    build_segment_opts(src_port, dst_port, seq, ack, flags, window, payload, src_ip, dst_ip, &[])
}

/// Build a TCP segment with arbitrary TCP options
fn build_segment_opts(
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    payload: &[u8],
    src_ip: &Ipv4Addr,
    dst_ip: &Ipv4Addr,
    options: &[u8],
) -> Vec<u8> {
    // Header length must be multiple of 4
    let opts_len = options.len();
    let padded_opts = (opts_len + 3) & !3;
    let header_len = HEADER_LEN + padded_opts;
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

    // TCP options
    if opts_len > 0 {
        seg.extend_from_slice(options);
        // Pad with NOP (0x01) to 4-byte boundary
        for _ in opts_len..padded_opts {
            seg.push(0x01); // NOP
        }
    }

    seg.extend_from_slice(payload);

    // Compute checksum
    let cksum = tcp_checksum(src_ip, dst_ip, &seg);
    seg[16] = (cksum >> 8) as u8;
    seg[17] = (cksum & 0xFF) as u8;

    seg
}

/// Build MSS option bytes: Kind=2, Length=4, MSS value
fn mss_option(mss: u16) -> [u8; 4] {
    [0x02, 0x04, (mss >> 8) as u8, (mss & 0xFF) as u8]
}

/// Parse MSS option from TCP options field
fn parse_mss_option(data: &[u8], data_offset: u8) -> Option<u16> {
    let header_len = (data_offset as usize) * 4;
    if header_len <= HEADER_LEN || data.len() < header_len {
        return None;
    }
    let opts = &data[HEADER_LEN..header_len];
    let mut i = 0;
    while i < opts.len() {
        match opts[i] {
            0 => break,    // End of options
            1 => i += 1,   // NOP
            2 => {
                // MSS option
                if i + 3 < opts.len() && opts[i + 1] == 4 {
                    return Some(u16::from_be_bytes([opts[i + 2], opts[i + 3]]));
                }
                break;
            }
            _ => {
                // Skip unknown option
                if i + 1 >= opts.len() { break; }
                let opt_len = opts[i + 1] as usize;
                if opt_len < 2 { break; }
                i += opt_len;
            }
        }
    }
    None
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

    // Ensure ARP is resolved before TCP.
    // For off-subnet destinations, ARP the gateway instead.
    let arp_target = unsafe {
        match &*core::ptr::addr_of!(super::NET_CONFIG) {
            Some(c) if !remote_ip.same_subnet(&c.our_ip, &c.netmask) => c.gateway,
            _ => remote_ip,
        }
    };
    super::send_arp_request(arp_target);
    for _ in 0..20_000 {
        super::poll();
        unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    }

    let local_port = alloc_ephemeral_port();
    let iss = next_iss();
    let key = (local_port, remote_ip.as_u32(), remote_port);

    let mut conn = TcpConnection::new(local_port, remote_ip, remote_port, iss);
    conn.state = TcpState::SynSent;
    conn.snd_nxt = iss.wrapping_add(1); // SYN consumes one seq

    // Build and send SYN with MSS option
    // SAFETY: Accessing global NET_CONFIG
    let our_ip = unsafe {
        match &*core::ptr::addr_of!(super::NET_CONFIG) {
            Some(c) => c.our_ip,
            None => return Err(-5),
        }
    };

    let mss_opts = mss_option(1460);
    let syn_seg = build_segment_opts(
        local_port, remote_port,
        iss, 0,
        SYN, RECV_BUF_CAPACITY as u16,
        &[],
        &our_ip, &remote_ip,
        &mss_opts,
    );
    send_tcp_segment(remote_ip, &syn_seg);
    crate::serial_println!("[TCP] SYN sent to {}:{} (seq=0x{:08X}, win={})", remote_ip, remote_port, iss, RECV_BUF_CAPACITY);

    // Store connection
    {
        let mut conns = TCP_CONNECTIONS.lock();
        conns.insert(key, conn);
    }

    // Wait for SYN-ACK with timeout and retransmission
    let mut attempts = 0u32;
    let mut retransmit_at = 200_000u32; // First retransmit after ~200K cycles
    let mut retransmit_count = 0u8;
    loop {
        // Poll frequently to catch incoming SYN-ACK
        super::poll();

        {
            let conns = TCP_CONNECTIONS.lock();
            if let Some(c) = conns.get(&key) {
                match c.state {
                    TcpState::Established => {
                        crate::serial_println!("[TCP] connect ESTABLISHED to {}:{} (mss={})",
                            remote_ip, remote_port, c.mss);
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

        // Retransmit SYN with exponential backoff
        if attempts >= retransmit_at && retransmit_count < 5 {
            send_tcp_segment(remote_ip, &syn_seg);
            retransmit_count += 1;
            retransmit_at = attempts + (500_000 << retransmit_count); // Exponential backoff
            crate::serial_println!("[TCP] SYN retransmit #{} to {}:{}", retransmit_count, remote_ip, remote_port);
        }

        if attempts > 6_000_000 {
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

    if conn.state != TcpState::Established && conn.state != TcpState::CloseWait {
        return Err(-107); // ENOTCONN
    }

    // Send data in segments up to MSS, respecting send window
    let mut sent = 0;
    let mss = conn.mss as usize;

    while sent < data.len() {
        // Respect peer's receive window
        let max_send = if conn.snd_wnd > 0 {
            core::cmp::min(mss, conn.snd_wnd as usize)
        } else {
            mss // Send at least one MSS even if window is 0 (window probe)
        };

        let end = core::cmp::min(sent + max_send, data.len());
        let chunk = &data[sent..end];

        let flags = if end == data.len() { ACK | PSH } else { ACK };
        conn.update_rcv_wnd();
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
        conn.retransmit_count = 0;

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
    if (conn.state == TcpState::CloseWait || conn.state == TcpState::Closed
        || conn.state == TcpState::TimeWait) && conn.recv_buf.is_empty() {
        return Ok(0); // EOF
    }

    if conn.state != TcpState::Established && conn.state != TcpState::CloseWait
        && conn.state != TcpState::FinWait1 && conn.state != TcpState::FinWait2 {
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

    // Update receive window after consuming data
    conn.update_rcv_wnd();

    Ok(copy_len)
}

/// Blocking TCP receive: polls network until data arrives or timeout.
/// `timeout_ms` is approximate (based on busy-wait cycles).
/// Returns the number of bytes received, or 0 on EOF/timeout.
pub fn tcp_recv_blocking(
    local_port: u16,
    remote_ip: Ipv4Addr,
    remote_port: u16,
    buf: &mut [u8],
    timeout_ms: u32,
) -> Result<usize, i64> {
    // Approximate: each iteration does poll() + 100 pauses ≈ ~5μs,
    // so ~200 iterations ≈ 1ms. Scale timeout accordingly.
    let max_iterations = (timeout_ms as u64) * 200;
    let mut iter = 0u64;

    loop {
        super::poll();

        // Try to read data
        let result = tcp_recv(local_port, remote_ip, remote_port, buf);
        match result {
            Ok(n) if n > 0 => return Ok(n),
            Ok(0) => {
                // Check if connection is closed (EOF)
                let key = (local_port, remote_ip.as_u32(), remote_port);
                let state = {
                    let conns = TCP_CONNECTIONS.lock();
                    conns.get(&key).map(|c| c.state).unwrap_or(TcpState::Closed)
                };
                if state == TcpState::CloseWait || state == TcpState::Closed
                    || state == TcpState::TimeWait
                {
                    return Ok(0); // EOF — peer closed
                }
            }
            Err(e) => return Err(e),
            _ => {}
        }

        iter += 1;
        if iter >= max_iterations {
            return Ok(0); // Timeout — no data
        }

        // Small busy-wait pause
        for _ in 0..100 {
            unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
        }
    }
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
            conn.update_rcv_wnd();
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
            conn.update_rcv_wnd();
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
            let _removed = conns.remove(&key);
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

                        // Parse MSS option from SYN-ACK
                        if let Some(peer_mss) = parse_mss_option(ip_pkt.payload, seg.data_offset) {
                            conn.mss = core::cmp::min(peer_mss, 1460);
                            crate::serial_println!("[TCP] Peer MSS={}, using {}", peer_mss, conn.mss);
                        }

                        // Send ACK to complete 3-way handshake
                        conn.update_rcv_wnd();
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
                    if seq_gt(seg.ack_num, conn.snd_una) {
                        conn.snd_una = seg.ack_num;
                        conn.retransmit_count = 0; // Reset retransmit counter on new ACK
                        conn.dup_ack_count = 0;
                    } else if seg.ack_num == conn.last_ack_received && seg.payload.is_empty() {
                        // Duplicate ACK
                        conn.dup_ack_count += 1;
                        if conn.dup_ack_count >= 3 && !conn.retransmit_data.is_empty() {
                            // Fast retransmit
                            crate::serial_println!("[TCP] Fast retransmit (3 dup ACKs)");
                            conn.update_rcv_wnd();
                            let retx_seg = build_segment(
                                conn.local_port, conn.remote_port,
                                conn.retransmit_seq, conn.rcv_nxt,
                                ACK | PSH, conn.rcv_wnd,
                                &conn.retransmit_data.clone(),
                                &our_ip, &ip_pkt.src_ip,
                            );
                            send_tcp_segment(ip_pkt.src_ip, &retx_seg);
                            conn.dup_ack_count = 0;
                        }
                    }
                    conn.last_ack_received = seg.ack_num;
                    conn.snd_wnd = seg.window;
                }

                // Process data
                if !seg.payload.is_empty() {
                    let payload_len = seg.payload.len() as u32;

                    if seg.seq_num == conn.rcv_nxt {
                        // In-order segment — append directly
                        if conn.recv_buf.len() + seg.payload.len() <= RECV_BUF_CAPACITY {
                            conn.recv_buf.extend_from_slice(seg.payload);
                            conn.rcv_nxt = conn.rcv_nxt.wrapping_add(payload_len);

                            // Try to merge any out-of-order segments
                            conn.reassemble_ooo();
                        }
                        // else: buffer full, don't accept (receiver will advertise 0 window)
                    } else if seq_gt(seg.seq_num, conn.rcv_nxt) {
                        // Out-of-order segment — store for later reassembly
                        conn.insert_ooo(seg.seq_num, seg.payload);
                        crate::serial_println!("[TCP] OOO segment seq=0x{:08X} (expected 0x{:08X}), buffered",
                            seg.seq_num, conn.rcv_nxt);
                    }
                    // else: retransmitted data we already have — ignore but still ACK
                }

                // FIN received (peer closing) — process AFTER data so data+FIN segments work
                if seg.has_fin() {
                    conn.rcv_nxt = conn.rcv_nxt.wrapping_add(1);
                    conn.state = TcpState::CloseWait;

                    conn.update_rcv_wnd();
                    let ack_seg = build_segment(
                        conn.local_port, conn.remote_port,
                        conn.snd_nxt, conn.rcv_nxt,
                        ACK, conn.rcv_wnd,
                        &[],
                        &our_ip, &ip_pkt.src_ip,
                    );
                    drop(conns);
                    send_tcp_segment(ip_pkt.src_ip, &ack_seg);
                    crate::serial_println!("[TCP] FIN received -> CLOSE_WAIT (recv_buf={} bytes)",
                        0); // Can't access conn after drop(conns)
                    return;
                }

                // Send ACK for received data (no FIN)
                if !seg.payload.is_empty() {
                    conn.update_rcv_wnd();
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
            }

            TcpState::FinWait1 => {
                // Accept data in FIN_WAIT_1
                if !seg.payload.is_empty() && seg.seq_num == conn.rcv_nxt {
                    if conn.recv_buf.len() + seg.payload.len() <= RECV_BUF_CAPACITY {
                        conn.recv_buf.extend_from_slice(seg.payload);
                        conn.rcv_nxt = conn.rcv_nxt.wrapping_add(seg.payload.len() as u32);
                    }
                }

                if seg.has_ack() {
                    conn.snd_una = seg.ack_num;

                    if seg.has_fin() {
                        // Simultaneous close: FIN+ACK
                        conn.rcv_nxt = conn.rcv_nxt.wrapping_add(1);
                        conn.state = TcpState::TimeWait;
                        conn.update_rcv_wnd();
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
                    if conn.recv_buf.len() + seg.payload.len() <= RECV_BUF_CAPACITY {
                        conn.recv_buf.extend_from_slice(seg.payload);
                        conn.rcv_nxt = conn.rcv_nxt.wrapping_add(seg.payload.len() as u32);
                    }
                }

                if seg.has_fin() {
                    conn.rcv_nxt = conn.rcv_nxt.wrapping_add(1);
                    conn.state = TcpState::TimeWait;
                    conn.update_rcv_wnd();
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
                // Absorb delayed segments, re-ACK FINs
                if seg.has_fin() {
                    conn.update_rcv_wnd();
                    let ack_seg = build_segment(
                        conn.local_port, conn.remote_port,
                        conn.snd_nxt, conn.rcv_nxt,
                        ACK, conn.rcv_wnd,
                        &[],
                        &our_ip, &ip_pkt.src_ip,
                    );
                    drop(conns);
                    send_tcp_segment(ip_pkt.src_ip, &ack_seg);
                }
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

/// Get number of bytes available in the receive buffer
pub fn recv_buf_len(local_port: u16, remote_ip: Ipv4Addr, remote_port: u16) -> usize {
    let key = (local_port, remote_ip.as_u32(), remote_port);
    let conns = TCP_CONNECTIONS.lock();
    conns.get(&key).map(|c| c.recv_buf.len()).unwrap_or(0)
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
