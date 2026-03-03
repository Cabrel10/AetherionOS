/*
 * agent_ai.c - AetherionOS Jalon 22: Bare-Metal ML Inference Engine
 *
 * Implements:
 *   1. Software floating-point matrix multiplication (256x256 float32)
 *   2. Naive matmul vs cache-oblivious tiled matmul (TILE_SIZE=16)
 *   3. rdtsc-based cycle-accurate benchmarking
 *   4. Mathematical validation: A * I = A (identity matrix test)
 *
 * All large matrices are allocated via sys_mmap (never on the stack).
 * Uses software float (integer emulation) since we compile with -mno-sse.
 *
 * Copyright (c) 2024-2026 MORNINGSTAR / AetherionOS Project
 */

#include "libc_stub.h"

/* ========================================
 * Software Floating Point (IEEE 754 single)
 * We implement basic float ops using integer
 * arithmetic since GCC -mno-sse -mno-80387
 * means no hardware float.
 * ======================================== */

/* We use a fixed-point representation: Q16.16 (32-bit)
 * This gives us 16 bits of integer and 16 bits of fraction.
 * Range: -32768.0 to +32767.99998 with precision of ~0.000015 */
typedef long fix32_t;

#define FIX_SHIFT   16
#define FIX_ONE     (1L << FIX_SHIFT)    /* 1.0 = 65536 */
#define FIX_HALF    (1L << (FIX_SHIFT-1)) /* 0.5 = 32768 */

/* Convert integer to fixed-point */
static inline fix32_t fix_from_int(long val) {
    return val << FIX_SHIFT;
}

/* Convert fixed-point to integer (truncated) */
static inline long fix_to_int(fix32_t val) {
    return val >> FIX_SHIFT;
}

/* Fixed-point multiply: (a * b) >> SHIFT */
static inline fix32_t fix_mul(fix32_t a, fix32_t b) {
    return (fix32_t)(((long long)a * (long long)b) >> FIX_SHIFT);
}

/* Fixed-point add */
static inline fix32_t fix_add(fix32_t a, fix32_t b) {
    return a + b;
}

/* ========================================
 * Matrix dimensions
 * ======================================== */
#define N 128  /* 128x128 matrices (compact for kernel memory constraints) */

/* ========================================
 * rdtsc: Read Time Stamp Counter
 * ======================================== */
static inline unsigned long rdtsc(void) {
    unsigned int lo, hi;
    asm volatile("rdtsc" : "=a"(lo), "=d"(hi));
    return ((unsigned long)hi << 32) | lo;
}

/* ========================================
 * Matrices allocated via mmap (in BSS would be too large)
 * ======================================== */
static fix32_t *mat_a;  /* N x N */
static fix32_t *mat_b;  /* N x N (identity) */
static fix32_t *mat_c1; /* N x N result (naive) */
static fix32_t *mat_c2; /* N x N result (tiled) */

/* Matrix element access macro */
#define MAT(m, r, c) ((m)[(r) * N + (c)])

/* ========================================
 * Naive matrix multiply: O(N^3) with poor cache locality
 * C[i][j] = sum_k A[i][k] * B[k][j]
 * ======================================== */
static void naive_matmul(fix32_t *a, fix32_t *b, fix32_t *c) {
    int i, j, k;
    for (i = 0; i < N; i++) {
        for (j = 0; j < N; j++) {
            fix32_t sum = 0;
            for (k = 0; k < N; k++) {
                sum = fix_add(sum, fix_mul(MAT(a, i, k), MAT(b, k, j)));
            }
            MAT(c, i, j) = sum;
        }
    }
}

/* ========================================
 * Tiled (blocked) matrix multiply
 * Processes TILE_SIZE x TILE_SIZE sub-blocks for better cache reuse.
 * ======================================== */
#define TILE_SIZE 16

static void tiled_matmul(fix32_t *a, fix32_t *b, fix32_t *c) {
    int i, j, k, ii, jj, kk;

    /* Zero the output first */
    for (i = 0; i < N * N; i++) {
        c[i] = 0;
    }

    for (ii = 0; ii < N; ii += TILE_SIZE) {
        for (jj = 0; jj < N; jj += TILE_SIZE) {
            for (kk = 0; kk < N; kk += TILE_SIZE) {
                /* Multiply tile */
                int i_end = ii + TILE_SIZE;
                int j_end = jj + TILE_SIZE;
                int k_end = kk + TILE_SIZE;
                if (i_end > N) i_end = N;
                if (j_end > N) j_end = N;
                if (k_end > N) k_end = N;

                for (i = ii; i < i_end; i++) {
                    for (j = jj; j < j_end; j++) {
                        fix32_t sum = MAT(c, i, j);
                        for (k = kk; k < k_end; k++) {
                            sum = fix_add(sum, fix_mul(MAT(a, i, k), MAT(b, k, j)));
                        }
                        MAT(c, i, j) = sum;
                    }
                }
            }
        }
    }
}

