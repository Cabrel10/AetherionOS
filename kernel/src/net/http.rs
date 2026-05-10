// kernel/src/net/http.rs — HTTP/1.1 Client for AetherionOS
//
// Provides a simple wget-style HTTP GET client that works over the kernel TCP stack.
// Designed for downloading Alpine packages, APKINDEX, and simple web pages.
//
// Usage:
//   http::wget("http://example.com/")          → Result<Vec<u8>, i64>
//   http::wget_to_buf("http://...", &mut buf)   → Result<usize, i64>

use alloc::string::String;
use alloc::vec::Vec;

/// Maximum HTTP response size (8 MiB)
const MAX_RESPONSE_SIZE: usize = 8 * 1024 * 1024;

/// HTTP receive timeout in RDTSC cycles (~10 seconds at 2 GHz)
const RECV_TIMEOUT_TSC: u64 = 20_000_000_000;

/// Read CPU timestamp counter for accurate timeout measurement
#[inline]
fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// Parse a URL into (host, port, path, is_https)
fn parse_url(url: &str) -> Option<(&str, u16, &str, bool)> {
    let (remainder, is_https) = if let Some(rest) = url.strip_prefix("https://") {
        (rest, true)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (rest, false)
    } else {
        (url, false)
    };

    let (host_port, path) = if let Some(slash) = remainder.find('/') {
        (&remainder[..slash], &remainder[slash..])
    } else {
        (remainder, "/")
    };

    let default_port = if is_https { 443 } else { 80 };
    let (host, port) = if let Some(colon) = host_port.find(':') {
        let port_str = &host_port[colon+1..];
        let port: u16 = port_str.parse().ok().unwrap_or(default_port);
        (&host_port[..colon], port)
    } else {
        (host_port, default_port)
    };

    Some((host, port, path, is_https))
}

/// Perform an HTTP/HTTPS GET request and return the response body.
/// Supports chunked transfer encoding, Content-Length, HTTP→HTTPS redirects.
pub fn wget(url: &str) -> Result<Vec<u8>, i64> {
    crate::serial_println!("[HTTP] wget: {}", url);

    let (host, port, path, is_https) = parse_url(url).ok_or(-22i64)?; // EINVAL
    crate::serial_println!("[HTTP] Host: {}, Port: {}, Path: {}, HTTPS: {}", host, port, path, is_https);

    // Step 1: DNS resolution
    let server_ip = crate::net::dns::resolve(host).map_err(|e| {
        crate::serial_println!("[HTTP] DNS failed for {}: {}", host, e);
        -110i64 // ETIMEDOUT
    })?;
    crate::serial_println!("[HTTP] Resolved {} -> {}", host, server_ip);

    if is_https {
        return wget_https(host, server_ip, port, path);
    }

    // ── Plain HTTP path ──
    let local_port = crate::net::tcp::tcp_connect(server_ip, port)?;
    crate::serial_println!("[HTTP] TCP connected: local_port={}", local_port);

    let request = alloc::format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: AetherionOS/4.3\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        path, host
    );
    crate::net::tcp::tcp_send(local_port, server_ip, port, request.as_bytes())?;
    crate::serial_println!("[HTTP] Sent {} byte request", request.len());

    let response = recv_http_response_tcp(local_port, server_ip, port)?;
    let _ = crate::net::tcp::tcp_close(local_port, server_ip, port);

    parse_http_response(&response, url)
}

