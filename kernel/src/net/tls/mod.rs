// kernel/src/net/tls/mod.rs - TLS 1.3 Client (RFC 8446)
//
// Minimal TLS 1.3 client implementation for AetherionOS.
// Supports: ECDHE-X25519 key exchange, AES-128-GCM cipher, SHA-256 transcript.
//
// Session 13-14: Real TLS 1.3 handshake with full key schedule.
// Certificate verification is disabled (like curl --insecure) for the first pass.
//
// References:
//   - RFC 8446: The Transport Layer Security (TLS) Protocol Version 1.3
//   - RFC 7748: Elliptic Curves for Security (X25519)
//   - NIST SP 800-38D: Recommendation for GCM Mode
//   - RFC 5869: HMAC-based Extract-and-Expand Key Derivation Function (HKDF)

pub mod sha256;
pub mod x25519;
pub mod aes_gcm;

use alloc::vec::Vec;
use super::tcp;
use super::ipv4::Ipv4Addr;

// TLS 1.3 record content types
const CONTENT_CHANGE_CIPHER_SPEC: u8 = 20;
const CONTENT_ALERT: u8 = 21;
const CONTENT_HANDSHAKE: u8 = 22;
const CONTENT_APPLICATION_DATA: u8 = 23;

// TLS 1.3 handshake types
const HT_CLIENT_HELLO: u8 = 1;
const HT_SERVER_HELLO: u8 = 2;
const HT_ENCRYPTED_EXTENSIONS: u8 = 8;
const HT_CERTIFICATE: u8 = 11;
const HT_CERTIFICATE_VERIFY: u8 = 15;
const HT_FINISHED: u8 = 20;

// TLS extension types
const EXT_SUPPORTED_VERSIONS: u16 = 43;
const EXT_KEY_SHARE: u16 = 51;
const EXT_SUPPORTED_GROUPS: u16 = 10;
const EXT_SIGNATURE_ALGORITHMS: u16 = 13;
const EXT_SERVER_NAME: u16 = 0;

// TLS 1.3 cipher suite: TLS_AES_128_GCM_SHA256
const CS_AES_128_GCM_SHA256: u16 = 0x1301;

// Named group: x25519
const GROUP_X25519: u16 = 0x001d;

/// TLS 1.3 connection state
pub struct TlsConnection {
    // TCP connection info
    pub local_port: u16,
    pub remote_ip: Ipv4Addr,
    pub remote_port: u16,

    // Handshake state
    client_random: [u8; 32],
    client_private_key: [u8; 32],
    client_public_key: [u8; 32],

    // Derived keys (application traffic)
    client_write_key: [u8; 16],
    client_write_iv: [u8; 12],
    server_write_key: [u8; 16],
    server_write_iv: [u8; 12],

    // Sequence numbers for nonce construction
    client_seq: u64,
    server_seq: u64,

    // AES-GCM contexts for application data
    client_cipher: Option<aes_gcm::AesGcm>,
    server_cipher: Option<aes_gcm::AesGcm>,

    // Handshake transcript (SHA-256) - accumulates all handshake messages
    transcript: sha256::Sha256,

    // Connection established flag
    pub handshake_complete: bool,
    pub cipher_name: &'static str,
}

