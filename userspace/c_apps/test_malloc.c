/*
 * test_malloc.c - Jalon 27: Dynamic memory allocation test
 */
#include "libc_stub.h"

static long do_brk(long addr) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"((long)12), "D"(addr) : "rcx","r11","memory");
    return ret;
}

void _start(void) {
    puts("=== Jalon 27: Dynamic Memory Allocation Test ===\n");

    long base = do_brk(0);
    if (base > 0)
        puts("[J27-OK] sys_brk(0) returns valid base\n");
    else
        puts("[J27-FAIL] sys_brk(0)\n");

    long nb = do_brk(base + 4096);
    if (nb >= base + 4096)
        puts("[J27-OK] sys_brk extend 4096\n");
    else
        puts("[J27-FAIL] sys_brk extend\n");

    if (nb >= base + 4096) {
        volatile char *p = (volatile char*)base;
        p[0]='A'; p[1]='B';
        if (p[0]=='A' && p[1]=='B')
            puts("[J27-OK] write/read allocated memory\n");
        else
            puts("[J27-FAIL] memory readback\n");
    }

    puts("=== test_malloc complete ===\n");
    exit(0);
}
