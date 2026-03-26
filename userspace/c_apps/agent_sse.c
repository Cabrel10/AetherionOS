/*
 * agent_sse.c - Jalon 33: SSE/AVX Ring 3 validation agent
 */
#include "libc_stub.h"

void _start(void) {
    puts("=== Jalon 33: SSE Ring 3 Validation Agent ===\n");

    /* SSE2 double test via inline asm */
    double a = 1.5, b = 3.0, result;
    __asm__ volatile(
        "movsd %1, %%xmm0\n\t"
        "movsd %2, %%xmm1\n\t"
        "addsd %%xmm1, %%xmm0\n\t"
        "movsd %%xmm0, %0"
        : "=m"(result)
        : "m"(a), "m"(b)
        : "xmm0", "xmm1"
    );

    if (result > 4.0 && result < 5.0) {
        puts("[SSE-OK] SSE2 addsd: 1.5 + 3.0 = 4.5\n");
    } else {
        puts("[SSE-FAIL] SSE2 addsd incorrect\n");
    }

    /* SSE2 integer test via inline asm */
    int ia[4] __attribute__((aligned(16))) = {10, 20, 30, 40};
    int ib[4] __attribute__((aligned(16))) = {1, 2, 3, 4};
    int ir[4] __attribute__((aligned(16)));

    __asm__ volatile(
        "movdqa (%1), %%xmm2\n\t"
        "movdqa (%2), %%xmm3\n\t"
        "paddd %%xmm3, %%xmm2\n\t"
        "movdqa %%xmm2, (%0)"
        :
        : "r"(ir), "r"(ia), "r"(ib)
        : "xmm2", "xmm3", "memory"
    );

    if (ir[0]==11 && ir[1]==22 && ir[2]==33 && ir[3]==44) {
        puts("[SSE-OK] SSE2 paddd: 10+1=11, 20+2=22, 30+3=33, 40+4=44\n");
    } else {
        puts("[SSE-FAIL] SSE2 paddd incorrect\n");
    }

    bus_publish(0x3301, 2, 2);
    puts("=== SSE validation complete ===\n");
    exit(0);
}