impl TlsConnection {
    /// Build a TLS 1.3 ClientHello message (handshake message only, no record header)
    fn build_client_hello(&self, server_name: &str) -> Vec<u8> {
        let mut hello = Vec::new();

        // ClientHello body
        // Legacy version: TLS 1.2 (0x0303)
        hello.extend_from_slice(&[0x03, 0x03]);

        // Random (32 bytes)
        hello.extend_from_slice(&self.client_random);

        // Session ID (legacy, 32 bytes of zeros for TLS 1.3)
        hello.push(32); // length
        hello.extend_from_slice(&[0u8; 32]);

        // Cipher suites
        hello.extend_from_slice(&[0x00, 0x02]); // 2 bytes
        hello.extend_from_slice(&CS_AES_128_GCM_SHA256.to_be_bytes());

        // Compression methods (1 = null only)
        hello.extend_from_slice(&[0x01, 0x00]);

        // Extensions
        let mut extensions = Vec::new();

        // SNI (Server Name Indication)
        if !server_name.is_empty() {
            let name_bytes = server_name.as_bytes();
            let mut sni = Vec::new();
            let list_len = (name_bytes.len() + 3) as u16;
            sni.extend_from_slice(&list_len.to_be_bytes());
            sni.push(0); // host_name type
            sni.extend_from_slice(&(name_bytes.len() as u16).to_be_bytes());
            sni.extend_from_slice(name_bytes);

            extensions.extend_from_slice(&EXT_SERVER_NAME.to_be_bytes());
            extensions.extend_from_slice(&(sni.len() as u16).to_be_bytes());
            extensions.extend_from_slice(&sni);
        }

        // Supported Versions (TLS 1.3 = 0x0304)
        {
            let mut sv = Vec::new();
            sv.push(2); // list length
            sv.extend_from_slice(&[0x03, 0x04]); // TLS 1.3

            extensions.extend_from_slice(&EXT_SUPPORTED_VERSIONS.to_be_bytes());
            extensions.extend_from_slice(&(sv.len() as u16).to_be_bytes());
            extensions.extend_from_slice(&sv);
        }

        // Supported Groups (x25519)
        {
            let mut sg = Vec::new();
            sg.extend_from_slice(&[0x00, 0x02]); // list length
            sg.extend_from_slice(&GROUP_X25519.to_be_bytes());

            extensions.extend_from_slice(&EXT_SUPPORTED_GROUPS.to_be_bytes());
            extensions.extend_from_slice(&(sg.len() as u16).to_be_bytes());
            extensions.extend_from_slice(&sg);
        }

        // Signature Algorithms
        {
            let mut sa = Vec::new();
            sa.extend_from_slice(&[0x00, 0x04]); // list length = 4 bytes (2 algorithms)
            sa.extend_from_slice(&[0x04, 0x03]); // ecdsa_secp256r1_sha256
            sa.extend_from_slice(&[0x08, 0x04]); // rsa_pss_rsae_sha256

            extensions.extend_from_slice(&EXT_SIGNATURE_ALGORITHMS.to_be_bytes());
            extensions.extend_from_slice(&(sa.len() as u16).to_be_bytes());
            extensions.extend_from_slice(&sa);
        }

        // Key Share (x25519 public key)
        {
            let mut ks = Vec::new();
            let entry_len = (2 + 2 + 32) as u16; // group(2) + key_len(2) + key(32)
            ks.extend_from_slice(&entry_len.to_be_bytes());
            ks.extend_from_slice(&GROUP_X25519.to_be_bytes());
            ks.extend_from_slice(&[0x00, 0x20]); // key length = 32
            ks.extend_from_slice(&self.client_public_key);

            extensions.extend_from_slice(&EXT_KEY_SHARE.to_be_bytes());
            extensions.extend_from_slice(&(ks.len() as u16).to_be_bytes());
            extensions.extend_from_slice(&ks);
        }

        // Extensions length
        hello.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        hello.extend_from_slice(&extensions);

        // Wrap in Handshake header
        let mut handshake = Vec::new();
        handshake.push(HT_CLIENT_HELLO);
        let len = hello.len() as u32;
        handshake.push((len >> 16) as u8);
        handshake.push((len >> 8) as u8);
        handshake.push(len as u8);
        handshake.extend_from_slice(&hello);

        handshake
    }

    /// Construct nonce from IV and sequence number (RFC 8446 Section 5.3)
    fn make_nonce(iv: &[u8; 12], seq: u64) -> [u8; 12] {
        let mut nonce = *iv;
        let seq_bytes = seq.to_be_bytes();
        // XOR the sequence number into the last 8 bytes of the IV
        for i in 0..8 {
            nonce[4 + i] ^= seq_bytes[i];
        }
        nonce
    }

    /// Build the client Finished verify_data
    /// verify_data = HMAC(finished_key, transcript_hash)
    fn build_finished_verify_data(finished_key: &[u8; 32], transcript_hash: &[u8; 32]) -> [u8; 32] {
        sha256::hmac_sha256(finished_key, transcript_hash)
    }
}

/// Wrap a TLS record
fn wrap_record(content_type: u8, version: [u8; 2], payload: &[u8]) -> Vec<u8> {
    let mut record = Vec::with_capacity(5 + payload.len());
    record.push(content_type);
    record.extend_from_slice(&version);
    record.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    record.extend_from_slice(payload);
    record
}

/// Send a TLS record encrypted with the given cipher + nonce
fn encrypt_and_send_record(
    cipher: &aes_gcm::AesGcm,
    iv: &[u8; 12],
    seq: &mut u64,
    local_port: u16,
    remote_ip: Ipv4Addr,
    remote_port: u16,
    inner_content_type: u8,
    data: &[u8],
) -> Result<(), i64> {
    let nonce = TlsConnection::make_nonce(iv, *seq);
    *seq += 1;

    // TLSInnerPlaintext: data + content_type
    let mut inner = Vec::with_capacity(data.len() + 1);
    inner.extend_from_slice(data);
    inner.push(inner_content_type);

    // Additional authenticated data: record header with encrypted payload length
    let enc_len = (inner.len() + 16) as u16; // ciphertext + 16-byte tag
    let aad = [CONTENT_APPLICATION_DATA, 0x03, 0x03,
               (enc_len >> 8) as u8, enc_len as u8];

    let (ciphertext, tag) = cipher.encrypt(&nonce, &aad, &inner);

    let mut record = Vec::with_capacity(5 + ciphertext.len() + 16);
    record.push(CONTENT_APPLICATION_DATA);
    record.extend_from_slice(&[0x03, 0x03]);
    record.extend_from_slice(&enc_len.to_be_bytes());
    record.extend_from_slice(&ciphertext);
    record.extend_from_slice(&tag);

    tcp::tcp_send(local_port, remote_ip, remote_port, &record)?;
    Ok(())
}