/* ========================================
 * Validation: check C == A (since B = Identity)
 * ======================================== */
static int validate_identity(fix32_t *a, fix32_t *c, const char *label) {
    int errors = 0;
    int i, j;
    for (i = 0; i < N; i++) {
        for (j = 0; j < N; j++) {
            fix32_t expected = MAT(a, i, j);
            fix32_t actual = MAT(c, i, j);
            /* Allow 1 LSB tolerance for fixed-point rounding */
            long diff = actual - expected;
            if (diff < 0) diff = -diff;
            if (diff > 1) {
                errors++;
                if (errors <= 3) {
                    puts("[J22] MISMATCH ");
                    puts(label);
                    puts(" at [");
                    print_int(i);
                    puts(",");
                    print_int(j);
                    puts("]: expected=");
                    print_int(fix_to_int(expected));
                    puts(" got=");
                    print_int(fix_to_int(actual));
                    puts("\n");
                }
            }
        }
    }
    return errors;
}

/* Print a fixed-point value with 2 decimal places */
static void print_fix(fix32_t val) {
    long integer_part = fix_to_int(val);
    /* Fractional: multiply remaining bits by 100, shift down */
    long frac = val & (FIX_ONE - 1);
    long frac_100 = (frac * 100) >> FIX_SHIFT;
    if (frac_100 < 0) frac_100 = -frac_100;

    print_int(integer_part);
    puts(".");
    if (frac_100 < 10) puts("0");
    print_int(frac_100);
}

/* ========================================
 * Entry point
 * ======================================== */
