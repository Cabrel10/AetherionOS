// kernel/src/net/tls/sha256.rs - SHA-256 (FIPS 180-4)
//
// Pure Rust implementation of SHA-256 for TLS 1.3 handshake.
// Reference: NIST FIPS 180-4, Section 6.2

use alloc::vec::Vec;

/// SHA-256 initial hash values (first 32 bits of the fractional parts
/// of the square roots of the first 8 primes)
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
    0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// SHA-256 round constants (first 32 bits of the fractional parts
/// of the cube roots of the first 64 primes)
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
    0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
    0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
    0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[inline(always)]
fn ch(x: u32, y: u32, z: u32) -> u32 { (x & y) ^ (!x & z) }
#[inline(always)]
fn maj(x: u32, y: u32, z: u32) -> u32 { (x & y) ^ (x & z) ^ (y & z) }
#[inline(always)]
fn bsig0(x: u32) -> u32 { x.rotate_right(2) ^ x.rotate_right(13) ^ x.rotate_right(22) }
#[inline(always)]
fn bsig1(x: u32) -> u32 { x.rotate_right(6) ^ x.rotate_right(11) ^ x.rotate_right(25) }
#[inline(always)]
fn ssig0(x: u32) -> u32 { x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3) }
#[inline(always)]
fn ssig1(x: u32) -> u32 { x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10) }

/// SHA-256 context for incremental hashing
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total_len: u64,
}

impl Sha256 {
    /// Create a new SHA-256 hasher
    pub fn new() -> Self {
        Sha256 {
            state: H0,
            buf: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    /// Update the hash with more data
    pub fn update(&mut self, data: &[u8]) {
        self.total_len += data.len() as u64;
        let mut offset = 0;

        // Fill buffer if partially filled
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            let take = core::cmp::min(need, data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            offset = take;

            if self.buf_len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }

        // Process complete blocks
        while offset + 64 <= data.len() {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[offset..offset + 64]);
            self.compress(&block);
            offset += 64;
        }

        // Buffer remaining
        if offset < data.len() {
            let remaining = data.len() - offset;
            self.buf[..remaining].copy_from_slice(&data[offset..]);
            self.buf_len = remaining;
        }
    }

    /// Get a snapshot of the current digest without consuming the hasher
    pub fn finalize_clone(&self) -> [u8; 32] {
        self.clone().finalize()
    }

    /// Finalize and return the 32-byte digest
    pub fn finalize(mut self) -> [u8; 32] {
        // Padding: append 0x80, zeros, then 64-bit big-endian bit length
        let bit_len = self.total_len * 8;

        // Append 0x80
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;

        // If not enough room for length (8 bytes), pad and compress
        if self.buf_len > 56 {
            for i in self.buf_len..64 {
                self.buf[i] = 0;
            }
            let block = self.buf;
            self.compress(&block);
            self.buf_len = 0;
            self.buf = [0u8; 64];
        }

        // Zero-pad to 56 bytes
        for i in self.buf_len..56 {
            self.buf[i] = 0;
        }

        // Append bit length (big-endian)
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());

        let block = self.buf;
        self.compress(&block);

        // Output
        let mut digest = [0u8; 32];
        for i in 0..8 {
            digest[i * 4..(i + 1) * 4].copy_from_slice(&self.state[i].to_be_bytes());
        }
        digest
    }

    fn compress(&mut self, block: &[u8; 64]) {
        // Prepare message schedule
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4], block[i * 4 + 1], block[i * 4 + 2], block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            w[i] = ssig1(w[i - 2])
                .wrapping_add(w[i - 7])
                .wrapping_add(ssig0(w[i - 15]))
                .wrapping_add(w[i - 16]);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;

        // 64 rounds
        for i in 0..64 {
            let t1 = h.wrapping_add(bsig1(e))
                .wrapping_add(ch(e, f, g))
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let t2 = bsig0(a).wrapping_add(maj(a, b, c));
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

/// Compute SHA-256 digest of data in one shot
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

/// HMAC-SHA256 (RFC 2104)
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; 64];

    if key.len() > 64 {
        let h = sha256(key);
        key_block[..32].copy_from_slice(&h);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    // Inner: key XOR ipad
    let mut inner = [0u8; 64];
    for i in 0..64 {
        inner[i] = key_block[i] ^ 0x36;
    }

    let mut h = Sha256::new();
    h.update(&inner);
    h.update(data);
    let inner_hash = h.finalize();

    // Outer: key XOR opad
    let mut outer = [0u8; 64];
    for i in 0..64 {
        outer[i] = key_block[i] ^ 0x5C;
    }

    let mut h2 = Sha256::new();
    h2.update(&outer);
    h2.update(&inner_hash);
    h2.finalize()
}

/// HKDF-Extract (RFC 5869): PRK = HMAC-Hash(salt, IKM)
pub fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    let salt_or_zero = if salt.is_empty() { &[0u8; 32][..] } else { salt };
    hmac_sha256(salt_or_zero, ikm)
}