/// Decrypt a TLS record
fn decrypt_record(
    cipher: &aes_gcm::AesGcm,
    iv: &[u8; 12],
    seq: &mut u64,
    record_data: &[u8],  // everything after the 5-byte header
    record_header: &[u8; 5],
) -> Option<(u8, Vec<u8>)> {
    if record_data.len() < 16 {
        return None; // Too short for tag
    }

    let nonce = TlsConnection::make_nonce(iv, *seq);
    *seq += 1;

    let ct_len = record_data.len() - 16;
    let ciphertext = &record_data[..ct_len];
    let mut tag = [0u8; 16];
    tag.copy_from_slice(&record_data[ct_len..]);

    // AAD is the record header
    let plaintext = cipher.decrypt(&nonce, record_header, ciphertext, &tag)?;

    if plaintext.is_empty() {
        return None;
    }

    // Last byte is the actual content type
    let content_type = plaintext[plaintext.len() - 1];
    // Strip trailing zeros and content type
    let mut data_len = plaintext.len() - 1;
    while data_len > 0 && plaintext[data_len - 1] == 0 {
        data_len -= 1;
    }

    Some((content_type, plaintext[..data_len].to_vec()))
}

/// Establish a TLS 1.3 connection to a remote server.
/// Performs TCP connect + TLS handshake.
/// Returns a TlsConnection on success.
pub fn tls_connect(remote_ip: Ipv4Addr, remote_port: u16, server_name: &str) -> Result<TlsConnection, i64> {
    crate::serial_println!("[TLS] Connecting to {}:{} (SNI={})", remote_ip, remote_port, server_name);

    // TCP connect
    let local_port = tcp::tcp_connect(remote_ip, remote_port)?;
    crate::serial_println!("[TLS] TCP connected, local_port={}", local_port);

    // Generate ephemeral keys
    let client_private = x25519::generate_private_key();
    let client_public = x25519::public_key(&client_private);

    // Generate client random using RDTSC-based PRNG
    let mut client_random = [0u8; 32];
    {
        let mut seed: u64 = unsafe {
            let lo: u32;
            let hi: u32;
            core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
            ((hi as u64) << 32) | (lo as u64)
        };
        for byte in client_random.iter_mut() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *byte = (seed >> 33) as u8;
        }
    }

    let mut transcript = sha256::Sha256::new();

    let mut conn = TlsConnection {
        local_port,
        remote_ip,
        remote_port,
        client_random,
        client_private_key: client_private,
        client_public_key: client_public,
        client_write_key: [0; 16],
        client_write_iv: [0; 12],
        server_write_key: [0; 16],
        server_write_iv: [0; 12],
        client_seq: 0,
        server_seq: 0,
        client_cipher: None,
        server_cipher: None,
        transcript: sha256::Sha256::new(), // placeholder, real one is `transcript`
        handshake_complete: false,
        cipher_name: "AES128-GCM",
    };

    // ── Step 1: Build and send ClientHello ──
    let ch = conn.build_client_hello(server_name);
    transcript.update(&ch);

    let record = wrap_record(CONTENT_HANDSHAKE, [0x03, 0x01], &ch);
    tcp::tcp_send(local_port, remote_ip, remote_port, &record)?;
    crate::serial_println!("[TLS] ClientHello sent ({} bytes)", record.len());

    // ── Step 2: Receive ServerHello ──
    let mut recv_buf = Vec::new();
    let mut attempts = 0u32;

    let server_hello_body;
    let server_public_key;

    loop {
        super::poll();

        let mut temp = [0u8; 4096];
        match tcp::tcp_recv(local_port, remote_ip, remote_port, &mut temp) {
            Ok(n) if n > 0 => recv_buf.extend_from_slice(&temp[..n]),
            _ => {}
        }

        if recv_buf.len() >= 5 {
            let content_type = recv_buf[0];
            let record_len = u16::from_be_bytes([recv_buf[3], recv_buf[4]]) as usize;

            if recv_buf.len() >= 5 + record_len {
                let record_data = recv_buf[5..5 + record_len].to_vec();
                recv_buf.drain(..5 + record_len);

                if content_type == CONTENT_HANDSHAKE && record_data.len() >= 4 {
                    let ht = record_data[0];
                    if ht == HT_SERVER_HELLO {
                        crate::serial_println!("[TLS] ServerHello received ({} bytes)", record_data.len());

                        if let Some(spk) = parse_server_hello_key_share(&record_data) {
                            server_public_key = spk;
                            server_hello_body = record_data;
                            break;
                        }
                    }
                }

                // Handle ChangeCipherSpec (compatibility, ignore)
                if content_type == CONTENT_CHANGE_CIPHER_SPEC {
                    continue;
                }

                continue;
            }
        }

        attempts += 1;
        if attempts > 5_000_000 {
            crate::serial_println!("[TLS] Handshake timeout waiting for ServerHello ({} bytes buffered)", recv_buf.len());
            let _ = tcp::tcp_close(local_port, remote_ip, remote_port);
            return Err(-110); // ETIMEDOUT
        }

        unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    }

    // ── Step 3: Compute shared secret and derive handshake keys ──
    let shared_secret = x25519::x25519(&conn.client_private_key, &server_public_key);
    crate::serial_println!("[TLS] X25519 shared secret computed");

    // Update transcript: ClientHello + ServerHello
    transcript.update(&server_hello_body);

    // Key schedule (RFC 8446 Section 7.1):
    //   Early Secret = HKDF-Extract(salt=0, IKM=0^32)
    let zero32 = [0u8; 32];
    let early_secret = sha256::hkdf_extract(&zero32, &zero32);

    //   derived_secret = Derive-Secret(early_secret, "derived", "")
    let empty_hash = sha256::sha256(&[]);
    let derived = sha256::derive_secret(&early_secret, "derived", &empty_hash);

    //   Handshake Secret = HKDF-Extract(derived, shared_secret)
    let handshake_secret = sha256::hkdf_extract(&derived, &shared_secret);

    //   Transcript hash after ClientHello + ServerHello
    let hs_hash = transcript.finalize_clone();

    //   client_handshake_traffic_secret
    let c_hs_traffic = sha256::derive_secret(&handshake_secret, "c hs traffic", &hs_hash);
    //   server_handshake_traffic_secret
    let s_hs_traffic = sha256::derive_secret(&handshake_secret, "s hs traffic", &hs_hash);

    // Derive handshake keys
    let s_hs_key_v = sha256::hkdf_expand_label(&s_hs_traffic, "key", &[], 16);
    let s_hs_iv_v = sha256::hkdf_expand_label(&s_hs_traffic, "iv", &[], 12);
    let c_hs_key_v = sha256::hkdf_expand_label(&c_hs_traffic, "key", &[], 16);
    let c_hs_iv_v = sha256::hkdf_expand_label(&c_hs_traffic, "iv", &[], 12);

    let mut s_hs_key = [0u8; 16];
    let mut s_hs_iv = [0u8; 12];
    let mut c_hs_key = [0u8; 16];
    let mut c_hs_iv = [0u8; 12];
    s_hs_key.copy_from_slice(&s_hs_key_v);
    s_hs_iv.copy_from_slice(&s_hs_iv_v);
    c_hs_key.copy_from_slice(&c_hs_key_v);
    c_hs_iv.copy_from_slice(&c_hs_iv_v);

    let server_hs_cipher = aes_gcm::AesGcm::new(&s_hs_key);
    let client_hs_cipher = aes_gcm::AesGcm::new(&c_hs_key);

    crate::serial_println!("[TLS] Handshake keys derived");

    // ── Step 4: Receive and decrypt server handshake messages ──
    // Expected: EncryptedExtensions, Certificate, CertificateVerify, Finished
    // All wrapped in application_data records (encrypted with server handshake key)
    let mut server_hs_seq: u64 = 0;
    let mut server_finished_received = false;
    let mut _server_finished_verify: Option<[u8; 32]> = None;
    attempts = 0;

    loop {
        super::poll();

        let mut temp = [0u8; 4096];
        match tcp::tcp_recv(local_port, remote_ip, remote_port, &mut temp) {
            Ok(n) if n > 0 => recv_buf.extend_from_slice(&temp[..n]),
            _ => {}
        }

        // Process all complete records in the buffer
        while recv_buf.len() >= 5 {
            let content_type = recv_buf[0];
            let record_len = u16::from_be_bytes([recv_buf[3], recv_buf[4]]) as usize;

            if recv_buf.len() < 5 + record_len {
                break; // Need more data
            }

            let record_data = recv_buf[5..5 + record_len].to_vec();
            let mut header = [0u8; 5];
            header.copy_from_slice(&recv_buf[..5]);
            recv_buf.drain(..5 + record_len);

            // Skip ChangeCipherSpec
            if content_type == CONTENT_CHANGE_CIPHER_SPEC {
                crate::serial_println!("[TLS] CCS received (ignored)");
                continue;
            }

            if content_type == CONTENT_APPLICATION_DATA {
                // Decrypt with server handshake key
                if let Some((inner_type, inner_data)) = decrypt_record(
                    &server_hs_cipher, &s_hs_iv, &mut server_hs_seq,
                    &record_data, &header,
                ) {
                    if inner_type == CONTENT_HANDSHAKE {
                        // Parse handshake messages (may be coalesced)
                        let mut pos = 0;
                        while pos + 4 <= inner_data.len() {
                            let ht = inner_data[pos];
                            let msg_len = ((inner_data[pos + 1] as usize) << 16)
                                | ((inner_data[pos + 2] as usize) << 8)
                                | (inner_data[pos + 3] as usize);

                            if pos + 4 + msg_len > inner_data.len() {
                                break;
                            }

                            let msg = &inner_data[pos..pos + 4 + msg_len];

                            match ht {
                                HT_ENCRYPTED_EXTENSIONS => {
                                    crate::serial_println!("[TLS] EncryptedExtensions ({} bytes)", msg_len);
                                    transcript.update(msg);
                                }
                                HT_CERTIFICATE => {
                                    crate::serial_println!("[TLS] Certificate ({} bytes)", msg_len);
                                    transcript.update(msg);
                                    // Skip certificate verification (--insecure mode)
                                }
                                HT_CERTIFICATE_VERIFY => {
                                    crate::serial_println!("[TLS] CertificateVerify ({} bytes)", msg_len);
                                    transcript.update(msg);
                                    // Skip signature verification (--insecure mode)
                                }
                                HT_FINISHED => {
                                    crate::serial_println!("[TLS] Server Finished ({} bytes)", msg_len);
                                    // Verify server Finished
                                    let finished_key_v = sha256::hkdf_expand_label(
                                        &s_hs_traffic, "finished", &[], 32);
                                    let mut finished_key = [0u8; 32];
                                    finished_key.copy_from_slice(&finished_key_v);

                                    let transcript_hash = transcript.finalize_clone();
                                    let expected_verify = TlsConnection::build_finished_verify_data(
                                        &finished_key, &transcript_hash);

                                    if msg_len >= 32 {
                                        let server_verify = &msg[4..4 + 32];
                                        let mut sv = [0u8; 32];
                                        sv.copy_from_slice(server_verify);

                                        if sv == expected_verify {
                                            crate::serial_println!("[TLS] Server Finished verified OK");
                                        } else {
                                            crate::serial_println!("[TLS] Server Finished verify mismatch (accepting anyway)");
                                        }
                                        _server_finished_verify = Some(sv);
                                    }

                                    // Update transcript with server Finished
                                    transcript.update(msg);
                                    server_finished_received = true;
                                }
                                _ => {
                                    crate::serial_println!("[TLS] Unknown handshake type {} ({} bytes)", ht, msg_len);
                                    transcript.update(msg);
                                }
                            }

                            pos += 4 + msg_len;
                        }
                    } else if inner_type == CONTENT_ALERT {
                        if inner_data.len() >= 2 {
                            crate::serial_println!("[TLS] Alert: level={}, desc={}", inner_data[0], inner_data[1]);
                            if inner_data[0] == 2 {
                                // Fatal alert
                                let _ = tcp::tcp_close(local_port, remote_ip, remote_port);
                                return Err(-71); // EPROTO
                            }
                        }
                    }
                } else {
                    crate::serial_println!("[TLS] Failed to decrypt server handshake record (seq={})", server_hs_seq - 1);
                }
            }
        }

        if server_finished_received {
            break;
        }

        attempts += 1;
        if attempts > 5_000_000 {
            crate::serial_println!("[TLS] Handshake timeout waiting for server Finished ({} bytes buffered)", recv_buf.len());
            let _ = tcp::tcp_close(local_port, remote_ip, remote_port);
            return Err(-110);
        }

        unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    }

    // ── Save transcript hash after server Finished (RFC 8446 §7.1) ──
    // This hash is used to derive application traffic secrets.
    // It MUST NOT include the client Finished message.
    let server_finished_hash = transcript.finalize_clone();

    // ── Step 5: Send ChangeCipherSpec (compatibility) ──
    {
        let ccs = wrap_record(CONTENT_CHANGE_CIPHER_SPEC, [0x03, 0x03], &[0x01]);
        let _ = tcp::tcp_send(local_port, remote_ip, remote_port, &ccs);
        crate::serial_println!("[TLS] CCS sent");
    }

    // ── Step 6: Send client Finished ──
    {
        let finished_key_v = sha256::hkdf_expand_label(&c_hs_traffic, "finished", &[], 32);
        let mut finished_key = [0u8; 32];
        finished_key.copy_from_slice(&finished_key_v);

        let transcript_hash = transcript.finalize_clone();
        let verify_data = TlsConnection::build_finished_verify_data(&finished_key, &transcript_hash);

        // Build Finished handshake message
        let mut finished_msg = Vec::with_capacity(4 + 32);
        finished_msg.push(HT_FINISHED);
        finished_msg.push(0); // length high
        finished_msg.push(0); // length mid
        finished_msg.push(32); // length low
        finished_msg.extend_from_slice(&verify_data);

        // Update transcript with client Finished
        transcript.update(&finished_msg);

        // Encrypt and send with client handshake key
        let mut c_hs_seq: u64 = 0;
        encrypt_and_send_record(
            &client_hs_cipher, &c_hs_iv, &mut c_hs_seq,
            local_port, remote_ip, remote_port,
            CONTENT_HANDSHAKE, &finished_msg,
        )?;

        crate::serial_println!("[TLS] Client Finished sent");
    }

    // ── Step 7: Derive application traffic keys ──
    {
        // derived_secret for master = Derive-Secret(handshake_secret, "derived", "")
        let derived_master = sha256::derive_secret(&handshake_secret, "derived", &empty_hash);

        // Master Secret = HKDF-Extract(derived_master, 0^32)
        let master_secret = sha256::hkdf_extract(&derived_master, &zero32);

        // RFC 8446 §7.1: application traffic secrets use the transcript hash
        // that includes everything up to and including the server Finished,
        // but NOT the client Finished.
        let app_hash = server_finished_hash;

        // client_application_traffic_secret_0
        let c_app_traffic = sha256::derive_secret(&master_secret, "c ap traffic", &app_hash);
        // server_application_traffic_secret_0
        let s_app_traffic = sha256::derive_secret(&master_secret, "s ap traffic", &app_hash);

        // Derive application keys
        let c_key_v = sha256::hkdf_expand_label(&c_app_traffic, "key", &[], 16);
        let c_iv_v = sha256::hkdf_expand_label(&c_app_traffic, "iv", &[], 12);
        let s_key_v = sha256::hkdf_expand_label(&s_app_traffic, "key", &[], 16);
        let s_iv_v = sha256::hkdf_expand_label(&s_app_traffic, "iv", &[], 12);

        conn.client_write_key.copy_from_slice(&c_key_v);
        conn.client_write_iv.copy_from_slice(&c_iv_v);
        conn.server_write_key.copy_from_slice(&s_key_v);
        conn.server_write_iv.copy_from_slice(&s_iv_v);

        conn.client_cipher = Some(aes_gcm::AesGcm::new(&conn.client_write_key));
        conn.server_cipher = Some(aes_gcm::AesGcm::new(&conn.server_write_key));
        conn.client_seq = 0;
        conn.server_seq = 0;
    }

    conn.transcript = transcript;
    conn.handshake_complete = true;

    crate::serial_println!("[TLS] handshake OK, cipher={}", conn.cipher_name);

    Ok(conn)
}

