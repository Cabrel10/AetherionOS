// kernel/src/net/tls/aes_gcm.rs - AES-128-GCM (NIST SP 800-38D)
//
// Software implementation of AES-128-GCM for TLS 1.3.
// Uses lookup tables for AES S-box (no AES-NI dependency for portability).
// Reference: FIPS 197 (AES), NIST SP 800-38D (GCM)

use alloc::vec::Vec;

// AES S-box (substitution table)
static SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

// AES round constants
static RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

/// GF(2^8) multiplication by 2 (xtime)
#[inline]
fn xtime(a: u8) -> u8 {
    let hi = (a >> 7) & 1;
    (a << 1) ^ (hi * 0x1b)
}

/// GF(2^8) multiplication
#[inline]
fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p: u8 = 0;
    for _ in 0..8 {
        if b & 1 != 0 {
            p ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    p
}

/// AES-128 expanded key schedule (11 round keys, 176 bytes)
pub struct Aes128 {
    rk: [[u8; 16]; 11],
}

impl Aes128 {
    /// Create AES-128 cipher from 16-byte key
    pub fn new(key: &[u8; 16]) -> Self {
        let mut rk = [[0u8; 16]; 11];
        rk[0].copy_from_slice(key);

        for i in 1..11 {
            let prev = rk[i - 1];
            // RotWord + SubWord + Rcon
            let temp = [
                SBOX[prev[13] as usize] ^ RCON[i - 1],
                SBOX[prev[14] as usize],
                SBOX[prev[15] as usize],
                SBOX[prev[12] as usize],
            ];
            for j in 0..4 {
                rk[i][j] = prev[j] ^ temp[j];
            }
            for j in 4..16 {
                rk[i][j] = prev[j] ^ rk[i][j - 4];
            }
        }

        Aes128 { rk }
    }

    /// Encrypt a single 16-byte block in-place
    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        // AddRoundKey 0
        for i in 0..16 { block[i] ^= self.rk[0][i]; }

        // Rounds 1-9
        for round in 1..10 {
            // SubBytes
            for i in 0..16 { block[i] = SBOX[block[i] as usize]; }

            // ShiftRows
            let tmp = block[1];
            block[1] = block[5]; block[5] = block[9]; block[9] = block[13]; block[13] = tmp;
            let tmp = block[2]; let tmp2 = block[6];
            block[2] = block[10]; block[6] = block[14]; block[10] = tmp; block[14] = tmp2;
            let tmp = block[15];
            block[15] = block[11]; block[11] = block[7]; block[7] = block[3]; block[3] = tmp;

            // MixColumns
            for col in 0..4 {
                let c = col * 4;
                let s0 = block[c]; let s1 = block[c+1]; let s2 = block[c+2]; let s3 = block[c+3];
                block[c]   = gmul(2, s0) ^ gmul(3, s1) ^ s2 ^ s3;
                block[c+1] = s0 ^ gmul(2, s1) ^ gmul(3, s2) ^ s3;
                block[c+2] = s0 ^ s1 ^ gmul(2, s2) ^ gmul(3, s3);
                block[c+3] = gmul(3, s0) ^ s1 ^ s2 ^ gmul(2, s3);
            }

            // AddRoundKey
            for i in 0..16 { block[i] ^= self.rk[round][i]; }
        }

        // Final round (no MixColumns)
        for i in 0..16 { block[i] = SBOX[block[i] as usize]; }

        let tmp = block[1];
        block[1] = block[5]; block[5] = block[9]; block[9] = block[13]; block[13] = tmp;
        let tmp = block[2]; let tmp2 = block[6];
        block[2] = block[10]; block[6] = block[14]; block[10] = tmp; block[14] = tmp2;
        let tmp = block[15];
        block[15] = block[11]; block[11] = block[7]; block[7] = block[3]; block[3] = tmp;

        for i in 0..16 { block[i] ^= self.rk[10][i]; }
    }
}

/// GCM multiplication in GF(2^128)
/// Operates on 128-bit values represented as [u8; 16] in big-endian
fn gcm_mul(x: &[u8; 16], h: &[u8; 16]) -> [u8; 16] {
    let mut z = [0u8; 16];
    let mut v = *h;

    for i in 0..128 {
        let byte_idx = i / 8;
        let bit_idx = 7 - (i % 8);
        if (x[byte_idx] >> bit_idx) & 1 == 1 {
            for j in 0..16 { z[j] ^= v[j]; }
        }
        // Shift V right by 1, with XOR of R if LSB was 1
        let lsb = v[15] & 1;
        for j in (1..16).rev() {
            v[j] = (v[j] >> 1) | ((v[j - 1] & 1) << 7);
        }
        v[0] >>= 1;
        if lsb == 1 {
            v[0] ^= 0xE1; // R = 0xE1000...0
        }
    }
    z
}

/// Increment the rightmost 32 bits of a 128-bit counter
fn inc32(counter: &mut [u8; 16]) {
    let mut c = u32::from_be_bytes([counter[12], counter[13], counter[14], counter[15]]);
    c = c.wrapping_add(1);
    counter[12..16].copy_from_slice(&c.to_be_bytes());
}

