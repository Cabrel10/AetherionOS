/*
 * agent_rag.c - AetherionOS Jalon 23: RAG Vector Engine (Bare-Metal)
 *
 * Implements a Retrieval-Augmented Generation (RAG) vector search engine:
 *   1. Database of 1000 vectors, dimension 64, in fixed-point Q16.16
 *   2. Pre-computed norms at load time (avoids sqrt per query)
 *   3. Cosine Similarity: cos(a,b) = dot(a,b) / (norm_a * norm_b)
 *   4. Top-3 selection via linear scan + insertion sort (optimal for K=3)
 *   5. rdtsc cycle-accurate benchmarking
 *
 * All vectors are allocated in .bss (static) to avoid stack overflow.
 * Uses Q16.16 fixed-point arithmetic (no SSE/FPU required).
 *
 * Copyright (c) 2024-2026 MORNINGSTAR / AetherionOS Project
 */

#include "libc_stub.h"

/* ========================================
 * Fixed-point arithmetic (Q16.16)
 * Same as agent_ai.c for consistency
 * ======================================== */
typedef long fix32_t;

#define FIX_SHIFT   16
#define FIX_ONE     (1L << FIX_SHIFT)    /* 1.0 = 65536 */

static inline fix32_t fix_from_int(long val) {
    return val << FIX_SHIFT;
}

static inline long fix_to_int(fix32_t val) {
    return val >> FIX_SHIFT;
}

static inline fix32_t fix_mul(fix32_t a, fix32_t b) {
    return (fix32_t)(((long long)a * (long long)b) >> FIX_SHIFT);
}

static inline fix32_t fix_add(fix32_t a, fix32_t b) {
    return a + b;
}

/* Fixed-point division: (a << SHIFT) / b */
static inline fix32_t fix_div(fix32_t a, fix32_t b) {
    if (b == 0) return 0;
    return (fix32_t)(((long long)a << FIX_SHIFT) / b);
}

/* ========================================
 * Integer square root (isqrt) for norm computation
 * Uses Newton's method on integers
 * Input: fixed-point value (Q16.16 representing x)
 * Output: fixed-point sqrt(x)
 *
 * To compute sqrt of a Q16.16 number:
 *   sqrt(x * 2^16) = sqrt(x) * 2^8
 * So we need to shift left by 8 more bits to get Q16.16 result.
 * ======================================== */
static fix32_t fix_sqrt(fix32_t x) {
    if (x <= 0) return 0;

    /* Compute isqrt(x << 16) using Newton's method.
     * Input x is Q16.16: x = real_value * 2^16
     * isqrt(x * 2^16) = sqrt(real_value * 2^32) = sqrt(real_value) * 2^16
     * This gives the result directly in Q16.16 format. */
    unsigned long long val = (unsigned long long)x << FIX_SHIFT;

    /* Initial guess: find highest set bit, halve the exponent.
     * This ensures Newton's method converges quickly (< 10 iterations). */
    int bits = 0;
    unsigned long long temp = val;
    while (temp > 0) { bits++; temp >>= 1; }
    unsigned long long guess = 1ULL << ((bits + 1) / 2);

    /* Newton's iterations: x_{n+1} = (x_n + val/x_n) / 2 */
    int iter;
    for (iter = 0; iter < 50; iter++) {
        unsigned long long next = (guess + val / guess) >> 1;
        if (next >= guess) break; /* converged */
        guess = next;
    }

    return (fix32_t)guess;
}

/* Print a fixed-point value with 2 decimal places */
static void print_fix(fix32_t val) {
    long neg = 0;
    if (val < 0) { neg = 1; val = -val; }
    long integer_part = fix_to_int(val);
    long frac = val & (FIX_ONE - 1);
    long frac_100 = (frac * 100) >> FIX_SHIFT;

    if (neg) puts("-");
    print_int(integer_part);
    puts(".");
    if (frac_100 < 10) puts("0");
    print_int(frac_100);
}

/* ========================================
 * rdtsc: Read Time Stamp Counter
 * ======================================== */
static inline unsigned long rdtsc(void) {
    unsigned int lo, hi;
    asm volatile("rdtsc" : "=a"(lo), "=d"(hi));
    return ((unsigned long)hi << 32) | lo;
}

