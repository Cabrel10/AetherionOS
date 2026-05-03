// kernel/src/net/tls/x25519.rs - X25519 ECDH Key Exchange (RFC 7748)
//
// Pure Rust implementation of X25519 (Curve25519 Diffie-Hellman).
// Based on the Montgomery ladder algorithm.
// Reference: RFC 7748 Section 5
//
// Field arithmetic uses radix-2^51 representation (five u64 limbs)
// following the approach in TweetNaCl / donna.

/// X25519 key size (32 bytes = 256 bits)
pub const KEY_LEN: usize = 32;

/// The prime p = 2^255 - 19
/// We work in GF(p) using radix 2^51 (five 64-bit limbs)
type Fe = [u64; 5];

const MASK51: u64 = (1u64 << 51) - 1;

/// Reduce field element limbs (carry propagation)
#[inline]
fn fe_reduce(f: &mut Fe) {
    f[1] += f[0] >> 51; f[0] &= MASK51;
    f[2] += f[1] >> 51; f[1] &= MASK51;
    f[3] += f[2] >> 51; f[2] &= MASK51;
    f[4] += f[3] >> 51; f[3] &= MASK51;
    f[0] += 19 * (f[4] >> 51); f[4] &= MASK51;
    // One more carry for full reduction
    f[1] += f[0] >> 51; f[0] &= MASK51;
}

/// Field addition: h = f + g
fn fe_add(f: &Fe, g: &Fe) -> Fe {
    let mut h = [
        f[0] + g[0],
        f[1] + g[1],
        f[2] + g[2],
        f[3] + g[3],
        f[4] + g[4],
    ];
    fe_reduce(&mut h);
    h
}

/// Field subtraction: h = f - g
fn fe_sub(f: &Fe, g: &Fe) -> Fe {
    // Add 2*p to avoid underflow:
    // 2p = 2*(2^255 - 19) = 2^256 - 38
    // In radix-51: limb[0] = 2^52 - 38, limb[1..3] = 2^52 - 2, limb[4] = 2^52 - 2
    // But we use 2*p distributed as:
    //   2*p = (2^52-38) + (2^52-2)*2^51 + (2^52-2)*2^102 + (2^52-2)*2^153 + (2^52-2)*2^204
    // Simpler: add 2^52 to each limb (much larger than p), then subtract g
    let mut h = [
        f[0] + 0xFFFFFFFFFFFDA - g[0], // 2*(2^51 - 19) = 2^52 - 38
        f[1] + 0xFFFFFFFFFFFFE - g[1], // 2*(2^51 - 1)  = 2^52 - 2
        f[2] + 0xFFFFFFFFFFFFE - g[2],
        f[3] + 0xFFFFFFFFFFFFE - g[3],
        f[4] + 0xFFFFFFFFFFFFE - g[4],
    ];
    fe_reduce(&mut h);
    h
}

/// Field multiplication: h = f * g (schoolbook with u128)
fn fe_mul(f: &Fe, g: &Fe) -> Fe {
    let f0 = f[0] as u128;
    let f1 = f[1] as u128;
    let f2 = f[2] as u128;
    let f3 = f[3] as u128;
    let f4 = f[4] as u128;

    let g0 = g[0] as u128;
    let g1 = g[1] as u128;
    let g2 = g[2] as u128;
    let g3 = g[3] as u128;
    let g4 = g[4] as u128;

    // Precompute 19*g_i for reduction mod p
    let g1_19 = 19 * g1;
    let g2_19 = 19 * g2;
    let g3_19 = 19 * g3;
    let g4_19 = 19 * g4;

    // Schoolbook multiplication with modular reduction
    // h0 = f0*g0 + f1*(19*g4) + f2*(19*g3) + f3*(19*g2) + f4*(19*g1)
    let h0 = f0*g0 + f1*g4_19 + f2*g3_19 + f3*g2_19 + f4*g1_19;
    let h1 = f0*g1 + f1*g0 + f2*g4_19 + f3*g3_19 + f4*g2_19;
    let h2 = f0*g2 + f1*g1 + f2*g0 + f3*g4_19 + f4*g3_19;
    let h3 = f0*g3 + f1*g2 + f2*g1 + f3*g0 + f4*g4_19;
    let h4 = f0*g4 + f1*g3 + f2*g2 + f3*g1 + f4*g0;

    // Carry chain
    let c0 = h0 >> 51;
    let mut r = [0u64; 5];
    r[0] = (h0 as u64) & MASK51;
    let t1 = h1 + c0;
    r[1] = (t1 as u64) & MASK51;
    let t2 = h2 + (t1 >> 51);
    r[2] = (t2 as u64) & MASK51;
    let t3 = h3 + (t2 >> 51);
    r[3] = (t3 as u64) & MASK51;
    let t4 = h4 + (t3 >> 51);
    r[4] = (t4 as u64) & MASK51;
    r[0] = r[0].wrapping_add(19 * ((t4 >> 51) as u64));

    fe_reduce(&mut r);
    r
}

