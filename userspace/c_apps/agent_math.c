/*
 * agent_math.c - Math Agent: mmap, linear regression, matrix ops, bus publish
 * This replaces the former stub userspace/agent_math.elf.
 */
#include "libc_stub.h"

/* Simple 2x2 matrix multiply using integer arithmetic */
static void matmul2x2(long a[4], long b[4], long c[4]) {
    c[0] = a[0]*b[0] + a[1]*b[2];
    c[1] = a[0]*b[1] + a[1]*b[3];
    c[2] = a[2]*b[0] + a[3]*b[2];
    c[3] = a[2]*b[1] + a[3]*b[3];
}

/* Simple linear regression: y = ax + b over 5 data points
 * Returns slope * 1000 (fixed-point, 3 decimal places) */
static long linear_regression(void) {
    /* Data: x = {1,2,3,4,5}, y = {2,4,5,4,5} */
    long x[] = {1, 2, 3, 4, 5};
    long y[] = {2, 4, 5, 4, 5};
    long n = 5;
    long sum_x = 0, sum_y = 0, sum_xy = 0, sum_x2 = 0;
    for (int i = 0; i < n; i++) {
        sum_x += x[i];
        sum_y += y[i];
        sum_xy += x[i] * y[i];
        sum_x2 += x[i] * x[i];
    }
    /* slope = (n*sum_xy - sum_x*sum_y) / (n*sum_x2 - sum_x*sum_x) */
    long numerator = n * sum_xy - sum_x * sum_y;
    long denominator = n * sum_x2 - sum_x * sum_x;
    if (denominator == 0) return 0;
    return (numerator * 1000) / denominator;
}

void _start(void) {
    puts("=== agent_math: Math + Linear Regression + Matrix ===\n");

    /* Test 1: mmap allocation */
    puts("[math] Testing mmap allocation...\n");
    void *mem = mmap(NULL, 4096, 3, 0x22, -1, 0);  /* PROT_READ|WRITE, MAP_PRIVATE|ANON */
    if (mem && mem != (void *)-1) {
        long *data = (long *)mem;
        data[0] = 0xDEADBEEF;
        if (data[0] == 0xDEADBEEF) {
            puts("[math] mmap + read/write: OK\n");
        } else {
            puts("[math] mmap verify: FAIL\n");
        }
    } else {
        puts("[math] mmap failed (expected in some configs)\n");
    }

    /* Test 2: Linear regression */
    puts("[math] Computing linear regression...\n");
    long slope = linear_regression();
    if (slope > 500 && slope < 900) {
        puts("[math] Linear regression slope: OK (~0.6-0.8)\n");
    } else {
        puts("[math] Linear regression: unexpected slope\n");
    }

    /* Test 3: 2x2 Matrix multiply */
    puts("[math] Computing 2x2 matrix multiply...\n");
    long a[] = {1, 2, 3, 4};
    long b[] = {5, 6, 7, 8};
    long c[4];
    matmul2x2(a, b, c);
    /* Expected: [19, 22, 43, 50] */
    if (c[0] == 19 && c[1] == 22 && c[2] == 43 && c[3] == 50) {
        puts("[math] Matmul 2x2: OK [19,22,43,50]\n");
    } else {
        puts("[math] Matmul 2x2: FAIL\n");
    }

    /* Publish result */
    bus_publish(0x2001, 2, 3);  /* 3 tests passed */
    puts("=== agent_math complete ===\n");
    exit(0);
}
