//! AetherionOS Ring 3 SSE/AVX Validation Agent - Jalon 33
//!
//! Demonstrates:
//! - SSE2 128-bit vector operations (XMM registers) in Ring 3
//! - f64 floating-point math using SSE2 (addsd, mulsd, sqrtsd)
//! - Vector dot product using packed doubles (addpd, mulpd)
//! - Chained SSE operations (Euclidean distance)
//!
//! The kernel is compiled with -sse,+soft-float (never touches XMM),
//! so user FPU state naturally survives syscalls without fxsave/fxrstor.
//!
//! Build: cargo build --release --target ../../x86_64-aetherion-user.json \
//!        -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::arch::asm;
use aetherion_sdk::*;

// ============================================================
// SSE2 Vector Math Operations (inline assembly)
// ============================================================

/// Add two f64 values using SSE2 addsd instruction.
#[inline(never)]
fn sse_add_f64(a: f64, b: f64) -> f64 {
    let mut result: f64 = 0.0;
    unsafe {
        asm!(
            "movsd xmm0, [{a}]",
            "movsd xmm1, [{b}]",
            "addsd xmm0, xmm1",
            "movsd [{out}], xmm0",
            a = in(reg) &a,
            b = in(reg) &b,
            out = in(reg) &mut result as *mut f64,
            out("xmm0") _,
            out("xmm1") _,
            options(nostack),
        );
    }
    result
}

/// Multiply two f64 values using SSE2 mulsd instruction.
#[inline(never)]
fn sse_mul_f64(a: f64, b: f64) -> f64 {
    let mut result: f64 = 0.0;
    unsafe {
        asm!(
            "movsd xmm0, [{a}]",
            "movsd xmm1, [{b}]",
            "mulsd xmm0, xmm1",
            "movsd [{out}], xmm0",
            a = in(reg) &a,
            b = in(reg) &b,
            out = in(reg) &mut result as *mut f64,
            out("xmm0") _,
            out("xmm1") _,
            options(nostack),
        );
    }
    result
}

/// Compute sqrt(x) using SSE2 sqrtsd instruction.
#[inline(never)]
fn sse_sqrt_f64(x: f64) -> f64 {
    let mut result: f64 = 0.0;
    unsafe {
        asm!(
            "movsd xmm0, [{x}]",
            "sqrtsd xmm1, xmm0",
            "movsd [{out}], xmm1",
            x = in(reg) &x,
            out = in(reg) &mut result as *mut f64,
            out("xmm0") _,
            out("xmm1") _,
            options(nostack),
        );
    }
    result
}

/// Compute dot product of two 2-element f64 vectors using packed SSE2.
/// Returns a[0]*b[0] + a[1]*b[1]
#[inline(never)]
fn sse_dot_product_2(a: &[f64; 2], b: &[f64; 2]) -> f64 {
    let mut result: f64 = 0.0;
    unsafe {
        asm!(
            "movupd xmm0, [{a}]",
            "movupd xmm1, [{b}]",
            "mulpd xmm0, xmm1",
            "movhlps xmm1, xmm0",
            "addsd xmm0, xmm1",
            "movsd [{out}], xmm0",
            a = in(reg) a.as_ptr(),
            b = in(reg) b.as_ptr(),
            out = in(reg) &mut result as *mut f64,
            out("xmm0") _,
            out("xmm1") _,
            options(nostack),
        );
    }
    result
}

/// 4-element dot product using SSE2 packed operations.
/// Returns a[0]*b[0] + a[1]*b[1] + a[2]*b[2] + a[3]*b[3]
#[inline(never)]
fn sse_dot_product_4(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    let mut result: f64 = 0.0;
    unsafe {
        asm!(
            "movupd xmm0, [{a}]",
            "movupd xmm1, [{b}]",
            "mulpd  xmm0, xmm1",
            "movupd xmm2, [{a} + 16]",
            "movupd xmm3, [{b} + 16]",
            "mulpd  xmm2, xmm3",
            "addpd  xmm0, xmm2",
            "movhlps xmm1, xmm0",
            "addsd   xmm0, xmm1",
            "movsd [{out}], xmm0",
            a = in(reg) a.as_ptr(),
            b = in(reg) b.as_ptr(),
            out = in(reg) &mut result as *mut f64,
            out("xmm0") _,
            out("xmm1") _,
            out("xmm2") _,
            out("xmm3") _,
            options(nostack),
        );
    }
    result
}

/// Approximate f64 comparison with tolerance.
fn approx_eq(a: f64, b: f64, tol: f64) -> bool {
    let diff = if a > b { a - b } else { b - a };
    diff < tol
}

