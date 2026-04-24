// kernel/src/net/socket.rs - Socket Abstraction for Userspace
//
// Provides syscall-accessible network sockets:
//   - SOCK_DGRAM (UDP)
//   - SOCK_RAW (ICMP)
//
// Socket API:
//   sys_socket(domain, type, protocol) -> fd
//   sys_sendto(fd, buf, len, flags, addr, addrlen) -> ssize_t
//   sys_recvfrom(fd, buf, len, flags, addr, addrlen) -> ssize_t

use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use spin::Mutex;
use lazy_static::lazy_static;

use super::ipv4::Ipv4Addr;

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
}

// Socket file descriptor table
lazy_static! {
    pub static ref SOCKET_TABLE: Mutex<BTreeMap<u32, Socket>> = Mutex::new(BTreeMap::new());
}

/// Next socket file descriptor (start at 100 to avoid collision with VFS FDs)
static NEXT_SOCKET_FD: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(100);

/// Create a new socket
/// Returns socket fd or negative error
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
            // Raw packet access (nmap, tcpdump, Wireshark)
            crate::serial_println!("[SOCKET] AF_PACKET socket requested (type={}, proto={})", sock_type, protocol);
        }
        AF_NETLINK => {
            // Netlink socket (ip, ss commands)
            crate::serial_println!("[SOCKET] AF_NETLINK socket requested (type={}, proto={})", sock_type, protocol);
        }
        _ => return (-93i64) as u64, // EPROTONOSUPPORT
    }

    let fd = NEXT_SOCKET_FD.fetch_add(1, core::sync::atomic::Ordering::SeqCst);

    let socket = Socket {
        domain,
        sock_type,
        protocol: if protocol == 0 { IPPROTO_UDP } else { protocol },
        bind_port: 0, // unbound
        recv_queue: Vec::new(),
        tcp_local_port: 0,
        tcp_remote_ip: super::ipv4::Ipv4Addr::new(0, 0, 0, 0),
        tcp_remote_port: 0,
        tcp_connected: false,
    };

    {
        let mut table = SOCKET_TABLE.lock();
        table.insert(fd, socket);
    }

    crate::serial_println!("[SOCKET] Created socket fd={} type={} proto={}", fd, sock_type, protocol);
    fd as u64
}

/// Send data through a socket
/// For ICMP: addr is (ip_a, ip_b, ip_c, ip_d, 0, 0) packed in a3
/// For UDP: addr is (ip, port) encoded
pub fn sys_sendto(fd: u32, buf_addr: u64, len: u64, _flags: u64, dest_ip: Ipv4Addr, dest_port: u16) -> u64 {
    let table = SOCKET_TABLE.lock();
    let socket = match table.get(&fd) {
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
            // Send raw ICMP packet
            drop(table); // Release lock before calling network
            let seq = (len & 0xFFFF) as u16; // Hack: use len low bits as sequence
            super::send_ping(dest_ip, seq);
            data.len() as u64
        }
        IPPROTO_UDP => {
            let src_port = socket.bind_port;
            drop(table);
            if super::send_udp(dest_ip, src_port, dest_port, &data) {
                data.len() as u64
            } else {
                (-5i64) as u64 // EIO
            }
        }
        _ => (-22i64) as u64, // EINVAL
    }
}