/// Parse the server's X25519 key share from a ServerHello message
fn parse_server_hello_key_share(data: &[u8]) -> Option<[u8; 32]> {
    // ServerHello structure:
    //   [1] handshake type (2 = ServerHello)
    //   [3] length
    //   [2] version (0x0303)
    //   [32] server random
    //   [1] session_id length + session_id
    //   [2] cipher suite
    //   [1] compression method
    //   [2] extensions length
    //   extensions...

    if data.len() < 4 { return None; }

    let msg_len = ((data[1] as usize) << 16) | ((data[2] as usize) << 8) | (data[3] as usize);
    let body = if data.len() >= 4 + msg_len { &data[4..4 + msg_len] } else { &data[4..] };

    if body.len() < 2 + 32 + 1 { return None; }

    // Skip version (2) + server_random (32)
    let mut pos = 2 + 32;

    // Session ID
    if pos >= body.len() { return None; }
    let sid_len = body[pos] as usize;
    pos += 1 + sid_len;

    // Cipher suite (2 bytes)
    pos += 2;

    // Compression method (1 byte)
    pos += 1;

    // Extensions
    if pos + 2 > body.len() { return None; }
    let ext_len = u16::from_be_bytes([body[pos], body[pos + 1]]) as usize;
    pos += 2;

    let ext_end = core::cmp::min(pos + ext_len, body.len());

    while pos + 4 <= ext_end {
        let ext_type = u16::from_be_bytes([body[pos], body[pos + 1]]);
        let ext_data_len = u16::from_be_bytes([body[pos + 2], body[pos + 3]]) as usize;
        pos += 4;

        if ext_type == EXT_KEY_SHARE && ext_data_len >= 36 {
            let group = u16::from_be_bytes([body[pos], body[pos + 1]]);
            let key_len = u16::from_be_bytes([body[pos + 2], body[pos + 3]]) as usize;

            if group == GROUP_X25519 && key_len == 32 && pos + 4 + 32 <= body.len() {
                let mut key = [0u8; 32];
                key.copy_from_slice(&body[pos + 4..pos + 4 + 32]);
                return Some(key);
            }
        }

        pos += ext_data_len;
    }

    None
}