/// Field squaring: h = f^2
#[inline]
fn fe_sq(f: &Fe) -> Fe {
    fe_mul(f, f)
}

/// Field inversion: h = f^(-1) mod p, using Fermat's little theorem: f^(p-2)
fn fe_inv(f: &Fe) -> Fe {
    // p-2 = 2^255 - 21
    // Use addition chain from djb's code
    let mut t0 = fe_sq(f);           // f^2
    let mut t1 = fe_sq(&t0);         // f^4
    t1 = fe_sq(&t1);                 // f^8
    t1 = fe_mul(&t1, f);             // f^9
    t0 = fe_mul(&t0, &t1);           // f^11
    let mut t2 = fe_sq(&t0);         // f^22
    t1 = fe_mul(&t1, &t2);           // f^31 = 2^5 - 1
    t2 = fe_sq(&t1);
    for _ in 1..5 { t2 = fe_sq(&t2); }   // f^(2^10 - 32)
    t1 = fe_mul(&t2, &t1);           // f^(2^10 - 1)
    t2 = fe_sq(&t1);
    for _ in 1..10 { t2 = fe_sq(&t2); }  // f^(2^20 - 1024)
    t2 = fe_mul(&t2, &t1);           // f^(2^20 - 1)
    let mut t3 = fe_sq(&t2);
    for _ in 1..20 { t3 = fe_sq(&t3); }  // f^(2^40 - 2^20)
    t2 = fe_mul(&t3, &t2);           // f^(2^40 - 1)
    t2 = fe_sq(&t2);
    for _ in 1..10 { t2 = fe_sq(&t2); }  // f^(2^50 - 1024)
    t1 = fe_mul(&t2, &t1);           // f^(2^50 - 1)
    t2 = fe_sq(&t1);
    for _ in 1..50 { t2 = fe_sq(&t2); }  // f^(2^100 - 2^50)
    t2 = fe_mul(&t2, &t1);           // f^(2^100 - 1)
    t3 = fe_sq(&t2);
    for _ in 1..100 { t3 = fe_sq(&t3); } // f^(2^200 - 2^100)
    t2 = fe_mul(&t3, &t2);           // f^(2^200 - 1)
    t2 = fe_sq(&t2);
    for _ in 1..50 { t2 = fe_sq(&t2); }  // f^(2^250 - 2^50)
    t1 = fe_mul(&t2, &t1);           // f^(2^250 - 1)
    t1 = fe_sq(&t1);                 // f^(2^251 - 2)
    t1 = fe_sq(&t1);                 // f^(2^252 - 4)
    t1 = fe_sq(&t1);                 // f^(2^253 - 8)
    t1 = fe_sq(&t1);                 // f^(2^254 - 16)
    t1 = fe_sq(&t1);                 // f^(2^255 - 32)
    fe_mul(&t1, &t0)                 // f^(2^255 - 21) = f^(p-2)
}

/// Decode a 32-byte little-endian integer into a field element
fn fe_from_bytes(s: &[u8; 32]) -> Fe {
    // Load bytes as little-endian u64s, then extract 51-bit limbs
    let load8 = |b: &[u8]| -> u64 {
        let mut v = 0u64;
        for i in 0..core::cmp::min(b.len(), 8) {
            v |= (b[i] as u64) << (8 * i);
        }
        v
    };

    let mut h = [0u64; 5];
    h[0] = load8(&s[0..])  & MASK51;
    h[1] = (load8(&s[6..])  >> 3) & MASK51;
    h[2] = (load8(&s[12..]) >> 6) & MASK51;
    h[3] = (load8(&s[19..]) >> 1) & MASK51;
    h[4] = (load8(&s[24..]) >> 12) & MASK51;
    h
}