/// AES-128-GCM context
pub struct AesGcm {
    cipher: Aes128,
    h: [u8; 16], // Hash subkey: AES_K(0^128)
}

impl AesGcm {
    /// Create a new AES-128-GCM context from a 16-byte key
    pub fn new(key: &[u8; 16]) -> Self {
        let cipher = Aes128::new(key);
        let mut h = [0u8; 16];
        cipher.encrypt_block(&mut h);
        AesGcm { cipher, h }
    }

    /// Encrypt and authenticate (AEAD)
    /// Returns (ciphertext, tag)
    pub fn encrypt(&self, nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> (Vec<u8>, [u8; 16]) {
        // J0 = nonce || 0x00000001
        let mut j0 = [0u8; 16];
        j0[..12].copy_from_slice(nonce);
        j0[15] = 1;

        // Initial counter block for tag
        let mut tag_ctr = j0;
        self.cipher.encrypt_block(&mut tag_ctr);

        // Encrypt plaintext using GCTR with J0+1 as initial counter
        let mut ctr = j0;
        inc32(&mut ctr);

        let mut ciphertext = Vec::with_capacity(plaintext.len());
        let mut offset = 0;

        while offset < plaintext.len() {
            let mut keystream = ctr;
            self.cipher.encrypt_block(&mut keystream);

            let remaining = plaintext.len() - offset;
            let block_len = core::cmp::min(remaining, 16);

            for i in 0..block_len {
                ciphertext.push(plaintext[offset + i] ^ keystream[i]);
            }

            offset += block_len;
            inc32(&mut ctr);
        }

        // Compute GHASH
        let tag = self.ghash(aad, &ciphertext, &tag_ctr);

        (ciphertext, tag)
    }

    /// Decrypt and verify (AEAD)
    /// Returns Some(plaintext) if tag is valid, None otherwise
    pub fn decrypt(&self, nonce: &[u8; 12], aad: &[u8], ciphertext: &[u8], tag: &[u8; 16]) -> Option<Vec<u8>> {
        // J0 = nonce || 0x00000001
        let mut j0 = [0u8; 16];
        j0[..12].copy_from_slice(nonce);
        j0[15] = 1;

        // Compute expected tag
        let mut tag_ctr = j0;
        self.cipher.encrypt_block(&mut tag_ctr);
        let expected_tag = self.ghash(aad, ciphertext, &tag_ctr);

        // Constant-time comparison
        let mut diff = 0u8;
        for i in 0..16 {
            diff |= tag[i] ^ expected_tag[i];
        }
        if diff != 0 {
            return None; // Tag mismatch
        }

        // Decrypt using GCTR with J0+1
        let mut ctr = j0;
        inc32(&mut ctr);

        let mut plaintext = Vec::with_capacity(ciphertext.len());
        let mut offset = 0;

        while offset < ciphertext.len() {
            let mut keystream = ctr;
            self.cipher.encrypt_block(&mut keystream);

            let remaining = ciphertext.len() - offset;
            let block_len = core::cmp::min(remaining, 16);

            for i in 0..block_len {
                plaintext.push(ciphertext[offset + i] ^ keystream[i]);
            }

            offset += block_len;
            inc32(&mut ctr);
        }

        Some(plaintext)
    }

    /// Encrypt in-place: same as encrypt but writes ciphertext + tag to output buffer
    pub fn encrypt_in_place(&self, nonce: &[u8; 12], aad: &[u8], plaintext: &[u8], output: &mut [u8]) -> usize {
        let (ct, tag) = self.encrypt(nonce, aad, plaintext);
        let total = ct.len() + 16;
        if output.len() >= total {
            output[..ct.len()].copy_from_slice(&ct);
            output[ct.len()..total].copy_from_slice(&tag);
        }
        total
    }

    /// GHASH: compute authentication tag
    fn ghash(&self, aad: &[u8], ciphertext: &[u8], tag_mask: &[u8; 16]) -> [u8; 16] {
        let mut s = [0u8; 16];

        // Process AAD
        let mut offset = 0;
        while offset < aad.len() {
            let mut block = [0u8; 16];
            let remaining = aad.len() - offset;
            let len = core::cmp::min(remaining, 16);
            block[..len].copy_from_slice(&aad[offset..offset + len]);
            for i in 0..16 { s[i] ^= block[i]; }
            s = gcm_mul(&s, &self.h);
            offset += 16;
        }

        // Process ciphertext
        offset = 0;
        while offset < ciphertext.len() {
            let mut block = [0u8; 16];
            let remaining = ciphertext.len() - offset;
            let len = core::cmp::min(remaining, 16);
            block[..len].copy_from_slice(&ciphertext[offset..offset + len]);
            for i in 0..16 { s[i] ^= block[i]; }
            s = gcm_mul(&s, &self.h);
            offset += 16;
        }

        // Length block: [len(A)]_64 || [len(C)]_64 in bits
        let mut len_block = [0u8; 16];
        len_block[..8].copy_from_slice(&((aad.len() as u64) * 8).to_be_bytes());
        len_block[8..].copy_from_slice(&((ciphertext.len() as u64) * 8).to_be_bytes());
        for i in 0..16 { s[i] ^= len_block[i]; }
        s = gcm_mul(&s, &self.h);

        // XOR with encrypted J0 (tag mask)
        for i in 0..16 { s[i] ^= tag_mask[i]; }
        s
    }
}

/// Run AES-128-GCM self-tests against NIST SP 800-38D test vectors
pub fn run_tests() {
    use crate::serial_println;
    let mut pass = 0u32;
    let mut fail = 0u32;

    // Test 1: AES-128 ECB single block (FIPS 197 Appendix B)
    //   Key = 2b7e151628aed2a6abf7158809cf4f3c
    //   Plaintext = 3243f6a8885a308d313198a2e0370734
    //   Ciphertext = 3925841d02dc09fbdc118597196a0b32
    {
        let key: [u8; 16] = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6,
            0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf, 0x4f, 0x3c,
        ];
        let mut block: [u8; 16] = [
            0x32, 0x43, 0xf6, 0xa8, 0x88, 0x5a, 0x30, 0x8d,
            0x31, 0x31, 0x98, 0xa2, 0xe0, 0x37, 0x07, 0x34,
        ];
        let expected: [u8; 16] = [
            0x39, 0x25, 0x84, 0x1d, 0x02, 0xdc, 0x09, 0xfb,
            0xdc, 0x11, 0x85, 0x97, 0x19, 0x6a, 0x0b, 0x32,
        ];
        let cipher = Aes128::new(&key);
        cipher.encrypt_block(&mut block);
        if block == expected { pass += 1; } else { fail += 1; serial_println!("[AES] FAIL: ECB single block"); }
    }