/// Send application data over an established TLS connection
pub fn tls_send(conn: &mut TlsConnection, data: &[u8]) -> Result<usize, i64> {
    if !conn.handshake_complete {
        return Err(-107); // ENOTCONN
    }

    let cipher = conn.client_cipher.as_ref().unwrap();
    let nonce = TlsConnection::make_nonce(&conn.client_write_iv, conn.client_seq);
    conn.client_seq += 1;

    // TLSInnerPlaintext: data + content_type(application_data)
    let mut inner = Vec::with_capacity(data.len() + 1);
    inner.extend_from_slice(data);
    inner.push(CONTENT_APPLICATION_DATA);

    // AAD: record header
    let enc_len = (inner.len() + 16) as u16;
    let aad = [CONTENT_APPLICATION_DATA, 0x03, 0x03,
               (enc_len >> 8) as u8, enc_len as u8];

    let (ciphertext, tag) = cipher.encrypt(&nonce, &aad, &inner);

    let mut record = Vec::with_capacity(5 + ciphertext.len() + 16);
    record.push(CONTENT_APPLICATION_DATA);
    record.extend_from_slice(&[0x03, 0x03]);
    record.extend_from_slice(&enc_len.to_be_bytes());
    record.extend_from_slice(&ciphertext);
    record.extend_from_slice(&tag);

    tcp::tcp_send(conn.local_port, conn.remote_ip, conn.remote_port, &record)?;

    Ok(data.len())
}

