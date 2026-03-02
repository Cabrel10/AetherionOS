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
pub const SOCK_RAW: u32 = 3;
pub const SOCK_DGRAM: u32 = 2;

/// Protocol numbers
pub const IPPROTO_ICMP: u32 = 1;
pub const IPPROTO_UDP: u32 = 17;

/// Address family
pub const AF_INET: u32 = 2;

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
}

// Socket file descriptor table
lazy_static! {
    static ref SOCKET_TABLE: Mutex<BTreeMap<u32, Socket>> = Mutex::new(BTreeMap::new());
}

/// Next socket file descriptor (start at 100 to avoid collision with VFS FDs)
static NEXT_SOCKET_FD: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(100);

/// Create a new socket
/// Returns socket fd or negative error
pub fn sys_socket(domain: u32, sock_type: u32, protocol: u32) -> u64 {
    if domain != AF_INET {
        return (-22i64) as u64; // EINVAL
    }

    match sock_type {
        SOCK_RAW if protocol == IPPROTO_ICMP => {},
        SOCK_DGRAM if protocol == IPPROTO_UDP || protocol == 0 => {},
        _ => return (-22i64) as u64, // EINVAL
    }

    let fd = NEXT_SOCKET_FD.fetch_add(1, core::sync::atomic::Ordering::SeqCst);

    let socket = Socket {
        domain,
        sock_type,
        protocol: if protocol == 0 { IPPROTO_UDP } else { protocol },
        bind_port: 0, // unbound
        recv_queue: Vec::new(),
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

    // Read data from user buffer
    let data = unsafe {
        let buf = buf_addr as *const u8;
        let mut v = Vec::with_capacity(len as usize);
        for i in 0..len as usize {
            v.push(core::ptr::read_volatile(buf.add(i)));
        }
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
                unsafe {
                    let dst = buf_addr as *mut u8;
                    for i in 0..copy_len {
                        core::ptr::write_volatile(dst.add(i), bytes[i]);
                    }
                }
                return copy_len as u64;
            }
        }
        return 0; // No data available (non-blocking)
    }

    // Pop from receive queue
    let datagram = socket.recv_queue.remove(0);
    let copy_len = core::cmp::min(datagram.data.len(), len as usize);
    unsafe {
        let dst = buf_addr as *mut u8;
        for i in 0..copy_len {
            core::ptr::write_volatile(dst.add(i), datagram.data[i]);
        }
    }

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
