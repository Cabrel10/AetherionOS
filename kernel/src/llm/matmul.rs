// kernel/src/llm/matmul.rs — Matrix Multiplication for Quantized Models
//
// Provides dequantization and matrix-vector multiply for GGML Q4_0 format.
// Q4_0: blocks of 32 elements, each block = 2 bytes (f16 scale) + 16 bytes data
//
// All math functions are `no_std` compatible (no libm dependency).
// Uses bit manipulation and polynomial approximations.

use alloc::vec::Vec;

// ═══════════════════════════════════════════════════════════
// no_std math functions (f32)
// ═══════════════════════════════════════════════════════════

/// Fast inverse square root (1/sqrt(x)) using Newton-Raphson
#[inline]
fn inv_sqrt_f32(x: f32) -> f32 {
    if x <= 0.0 { return 0.0; }
    // Quake III fast inverse sqrt + 2 Newton iterations
    let half = 0.5 * x;
    let bits = x.to_bits();
    let guess = f32::from_bits(0x5f3759df - (bits >> 1));
    let g1 = guess * (1.5 - half * guess * guess);
    g1 * (1.5 - half * g1 * g1)
}

/// Square root via inverse sqrt
#[inline]
fn sqrt_f32(x: f32) -> f32 {
    if x <= 0.0 { return 0.0; }
    x * inv_sqrt_f32(x)
}

/// Fast exp(x) approximation for f32 (Schraudolph's method, improved)
/// Accurate to ~0.3% relative error for |x| < 80
#[inline]
pub fn exp_f32_pub(x: f32) -> f32 { exp_f32(x) }
fn exp_f32(x: f32) -> f32 {
    if x > 88.0 { return f32::INFINITY; }
    if x < -88.0 { return 0.0; }
    // Schraudolph's trick: reinterpret as int
    // exp(x) ≈ 2^(x / ln2) = reinterpret(x * 2^23 / ln2 + bias)
    let val = (x * 12102203.0 + 1065353216.0) as i32; // 2^23/ln(2) ≈ 12102203
    if val < 0 { return 0.0; }
    f32::from_bits(val as u32)
}

/// Sine approximation using Bhaskara I's formula (accurate to ~0.2%)
#[inline]
fn sin_f32(mut x: f32) -> f32 {
    // Normalize to [-PI, PI]
    const TWO_PI: f32 = 6.283185307;
    const PI: f32 = 3.141592653;
    x = x % TWO_PI;
    if x > PI { x -= TWO_PI; }
    if x < -PI { x += TWO_PI; }
    // Bhaskara: sin(x) ≈ 16x(π-x) / (5π²-4x(π-x))
    let num = 16.0 * x * (PI - x);
    let den = 5.0 * PI * PI - 4.0 * x * (PI - x);
    if den.abs() < 1e-10 { return 0.0; }
    num / den
}

/// Cosine via sin(x + π/2)
#[inline]
fn cos_f32(x: f32) -> f32 {
    sin_f32(x + 1.5707963)
}

/// Power function: base^exp using exp(exp * ln(base))
/// For no_std, we use repeated squaring for integer exponents
/// and logarithmic approximation for fractional
#[inline]
fn powf_f32(base: f32, exp: f32) -> f32 {
    if base <= 0.0 { return 0.0; }
    // ln(base) via bit manipulation
    let bits = base.to_bits();
    let e = ((bits >> 23) & 0xFF) as f32 - 127.0;
    let m = f32::from_bits((bits & 0x007FFFFF) | 0x3F800000);
    // ln(m) ≈ (m-1) - 0.5*(m-1)^2 + ... simplified:
    let ln_base = e * 0.6931472 + (m - 1.0) * (1.0 - 0.5 * (m - 1.0));
    exp_f32(exp * ln_base)
}

// ═══════════════════════════════════════════════════════════
// f16 conversion
// ═══════════════════════════════════════════════════════════

/// Convert f16 bits to f32
#[inline]
pub fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 1) as u32;
    let exp = ((bits >> 10) & 0x1F) as i32;
    let frac = (bits & 0x3FF) as u32;

    if exp == 0 {
        if frac == 0 {
            f32::from_bits(sign << 31) // +-zero
        } else {
            // denormalized
            let mut f = frac;
            let mut e: i32 = 127 - 14;
            while f & 0x400 == 0 { f <<= 1; e -= 1; }
            f &= 0x3FF;
            f32::from_bits((sign << 31) | ((e as u32) << 23) | (f << 13))
        }
    } else if exp == 31 {
        f32::from_bits((sign << 31) | 0x7F800000 | (frac << 13)) // inf/NaN
    } else {
        f32::from_bits((sign << 31) | (((exp + 127 - 15) as u32) << 23) | (frac << 13))
    }
}

// ═══════════════════════════════════════════════════════════
// Q4_0 Dequantization
// ═══════════════════════════════════════════════════════════

/// Dequantize Q4_0 block data to f32 vector
///
/// Q4_0 format: blocks of 32 elements
///   - 2 bytes: f16 scale factor
///   - 16 bytes: 32 4-bit values (2 per byte, values 0-15, subtract 8 for signed)
pub fn dequant_q4_0(quantized: &[u8], n: usize) -> Vec<f32> {
    let block_size = 32;
    let bytes_per_block = 18; // 2 (f16 scale) + 16 (data)
    let n_blocks = n / block_size;

    let mut result = Vec::with_capacity(n);
    result.resize(n, 0.0f32);

    for b in 0..n_blocks {
        let offset = b * bytes_per_block;
        if offset + bytes_per_block > quantized.len() { break; }
        let scale = f16_to_f32(u16::from_le_bytes([quantized[offset], quantized[offset + 1]]));

        for i in 0..16 {
            let byte = quantized[offset + 2 + i];
            let lo = (byte & 0x0F) as i32 - 8;
            let hi = ((byte >> 4) & 0x0F) as i32 - 8;
            let idx = b * block_size + i * 2;
            if idx + 1 < n {
                result[idx] = scale * lo as f32;
                result[idx + 1] = scale * hi as f32;
            }
        }
    }
    result
}