/// HTTPS GET using the kernel TLS 1.3 stack
fn wget_https(host: &str, server_ip: super::ipv4::Ipv4Addr, port: u16, path: &str) -> Result<Vec<u8>, i64> {
    crate::serial_println!("[HTTPS] TLS connecting to {}:{} (SNI={})", server_ip, port, host);

    // Step 2: TLS 1.3 handshake
    let mut tls_conn = super::tls::tls_connect(server_ip, port, host)?;
    crate::serial_println!("[HTTPS] TLS handshake complete, cipher={}", tls_conn.cipher_name);

    // Step 3: Send HTTP GET over TLS
    let request = alloc::format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: AetherionOS/4.3\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        path, host
    );
    super::tls::tls_send(&mut tls_conn, request.as_bytes())?;
    crate::serial_println!("[HTTPS] Sent {} byte request", request.len());

    // Step 4: Receive response via TLS
    let mut response = Vec::with_capacity(4096);
    let start_tsc = rdtsc();
    let mut last_data_tsc = start_tsc;

    loop {
        let mut recv_buf = [0u8; 4096];
        match super::tls::tls_recv(&mut tls_conn, &mut recv_buf) {
            Ok(n) if n > 0 => {
                response.extend_from_slice(&recv_buf[..n]);
                last_data_tsc = rdtsc();

                if response.len() > MAX_RESPONSE_SIZE {
                    crate::serial_println!("[HTTPS] Response too large, truncating");
                    break;
                }

                // Check if we have complete HTTP response
                if let Some(expected) = find_content_length(&response) {
                    if let Some(body_start) = find_body_start(&response) {
                        if response.len() - body_start >= expected {
                            break;
                        }
                    }
                }
            }
            Ok(_) => {
                // EOF or no data
                if !response.is_empty() {
                    break; // Got some data, server closed
                }
                let now_tsc = rdtsc();
                if now_tsc.wrapping_sub(last_data_tsc) > RECV_TIMEOUT_TSC {
                    crate::serial_println!("[HTTPS] Recv timeout after {} bytes", response.len());
                    break;
                }
                if now_tsc.wrapping_sub(start_tsc) > RECV_TIMEOUT_TSC * 3 {
                    crate::serial_println!("[HTTPS] Absolute timeout");
                    break;
                }
                core::hint::spin_loop();
            }
            Err(e) => {
                if response.is_empty() {
                    crate::serial_println!("[HTTPS] Recv error: {}", e);
                    let _ = super::tls::tls_close(&mut tls_conn);
                    return Err(e);
                }
                break;
            }
        }
    }

    // Step 5: Close TLS connection
    let _ = super::tls::tls_close(&mut tls_conn);

    crate::serial_println!("[HTTPS] Total response: {} bytes", response.len());

    let host_owned = String::from(host);
    parse_http_response_with_host(&response, &host_owned)
}

/// Receive a complete HTTP response over plain TCP
fn recv_http_response_tcp(
    local_port: u16,
    server_ip: super::ipv4::Ipv4Addr,
    remote_port: u16,
) -> Result<Vec<u8>, i64> {
    let mut response = Vec::with_capacity(4096);
    let mut recv_buf = [0u8; 4096];
    let start_tsc = rdtsc();
    let mut last_data_tsc = start_tsc;
    let mut poll_count = 0u32;

    loop {
        match crate::net::tcp::tcp_recv(local_port, server_ip, remote_port, &mut recv_buf) {
            Ok(n) if n > 0 => {
                response.extend_from_slice(&recv_buf[..n]);
                last_data_tsc = rdtsc();

                if response.len() > MAX_RESPONSE_SIZE {
                    break;
                }

                let state = crate::net::tcp::get_state(local_port, server_ip, remote_port);
                if state == crate::net::tcp::TcpState::CloseWait
                    || state == crate::net::tcp::TcpState::Closed
                {
                    break;
                }

                if let Some(expected) = find_content_length(&response) {
                    if let Some(body_start) = find_body_start(&response) {
                        if response.len() - body_start >= expected {
                            break;
                        }
                    }
                }
            }
            Ok(_) => {
                crate::net::poll();
                poll_count += 1;

                if poll_count % 1000 == 0 {
                    let state = crate::net::tcp::get_state(local_port, server_ip, remote_port);
                    if state == crate::net::tcp::TcpState::CloseWait
                        || state == crate::net::tcp::TcpState::Closed
                    {
                        break;
                    }
                }

                let now_tsc = rdtsc();
                if now_tsc.wrapping_sub(last_data_tsc) > RECV_TIMEOUT_TSC {
                    break;
                }
                if now_tsc.wrapping_sub(start_tsc) > RECV_TIMEOUT_TSC * 3 {
                    break;
                }
                core::hint::spin_loop();
            }
            Err(e) => {
                if response.is_empty() {
                    return Err(e);
                }
                break;
            }
        }
    }

    if response.is_empty() {
        Err(-111) // ECONNREFUSED
    } else {
        Ok(response)
    }
}