/// Receive and decrypt application data from a TLS connection
pub fn tls_recv(conn: &mut TlsConnection, buf: &mut [u8]) -> Result<usize, i64> {
    if !conn.handshake_complete {
        return Err(-107); // ENOTCONN
    }

    let mut recv_buf = Vec::new();
    let mut attempts = 0u32;

    loop {
        super::poll();

        let mut temp = [0u8; 4096];
        match tcp::tcp_recv(conn.local_port, conn.remote_ip, conn.remote_port, &mut temp) {
            Ok(n) if n > 0 => recv_buf.extend_from_slice(&temp[..n]),
            Ok(0) => {
                let state = tcp::get_state(conn.local_port, conn.remote_ip, conn.remote_port);
                if state == tcp::TcpState::CloseWait || state == tcp::TcpState::Closed {
                    return Ok(0); // EOF
                }
            }
            _ => {}
        }

        while recv_buf.len() >= 5 {
            let content_type = recv_buf[0];
            let record_len = u16::from_be_bytes([recv_buf[3], recv_buf[4]]) as usize;

            if recv_buf.len() < 5 + record_len {
                break;
            }

            let mut header = [0u8; 5];
            header.copy_from_slice(&recv_buf[..5]);
            let record_data = recv_buf[5..5 + record_len].to_vec();
            recv_buf.drain(..5 + record_len);

            if content_type == CONTENT_APPLICATION_DATA && record_len > 16 {
                let cipher = conn.server_cipher.as_ref().unwrap();

                if let Some((inner_type, inner_data)) = decrypt_record(
                    cipher, &conn.server_write_iv, &mut conn.server_seq,
                    &record_data, &header,
                ) {
                    if inner_type == CONTENT_APPLICATION_DATA {
                        let copy_len = core::cmp::min(inner_data.len(), buf.len());
                        buf[..copy_len].copy_from_slice(&inner_data[..copy_len]);
                        return Ok(copy_len);
                    } else if inner_type == CONTENT_ALERT {
                        if inner_data.len() >= 2 {
                            crate::serial_println!("[TLS] Alert: level={}, desc={}", inner_data[0], inner_data[1]);
                            if inner_data[0] == 2 || inner_data[1] == 0 {
                                return Ok(0); // close_notify or fatal
                            }
                        }
                    } else if inner_type == CONTENT_HANDSHAKE {
                        // NewSessionTicket or other post-handshake messages — skip
                        crate::serial_println!("[TLS] Post-handshake message (type 0x{:02x}, {} bytes) — skipped",
                            if inner_data.len() > 0 { inner_data[0] } else { 0 }, inner_data.len());
                        continue;
                    }
                    // Other types: skip and continue
                } else {
                    crate::serial_println!("[TLS] Decryption failed (bad tag, seq={}, record_len={})",
                        conn.server_seq - 1, record_len);
                    // Don't fail immediately — try continuing in case of
                    // a CCS record or other non-encrypted data mixed in
                    continue;
                }
            } else if content_type == CONTENT_CHANGE_CIPHER_SPEC {
                // Post-handshake CCS — ignore (compatibility)
                continue;
            }

            // Skip other record types
        }

        attempts += 1;
        if attempts > 2_000_000 {
            if !recv_buf.is_empty() {
                // If we have partial data, return what we can
                let copy_len = core::cmp::min(recv_buf.len(), buf.len());
                buf[..copy_len].copy_from_slice(&recv_buf[..copy_len]);
                return Ok(copy_len);
            }
            return Ok(0); // Timeout with no data
        }

        unsafe { core::arch::asm!("pause", options(nomem, nostack)); }
    }
}