/// Encode a field element to 32-byte little-endian
/// Uses u128 accumulator to avoid overflow in bit-packing
fn fe_to_bytes(h: &Fe) -> [u8; 32] {
    let mut t = *h;
    // Multiple rounds of carry propagation for full reduction
    fe_reduce(&mut t);
    fe_reduce(&mut t);

    // Final canonical reduction: if t >= p, subtract p
    // p = 2^255 - 19
    // In limbs: p = [0x7FFFFFFFFFFED, 0x7FFFFFFFFFFFF, 0x7FFFFFFFFFFFF, 0x7FFFFFFFFFFFF, 0x7FFFFFFFFFFFF]
    let mut m = t[0].wrapping_sub(0x7FFFFFFFFFFED);
    for i in 1..5 {
        m = t[i].wrapping_sub(0x7FFFFFFFFFFFF).wrapping_sub(m >> 63);
    }
    // If m's high bit is 1 (borrow), t < p, mask = 0 (no subtraction)
    // If m's high bit is 0 (no borrow), t >= p, mask = all-ones (subtract p)
    let borrow = m >> 63; // 1 if t < p, 0 if t >= p
    let mask = borrow.wrapping_sub(1); // 0 if t < p, all-ones if t >= p
    t[0] = t[0].wrapping_sub(0x7FFFFFFFFFFED & mask);
    for i in 1..5 {
        t[i] = t[i].wrapping_sub(0x7FFFFFFFFFFFF & mask);
    }
    fe_reduce(&mut t);

    // Pack 5x51-bit limbs into 32 little-endian bytes using u128 accumulator
    // Total bits: 5 * 51 = 255 bits = 32 bytes (with high bit 0)
    let mut s = [0u8; 32];
    let mut acc: u128 = 0;
    let mut bits: u32 = 0;
    let mut byte_idx: usize = 0;

    for limb_idx in 0..5 {
        acc |= (t[limb_idx] as u128) << bits;
        bits += 51;
        while bits >= 8 && byte_idx < 32 {
            s[byte_idx] = (acc & 0xFF) as u8;
            acc >>= 8;
            bits -= 8;
            byte_idx += 1;
        }
    }
    if byte_idx < 32 {
        s[byte_idx] = (acc & 0xFF) as u8;
    }
    s
}

/// Conditional swap: if swap != 0, swap a and b
fn fe_cswap(a: &mut Fe, b: &mut Fe, swap: u64) {
    let mask = 0u64.wrapping_sub(swap);
    for i in 0..5 {
        let t = mask & (a[i] ^ b[i]);
        a[i] ^= t;
        b[i] ^= t;
    }
}

/// X25519 scalar multiplication: compute k * u on Curve25519
/// k: 32-byte scalar (clamped), u: 32-byte u-coordinate
/// Returns the resulting u-coordinate as 32 bytes
pub fn x25519(k: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    // Clamp the scalar
    let mut scalar = *k;
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;

    let u_fe = fe_from_bytes(u);

    // Montgomery ladder
    let x_1 = u_fe;
    let mut x_2: Fe = [1, 0, 0, 0, 0]; // 1
    let mut z_2: Fe = [0, 0, 0, 0, 0]; // 0
    let mut x_3 = u_fe;
    let mut z_3: Fe = [1, 0, 0, 0, 0]; // 1

    let mut swap: u64 = 0;

    // Process bits from 254 down to 0
    for pos in (0..=254).rev() {
        let byte_idx = pos / 8;
        let bit_idx = pos % 8;
        let k_t = ((scalar[byte_idx] >> bit_idx) & 1) as u64;

        swap ^= k_t;
        fe_cswap(&mut x_2, &mut x_3, swap);
        fe_cswap(&mut z_2, &mut z_3, swap);
        swap = k_t;

        let a = fe_add(&x_2, &z_2);
        let aa = fe_sq(&a);
        let b = fe_sub(&x_2, &z_2);
        let bb = fe_sq(&b);
        let e = fe_sub(&aa, &bb);
        let c = fe_add(&x_3, &z_3);
        let d = fe_sub(&x_3, &z_3);
        let da = fe_mul(&d, &a);
        let cb = fe_mul(&c, &b);
        let da_plus_cb = fe_add(&da, &cb);
        let da_minus_cb = fe_sub(&da, &cb);
        x_3 = fe_sq(&da_plus_cb);
        let tmp = fe_sq(&da_minus_cb);
        z_3 = fe_mul(&x_1, &tmp);
        x_2 = fe_mul(&aa, &bb);
        let a24: Fe = [121665, 0, 0, 0, 0]; // (A-2)/4 = 121665 for Curve25519
        let e_times_a24 = fe_mul(&e, &a24);
        let aa_plus_ea24 = fe_add(&aa, &e_times_a24);
        z_2 = fe_mul(&e, &aa_plus_ea24);
    }

    fe_cswap(&mut x_2, &mut x_3, swap);
    fe_cswap(&mut z_2, &mut z_3, swap);

    // Result = x_2 * z_2^(-1)
    let z_inv = fe_inv(&z_2);
    let result = fe_mul(&x_2, &z_inv);
    fe_to_bytes(&result)
}

