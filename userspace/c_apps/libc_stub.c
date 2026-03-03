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

/* Print unsigned long in hex */
void print_hex(unsigned long val) {
    const char hex[] = "0123456789ABCDEF";
    char buf[19]; /* "0x" + 16 digits + '\0' */
    buf[0] = '0';
    buf[1] = 'x';
    int i;
    for (i = 0; i < 16; i++) {
        buf[2 + i] = hex[(val >> (60 - i * 4)) & 0xF];
    }
    buf[18] = '\0';
    puts(buf);
}

/* ========================================
 * Network syscalls (Couche 17+18)
 * ======================================== */

/* ICMP ping: syscall 210 */
long net_ping(int a, int b, int c, int d, int seq) {
    unsigned long ip = pack_ip(a, b, c, d);
    return syscall2(210, (long)ip, (long)seq);
}

/* TCP connect: syscall 42(fd, packed_ip, port)
 * Kernel reads: a1=fd, a2=packed_ip, a3=port */
long tcp_connect(int fd, int a, int b, int c, int d, int port) {
    unsigned long ip = pack_ip(a, b, c, d);
    return syscall3(42, (long)fd, (long)ip, (long)port);
}

/* TCP send: use syscall 44 (sendto) but with TCP semantics
 * The kernel sendto for TCP sockets uses buf directly */
long tcp_send(int fd, const void *buf, size_t len) {
    /* For TCP: syscall 44(fd, buf, encoded_dest)
     * We encode length in buf_addr and use the TCP socket's remote from connect
     * Simpler: the kernel's sys_sendto for SOCK_STREAM reads data from buf
     * Actually, we need a dedicated TCP send path.
     * Let's use the sendto with a special encoding:
     * a1=fd, a2=buf_addr (with length prefix), a3=0 (use connected remote) */
    
    /* Build a buffer with 8-byte length prefix + data */
    /* Actually, to keep it simple, let's route through the tcp_send in socket.rs
     * which already handles SOCK_STREAM differently */
    
    /* Use a simplified approach: syscall 44 with len encoded */
    /* The kernel dispatcher reads: fd, buf_addr, encoded_dest */
    /* For TCP sockets, we'll encode the length in the first 8 bytes */
    
    /* Simplest: just pass buf and encode len in a3 */
    /* But the kernel expects the sendto format with len prefix...
     * Let's use a separate syscall approach: pack len in a3 */
    
    /* Use a small stack buffer approach */
    unsigned char tmp[1500];
    if (len > 1492) len = 1492;
    
    /* 8-byte length prefix */
    unsigned long l = (unsigned long)len;
    memcpy(tmp, &l, 8);
    memcpy(tmp + 8, buf, len);
    
    /* syscall 44: sendto(fd, tmp, 0) - a3=0 means use connected TCP */
    return syscall3(44, (long)fd, (long)tmp, 0);
}

/* TCP read: syscall 212(fd, buf, len) */
long tcp_read(int fd, void *buf, size_t len) {
    return syscall3(212, (long)fd, (long)buf, (long)len);
}

/* TCP shutdown: syscall 47(fd) */
long tcp_shutdown(int fd) {
    return syscall1(47, (long)fd);
}

/* DNS gethostbyname: syscall 211(name_addr) -> packed IP */
long gethostbyname(const char *name) {
    return syscall1(211, (long)name);
}

/* ========================================
 * Threading syscalls (Couche 20)
 * ======================================== */

/* sys_clone: syscall 56(child_stack)
 * Creates a new thread sharing the parent address space.
 * child_stack must have the function pointer at (stack_top - 8). */
long sys_clone(void *child_stack) {
    return syscall1(56, (long)child_stack);
}

/* sys_yield: syscall 24 - voluntarily yield CPU */
long sys_yield(void) {
    return syscall1(24, 0);
}

/* sys_wait: syscall 61(pid) - wait for child to terminate */
long sys_wait(long pid) {
    return syscall1(61, pid);
}

/* thread_create: high-level wrapper
 * 1. Allocates a 64 KiB stack via mmap
 * 2. Writes function pointer at (stack_top - 8)
 * 3. Calls sys_clone with the new stack top
 * Returns child PID or negative error */
long thread_create(void (*start_routine)(void)) {
    /* Allocate 64 KiB for the thread stack */
    unsigned long stack_size = 65536;
    void *stack_base = mmap(0, stack_size, 0, 0, 0, 0);
    if ((long)stack_base < 0) {
        return -1; /* mmap failed */
    }

    /* Stack grows downward: top is base + size */
    unsigned long stack_top = (unsigned long)stack_base + stack_size;

    /* Write function pointer at (stack_top - 8) — the kernel reads this */
    unsigned long *fn_slot = (unsigned long *)(stack_top - 8);
    *fn_slot = (unsigned long)start_routine;

    /* Call sys_clone with the stack top */
    long ret = sys_clone((void *)stack_top);
    return ret;
}
