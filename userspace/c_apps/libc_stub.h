/*
 * libc_stub.h - Minimal libc for AetherionOS Ring 3 C programs
 *
 * Provides syscall wrappers via inline assembly (GCC asm volatile).
 * Targets the AetherionOS kernel syscall ABI (Linux x86_64 compatible).
 *
 * Syscall numbers:
 *   0   sys_read(fd, buf, len)
 *   1   sys_write(fd, buf, len)
 *   9   sys_mmap(addr, len, prot, flags, fd, offset)
 *  20   sys_getpid()
 *  60   sys_exit(code)
 * 201   sys_bus_publish(intent, priority, data)
 * 202   sys_vga_write(row, col, color_char)
 */

#ifndef _LIBC_STUB_H
#define _LIBC_STUB_H

/* Basic types */
typedef unsigned long  size_t;
typedef long           ssize_t;
typedef long           off_t;

/* NULL */
#define NULL ((void *)0)

/* Syscall primitives */
long syscall1(long n, long a1);
long syscall2(long n, long a1, long a2);
long syscall3(long n, long a1, long a2, long a3);
long syscall6(long n, long a1, long a2, long a3, long a4, long a5, long a6);

/* POSIX-like wrappers */
ssize_t write(int fd, const void *buf, size_t count);
ssize_t read(int fd, void *buf, size_t count);
void exit(int status);
long getpid(void);

/* AetherionOS-specific */
void *mmap(void *addr, size_t len, int prot, int flags, int fd, off_t offset);
long bus_publish(long intent, int priority, long data);
long vga_write(int row, int col, long color_char);

/* String utilities */
size_t strlen(const char *s);
void *memset(void *s, int c, size_t n);
void *memcpy(void *dest, const void *src, size_t n);
int strcmp(const char *s1, const char *s2);

/* Simple integer-to-string (base 10) */
int itoa(long value, char *buf, int bufsize);

/* Print a string to stdout */
void puts(const char *s);

/* Print a formatted integer */
void print_int(long val);

#endif /* _LIBC_STUB_H */
