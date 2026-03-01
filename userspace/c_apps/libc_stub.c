/*
 * libc_stub.c - Minimal libc implementation for AetherionOS
 *
 * All I/O goes through the SYSCALL instruction to the AetherionOS kernel.
 * This file is compiled with -nostdlib -fno-builtin to create bare-metal
 * C programs that run in Ring 3 user space.
 *
 * Copyright (c) 2024-2026 MORNINGSTAR / AetherionOS Project
 */

#include "libc_stub.h"

/* ========================================
 * Syscall primitives (GCC inline assembly)
 * Uses the Linux x86_64 syscall ABI:
 *   RAX = syscall number
 *   RDI = arg1, RSI = arg2, RDX = arg3
 *   R10 = arg4, R8 = arg5, R9 = arg6
 *   SYSCALL instruction
 *   Return value in RAX
 * ======================================== */

long syscall1(long n, long a1) {
    long ret;
    asm volatile("syscall"
        : "=a"(ret)
        : "a"(n), "D"(a1)
        : "rcx", "r11", "memory");
    return ret;
}

long syscall2(long n, long a1, long a2) {
    long ret;
    asm volatile("syscall"
        : "=a"(ret)
        : "a"(n), "D"(a1), "S"(a2)
        : "rcx", "r11", "memory");
    return ret;
}

long syscall3(long n, long a1, long a2, long a3) {
    long ret;
    asm volatile("syscall"
        : "=a"(ret)
        : "a"(n), "D"(a1), "S"(a2), "d"(a3)
        : "rcx", "r11", "memory");
    return ret;
}

long syscall6(long n, long a1, long a2, long a3, long a4, long a5, long a6) {
    long ret;
    register long r10 asm("r10") = a4;
    register long r8  asm("r8")  = a5;
    register long r9  asm("r9")  = a6;
    asm volatile("syscall"
        : "=a"(ret)
        : "a"(n), "D"(a1), "S"(a2), "d"(a3), "r"(r10), "r"(r8), "r"(r9)
        : "rcx", "r11", "memory");
    return ret;
}

/* ========================================
 * POSIX-like wrappers
 * ======================================== */

ssize_t write(int fd, const void *buf, size_t count) {
    return (ssize_t)syscall3(1, (long)fd, (long)buf, (long)count);
}

ssize_t read(int fd, void *buf, size_t count) {
    return (ssize_t)syscall3(0, (long)fd, (long)buf, (long)count);
}

void exit(int status) {
    syscall2(60, (long)status, 0);
    /* Unreachable - loop forever if syscall somehow returns */
    while(1) { asm volatile("hlt"); }
}

long getpid(void) {
    return syscall1(20, 0);
}

/* ========================================
 * AetherionOS extensions
 * ======================================== */

void *mmap(void *addr, size_t len, int prot, int flags, int fd, off_t offset) {
    return (void *)syscall6(9, (long)addr, (long)len, (long)prot,
                            (long)flags, (long)fd, (long)offset);
}

long bus_publish(long intent, int priority, long data) {
    return syscall3(201, intent, (long)priority, data);
}

long vga_write(int row, int col, long color_char) {
    return syscall3(202, (long)row, (long)col, color_char);
}

/* ========================================
 * String utilities
 * ======================================== */

size_t strlen(const char *s) {
    size_t len = 0;
    while (s[len] != '\0') len++;
    return len;
}

void *memset(void *s, int c, size_t n) {
    unsigned char *p = (unsigned char *)s;
    while (n--) *p++ = (unsigned char)c;
    return s;
}

void *memcpy(void *dest, const void *src, size_t n) {
    unsigned char *d = (unsigned char *)dest;
    const unsigned char *s = (const unsigned char *)src;
    while (n--) *d++ = *s++;
    return dest;
}

int strcmp(const char *s1, const char *s2) {
    while (*s1 && (*s1 == *s2)) { s1++; s2++; }
    return *(unsigned char *)s1 - *(unsigned char *)s2;
}

/* Integer to ASCII (base 10) */
int itoa(long value, char *buf, int bufsize) {
    char tmp[24];
    int i = 0, neg = 0;

    if (value < 0) { neg = 1; value = -value; }
    if (value == 0) { tmp[i++] = '0'; }
    else {
        while (value > 0 && i < 22) {
            tmp[i++] = '0' + (value % 10);
            value /= 10;
        }
    }

    int len = i + neg;
    if (len >= bufsize) return -1;

    int pos = 0;
    if (neg) buf[pos++] = '-';
    while (i > 0) buf[pos++] = tmp[--i];
    buf[pos] = '\0';
    return pos;
}

/* Print a string to stdout */
void puts(const char *s) {
    write(1, s, strlen(s));
}

/* Print an integer to stdout */
void print_int(long val) {
    char buf[24];
    itoa(val, buf, sizeof(buf));
    puts(buf);
}