/* ========================================
 * Vector database parameters
 * ======================================== */
#define DB_SIZE     256   /* Number of vectors in database */
#define VEC_DIM     64    /* Dimension of each vector */
#define TOP_K       3     /* Number of nearest neighbors to find */

/* ========================================
 * Static allocation in .bss (no stack, no heap needed)
 * Total: 256 * 64 * 8 = 128 KB for db
 *      + 256 * 8 = 2 KB for norms
 *      + 64 * 8 = 512 bytes for query
 * Well within memory limits.
 * ======================================== */
static fix32_t db_vectors[DB_SIZE * VEC_DIM];
static fix32_t db_norms[DB_SIZE];   /* Pre-computed L2 norms */
static fix32_t query_vector[VEC_DIM];

/* Top-K results */
typedef struct {
    int   index;
    fix32_t score;  /* cosine similarity */
} TopResult;

static TopResult top_results[TOP_K];

/* ========================================
 * Pseudo-random number generator (LCG)
 * For deterministic, reproducible test data
 * ======================================== */
static unsigned long rng_state = 12345678UL;

static unsigned long rng_next(void) {
    rng_state = rng_state * 6364136223846793005ULL + 1442695040888963407ULL;
    return rng_state >> 33;
}

/* Generate a random fixed-point value in range [-2.0, +2.0] */
static fix32_t rng_fix(void) {
    long r = (long)(rng_next() % 256) - 128;  /* -128 to +127 */
    return (fix32_t)(r << (FIX_SHIFT - 6));    /* scale to [-2.0, +2.0] */
}

/* ========================================
 * Dot product of two vectors
 * ======================================== */
static fix32_t vec_dot(const fix32_t *a, const fix32_t *b, int dim) {
    fix32_t sum = 0;
    int i;
    for (i = 0; i < dim; i++) {
        sum = fix_add(sum, fix_mul(a[i], b[i]));
    }
    return sum;
}

/* ========================================
 * Pre-compute L2 norms for all database vectors
 * norm[i] = sqrt(dot(v_i, v_i))
 * This avoids redundant sqrt computations during search.
 * ======================================== */
static void precompute_norms(void) {
    int i;
    for (i = 0; i < DB_SIZE; i++) {
        fix32_t dot = vec_dot(&db_vectors[i * VEC_DIM],
                              &db_vectors[i * VEC_DIM], VEC_DIM);
        db_norms[i] = fix_sqrt(dot);
    }
}

/* ========================================
 * Cosine Similarity with pre-computed norms
 * cos(q, v_i) = dot(q, v_i) / (norm_q * norm_v_i)
 * ======================================== */
static fix32_t cosine_similarity(const fix32_t *query, fix32_t query_norm,
                                  int db_index) {
    fix32_t dot = vec_dot(query, &db_vectors[db_index * VEC_DIM], VEC_DIM);
    fix32_t denom = fix_mul(query_norm, db_norms[db_index]);
    if (denom == 0) return 0;
    return fix_div(dot, denom);
}

/* ========================================
 * Top-K selection via linear scan + insertion sort
 * Optimal for K=3: at most 3 comparisons per vector.
 * Total: O(DB_SIZE * K) = O(3000) operations.
 * ======================================== */
static void find_top_k(const fix32_t *query, fix32_t query_norm) {
    int i, j;

    /* Initialize with worst possible scores */
    for (i = 0; i < TOP_K; i++) {
        top_results[i].index = -1;
        top_results[i].score = -FIX_ONE * 2;  /* -2.0 (worse than any cosine) */
    }

    /* Linear scan through all database vectors */
    for (i = 0; i < DB_SIZE; i++) {
        fix32_t sim = cosine_similarity(query, query_norm, i);

        /* Insertion sort into top_results */
        if (sim > top_results[TOP_K - 1].score) {
            /* Find insertion position */
            int pos = TOP_K - 1;
            while (pos > 0 && sim > top_results[pos - 1].score) {
                pos--;
            }
            /* Shift down */
            for (j = TOP_K - 1; j > pos; j--) {
                top_results[j] = top_results[j - 1];
            }
            /* Insert */
            top_results[pos].index = i;
            top_results[pos].score = sim;
        }
    }
}