/// Receive data from a socket (non-blocking)
pub fn sys_recvfrom(fd: u32, buf_addr: u64, len: u64) -> u64 {
    // Poll network first
    super::poll();

    let mut table = SOCKET_TABLE.lock();
    let socket = match table.get_mut(&fd) {
        Some(s) => s,
        None => return (-9i64) as u64, // EBADF
    };

    if socket.recv_queue.is_empty() {
        // For ICMP sockets, check the ping reply buffer
        if socket.protocol == IPPROTO_ICMP {
            // Check all pending ping replies
            let mut replies = super::PING_REPLIES.lock();
            if let Some((&seq, &(ip, _rtt))) = replies.iter().next() {
                replies.remove(&seq);
                drop(replies);

                // Build a fake ICMP reply for the userspace
                let reply_data = alloc::format!("PONG from {} seq={}", ip, seq);
                let bytes = reply_data.as_bytes();
                let copy_len = core::cmp::min(bytes.len(), len as usize);
                unsafe { crate::arch::x86_64::syscall::copy_to_user_pub(
                    buf_addr, &bytes[..copy_len],
                ) };
                return copy_len as u64;
            }
        }
        return 0; // No data available (non-blocking)
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
    let mut table = SOCKET_TABLE.lock();
    match table.get_mut(&fd) {
        Some(socket) => {
            socket.bind_port = port;
            crate::serial_println!("[SOCKET] fd={} bound to port {}", fd, port);
            0
        }
        None => (-9i64) as u64, // EBADF
    }
}

/// TCP connect: initiate 3-way handshake
pub fn sys_connect(fd: u32, ip_a: u8, ip_b: u8, ip_c: u8, ip_d: u8, port: u16) -> u64 {
    let remote_ip = super::ipv4::Ipv4Addr::new(ip_a, ip_b, ip_c, ip_d);

    // Verify socket exists and is a TCP socket
    {
        let table = SOCKET_TABLE.lock();
        match table.get(&fd) {
            Some(s) if s.sock_type == SOCK_STREAM => {},
            Some(_) => return (-22i64) as u64, // EINVAL
            None => return (-9i64) as u64, // EBADF
        }
    }

    crate::serial_println!("[SOCKET] sys_connect fd={} -> {}:{}", fd, remote_ip, port);

    // Perform TCP connect
    match super::tcp::tcp_connect(remote_ip, port) {
        Ok(local_port) => {
            // Update socket with TCP connection info
            let mut table = SOCKET_TABLE.lock();
            if let Some(socket) = table.get_mut(&fd) {
                socket.tcp_local_port = local_port;
                socket.tcp_remote_ip = remote_ip;
                socket.tcp_remote_port = port;
                socket.tcp_connected = true;
            }
            crate::serial_println!("[SOCKET] TCP connected fd={} local_port={}", fd, local_port);
            0 // Success
        }
        Err(e) => e as u64, // Negative error code
    }
}

/// TCP send data
pub fn sys_tcp_send(fd: u32, buf_addr: u64, len: u64) -> u64 {
    let (local_port, remote_ip, remote_port, connected) = {
        let table = SOCKET_TABLE.lock();
        match table.get(&fd) {
            Some(s) if s.tcp_connected => (s.tcp_local_port, s.tcp_remote_ip, s.tcp_remote_port, true),
            _ => return (-9i64) as u64,
        }
    };

    if !connected {
        return (-107i64) as u64; // ENOTCONN
    }

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

    match super::tcp::tcp_send(local_port, remote_ip, remote_port, &data) {
        Ok(sent) => sent as u64,
        Err(e) => e as u64,
    }
}

/// TCP receive data
pub fn sys_tcp_recv(fd: u32, buf_addr: u64, len: u64) -> u64 {
    let (local_port, remote_ip, remote_port, connected) = {
        let table = SOCKET_TABLE.lock();
        match table.get(&fd) {
            Some(s) if s.tcp_connected => (s.tcp_local_port, s.tcp_remote_ip, s.tcp_remote_port, true),
            _ => return (-9i64) as u64,
        }
    };

    if !connected {
        return (-107i64) as u64; // ENOTCONN
    }

    // Allocate a temp buffer on kernel side
    let mut temp_buf = Vec::with_capacity(len as usize);
    temp_buf.resize(len as usize, 0u8);

    match super::tcp::tcp_recv(local_port, remote_ip, remote_port, &mut temp_buf) {
        Ok(bytes_read) => {
            // Copy to user buffer (KPTI-safe)
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
    let (local_port, remote_ip, remote_port, connected) = {
        let table = SOCKET_TABLE.lock();
        match table.get(&fd) {
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
        if let Some(socket) = table.get_mut(&fd) {
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
        // Find null terminator
        let len = raw.iter().position(|&b| b == 0).unwrap_or(copied);
        match alloc::string::String::from_utf8(raw[..len].to_vec()) {
            Ok(s) => s,
            Err(_) => return (-22i64) as u64, // EINVAL
        }
    };

    crate::serial_println!("[SOCKET] sys_gethostbyname('{}')", domain);

    match super::dns::resolve(&domain) {
        Ok(ip) => {
            // Return IP as packed u32: (a<<24 | b<<16 | c<<8 | d)
            let octets = ip.0;
            let packed = ((octets[0] as u64) << 24)
                       | ((octets[1] as u64) << 16)
                       | ((octets[2] as u64) << 8)
                       | (octets[3] as u64);
            crate::serial_println!("[SOCKET] DNS resolved: {} -> 0x{:08X}", domain, packed);
            packed
        }
        Err(e) => e as u64,
    }
}

/// Close a socket and clean up resources
pub fn sys_socket_close(fd: u32) -> u64 {
    // If TCP and connected, do TCP shutdown first
    {
        let table = SOCKET_TABLE.lock();
        if let Some(s) = table.get(&fd) {
            if s.tcp_connected {
                let (lp, rip, rp) = (s.tcp_local_port, s.tcp_remote_ip, s.tcp_remote_port);
                drop(table);
                let _ = super::tcp::tcp_close(lp, rip, rp);
            }
        }
    }
    // Remove from socket table
    let mut table = SOCKET_TABLE.lock();
    match table.remove(&fd) {
        Some(_) => {
            crate::serial_println!("[SOCKET] fd={} closed", fd);
            0
        }
        None => (-9i64) as u64, // EBADF
    }
}

/// TCP receive with blocking poll (timeout ~500ms worth of iterations)
/// Polls the network repeatedly to wait for incoming data before returning 0
pub fn sys_tcp_recv_blocking(fd: u32, buf_addr: u64, len: u64) -> u64 {
    let (local_port, remote_ip, remote_port, connected) = {
        let table = SOCKET_TABLE.lock();
        match table.get(&fd) {
            Some(s) if s.tcp_connected => (s.tcp_local_port, s.tcp_remote_ip, s.tcp_remote_port, true),
            _ => return (-9i64) as u64,
        }
    };

    if !connected {
        return (-107i64) as u64; // ENOTCONN
    }

    // Try up to 500,000 poll iterations (~500ms at typical instruction speed)
    for attempt in 0..500_000u32 {
        super::poll();

        let mut temp_buf = alloc::vec![0u8; len as usize];
        match super::tcp::tcp_recv(local_port, remote_ip, remote_port, &mut temp_buf) {
            Ok(bytes_read) if bytes_read > 0 => {
                unsafe { crate::arch::x86_64::syscall::copy_to_user_pub(
                    buf_addr, &temp_buf[..bytes_read],
                ) };
                return bytes_read as u64;
            }
            Ok(0) => {
                // Check if connection closed
                let state = super::tcp::get_state(local_port, remote_ip, remote_port);
                if state == super::tcp::TcpState::CloseWait || state == super::tcp::TcpState::Closed {
                    return 0; // EOF
                }
            }
            Err(e) => return e as u64,
            _ => {}
        }

        if attempt % 50_000 == 0 && attempt > 0 {
            // Periodic poll burst
            for _ in 0..100 {
                super::poll();
                unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
            }
        }
        unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    }

    0 // No data after timeout (non-blocking return)
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
