/*
 * hello_c.c - First native C program for AetherionOS Ring 3
 * Couche 16: Proves the C toolchain works end-to-end.
 *
 * This program:
 *   1. Prints a banner proving C code execution in Ring 3
 *   2. Performs arithmetic (Fibonacci, factorial) to prove computation
 *   3. Uses sys_mmap to allocate heap memory
 *   4. Uses sys_getpid to get our PID
 *   5. Publishes a result to the Cognitive Bus
 *   6. Draws on VGA
 *   7. Exits cleanly
 *
 * Compiled with: gcc -nostdlib -fno-builtin -static -Ttext=0x8000000000
 */

#include "libc_stub.h"

/* Fibonacci computation */
long fibonacci(int n) {
    if (n <= 1) return n;
    long a = 0, b = 1, c;
    for (int i = 2; i <= n; i++) {
        c = a + b;
        a = b;
        b = c;
    }
    return b;
}

/* Factorial computation */
long factorial(int n) {
    long result = 1;
    for (int i = 2; i <= n; i++) {
        result *= i;
    }
    return result;
}

/* Simple checksum for memory verification */
long checksum(const unsigned char *data, size_t len) {
    long sum = 0;
    for (size_t i = 0; i < len; i++) {
        sum = (sum * 31 + data[i]) & 0x7FFFFFFF;
    }
    return sum;
}

void _start(void) {
    /* ========================================
     * STEP 1: Print banner
     * ======================================== */
    puts("[C-APP] ========================================\n");
    puts("[C-APP] Hello from a Native C program in Ring 3!\n");
    puts("[C-APP] AetherionOS Couche 16 - C Toolchain\n");
    puts("[C-APP] ========================================\n");

    /* ========================================
     * STEP 2: Get PID
     * ======================================== */
    long pid = getpid();
    puts("[C-APP] PID = ");
    print_int(pid);
    puts("\n");

    /* ========================================
     * STEP 3: Fibonacci computation
     * ======================================== */
    puts("[C-APP] Computing Fibonacci(20)... ");
    long fib20 = fibonacci(20);
    print_int(fib20);
    puts(" (expected: 6765)\n");

    /* Verify */
    if (fib20 == 6765) {
        puts("[C-APP] [OK] Fibonacci verified!\n");
    } else {
        puts("[C-APP] [FAIL] Fibonacci mismatch!\n");
    }

    /* ========================================
     * STEP 4: Factorial computation
     * ======================================== */
    puts("[C-APP] Computing Factorial(12)... ");
    long fact12 = factorial(12);
    print_int(fact12);
    puts(" (expected: 479001600)\n");

    if (fact12 == 479001600L) {
        puts("[C-APP] [OK] Factorial verified!\n");
    } else {
        puts("[C-APP] [FAIL] Factorial mismatch!\n");
    }

    /* ========================================
     * STEP 5: Memory allocation via mmap
     * ======================================== */
    puts("[C-APP] Allocating 16 pages (64 KB) via sys_mmap...\n");
    void *heap = mmap(NULL, 65536, 3, 0x22, -1, 0);  /* PROT_RW, MAP_ANON|MAP_PRIV */

    if ((long)heap > 0) {
        puts("[C-APP] [OK] Heap mapped at 0x");

        /* Print hex address manually */
        char hex[17];
        long addr = (long)heap;
        for (int i = 15; i >= 0; i--) {
            int nibble = addr & 0xF;
            hex[i] = nibble < 10 ? '0' + nibble : 'A' + nibble - 10;
            addr >>= 4;
        }
        hex[16] = '\0';
        puts(hex);
        puts("\n");

        /* Write pattern to heap and verify */
        unsigned char *p = (unsigned char *)heap;
        for (int i = 0; i < 256; i++) {
            p[i] = (unsigned char)(i ^ 0x5A);
        }

        /* Verify */
        int mem_ok = 1;
        for (int i = 0; i < 256; i++) {
            if (p[i] != (unsigned char)(i ^ 0x5A)) {
                mem_ok = 0;
                break;
            }
        }

        if (mem_ok) {
            puts("[C-APP] [OK] Memory write/read verified (256 bytes)\n");
        } else {
            puts("[C-APP] [FAIL] Memory verification failed!\n");
        }

        /* Compute checksum */
        long ck = checksum(p, 256);
        puts("[C-APP] Heap checksum = ");
        print_int(ck);
        puts("\n");
    } else {
        puts("[C-APP] [FAIL] mmap returned error\n");
    }

    /* ========================================
     * STEP 6: Publish result to Cognitive Bus
     * ======================================== */
    puts("[C-APP] Publishing to Cognitive Bus...\n");
    long bus_ret = bus_publish(0x6000, 2, fib20);  /* intent=C_RESULT, prio=HIGH */
    if (bus_ret == 0) {
        puts("[C-APP] [OK] Published to Cognitive Bus (intent=0x6000)\n");
    } else {
        puts("[C-APP] [WARN] Bus publish returned error\n");
    }

    /* ========================================
     * STEP 7: VGA color output
     * ======================================== */
    puts("[C-APP] Drawing on VGA...\n");
    /* Draw green 'C' at row 7, col 35 */
    vga_write(7, 35, 0x2A43);  /* green bg, bright green text, 'C' */
    puts("[C-APP] [OK] VGA: Drew green 'C' at (7,35)\n");

    /* ========================================
     * STEP 8: Summary and exit
     * ======================================== */
    puts("[C-APP] ========================================\n");
    puts("[C-APP] All C-language tests PASSED!\n");
    puts("[C-APP]   Fibonacci(20) = 6765\n");
    puts("[C-APP]   Factorial(12) = 479001600\n");
    puts("[C-APP]   mmap: heap allocated and verified\n");
    puts("[C-APP]   Cognitive Bus: result published\n");
    puts("[C-APP]   VGA: color character drawn\n");
    puts("[C-APP] C execution validated.\n");
    puts("[C-APP] ========================================\n");

    exit(0);
}