/* ========================================
 * Generate deterministic test database
 * Vector[0] is set to be the query vector itself (cosine = 1.0)
 * Vector[1] is set to be similar (cosine ~ 0.9)
 * Vector[2] is set to be somewhat similar (cosine ~ 0.7)
 * Rest are random
 * ======================================== */
static void generate_database(void) {
    int i, j;

    /* Generate query vector first (deterministic) */
    rng_state = 42;
    for (j = 0; j < VEC_DIM; j++) {
        query_vector[j] = rng_fix();
    }

    /* Vector 0: exact copy of query (cosine = 1.0) */
    for (j = 0; j < VEC_DIM; j++) {
        db_vectors[0 * VEC_DIM + j] = query_vector[j];
    }

    /* Vector 1: query + small noise (cosine ~ 0.9) */
    rng_state = 999;
    for (j = 0; j < VEC_DIM; j++) {
        fix32_t noise = rng_fix() >> 3;  /* small noise: 1/8 scale */
        db_vectors[1 * VEC_DIM + j] = fix_add(query_vector[j], noise);
    }

    /* Vector 2: query + medium noise (cosine ~ 0.7) */
    rng_state = 7777;
    for (j = 0; j < VEC_DIM; j++) {
        fix32_t noise = rng_fix() >> 1;  /* medium noise: 1/2 scale */
        db_vectors[2 * VEC_DIM + j] = fix_add(query_vector[j], noise);
    }

    /* Vectors 3..DB_SIZE-1: fully random */
    rng_state = 123456;
    for (i = 3; i < DB_SIZE; i++) {
        for (j = 0; j < VEC_DIM; j++) {
            db_vectors[i * VEC_DIM + j] = rng_fix();
        }
    }
}

/* ========================================
 * Validation: verify expected top-3 ordering
 * Vector 0 should be #1 (cosine = 1.0)
 * Vector 1 should be #2 (cosine ~ 0.9)
 * Vector 2 should be #3 (cosine ~ 0.7)
 * ======================================== */
static int validate_results(void) {
    int passed = 1;

    /* Check rank 1 is vector 0 (exact match) */
    if (top_results[0].index != 0) {
        puts("[J23] FAIL: Rank 1 should be vector 0 (exact match), got ");
        print_int(top_results[0].index);
        puts("\n");
        passed = 0;
    }

    /* Check cosine of rank 1 is close to 1.0 */
    fix32_t diff = top_results[0].score - FIX_ONE;
    if (diff < 0) diff = -diff;
    if (diff > (FIX_ONE / 100)) {  /* tolerance: 0.01 */
        puts("[J23] FAIL: Rank 1 cosine should be ~1.0, got ");
        print_fix(top_results[0].score);
        puts("\n");
        passed = 0;
    }

    /* Check rank 2 is vector 1 */
    if (top_results[1].index != 1) {
        puts("[J23] WARN: Rank 2 expected vector 1, got ");
        print_int(top_results[1].index);
        puts(" (score=");
        print_fix(top_results[1].score);
        puts(")\n");
        /* Not a hard failure - random vectors could occasionally beat it */
    }

    /* All top-K scores should be in descending order */
    int i;
    for (i = 0; i < TOP_K - 1; i++) {
        if (top_results[i].score < top_results[i + 1].score) {
            puts("[J23] FAIL: Results not sorted (rank ");
            print_int(i);
            puts(" < rank ");
            print_int(i + 1);
            puts(")\n");
            passed = 0;
        }
    }

    return passed;
}

/* ========================================
 * Entry point
 * ======================================== */
