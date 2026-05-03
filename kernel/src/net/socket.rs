// kernel/src/net/socket.rs - Socket Abstraction for Userspace
//
// Provides syscall-accessible network sockets:
//   - SOCK_STREAM (TCP)
//   - SOCK_DGRAM (UDP)
//   - SOCK_RAW (ICMP)
//
// Session 13: Socket FDs now registered in process FD table as FdType::Socket
// so that read(2)/write(2) on socket FDs are properly routed through the
// unified FD dispatch (Jalon 79).
//
// Socket API:
//   sys_socket(domain, type, protocol) -> fd
//   sys_connect(fd, ip_a, ip_b, ip_c, ip_d, port) -> 0 or error
//   sys_sendto(fd, buf, len, flags, addr, addrlen) -> ssize_t
//   sys_recvfrom(fd, buf, len, flags, addr, addrlen) -> ssize_t
//   sys_tcp_send(fd, buf_addr, len) -> bytes_sent
//   sys_tcp_recv_blocking(fd, buf_addr, len) -> bytes_read

use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use alloc::string::String;
use spin::Mutex;
use lazy_static::lazy_static;

use super::ipv4::Ipv4Addr;
use super::tls;

/// Socket types
pub const SOCK_STREAM: u32 = 1;
pub const SOCK_RAW: u32 = 3;
pub const SOCK_DGRAM: u32 = 2;

/// Protocol numbers
pub const IPPROTO_ICMP: u32 = 1;
pub const IPPROTO_TCP: u32 = 6;
pub const IPPROTO_UDP: u32 = 17;

/// Address families
pub const AF_INET: u32 = 2;
pub const AF_PACKET: u32 = 17;
pub const AF_NETLINK: u32 = 16;

/// Maximum pending received packets per socket
const MAX_RECV_QUEUE: usize = 16;

/// Maximum packet size
const MAX_PACKET_SIZE: usize = 1500;

/// A received datagram
#[derive(Clone)]
pub struct RecvDatagram {
    pub src_ip: Ipv4Addr,
    pub src_port: u16,
    pub data: Vec<u8>,
}

/// A network socket
pub struct Socket {
    pub domain: u32,
    pub sock_type: u32,
    pub protocol: u32,
    pub bind_port: u16,
    pub recv_queue: Vec<RecvDatagram>,
    // TCP connection state
    pub tcp_local_port: u16,
    pub tcp_remote_ip: super::ipv4::Ipv4Addr,
    pub tcp_remote_port: u16,
    pub tcp_connected: bool,
    // TLS state (for HTTPS connections to port 443)
    pub tls_active: bool,
}

// Socket file descriptor table (internal socket state, keyed by socket_id)
lazy_static! {
    pub static ref SOCKET_TABLE: Mutex<BTreeMap<u32, Socket>> = Mutex::new(BTreeMap::new());
}

/// Next socket ID (internal, not exposed to userspace directly)
static NEXT_SOCKET_ID: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(100);