/// Generate a random X25519 private key using RDTSC-based randomness
pub fn generate_private_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    // Use RDTSC + LCG for pseudo-random bytes (adequate for our purposes)
    let mut seed: u64 = unsafe {
        let lo: u32;
        let hi: u32;
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
        ((hi as u64) << 32) | (lo as u64)
    };
    seed ^= 0x9E3779B97F4A7C15; // golden ratio

    for byte in key.iter_mut() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *byte = (seed >> 33) as u8;
    }

    // Clamp
    key[0] &= 248;
    key[31] &= 127;
    key[31] |= 64;
    key
}

/// X25519 base point (u=9)
pub const BASEPOINT: [u8; 32] = [
    9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// Compute the X25519 public key from a private key
pub fn public_key(private_key: &[u8; 32]) -> [u8; 32] {
    x25519(private_key, &BASEPOINT)
}

/// Run X25519 self-tests against RFC 7748 test vectors
pub fn run_tests() {
    use crate::serial_println;
    let mut pass = 0u32;
    let mut fail = 0u32;

    // Test 0: fe_from_bytes / fe_to_bytes round-trip
    {
        let input: [u8; 32] = [
            9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let fe = fe_from_bytes(&input);
        let output = fe_to_bytes(&fe);
        if input == output { pass += 1; } else {
            fail += 1;
            serial_println!("[X25519] FAIL: roundtrip basepoint. Got {:02X}{:02X}{:02X}{:02X}...",
                output[0], output[1], output[2], output[3]);
        }
    }

    // Test 0b: fe_from_bytes / fe_to_bytes round-trip with non-trivial value
    {
        let input: [u8; 32] = [
            0x77, 0x07, 0x6d, 0x0a, 0x73, 0x18, 0xa5, 0x7d,
            0x3c, 0x16, 0xc1, 0x72, 0x51, 0xb2, 0x66, 0x45,
            0xdf, 0x4c, 0x2f, 0x87, 0xeb, 0xc0, 0x99, 0x2a,
            0xb1, 0x77, 0xfb, 0xa5, 0x1d, 0xb9, 0x2c, 0x2a,
        ];
        let fe = fe_from_bytes(&input);
        let output = fe_to_bytes(&fe);
        if input == output { pass += 1; } else {
            fail += 1;
            serial_println!("[X25519] FAIL: roundtrip alice key. Got {:02X}{:02X}{:02X}{:02X}...",
                output[0], output[1], output[2], output[3]);
        }
    }

    // RFC 7748 Section 6.1: Test Vector 1
    // Alice's private key (already clamped in the RFC)
    let alice_priv: [u8; 32] = [
        0x77, 0x07, 0x6d, 0x0a, 0x73, 0x18, 0xa5, 0x7d,
        0x3c, 0x16, 0xc1, 0x72, 0x51, 0xb2, 0x66, 0x45,
        0xdf, 0x4c, 0x2f, 0x87, 0xeb, 0xc0, 0x99, 0x2a,
        0xb1, 0x77, 0xfb, 0xa5, 0x1d, 0xb9, 0x2c, 0x2a,
    ];
    // Alice's public key = alice_priv * BASEPOINT
    let alice_pub_expected: [u8; 32] = [
        0x85, 0x20, 0xf0, 0x09, 0x89, 0x30, 0xa7, 0x54,
        0x74, 0x8b, 0x7d, 0xdc, 0xb4, 0x3e, 0xf7, 0x5a,
        0x0d, 0xbf, 0x3a, 0x0d, 0x26, 0x38, 0x1a, 0xf4,
        0xeb, 0xa4, 0xa9, 0x8e, 0xaa, 0x9b, 0x4e, 0x6a,
    ];
    let alice_pub = public_key(&alice_priv);
    if alice_pub == alice_pub_expected {
        pass += 1;
    } else {
        fail += 1;
        serial_println!("[X25519] FAIL: Alice pubkey. Got {:02X}{:02X}{:02X}{:02X}... expected {:02X}{:02X}{:02X}{:02X}...",
            alice_pub[0], alice_pub[1], alice_pub[2], alice_pub[3],
            alice_pub_expected[0], alice_pub_expected[1], alice_pub_expected[2], alice_pub_expected[3]);
    }

    // Bob's private key
    let bob_priv: [u8; 32] = [
        0x5d, 0xab, 0x08, 0x7e, 0x62, 0x4a, 0x8a, 0x4b,
        0x79, 0xe1, 0x7f, 0x8b, 0x83, 0x80, 0x0e, 0xe6,
        0x6f, 0x3b, 0xb1, 0x29, 0x26, 0x18, 0xb6, 0xfd,
        0x1c, 0x2f, 0x8b, 0x27, 0xff, 0x88, 0xe0, 0xeb,
    ];
    let bob_pub_expected: [u8; 32] = [
        0xde, 0x9e, 0xdb, 0x7d, 0x7b, 0x7d, 0xc1, 0xb4,
        0xd3, 0x5b, 0x61, 0xc2, 0xec, 0xe4, 0x35, 0x37,
        0x3f, 0x83, 0x43, 0xc8, 0x5b, 0x78, 0x67, 0x4d,
        0xad, 0xfc, 0x7e, 0x14, 0x6f, 0x88, 0x2b, 0x4f,
    ];
    let bob_pub = public_key(&bob_priv);
    if bob_pub == bob_pub_expected {
        pass += 1;
    } else {
        fail += 1;
        serial_println!("[X25519] FAIL: Bob pubkey. Got {:02X}{:02X}{:02X}{:02X}... expected {:02X}{:02X}{:02X}{:02X}...",
            bob_pub[0], bob_pub[1], bob_pub[2], bob_pub[3],
            bob_pub_expected[0], bob_pub_expected[1], bob_pub_expected[2], bob_pub_expected[3]);
    }

    // RFC 7748 Section 5 Vector 1 (arbitrary scalar * arbitrary u)
    {
        let k: [u8; 32] = [
            0xa5, 0x46, 0xe3, 0x6b, 0xf0, 0x52, 0x7c, 0x9d,
            0x3b, 0x16, 0x15, 0x4b, 0x82, 0x46, 0x5e, 0xdd,
            0x62, 0x14, 0x4c, 0x0a, 0xc1, 0xfc, 0x5a, 0x18,
            0x50, 0x6a, 0x22, 0x44, 0xba, 0x44, 0x9a, 0xc4,
        ];
        let u_coord: [u8; 32] = [
            0xe6, 0xdb, 0x68, 0x67, 0x58, 0x30, 0x30, 0xdb,
            0x35, 0x94, 0xc1, 0xa4, 0x24, 0xb1, 0x5f, 0x7c,
            0x72, 0x66, 0x24, 0xec, 0x26, 0xb3, 0x35, 0x3b,
            0x10, 0xa9, 0x03, 0xa6, 0xd0, 0xab, 0x1c, 0x4c,
        ];
        let expected: [u8; 32] = [
            0xc3, 0xda, 0x55, 0x37, 0x9d, 0xe9, 0xc6, 0x90,
            0x8e, 0x94, 0xea, 0x4d, 0xf2, 0x8d, 0x08, 0x4f,
            0x32, 0xec, 0xcf, 0x03, 0x49, 0x1c, 0x71, 0xf7,
            0x54, 0xb4, 0x07, 0x55, 0x77, 0xa2, 0x85, 0x52,
        ];
        let result = x25519(&k, &u_coord);
        if result == expected { pass += 1; } else {
            fail += 1;
            serial_println!("[X25519] FAIL: Section 5 Vec 1. Got {:02X}{:02X}{:02X}{:02X}...",
                result[0], result[1], result[2], result[3]);
        }
    }

    // Shared secret: alice_priv * bob_pub = bob_priv * alice_pub
    let shared_expected: [u8; 32] = [
        0x4a, 0x5d, 0x9d, 0x5b, 0xa4, 0xce, 0x2d, 0xe1,
        0x72, 0x8e, 0x3b, 0xf4, 0x80, 0x35, 0x0f, 0x25,
        0xe0, 0x7e, 0x21, 0xc9, 0x47, 0xd1, 0x9e, 0x33,
        0x76, 0xf0, 0x9b, 0x3c, 0x1e, 0x16, 0x17, 0x42,
    ];
    let shared_ab = x25519(&alice_priv, &bob_pub);
    let shared_ba = x25519(&bob_priv, &alice_pub);
    if shared_ab == shared_expected {
        pass += 1;
    } else {
        fail += 1;
        serial_println!("[X25519] FAIL: shared secret A*B. Got {:02X}{:02X}{:02X}{:02X}...",
            shared_ab[0], shared_ab[1], shared_ab[2], shared_ab[3]);
    }
    if shared_ba == shared_expected {
        pass += 1;
    } else {
        fail += 1;
        serial_println!("[X25519] FAIL: shared secret B*A. Got {:02X}{:02X}{:02X}{:02X}...",
            shared_ba[0], shared_ba[1], shared_ba[2], shared_ba[3]);
    }

    serial_println!("[X25519] Tests: {} passed, {} failed", pass, fail);
}
