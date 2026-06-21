//! AVX2/FMA-accelerated kernels for Q8_0 matrix–vector multiply.
//!
//! # Format note (important)
//! In AetherionOS the production weights are stored Q8_0 (int8 + f16 scale)
//! but the **activation** `x` is a plain `f32` vector. So the hot dot product
//! is NOT int8×int8 — it is `int8(weight) * f32(activation)`, accumulated in
//! f32 and scaled once per 32-element block by the f16 block scale.
//!
//! That means the right SIMD shape is:
//!   1. load 32 × i8 weights, sign-extend i8 → i32 → f32
//!   2. load 32 × f32 activations
//!   3. FMA-accumulate `w_f32 * x_f32`
//!   4. horizontal-sum, multiply by the (sanitised) block scale
//!
//! # Safety / prerequisites
//! All `#[target_feature(enable = "avx2,fma")]` functions are `unsafe` and may
//! only be called after the CPU has been verified to support AVX2 + FMA AND
//! after `arch::x86_64::context::enable_avx()` has configured CR4.OSXSAVE and
//! XCR0 (done once at BSP boot in `limine_entry.rs`). Callers must gate on
//! [`has_avx2_fma`].

#![allow(clippy::missing_safety_doc)]

use core::arch::x86_64::*;
use core::sync::atomic::{AtomicU8, Ordering};

use super::super::llm::matmul::f16_to_f32;

// ───────────────────────────────────────────────────────────────────────────
// Cached capability flag
// ───────────────────────────────────────────────────────────────────────────
//
// 0 = unknown, 1 = unavailable, 2 = available.
// Populated once via `init_caps()` at boot (after enable_avx()), then read
// lock-free on every matmul. We deliberately do NOT run CPUID on the hot path.
static AVX2_FMA_CAP: AtomicU8 = AtomicU8::new(0);

/// Probe AVX2+FMA via the kernel's existing CPUID helpers and cache the result.
///
/// Call this ONCE at boot, *after* `enable_avx()` has set CR4.OSXSAVE / XCR0,
/// otherwise executing AVX2 instructions would #UD even though CPUID reports
/// support. Returns the resolved capability.
pub fn init_caps() -> bool {
    let avx2 = crate::arch::x86_64::context::cpu_has_avx2();
    let fma = crate::arch::x86_64::context::cpu_has_fma();
    let osxsave = crate::arch::x86_64::context::is_xsave_enabled();
    let ok = avx2 && fma && osxsave;
    AVX2_FMA_CAP.store(if ok { 2 } else { 1 }, Ordering::Release);
    crate::serial_println!(
        "[AVX2] capability probe: avx2={} fma={} osxsave_ready={} => {}",
        avx2,
        fma,
        osxsave,
        if ok { "ENABLED (vectorised Q8_0)" } else { "disabled (scalar fallback)" }
    );
    ok
}

/// Fast, lock-free check used on the matmul hot path.
///
/// If `init_caps()` has not run yet (cap == unknown) we conservatively report
/// `false` so the scalar path is used — correctness over speed until boot has
/// explicitly enabled the vector path.
#[inline(always)]
pub fn has_avx2_fma() -> bool {
    AVX2_FMA_CAP.load(Ordering::Acquire) == 2
}

// ───────────────────────────────────────────────────────────────────────────
// Horizontal sum of a __m256 (8 × f32)
// ───────────────────────────────────────────────────────────────────────────

#[target_feature(enable = "avx2,fma")]
#[inline]
unsafe fn hsum256_ps(v: __m256) -> f32 {
    // Reduce 256 → 128 by adding the high and low lanes.
    let lo = _mm256_castps256_ps128(v);
    let hi = _mm256_extractf128_ps(v, 1);
    let sum128 = _mm_add_ps(lo, hi);
    // Reduce 128 → 64 → 32.
    let shuf = _mm_movehdup_ps(sum128); // [1,1,3,3]
    let sums = _mm_add_ps(sum128, shuf);
    let shuf2 = _mm_movehl_ps(shuf, sums);
    let sums2 = _mm_add_ss(sums, shuf2);
    _mm_cvtss_f32(sums2)
}

// ───────────────────────────────────────────────────────────────────────────
// One Q8_0 block (32 int8 weights) · 32 f32 activations
// ───────────────────────────────────────────────────────────────────────────