/// Create a new socket and register it in the process FD table.
/// Returns the process-local fd (small integer, 3+) on success, or negative error.
pub fn sys_socket(domain: u32, sock_type: u32, protocol: u32) -> u64 {
    // Jalon 110d: Support AF_PACKET and AF_NETLINK for nmap/raw socket tools
    match domain {
        AF_INET => {
            match sock_type & 0xF { // mask out SOCK_NONBLOCK/SOCK_CLOEXEC flags
                1 => {}, // SOCK_STREAM (TCP)
                2 => {}, // SOCK_DGRAM (UDP)
                3 => {}, // SOCK_RAW (ICMP/raw)
                _ => return (-22i64) as u64, // EINVAL
            }
        }
        AF_PACKET => {
            crate::serial_println!("[SOCKET] AF_PACKET socket requested (type={}, proto={})", sock_type, protocol);
        }
        AF_NETLINK => {
            crate::serial_println!("[SOCKET] AF_NETLINK socket requested (type={}, proto={})", sock_type, protocol);
        }
        _ => return (-93i64) as u64, // EPROTONOSUPPORT
    }

    let socket_id = NEXT_SOCKET_ID.fetch_add(1, core::sync::atomic::Ordering::SeqCst);

    let effective_proto = if protocol == 0 {
        match sock_type & 0xF {
            1 => IPPROTO_TCP,
            2 => IPPROTO_UDP,
            3 => IPPROTO_ICMP,
            _ => IPPROTO_UDP,
        }
    } else {
        protocol
    };

    let socket = Socket {
        domain,
        sock_type: sock_type & 0xF, // strip flags
        protocol: effective_proto,
        bind_port: 0,
        recv_queue: Vec::new(),
        tcp_local_port: 0,
        tcp_remote_ip: super::ipv4::Ipv4Addr::new(0, 0, 0, 0),
        tcp_remote_port: 0,
        tcp_connected: false,
        tls_active: false,
    };

    {
        let mut table = SOCKET_TABLE.lock();
        table.insert(socket_id, socket);
    }

    // Register in process FD table as FdType::Socket
    let current_pid = crate::scheduler::current_pid();
    let fd = crate::process::with_fd_table_mut(current_pid, |fdt| {
        fdt.alloc_socket_fd(socket_id)
    });

    match fd {
        Some(Some(process_fd)) => {
            crate::serial_println!("[SOCKET] Created socket fd={} socket_id={} type={} proto={}",
                process_fd, socket_id, sock_type & 0xF, effective_proto);
            process_fd as u64
        }
        _ => {
            // FD table full, clean up socket
            let mut table = SOCKET_TABLE.lock();
            table.remove(&socket_id);
            crate::serial_println!("[SOCKET] FD table full, cannot create socket");
            (-24i64) as u64 // EMFILE
        }
    }
}

/// Send data through a socket
/// For ICMP: addr is (ip_a, ip_b, ip_c, ip_d, 0, 0) packed in a3
/// For UDP: addr is (ip, port) encoded
pub fn sys_sendto(fd: u32, buf_addr: u64, len: u64, _flags: u64, dest_ip: Ipv4Addr, dest_port: u16) -> u64 {
    // Resolve socket_id from process FD table
    let socket_id = resolve_socket_id(fd);
    let socket_id = match socket_id {
        Some(id) => id,
        None => {
            // Legacy: try fd directly as socket_id
            let table = SOCKET_TABLE.lock();
            if table.contains_key(&fd) { fd } else { return (-9i64) as u64; } // EBADF
        }
    };

    let mut table = SOCKET_TABLE.lock();
    let socket = match table.get_mut(&socket_id) {
        Some(s) => s,
        None => return (-9i64) as u64, // EBADF
    };

    // Read data from user buffer (KPTI-safe)
    let data = {
        let mut v = alloc::vec![0u8; len as usize];
        let copied = unsafe { crate::arch::x86_64::syscall::copy_from_user_pub(
            &mut v, buf_addr, len as usize,
        ) };
        if copied == 0 && len > 0 {
            return (-14i64) as u64; // EFAULT
        }
        v.truncate(copied);
        v
    };

    match socket.protocol {
        IPPROTO_ICMP => {
            drop(table);
            let seq = (len & 0xFFFF) as u16;
            super::send_ping(dest_ip, seq);
            data.len() as u64
        }
        IPPROTO_UDP => {
            let mut src_port = socket.bind_port;
            // Auto-assign ephemeral port if not bound
            if src_port == 0 {
                static NEXT_UDP_PORT: core::sync::atomic::AtomicU16 = core::sync::atomic::AtomicU16::new(49152);
                src_port = NEXT_UDP_PORT.fetch_add(1, core::sync::atomic::Ordering::SeqCst);
                if src_port == 0 || src_port < 49152 {
                    src_port = 49152;
                    NEXT_UDP_PORT.store(49153, core::sync::atomic::Ordering::SeqCst);
                }
                socket.bind_port = src_port;
                crate::serial_println!("[UDP] Auto-bound socket {} to port {}", socket_id, src_port);
            }
            drop(table);
            // ═══ DNS Interception: Extract hostname from DNS queries to port 53 ═══
            // BusyBox/musl's getaddrinfo sends DNS queries via sendto(udp_fd, ..., dns:53).
            // We intercept these to extract the queried hostname for TLS SNI.
            if dest_port == 53 && data.len() > 12 {
                // DNS query format: 2B txid + 2B flags + 2B qdcount + 6B counts + QNAME
                // QNAME starts at offset 12: length-prefixed labels (e.g., \x03www\x07example\x03com\x00)
                let mut hostname = alloc::string::String::new();
                let mut pos = 12;
                while pos < data.len() {
                    let label_len = data[pos] as usize;
                    if label_len == 0 { break; }
                    pos += 1;
                    if pos + label_len > data.len() { break; }
                    if !hostname.is_empty() { hostname.push('.'); }
                    if let Ok(label) = core::str::from_utf8(&data[pos..pos + label_len]) {
                        hostname.push_str(label);
                    }
                    pos += label_len;
                }
                if !hostname.is_empty() {
                    crate::serial_println!("[DNS-SNIFF] Query for '{}' -> storing SNI", hostname);
                    set_sni_for_next_connect(&hostname);
                }
            }
            if super::send_udp(dest_ip, src_port, dest_port, &data) {
                data.len() as u64
            } else {
                (-5i64) as u64 // EIO
            }
        }
        _ => (-22i64) as u64, // EINVAL
    }
}