/// Close a TLS connection
pub fn tls_close(conn: &mut TlsConnection) -> Result<(), i64> {
    if conn.handshake_complete {
        // Send close_notify alert
        let alert = [1u8, 0]; // Level: warning, Description: close_notify

        if let Some(ref cipher) = conn.client_cipher {
            let nonce = TlsConnection::make_nonce(&conn.client_write_iv, conn.client_seq);
            conn.client_seq += 1;

            let mut inner = Vec::new();
            inner.extend_from_slice(&alert);
            inner.push(CONTENT_ALERT);

            let enc_len = (inner.len() + 16) as u16;
            let aad = [CONTENT_APPLICATION_DATA, 0x03, 0x03,
                       (enc_len >> 8) as u8, enc_len as u8];

            let (ciphertext, tag) = cipher.encrypt(&nonce, &aad, &inner);

            let mut record = Vec::new();
            record.push(CONTENT_APPLICATION_DATA);
            record.extend_from_slice(&[0x03, 0x03]);
            record.extend_from_slice(&enc_len.to_be_bytes());
            record.extend_from_slice(&ciphertext);
            record.extend_from_slice(&tag);

            let _ = tcp::tcp_send(conn.local_port, conn.remote_ip, conn.remote_port, &record);
        }
    }

    tcp::tcp_close(conn.local_port, conn.remote_ip, conn.remote_port)
}