/// Print a truncated f64 as "integer.decimal" (2 decimal places).
fn print_f64_approx(val: f64) {
    if val < 0.0 {
        print("-");
        print_f64_approx(0.0 - val);
        return;
    }
    let int_part = val as u64;
    let frac = val - (int_part as f64);
    let frac_2 = (frac * 100.0) as u64;
    print_u64(int_part);
    print(".");
    if frac_2 < 10 { print("0"); }
    print_u64(frac_2);
}

// ============================================================
// Main: SSE Validation Suite  (5 tests)
// ============================================================

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("========================================");
    println("[J33] SSE/AVX Ring 3 Validation Agent");
    println("========================================");

    let pid = sys_getpid();
    print("[J33] PID = ");
    print_u64(pid);
    println("");

    let mut passed: u32 = 0;
    let mut failed: u32 = 0;

    // ---- TEST 1: SSE2 f64 addition ----
    print("[TEST 1/5] SSE2 addsd (3.0 + 4.0)... ");
    let sum = sse_add_f64(3.0, 4.0);
    if approx_eq(sum, 7.0, 0.001) {
        print("[J33-OK] SSE add: ");
        print_f64_approx(sum);
        println("");
        passed += 1;
    } else {
        print("FAIL (got ");
        print_f64_approx(sum);
        println(")");
        failed += 1;
    }

    // ---- TEST 2: SSE2 f64 multiplication ----
    print("[TEST 2/5] SSE2 mulsd (6.0 * 7.0)... ");
    let prod = sse_mul_f64(6.0, 7.0);
    if approx_eq(prod, 42.0, 0.001) {
        print("OK (");
        print_f64_approx(prod);
        println(")");
        passed += 1;
    } else {
        print("FAIL (got ");
        print_f64_approx(prod);
        println(")");
        failed += 1;
    }

    // ---- TEST 3: SSE2 sqrt ----
    print("[TEST 3/5] SSE2 sqrtsd (144.0)... ");
    let sq = sse_sqrt_f64(144.0);
    if approx_eq(sq, 12.0, 0.001) {
        print("OK (");
        print_f64_approx(sq);
        println(")");
        passed += 1;
    } else {
        print("FAIL (got ");
        print_f64_approx(sq);
        println(")");
        failed += 1;
    }

    // ---- TEST 4: SSE2 4-element dot product ----
    print("[TEST 4/5] SSE2 dot product 4D ([1,2,3,4].[5,6,7,8])... ");
    let a4: [f64; 4] = [1.0, 2.0, 3.0, 4.0];
    let b4: [f64; 4] = [5.0, 6.0, 7.0, 8.0];
    let dp4 = sse_dot_product_4(&a4, &b4); // 5 + 12 + 21 + 32 = 70
    if approx_eq(dp4, 70.0, 0.001) {
        print("[J33-OK] Dot product = ");
        print_f64_approx(dp4);
        println("");
        passed += 1;
    } else {
        print("FAIL (got ");
        print_f64_approx(dp4);
        println(")");
        failed += 1;
    }

    // ---- TEST 5: Chained SSE operations (Euclidean distance) ----
    // distance = sqrt((x2-x1)^2 + (y2-y1)^2)
    // (0,0) to (3,4) => distance = 5.0
    print("[TEST 5/5] Chained SSE: Euclidean distance (0,0)-(3,4)... ");
    let dx = sse_add_f64(3.0, 0.0);
    let dy = sse_add_f64(4.0, 0.0);
    let dx2 = sse_mul_f64(dx, dx);
    let dy2 = sse_mul_f64(dy, dy);
    let sum_sq = sse_add_f64(dx2, dy2);
    let dist = sse_sqrt_f64(sum_sq);
    if approx_eq(dist, 5.0, 0.001) {
        print("OK (");
        print_f64_approx(dist);
        println(")");
        passed += 1;
    } else {
        print("FAIL (got ");
        print_f64_approx(dist);
        println(")");
        failed += 1;
    }

    // ---- Summary ----
    println("");
    println("========================================");
    print("[J33] Results: ");
    print_u64(passed as u64);
    print("/");
    print_u64((passed + failed) as u64);
    println(" tests passed");

    if failed == 0 {
        println("[J33] ALL TESTS PASSED");
        println("[J33] SSE2 vector operations verified in Ring 3");
        // Publish success on Cognitive Bus (intent=0x21=33, data=passed count)
        sys_bus_publish(0x21, 1, passed as u64);
    } else {
        println("[J33] SOME TESTS FAILED!");
    }

    println("========================================");

    if failed == 0 { 0 } else { 1 }
}