/// Receive data from a socket (handles both TCP and UDP)
pub fn sys_recvfrom(fd: u32, buf_addr: u64, len: u64) -> u64 {
    // Resolve socket_id from process FD table
    let socket_id = resolve_socket_id(fd).unwrap_or(fd);

    // Check if this is a TCP socket — route to blocking TCP recv
    {
        let table = SOCKET_TABLE.lock();
        if let Some(s) = table.get(&socket_id) {
            if s.tcp_connected {
                drop(table);
                return sys_tcp_recv_blocking_by_id(socket_id, buf_addr, len);
            }
        }
    }

    // Poll network first (for UDP/ICMP)
    super::poll();

    let mut table = SOCKET_TABLE.lock();
    let socket = match table.get_mut(&socket_id) {
        Some(s) => s,
        None => return (-9i64) as u64, // EBADF
    };

    if socket.recv_queue.is_empty() {
        // For ICMP sockets, check the ping reply buffer
        if socket.protocol == IPPROTO_ICMP {
            let mut replies = super::PING_REPLIES.lock();
            if let Some((&seq, &(ip, _rtt))) = replies.iter().next() {
                replies.remove(&seq);
                drop(replies);

                let reply_data = alloc::format!("PONG from {} seq={}", ip, seq);
                let bytes = reply_data.as_bytes();
                let copy_len = core::cmp::min(bytes.len(), len as usize);
                unsafe { crate::arch::x86_64::syscall::copy_to_user_pub(
                    buf_addr, &bytes[..copy_len],
                ) };
                return copy_len as u64;
            }
        }

        // ═══ Blocking UDP recvfrom: poll network until data arrives ═══
        // DNS responses typically arrive within 10-100ms. Block for up to ~2s.
        if socket.protocol == IPPROTO_UDP {
            let bind_port = socket.bind_port;
            drop(table); // Release lock before blocking

            for attempt in 0..2_000_000u64 {
                // Poll network (processes incoming packets)
                if attempt % 100 == 0 {
                    super::poll();
                }
                // Enable interrupts briefly to allow VirtIO IRQ
                if attempt % 1000 == 0 {
                    unsafe { core::arch::asm!("sti; pause; cli", options(nomem, nostack)); }
                }

                // Check if data arrived
                let mut tbl = SOCKET_TABLE.lock();
                if let Some(sock) = tbl.get_mut(&socket_id) {
                    if !sock.recv_queue.is_empty() {
                        let datagram = sock.recv_queue.remove(0);
                        drop(tbl);
                        let copy_len = core::cmp::min(datagram.data.len(), len as usize);
                        unsafe { crate::arch::x86_64::syscall::copy_to_user_pub(
                            buf_addr, &datagram.data[..copy_len],
                        ) };
                        return copy_len as u64;
                    }
                }
                drop(tbl);

                // Brief pause
                unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
            }
            crate::serial_println!("[UDP-RECV] timeout after blocking wait (port={})", bind_port);
            return 0; // Timeout
        }

        return 0; // No data available (non-UDP)
    }

    // Pop from receive queue
    let datagram = socket.recv_queue.remove(0);
    let copy_len = core::cmp::min(datagram.data.len(), len as usize);
    unsafe { crate::arch::x86_64::syscall::copy_to_user_pub(
        buf_addr, &datagram.data[..copy_len],
    ) };

    copy_len as u64
}