/// Dot product of a single Q8_0 weight block (`32 × i8`) with 32 f32
/// activations, returning the *unscaled* f32 accumulation (caller applies the
/// block scale). Requires AVX2 + FMA.
///
/// `w` must point to at least 32 i8; `x` to at least 32 f32.
#[target_feature(enable = "avx2,fma")]
#[inline]
unsafe fn dot_block32_i8_f32(w: *const i8, x: *const f32) -> f32 {
    // Load 32 i8 weights (low 16 + high 16) into one __m256i.
    let w_lo = _mm_loadu_si128(w as *const __m128i);
    let w_hi = _mm_loadu_si128(w.add(16) as *const __m128i);

    // Sign-extend i8 → i32 in four 8-wide groups.
    let w0 = _mm256_cvtepi8_epi32(w_lo); // elements 0..8
    let w1 = _mm256_cvtepi8_epi32(_mm_srli_si128(w_lo, 8)); // 8..16
    let w2 = _mm256_cvtepi8_epi32(w_hi); // 16..24
    let w3 = _mm256_cvtepi8_epi32(_mm_srli_si128(w_hi, 8)); // 24..32

    // i32 → f32.
    let wf0 = _mm256_cvtepi32_ps(w0);
    let wf1 = _mm256_cvtepi32_ps(w1);
    let wf2 = _mm256_cvtepi32_ps(w2);
    let wf3 = _mm256_cvtepi32_ps(w3);

    // Load matching 32 f32 activations.
    let xf0 = _mm256_loadu_ps(x);
    let xf1 = _mm256_loadu_ps(x.add(8));
    let xf2 = _mm256_loadu_ps(x.add(16));
    let xf3 = _mm256_loadu_ps(x.add(24));

    // FMA accumulate: acc += w * x.
    let mut acc = _mm256_setzero_ps();
    acc = _mm256_fmadd_ps(wf0, xf0, acc);
    acc = _mm256_fmadd_ps(wf1, xf1, acc);
    acc = _mm256_fmadd_ps(wf2, xf2, acc);
    acc = _mm256_fmadd_ps(wf3, xf3, acc);

    hsum256_ps(acc)
}

// ───────────────────────────────────────────────────────────────────────────
// Full Q8_0 matrix–vector multiply (AVX2 path)
// ───────────────────────────────────────────────────────────────────────────

/// AVX2/FMA implementation of `matmul_q8_0`.
///
/// Semantics are identical to the scalar `llm::matmul::matmul_q8_0`:
///   * `q8_data` is row-major Q8_0 (per block: 2-byte f16 scale + 32 × i8).
///   * `x` is the f32 activation vector of length `d_in`.
///   * `out[row] = Σ_blocks scale_b · Σ_{i in block} w_i · x_i`.
///   * f16 scales that are non-finite or `|scale| > 1000.0` are sanitised to 0,
///     matching the scalar path's corrupt-GGUF tolerance.
///   * Non-finite row results are flushed to 0.
///
/// # Safety
/// Caller must ensure AVX2 + FMA are available and `enable_avx()` ran at boot.
#[target_feature(enable = "avx2,fma")]
pub unsafe fn matmul_q8_0_avx2(
    out: &mut [f32],
    x: &[f32],
    q8_data: &[u8],
    d_in: usize,
    d_out: usize,
) {
    let blocks_per_row = d_in / 32;
    let bytes_per_row = blocks_per_row * 34; // 2 (f16) + 32 (i8) per block

    for row in 0..d_out {
        let row_start = row * bytes_per_row;
        if row_start + bytes_per_row > q8_data.len() {
            out[row] = 0.0;
            continue;
        }

        let mut acc = 0.0f32;
        for b in 0..blocks_per_row {
            let off = row_start + b * 34;
            let raw_scale =
                f16_to_f32(u16::from_le_bytes([q8_data[off], q8_data[off + 1]]));
            // Same sanitisation as the scalar path.
            let scale = if !raw_scale.is_finite() || raw_scale.abs() > 1000.0 {
                0.0
            } else {
                raw_scale
            };
            if scale == 0.0 {
                continue; // zeroed block contributes nothing
            }

            let base_idx = b * 32;
            let w_ptr = q8_data.as_ptr().add(off + 2) as *const i8;
            let x_ptr = x.as_ptr().add(base_idx);
            let block_dot = dot_block32_i8_f32(w_ptr, x_ptr);
            acc += scale * block_dot;
        }
        out[row] = if acc.is_finite() { acc } else { 0.0 };
    }
}