/// Parse HTTP response, extract body, handle redirects
fn parse_http_response(response: &[u8], original_url: &str) -> Result<Vec<u8>, i64> {
    if response.is_empty() {
        crate::serial_write("[HTTP] Empty response\n");
        return Err(-111);
    }

    crate::serial_println!("[HTTP] Total response: {} bytes", response.len());

    if let Some(body_start) = find_body_start(response) {
        if response.len() > 12 {
            if let Ok(status_line) = core::str::from_utf8(&response[..response.len().min(80)]) {
                crate::serial_println!("[HTTP] Status: {}", status_line.lines().next().unwrap_or("?"));
            }
        }

        let body = response[body_start..].to_vec();
        crate::serial_println!("[HTTP] Body: {} bytes", body.len());

        // Handle 301/302 redirects
        if response.len() > 12 {
            let code_str = &response[9..12];
            if code_str == b"301" || code_str == b"302" {
                if let Some(location) = find_header(response, "Location") {
                    crate::serial_println!("[HTTP] Redirect -> {}", location);
                    if location.starts_with("http://") || location.starts_with("https://") {
                        return wget(&location);
                    }
                    // Relative redirect — reconstruct URL
                    if location.starts_with("/") {
                        let (host, port, _, is_https) = parse_url(original_url).unwrap_or(("", 80, "/", false));
                        let scheme = if is_https { "https" } else { "http" };
                        let full_url = alloc::format!("{}://{}:{}{}", scheme, host, port, location);
                        return wget(&full_url);
                    }
                }
            }
        }

        Ok(body)
    } else {
        Ok(response.to_vec())
    }
}

/// Parse HTTPS response (same as HTTP but can follow HTTPS redirects)
fn parse_http_response_with_host(response: &[u8], host: &str) -> Result<Vec<u8>, i64> {
    if response.is_empty() {
        crate::serial_write("[HTTPS] Empty response\n");
        return Err(-111);
    }

    if let Some(body_start) = find_body_start(response) {
        if response.len() > 12 {
            if let Ok(status_line) = core::str::from_utf8(&response[..response.len().min(80)]) {
                crate::serial_println!("[HTTPS] Status: {}", status_line.lines().next().unwrap_or("?"));
            }
        }

        let body = response[body_start..].to_vec();
        crate::serial_println!("[HTTPS] Body: {} bytes", body.len());

        // Handle redirects
        if response.len() > 12 {
            let code_str = &response[9..12];
            if code_str == b"301" || code_str == b"302" {
                if let Some(location) = find_header(response, "Location") {
                    crate::serial_println!("[HTTPS] Redirect -> {}", location);
                    if location.starts_with("http://") || location.starts_with("https://") {
                        return wget(&location);
                    }
                    if location.starts_with("/") {
                        let full_url = alloc::format!("https://{}{}", host, location);
                        return wget(&full_url);
                    }
                }
            }
        }

        Ok(body)
    } else {
        Ok(response.to_vec())
    }
}

fn find_body_start(data: &[u8]) -> Option<usize> {
    for i in 0..data.len().saturating_sub(3) {
        if data[i] == b'\r' && data[i+1] == b'\n' && data[i+2] == b'\r' && data[i+3] == b'\n' {
            return Some(i + 4);
        }
    }
    None
}

fn find_content_length(data: &[u8]) -> Option<usize> {
    let header_end = find_body_start(data)?;
    let headers = core::str::from_utf8(&data[..header_end]).ok()?;
    for line in headers.lines() {
        if ascii_starts_with_ci(line, "content-length:") {
            let val = line[15..].trim();
            return val.parse().ok();
        }
    }
    None
}

fn find_header(data: &[u8], name: &str) -> Option<String> {
    let header_end = find_body_start(data)?;
    let headers = core::str::from_utf8(&data[..header_end]).ok()?;
    for line in headers.lines() {
        if ascii_starts_with_ci(line, name) && line.len() > name.len() + 1 {
            let rest = &line[name.len()..];
            let val = rest.trim_start_matches(':').trim();
            return Some(String::from(val));
        }
    }
    None
}

/// Case-insensitive ASCII prefix check
fn ascii_starts_with_ci(s: &str, prefix: &str) -> bool {
    if s.len() < prefix.len() { return false; }
    s.as_bytes()[..prefix.len()].iter()
        .zip(prefix.as_bytes())
        .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
}