/// Bind a socket to a port
pub fn sys_bind(fd: u32, port: u16) -> u64 {
    let socket_id = resolve_socket_id(fd).unwrap_or(fd);
    let mut table = SOCKET_TABLE.lock();
    match table.get_mut(&socket_id) {
        Some(socket) => {
            socket.bind_port = port;
            crate::serial_println!("[SOCKET] fd={} (sid={}) bound to port {}", fd, socket_id, port);
            0
        }
        None => (-9i64) as u64, // EBADF
    }
}

/// TCP connect: initiate 3-way handshake (with transparent TLS for port 443)
pub fn sys_connect(fd: u32, ip_a: u8, ip_b: u8, ip_c: u8, ip_d: u8, port: u16) -> u64 {
    let remote_ip = super::ipv4::Ipv4Addr::new(ip_a, ip_b, ip_c, ip_d);
    let socket_id = resolve_socket_id(fd).unwrap_or(fd);

    // Verify socket exists and is a TCP socket
    {
        let table = SOCKET_TABLE.lock();
        match table.get(&socket_id) {
            Some(s) if s.sock_type == SOCK_STREAM || s.sock_type == (SOCK_STREAM | 0x800) => {},
            Some(s) => {
                crate::serial_println!("[SOCKET] connect: fd={} sid={} not TCP (type={})", fd, socket_id, s.sock_type);
                return (-22i64) as u64; // EINVAL
            }
            None => {
                crate::serial_println!("[SOCKET] connect: fd={} sid={} not found", fd, socket_id);
                return (-9i64) as u64; // EBADF
            }
        }
    }

    crate::serial_println!("[SOCKET] sys_connect fd={} (sid={}) -> {}:{}", fd, socket_id, remote_ip, port);

    // For port 443 (HTTPS), use TLS
    if port == 443 {
        crate::serial_println!("[SOCKET] Port 443 detected, initiating TLS connection");
        // Use IP as SNI fallback (proper SNI comes from hostname resolution)
        let sni = get_sni_for_socket(socket_id);
        let sni_str = sni.as_deref().unwrap_or("");

        match tls::tls_connect(remote_ip, port, sni_str) {
            Ok(tls_conn) => {
                // Store TLS connection
                let local_port = tls_conn.local_port;
                {
                    let mut tls_table = TLS_TABLE.lock();
                    tls_table.insert(socket_id, tls_conn);
                }
                // Update socket with connection info
                {
                    let mut table = SOCKET_TABLE.lock();
                    if let Some(socket) = table.get_mut(&socket_id) {
                        socket.tcp_local_port = local_port;
                        socket.tcp_remote_ip = remote_ip;
                        socket.tcp_remote_port = port;
                        socket.tcp_connected = true;
                        socket.tls_active = true;
                    }
                }
                crate::serial_println!("[SOCKET] TLS+TCP connected fd={} sid={}", fd, socket_id);
                0
            }
            Err(e) => {
                crate::serial_println!("[SOCKET] TLS connect failed: {}", e);
                e as u64
            }
        }
    } else {
        // Plain TCP connect
        match super::tcp::tcp_connect(remote_ip, port) {
            Ok(local_port) => {
                let mut table = SOCKET_TABLE.lock();
                if let Some(socket) = table.get_mut(&socket_id) {
                    socket.tcp_local_port = local_port;
                    socket.tcp_remote_ip = remote_ip;
                    socket.tcp_remote_port = port;
                    socket.tcp_connected = true;
                }
                crate::serial_println!("[SOCKET] TCP connected fd={} sid={} local_port={}", fd, socket_id, local_port);
                0 // Success
            }
            Err(e) => e as u64, // Negative error code
        }
    }
}

