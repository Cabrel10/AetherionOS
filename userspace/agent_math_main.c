/*
 * agent_math_main.c - AetherionOS Math Agent
 */
#include "libc_stub.h"

void _start(void) {
    puts("=== AetherionOS Math Agent ===\n");

    /* factorial(12) = 479001600 */
    long fact = 1;
    for (int i = 1; i <= 12; i++) fact *= i;
    puts("[MATH] factorial(12) = ");
    print_int(fact);
    puts("\n");

    if (fact == 479001600)
        puts("[MATH-OK] Integer computation correct\n");
    else
        puts("[MATH-FAIL] Integer computation incorrect\n");

    /* Fibonacci(20) = 6765 */
    long a = 0, b = 1;
    for (int i = 0; i < 20; i++) { long t = a + b; a = b; b = t; }
    puts("[MATH] fibonacci(20) = ");
    print_int(b);
    puts("\n");

    if (b == 6765)
        puts("[MATH-OK] Fibonacci correct\n");
    else
        puts("[MATH-FAIL] Fibonacci incorrect\n");

    bus_publish(0x2201, 2, (unsigned long)fact);
    puts("=== Math Agent complete ===\n");
    exit(0);
}