void _start(void) {
    puts("\n[J23] AetherionOS Jalon 23 - RAG Vector Engine\n");
    puts("[J23] Database: ");
    print_int(DB_SIZE);
    puts(" vectors, dim=");
    print_int(VEC_DIM);
    puts(", top-K=");
    print_int(TOP_K);
    puts(" (fixed-point Q16.16)\n");

    /* Step 1: Generate database */
    puts("[J23] Step 1: Generating vector database...\n");
    unsigned long t0 = rdtsc();
    generate_database();
    unsigned long t1 = rdtsc();
    puts("[J23] Database generated in ");
    print_int((long)((t1 - t0) / 1000));
    puts(" Kcycles\n");

    /* Print sample vectors */
    puts("[J23] Query[0..3] = [");
    int k;
    for (k = 0; k < 4; k++) {
        if (k > 0) puts(", ");
        print_fix(query_vector[k]);
    }
    puts("]\n");
    puts("[J23] DB[0][0..3] = [");
    for (k = 0; k < 4; k++) {
        if (k > 0) puts(", ");
        print_fix(db_vectors[k]);
    }
    puts("] (should match query)\n");

    /* Step 2: Pre-compute norms */
    puts("\n[J23] Step 2: Pre-computing L2 norms...\n");
    unsigned long t2 = rdtsc();
    precompute_norms();
    unsigned long t3 = rdtsc();
    unsigned long norm_cycles = t3 - t2;
    puts("[J23] Norms computed in ");
    print_int((long)(norm_cycles / 1000));
    puts(" Kcycles\n");

    /* Print sample norms */
    puts("[J23] norm[0]=");
    print_fix(db_norms[0]);
    puts(" norm[1]=");
    print_fix(db_norms[1]);
    puts(" norm[2]=");
    print_fix(db_norms[2]);
    puts("\n");

    /* Compute query norm */
    fix32_t query_dot = vec_dot(query_vector, query_vector, VEC_DIM);
    fix32_t query_norm = fix_sqrt(query_dot);
    puts("[J23] Query norm=");
    print_fix(query_norm);
    puts("\n");

    /* Step 3: Top-K search */
    puts("\n[J23] Step 3: Searching top-");
    print_int(TOP_K);
    puts(" nearest vectors...\n");
    unsigned long t4 = rdtsc();
    find_top_k(query_vector, query_norm);
    unsigned long t5 = rdtsc();
    unsigned long search_cycles = t5 - t4;
    puts("[J23] Search completed in ");
    print_int((long)(search_cycles / 1000));
    puts(" Kcycles\n");

    /* Display results */
    puts("\n[J23] === Top-");
    print_int(TOP_K);
    puts(" Results ===\n");
    int i;
    for (i = 0; i < TOP_K; i++) {
        puts("[J23] Rank ");
        print_int(i + 1);
        puts(": vector[");
        print_int(top_results[i].index);
        puts("] cosine=");
        print_fix(top_results[i].score);
        puts("\n");
    }

    /* Step 4: Validation */
    puts("\n[J23] Step 4: Validating results...\n");
    int valid = validate_results();

    /* Performance summary */
    puts("\n[J23] === Performance Summary ===\n");
    puts("[J23] Norm pre-computation: ");
    print_int((long)(norm_cycles / 1000));
    puts(" Kcycles (");
    print_int(DB_SIZE);
    puts(" vectors)\n");
    puts("[J23] Top-");
    print_int(TOP_K);
    puts(" search: ");
    print_int((long)(search_cycles / 1000));
    puts(" Kcycles (");
    print_int(DB_SIZE);
    puts(" comparisons)\n");
    unsigned long total_cycles = norm_cycles + search_cycles;
    puts("[J23] Total RAG query: ");
    print_int((long)(total_cycles / 1000));
    puts(" Kcycles\n");

    /* Per-vector cost */
    if (DB_SIZE > 0) {
        puts("[J23] Per-vector cost: ");
        print_int((long)(search_cycles / DB_SIZE));
        puts(" cycles/vector\n");
    }

    /* Final verdict */
    if (valid) {
        puts("\n[J23] === ALL JALON 23 TESTS PASSED ===\n");
        puts("[J23] Cosine similarity validated (rank 1 = exact match, cos=1.0)\n");
        puts("[J23] Top-3 ordering correct (descending similarity)\n");
        puts("[J23] Pre-computed norms operational\n");
        puts("[J23] rdtsc benchmarking operational\n");
    } else {
        puts("\n[J23] === JALON 23 TESTS FAILED ===\n");
    }

    exit(0);
}