/// TCP send data (by process fd) — transparently handles TLS if active
pub fn sys_tcp_send(fd: u32, buf_addr: u64, len: u64) -> u64 {
    let socket_id = resolve_socket_id(fd).unwrap_or(fd);

    let (local_port, remote_ip, remote_port, connected, is_tls) = {
        let table = SOCKET_TABLE.lock();
        match table.get(&socket_id) {
            Some(s) if s.tcp_connected => (
                s.tcp_local_port, s.tcp_remote_ip, s.tcp_remote_port, true, s.tls_active
            ),
            _ => return (-9i64) as u64,
        }
    };

    if !connected {
        return (-107i64) as u64; // ENOTCONN
    }

    // Read data from user buffer (KPTI-safe)
    let data = {
        let actual_len = core::cmp::min(len as usize, 65536); // Cap at 64KB
        let mut v = alloc::vec![0u8; actual_len];
        let copied = unsafe { crate::arch::x86_64::syscall::copy_from_user_pub(
            &mut v, buf_addr, actual_len,
        ) };
        if copied == 0 && len > 0 {
            return (-14i64) as u64; // EFAULT
        }
        v.truncate(copied);
        v
    };

    if is_tls {
        // Send through TLS
        let mut tls_table = TLS_TABLE.lock();
        if let Some(tls_conn) = tls_table.get_mut(&socket_id) {
            match tls::tls_send(tls_conn, &data) {
                Ok(sent) => sent as u64,
                Err(e) => e as u64,
            }
        } else {
            (-107i64) as u64 // ENOTCONN
        }
    } else {
        match super::tcp::tcp_send(local_port, remote_ip, remote_port, &data) {
            Ok(sent) => sent as u64,
            Err(e) => e as u64,
        }
    }
}

/// TCP receive data (non-blocking, by process fd)
pub fn sys_tcp_recv(fd: u32, buf_addr: u64, len: u64) -> u64 {
    let socket_id = resolve_socket_id(fd).unwrap_or(fd);

    let (local_port, remote_ip, remote_port, connected) = {
        let table = SOCKET_TABLE.lock();
        match table.get(&socket_id) {
            Some(s) if s.tcp_connected => (s.tcp_local_port, s.tcp_remote_ip, s.tcp_remote_port, true),
            _ => return (-9i64) as u64,
        }
    };

    if !connected {
        return (-107i64) as u64; // ENOTCONN
    }

    let actual_len = core::cmp::min(len as usize, 65536);
    let mut temp_buf = alloc::vec![0u8; actual_len];

    match super::tcp::tcp_recv(local_port, remote_ip, remote_port, &mut temp_buf) {
        Ok(bytes_read) => {
            if bytes_read > 0 {
                unsafe { crate::arch::x86_64::syscall::copy_to_user_pub(
                    buf_addr, &temp_buf[..bytes_read],
                ) };
            }
            bytes_read as u64
        }
        Err(e) => e as u64,
    }
}

/// TCP shutdown
pub fn sys_tcp_shutdown(fd: u32) -> u64 {
    let socket_id = resolve_socket_id(fd).unwrap_or(fd);

    let (local_port, remote_ip, remote_port, connected) = {
        let table = SOCKET_TABLE.lock();
        match table.get(&socket_id) {
            Some(s) if s.tcp_connected => (s.tcp_local_port, s.tcp_remote_ip, s.tcp_remote_port, true),
            _ => return (-9i64) as u64,
        }
    };

    if !connected {
        return 0; // Already closed
    }

    let result = match super::tcp::tcp_close(local_port, remote_ip, remote_port) {
        Ok(()) => 0u64,
        Err(e) => e as u64,
    };

    // Mark socket as disconnected
    {
        let mut table = SOCKET_TABLE.lock();
        if let Some(socket) = table.get_mut(&socket_id) {
            socket.tcp_connected = false;
        }
    }

    result
}