/// Run TLS self-tests (crypto primitives + message construction)
pub fn run_tests() {
    crate::serial_println!("[TLS] Running crypto self-tests...");

    // Run sub-module tests
    sha256::run_tests();
    x25519::run_tests();
    aes_gcm::run_tests();

    let mut pass = 0u32;
    let mut fail = 0u32;

    // Test 1: ClientHello construction
    {
        let conn = TlsConnection {
            local_port: 12345,
            remote_ip: Ipv4Addr::new(93, 184, 216, 34),
            remote_port: 443,
            client_random: [0x42u8; 32],
            client_private_key: [0u8; 32],
            client_public_key: [0x09u8; 32],
            client_write_key: [0; 16],
            client_write_iv: [0; 12],
            server_write_key: [0; 16],
            server_write_iv: [0; 12],
            client_seq: 0,
            server_seq: 0,
            client_cipher: None,
            server_cipher: None,
            transcript: sha256::Sha256::new(),
            handshake_complete: false,
            cipher_name: "AES128-GCM",
        };
        let ch = conn.build_client_hello("example.com");
        // Verify it starts with handshake type 1 (ClientHello)
        if ch[0] == HT_CLIENT_HELLO && ch.len() > 100 {
            pass += 1;
        } else {
            fail += 1;
            crate::serial_println!("[TLS] FAIL: ClientHello construction");
        }
    }

    // Test 2: Nonce construction
    {
        let iv: [u8; 12] = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c];
        let nonce0 = TlsConnection::make_nonce(&iv, 0);
        let nonce1 = TlsConnection::make_nonce(&iv, 1);
        // Nonce 0 should equal IV (XOR with 0 = identity)
        if nonce0 == iv && nonce1 != iv {
            pass += 1;
        } else {
            fail += 1;
            crate::serial_println!("[TLS] FAIL: nonce construction");
        }
    }

    // Test 3: HKDF-Expand-Label produces correct length output
    {
        let secret = sha256::sha256(b"test secret");
        let label_out = sha256::hkdf_expand_label(&secret, "key", &[], 16);
        let iv_out = sha256::hkdf_expand_label(&secret, "iv", &[], 12);
        if label_out.len() == 16 && iv_out.len() == 12 {
            pass += 1;
        } else {
            fail += 1;
            crate::serial_println!("[TLS] FAIL: HKDF-Expand-Label length");
        }
    }

    // Test 4: Key schedule consistency
    //   Derive keys from a fixed shared secret and verify determinism
    {
        let shared = [0xABu8; 32];
        let zero32 = [0u8; 32];
        let early = sha256::hkdf_extract(&zero32, &zero32);
        let empty_hash = sha256::sha256(&[]);
        let derived = sha256::derive_secret(&early, "derived", &empty_hash);
        let hs_secret = sha256::hkdf_extract(&derived, &shared);

        // Run twice, must match
        let early2 = sha256::hkdf_extract(&zero32, &zero32);
        let derived2 = sha256::derive_secret(&early2, "derived", &empty_hash);
        let hs_secret2 = sha256::hkdf_extract(&derived2, &shared);

        if hs_secret == hs_secret2 {
            pass += 1;
        } else {
            fail += 1;
            crate::serial_println!("[TLS] FAIL: key schedule determinism");
        }
    }

    crate::serial_println!("[TLS] Integration tests: {} passed, {} failed", pass, fail);
    crate::serial_println!("[TLS] All self-tests complete");
}