/// HKDF-Expand (RFC 5869): OKM = T(1) || T(2) || ... where T(i) = HMAC-Hash(PRK, T(i-1) || info || i)
pub fn hkdf_expand(prk: &[u8], info: &[u8], length: usize) -> Vec<u8> {
    let mut okm = Vec::with_capacity(length);
    let mut t = Vec::new();
    let mut counter = 1u8;

    while okm.len() < length {
        let mut input = Vec::new();
        input.extend_from_slice(&t);
        input.extend_from_slice(info);
        input.push(counter);

        let block = hmac_sha256(prk, &input);
        t = block.to_vec();

        let remaining = length - okm.len();
        let take = core::cmp::min(remaining, 32);
        okm.extend_from_slice(&block[..take]);

        counter += 1;
    }

    okm
}

/// TLS 1.3 HKDF-Expand-Label (RFC 8446 Section 7.1)
pub fn hkdf_expand_label(secret: &[u8], label: &str, context: &[u8], length: usize) -> Vec<u8> {
    // HkdfLabel:
    //   uint16 length;
    //   opaque label<7..255> = "tls13 " + Label;
    //   opaque context<0..255> = Hash.length;
    let tls_label = alloc::format!("tls13 {}", label);
    let label_bytes = tls_label.as_bytes();

    let mut hkdf_label = Vec::new();
    hkdf_label.extend_from_slice(&(length as u16).to_be_bytes());
    hkdf_label.push(label_bytes.len() as u8);
    hkdf_label.extend_from_slice(label_bytes);
    hkdf_label.push(context.len() as u8);
    hkdf_label.extend_from_slice(context);

    hkdf_expand(secret, &hkdf_label, length)
}

/// TLS 1.3 Derive-Secret (RFC 8446 Section 7.1)
pub fn derive_secret(secret: &[u8], label: &str, messages_hash: &[u8; 32]) -> [u8; 32] {
    let expanded = hkdf_expand_label(secret, label, messages_hash, 32);
    let mut result = [0u8; 32];
    result.copy_from_slice(&expanded);
    result
}

/// Run SHA-256 / HMAC / HKDF self-tests against known test vectors
pub fn run_tests() {
    use crate::serial_println;
    let mut pass = 0u32;
    let mut fail = 0u32;

    // Test 1: SHA-256("") = e3b0c44298fc1c149afbf4c8996fb924...
    {
        let h = sha256(b"");
        let expected: [u8; 32] = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14,
            0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24,
            0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c,
            0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
        ];
        if h == expected { pass += 1; } else { fail += 1; serial_println!("[SHA256] FAIL: empty string"); }
    }

    // Test 2: SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
    {
        let h = sha256(b"abc");
        let expected: [u8; 32] = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea,
            0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23,
            0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c,
            0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
        ];
        if h == expected { pass += 1; } else { fail += 1; serial_println!("[SHA256] FAIL: abc"); }
    }

    // Test 3: incremental update matches one-shot
    {
        let mut h1 = Sha256::new();
        h1.update(b"ab");
        h1.update(b"c");
        let r1 = h1.finalize();
        let r2 = sha256(b"abc");
        if r1 == r2 { pass += 1; } else { fail += 1; serial_println!("[SHA256] FAIL: incremental"); }
    }

    // Test 4: finalize_clone doesn't consume the hasher
    {
        let mut h = Sha256::new();
        h.update(b"abc");
        let snap = h.finalize_clone();
        // Adding more data should change the result
        h.update(b"def");
        let full = h.finalize();
        let expected_abc = sha256(b"abc");
        let expected_abcdef = sha256(b"abcdef");
        if snap == expected_abc && full == expected_abcdef { pass += 1; } else { fail += 1; serial_println!("[SHA256] FAIL: finalize_clone"); }
    }

    // Test 5: HMAC-SHA256 (RFC 4231 Test Case 2)
    //   Key = "Jefe", Data = "what do ya want for nothing?"
    //   HMAC = 5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843
    {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let expected: [u8; 32] = [
            0x5b, 0xdc, 0xc1, 0x46, 0xbf, 0x60, 0x75, 0x4e,
            0x6a, 0x04, 0x24, 0x26, 0x08, 0x95, 0x75, 0xc7,
            0x5a, 0x00, 0x3f, 0x08, 0x9d, 0x27, 0x39, 0x83,
            0x9d, 0xec, 0x58, 0xb9, 0x64, 0xec, 0x38, 0x43,
        ];
        if mac == expected { pass += 1; } else { fail += 1; serial_println!("[SHA256] FAIL: HMAC-SHA256"); }
    }

    serial_println!("[SHA256] Tests: {} passed, {} failed", pass, fail);
}