/// DNS resolution: gethostbyname
pub fn sys_gethostbyname(name_addr: u64) -> u64 {
    // Read domain name from user space using copy_from_user (KPTI-safe)
    let domain = {
        let mut raw = [0u8; 256];
        let copied = unsafe { crate::arch::x86_64::syscall::copy_from_user_pub(
            &mut raw, name_addr, 256,
        ) };
        if copied == 0 {
            return (-14i64) as u64; // EFAULT
        }
        let len = raw.iter().position(|&b| b == 0).unwrap_or(copied);
        match alloc::string::String::from_utf8(raw[..len].to_vec()) {
            Ok(s) => s,
            Err(_) => return (-22i64) as u64, // EINVAL
        }
    };

    crate::serial_println!("[SOCKET] sys_gethostbyname('{}')", domain);

    match super::dns::resolve(&domain) {
        Ok(ip) => {
            let octets = ip.0;
            let packed = ((octets[0] as u64) << 24)
                       | ((octets[1] as u64) << 16)
                       | ((octets[2] as u64) << 8)
                       | (octets[3] as u64);
            crate::serial_println!("[SOCKET] DNS resolved: {} -> 0x{:08X}", domain, packed);
            // Store hostname for TLS SNI (used by subsequent connect to port 443)
            set_sni_for_next_connect(&domain);
            packed
        }
        Err(e) => e as u64,
    }
}

/// Close a socket and clean up resources
pub fn sys_socket_close(fd: u32) -> u64 {
    let socket_id = resolve_socket_id(fd).unwrap_or(fd);

    // Check if TLS is active and close TLS connection first
    {
        let mut tls_table = TLS_TABLE.lock();
        if let Some(mut tls_conn) = tls_table.remove(&socket_id) {
            let _ = tls::tls_close(&mut tls_conn);
            crate::serial_println!("[SOCKET] TLS connection closed for sid={}", socket_id);
        }
    }

    // If TCP and connected, do TCP shutdown
    {
        let table = SOCKET_TABLE.lock();
        if let Some(s) = table.get(&socket_id) {
            if s.tcp_connected && !s.tls_active {
                // Only close TCP directly if not TLS (TLS close already closed TCP)
                let (lp, rip, rp) = (s.tcp_local_port, s.tcp_remote_ip, s.tcp_remote_port);
                drop(table);
                let _ = super::tcp::tcp_close(lp, rip, rp);
            }
        }
    }
    // Remove from socket table
    let mut table = SOCKET_TABLE.lock();
    match table.remove(&socket_id) {
        Some(_) => {
            crate::serial_println!("[SOCKET] fd={} sid={} closed", fd, socket_id);
            0
        }
        None => (-9i64) as u64, // EBADF
    }
}

/// TCP receive with blocking poll (timeout ~2s worth of iterations)
/// Polls the network repeatedly to wait for incoming data before returning 0.
/// Session 13: Increased timeout, improved polling efficiency.
pub fn sys_tcp_recv_blocking(fd: u32, buf_addr: u64, len: u64) -> u64 {
    let socket_id = resolve_socket_id(fd).unwrap_or(fd);
    sys_tcp_recv_blocking_by_id(socket_id, buf_addr, len)
}