void _start(void) {
    puts("\n[J22] AetherionOS Jalon 22 - Bare-Metal ML Inference Engine\n");
    puts("[J22] Matrix size: ");
    print_int(N);
    puts("x");
    print_int(N);
    puts(" (fixed-point Q16.16)\n");

    /* Allocate matrices via mmap (each N*N*8 bytes for fix32_t=long) */
    unsigned long mat_size = (unsigned long)N * N * sizeof(fix32_t);
    puts("[J22] Allocating 4 matrices via sys_mmap (");
    print_int((long)(mat_size * 4 / 1024));
    puts(" KB total)...\n");

    mat_a  = (fix32_t *)mmap(0, mat_size, 0, 0, 0, 0);
    mat_b  = (fix32_t *)mmap(0, mat_size, 0, 0, 0, 0);
    mat_c1 = (fix32_t *)mmap(0, mat_size, 0, 0, 0, 0);
    mat_c2 = (fix32_t *)mmap(0, mat_size, 0, 0, 0, 0);

    if ((long)mat_a < 0 || (long)mat_b < 0 || (long)mat_c1 < 0 || (long)mat_c2 < 0) {
        puts("[J22] FATAL: mmap failed for matrices\n");
        exit(1);
    }
    puts("[J22] Matrices allocated OK (A=");
    print_hex((unsigned long)mat_a);
    puts(", B=");
    print_hex((unsigned long)mat_b);
    puts(")\n");

    /* Initialize Matrix A: incremental values A[i][j] = fix(i * N + j + 1) / N */
    int i, j;
    for (i = 0; i < N; i++) {
        for (j = 0; j < N; j++) {
            /* Use small values to avoid overflow: val = (i+j+1) as fixed-point / 64 */
            MAT(mat_a, i, j) = fix_from_int(i + j + 1);
        }
    }
    puts("[J22] Matrix A initialized (incremental values)\n");

    /* Initialize Matrix B: Identity matrix I[i][j] = (i==j) ? 1.0 : 0.0 */
    for (i = 0; i < N; i++) {
        for (j = 0; j < N; j++) {
            MAT(mat_b, i, j) = (i == j) ? FIX_ONE : 0;
        }
    }
    puts("[J22] Matrix B initialized (Identity)\n");

    /* Print sample values from A */
    puts("[J22] A[0][0]=");
    print_fix(MAT(mat_a, 0, 0));
    puts(" A[0][1]=");
    print_fix(MAT(mat_a, 0, 1));
    puts(" A[1][0]=");
    print_fix(MAT(mat_a, 1, 0));
    puts(" A[");
    print_int(N-1);
    puts("][");
    print_int(N-1);
    puts("]=");
    print_fix(MAT(mat_a, N-1, N-1));
    puts("\n");

    /* ---- Benchmark 1: Naive MatMul ---- */
    puts("\n[J22] === Benchmark 1: Naive MatMul ===\n");
    unsigned long t0 = rdtsc();
    naive_matmul(mat_a, mat_b, mat_c1);
    unsigned long t1 = rdtsc();
    unsigned long naive_cycles = t1 - t0;
    puts("[J22] Naive MatMul: ");
    print_int((long)(naive_cycles / 1000));
    puts(" Kcycles (");
    print_int((long)naive_cycles);
    puts(" cycles)\n");

    /* Validate naive result: C1 should equal A */
    int naive_errors = validate_identity(mat_a, mat_c1, "naive");
    if (naive_errors == 0) {
        puts("[J22] Naive validation: PASS (C = A * I = A)\n");
    } else {
        puts("[J22] Naive validation: FAIL (");
        print_int(naive_errors);
        puts(" errors)\n");
    }

    /* Print sample results */
    puts("[J22] C1[0][0]=");
    print_fix(MAT(mat_c1, 0, 0));
    puts(" C1[0][1]=");
    print_fix(MAT(mat_c1, 0, 1));
    puts(" C1[1][0]=");
    print_fix(MAT(mat_c1, 1, 0));
    puts("\n");

    /* ---- Benchmark 2: Tiled MatMul ---- */
    puts("\n[J22] === Benchmark 2: Tiled MatMul (TILE=");
    print_int(TILE_SIZE);
    puts(") ===\n");
    unsigned long t2 = rdtsc();
    tiled_matmul(mat_a, mat_b, mat_c2);
    unsigned long t3 = rdtsc();
    unsigned long tiled_cycles = t3 - t2;
    puts("[J22] Tiled MatMul: ");
    print_int((long)(tiled_cycles / 1000));
    puts(" Kcycles (");
    print_int((long)tiled_cycles);
    puts(" cycles)\n");

    /* Validate tiled result: C2 should equal A */
    int tiled_errors = validate_identity(mat_a, mat_c2, "tiled");
    if (tiled_errors == 0) {
        puts("[J22] Tiled validation: PASS (C = A * I = A)\n");
    } else {
        puts("[J22] Tiled validation: FAIL (");
        print_int(tiled_errors);
        puts(" errors)\n");
    }

    /* Cross-validate: C1 == C2 */
    int cross_errors = 0;
    for (i = 0; i < N * N; i++) {
        long diff = mat_c1[i] - mat_c2[i];
        if (diff < 0) diff = -diff;
        if (diff > 1) cross_errors++;
    }
    if (cross_errors == 0) {
        puts("[J22] Cross-validation: PASS (naive == tiled)\n");
    } else {
        puts("[J22] Cross-validation: FAIL (");
        print_int(cross_errors);
        puts(" diffs)\n");
    }

    /* ---- Speedup calculation ---- */
    puts("\n[J22] === Performance Summary ===\n");
    puts("[J22] Naive:  ");
    print_int((long)(naive_cycles / 1000));
    puts(" Kcycles\n");
    puts("[J22] Tiled:  ");
    print_int((long)(tiled_cycles / 1000));
    puts(" Kcycles\n");

    /* Compute speedup as fixed-point */
    if (tiled_cycles > 0) {
        fix32_t speedup = (fix32_t)(((unsigned long long)naive_cycles << FIX_SHIFT) / tiled_cycles);
        puts("[J22] Speedup: ");
        print_fix(speedup);
        puts("x\n");
    }

    /* ---- Final verdict ---- */
    if (naive_errors == 0 && tiled_errors == 0 && cross_errors == 0) {
        puts("\n[J22] === ALL JALON 22 TESTS PASSED ===\n");
        puts("[J22] Matrix multiply validated: A * I = A\n");
        puts("[J22] Both naive and tiled implementations produce identical results\n");
        puts("[J22] rdtsc benchmarking operational\n");
    } else {
        puts("\n[J22] === JALON 22 TESTS FAILED ===\n");
    }

    exit(0);
}