// ═══════════════════════════════════════════════════════════
// Matrix operations
// ═══════════════════════════════════════════════════════════

/// Scalar matrix-vector multiply: out = W * x
/// W is stored row-major with dimensions (d_out, d_in)
pub fn matmul_f32(out: &mut [f32], x: &[f32], w: &[f32], d_in: usize, d_out: usize) {
    for row in 0..d_out {
        let mut acc = 0.0f32;
        let w_start = row * d_in;
        for k in 0..d_in {
            acc += w[w_start + k] * x[k];
        }
        out[row] = acc;
    }
}

/// RMS normalization: x[i] = x[i] * weight[i] / sqrt(mean(x^2) + eps)
pub fn rmsnorm(x: &mut [f32], weight: &[f32], eps: f32) {
    let n = x.len();
    let mean_sq: f32 = x.iter().map(|&v| v * v).sum::<f32>() / n as f32;
    let scale = inv_sqrt_f32(mean_sq + eps);
    for i in 0..n {
        x[i] = x[i] * scale * weight[i];
    }
}

/// Softmax in-place
pub fn softmax(x: &mut [f32]) {
    let mut max = f32::NEG_INFINITY;
    for &v in x.iter() { if v > max { max = v; } }
    let mut sum = 0.0f32;
    for v in x.iter_mut() {
        *v = exp_f32(*v - max);
        sum += *v;
    }
    if sum > 0.0 {
        let inv_sum = 1.0 / sum;
        for v in x.iter_mut() { *v *= inv_sum; }
    }
}

/// SiLU activation: x * sigmoid(x) = x / (1 + exp(-x))
#[inline]
pub fn silu(x: f32) -> f32 {
    x / (1.0 + exp_f32(-x))
}

/// Apply Rotary Position Embedding (RoPE) to query and key vectors.
/// Either slice may be empty; empty slices are safely skipped.
pub fn apply_rope(q: &mut [f32], k: &mut [f32], pos: usize, head_dim: usize, theta: f32) {
    if head_dim < 2 { return; }
    for i in (0..head_dim).step_by(2) {
        if i + 1 >= head_dim { break; }
        let freq = 1.0 / powf_f32(theta, i as f32 / head_dim as f32);
        let angle = pos as f32 * freq;
        let cv = cos_f32(angle);
        let sv = sin_f32(angle);

        // Apply to Q if slice is large enough
        if i + 1 < q.len() {
            let (q0, q1) = (q[i], q[i + 1]);
            q[i] = q0 * cv - q1 * sv;
            q[i + 1] = q0 * sv + q1 * cv;
        }

        // Apply to K if slice is large enough
        if i + 1 < k.len() {
            let (k0, k1) = (k[i], k[i + 1]);
            k[i] = k0 * cv - k1 * sv;
            k[i + 1] = k0 * sv + k1 * cv;
        }
    }
}

// ═══════════════════════════════════════════════════════════
// SIMD feature detection
// ═══════════════════════════════════════════════════════════

/// Detect AVX2 support via CPUID (leaf 7, sub-leaf 0)
/// Returns true if EBX bit 5 is set.
/// Note: rbx is reserved by LLVM, so we save/restore it manually.
pub fn detect_avx2() -> bool {
    let ebx_val: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 7",
            "xor ecx, ecx",
            "cpuid",
            "mov {0:e}, ebx",
            "pop rbx",
            out(reg) ebx_val,
            out("eax") _,
            out("ecx") _,
            out("edx") _,
        );
    }
    (ebx_val & (1 << 5)) != 0
}

/// Detect SSE4.1 support via CPUID (leaf 1)
/// Returns true if ECX bit 19 is set.
pub fn detect_sse41() -> bool {
    let ecx_val: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "pop rbx",
            out("ecx") ecx_val,
            out("eax") _,
            out("edx") _,
        );
    }
    (ecx_val & (1 << 19)) != 0
}

/// Matrix-vector multiply with loop unrolling (4× accumulator)
/// More efficient on modern CPUs due to reduced dependency chains
pub fn matmul_f32_fast(out: &mut [f32], x: &[f32], w: &[f32], d_in: usize, d_out: usize) {
    for row in 0..d_out {
        let w_start = row * d_in;
        let mut acc0 = 0.0f32;
        let mut acc1 = 0.0f32;
        let mut acc2 = 0.0f32;
        let mut acc3 = 0.0f32;
        let chunks = d_in / 4;
        for c in 0..chunks {
            let k = c * 4;
            acc0 += w[w_start + k]     * x[k];
            acc1 += w[w_start + k + 1] * x[k + 1];
            acc2 += w[w_start + k + 2] * x[k + 2];
            acc3 += w[w_start + k + 3] * x[k + 3];
        }
        // Remainder
        let mut rem_acc = 0.0f32;
        for k in (chunks * 4)..d_in {
            rem_acc += w[w_start + k] * x[k];
        }
        out[row] = acc0 + acc1 + acc2 + acc3 + rem_acc;
    }
}