/// Internal blocking recv by socket_id — transparently handles TLS if active
fn sys_tcp_recv_blocking_by_id(socket_id: u32, buf_addr: u64, len: u64) -> u64 {
    let (local_port, remote_ip, remote_port, connected, is_tls) = {
        let table = SOCKET_TABLE.lock();
        match table.get(&socket_id) {
            Some(s) if s.tcp_connected => (
                s.tcp_local_port, s.tcp_remote_ip, s.tcp_remote_port, true, s.tls_active
            ),
            _ => return (-9i64) as u64,
        }
    };

    if !connected {
        return (-107i64) as u64; // ENOTCONN
    }

    let actual_len = core::cmp::min(len as usize, 65536);

    // TLS path: decrypt through TLS layer
    if is_tls {
        let mut temp_buf = alloc::vec![0u8; actual_len];
        let mut tls_table = TLS_TABLE.lock();
        if let Some(tls_conn) = tls_table.get_mut(&socket_id) {
            match tls::tls_recv(tls_conn, &mut temp_buf) {
                Ok(bytes_read) if bytes_read > 0 => {
                    unsafe { crate::arch::x86_64::syscall::copy_to_user_pub(
                        buf_addr, &temp_buf[..bytes_read],
                    ) };
                    return bytes_read as u64;
                }
                Ok(_) => return 0, // EOF or timeout
                Err(e) => return e as u64,
            }
        } else {
            return (-107i64) as u64; // ENOTCONN
        }
    }

    // Plain TCP path — poll for up to ~5 seconds
    for attempt in 0..5_000_000u32 {
        if attempt % 5 == 0 {
            super::poll();
        }

        let data_available = super::tcp::recv_buf_len(local_port, remote_ip, remote_port);
        if data_available > 0 {
            let mut temp_buf = alloc::vec![0u8; actual_len];
            match super::tcp::tcp_recv(local_port, remote_ip, remote_port, &mut temp_buf) {
                Ok(bytes_read) if bytes_read > 0 => {
                    unsafe { crate::arch::x86_64::syscall::copy_to_user_pub(
                        buf_addr, &temp_buf[..bytes_read],
                    ) };
                    return bytes_read as u64;
                }
                _ => {}
            }
        }

        if attempt % 1000 == 0 {
            let state = super::tcp::get_state(local_port, remote_ip, remote_port);
            if state == super::tcp::TcpState::CloseWait || state == super::tcp::TcpState::Closed
                || state == super::tcp::TcpState::TimeWait {
                let remaining = super::tcp::recv_buf_len(local_port, remote_ip, remote_port);
                if remaining > 0 {
                    let mut temp_buf = alloc::vec![0u8; core::cmp::min(remaining, actual_len)];
                    if let Ok(n) = super::tcp::tcp_recv(local_port, remote_ip, remote_port, &mut temp_buf) {
                        if n > 0 {
                            unsafe { crate::arch::x86_64::syscall::copy_to_user_pub(
                                buf_addr, &temp_buf[..n],
                            ) };
                            return n as u64;
                        }
                    }
                }
                return 0; // EOF
            }
        }

        unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    }

    0 // No data after timeout
}

/// Deliver an incoming UDP packet to the appropriate socket
pub fn deliver_udp(src_ip: Ipv4Addr, src_port: u16, dst_port: u16, data: &[u8]) {
    let mut table = SOCKET_TABLE.lock();
    for (_fd, socket) in table.iter_mut() {
        if socket.protocol == IPPROTO_UDP && socket.bind_port == dst_port {
            if socket.recv_queue.len() < MAX_RECV_QUEUE {
                socket.recv_queue.push(RecvDatagram {
                    src_ip,
                    src_port,
                    data: Vec::from(data),
                });
            }
        }
    }
}

/// Resolve a process fd to a socket_id by looking up the FD table
fn resolve_socket_id(fd: u32) -> Option<u32> {
    let current_pid = crate::scheduler::current_pid();
    crate::process::with_fd_table(current_pid, |fdt| {
        if let Some(entry) = fdt.get(fd as usize) {
            if entry.fd_type == crate::process::FdType::Socket {
                return Some(entry.socket_id);
            }
        }
        None
    }).flatten()
}

// ── TLS Connection Table ──
// Maps socket_id -> TlsConnection for active TLS sessions
lazy_static! {
    static ref TLS_TABLE: Mutex<BTreeMap<u32, tls::TlsConnection>> = Mutex::new(BTreeMap::new());
}

// ── SNI (Server Name Indication) Table ──
// Maps socket_id -> hostname for TLS SNI extension
// Set by gethostbyname before connect() is called
lazy_static! {
    static ref SNI_TABLE: Mutex<BTreeMap<u32, String>> = Mutex::new(BTreeMap::new());
}

/// Store the hostname for a socket's SNI (called during DNS resolution)
pub fn set_sni_for_next_connect(hostname: &str) {
    // Store the hostname for the most recently resolved domain
    // This is used by sys_connect when establishing a TLS connection
    let mut table = SNI_TABLE.lock();
    // Use 0 as a "pending" key — sys_connect will pick it up
    table.insert(0, String::from(hostname));
}

/// Get and consume the SNI hostname for a socket
fn get_sni_for_socket(_socket_id: u32) -> Option<String> {
    let mut table = SNI_TABLE.lock();
    table.remove(&0) // Consume the pending hostname
}