    // Test 2: AES-128-GCM encrypt then decrypt roundtrip
    {
        let key = [0u8; 16]; // all zeros
        let nonce = [0u8; 12];
        let plaintext = b"Hello, TLS 1.3!";
        let aad = b"";

        let gcm = AesGcm::new(&key);
        let (ct, tag) = gcm.encrypt(&nonce, aad, plaintext);
        let result = gcm.decrypt(&nonce, aad, &ct, &tag);
        match result {
            Some(pt) if pt == plaintext => { pass += 1; }
            _ => { fail += 1; serial_println!("[AES-GCM] FAIL: roundtrip"); }
        }
    }

    // Test 3: Verify bad tag is rejected
    {
        let key = [0u8; 16];
        let nonce = [0u8; 12];
        let plaintext = b"test data";
        let gcm = AesGcm::new(&key);
        let (ct, mut tag) = gcm.encrypt(&nonce, b"", plaintext);
        tag[0] ^= 0xFF; // corrupt the tag
        let result = gcm.decrypt(&nonce, b"", &ct, &tag);
        if result.is_none() { pass += 1; } else { fail += 1; serial_println!("[AES-GCM] FAIL: bad tag accepted"); }
    }

    // Test 4: NIST GCM Test Case 2 (SP 800-38D)
    //   Key = 00000000000000000000000000000000
    //   IV  = 000000000000000000000000
    //   PT  = 00000000000000000000000000000000 (16 zero bytes)
    //   CT  = 0388dace60b6a392f328c2b971b2fe78
    //   Tag = ab6e47d42cec13bdf53a67b21257bddf
    {
        let key = [0u8; 16];
        let nonce = [0u8; 12];
        let plaintext = [0u8; 16];
        let expected_ct: [u8; 16] = [
            0x03, 0x88, 0xda, 0xce, 0x60, 0xb6, 0xa3, 0x92,
            0xf3, 0x28, 0xc2, 0xb9, 0x71, 0xb2, 0xfe, 0x78,
        ];
        let expected_tag: [u8; 16] = [
            0xab, 0x6e, 0x47, 0xd4, 0x2c, 0xec, 0x13, 0xbd,
            0xf5, 0x3a, 0x67, 0xb2, 0x12, 0x57, 0xbd, 0xdf,
        ];
        let gcm = AesGcm::new(&key);
        let (ct, tag) = gcm.encrypt(&nonce, &[], &plaintext);
        if ct == expected_ct && tag == expected_tag {
            pass += 1;
        } else {
            fail += 1;
            serial_println!("[AES-GCM] FAIL: NIST Test Case 2");
            serial_println!("  CT: {:02X}{:02X}{:02X}{:02X}... expected {:02X}{:02X}{:02X}{:02X}...",
                ct[0], ct[1], ct[2], ct[3], expected_ct[0], expected_ct[1], expected_ct[2], expected_ct[3]);
            serial_println!("  Tag: {:02X}{:02X}{:02X}{:02X}... expected {:02X}{:02X}{:02X}{:02X}...",
                tag[0], tag[1], tag[2], tag[3], expected_tag[0], expected_tag[1], expected_tag[2], expected_tag[3]);
        }
    }

    serial_println!("[AES-GCM] Tests: {} passed, {} failed", pass, fail);
}
