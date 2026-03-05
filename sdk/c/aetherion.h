/*
 * aetherion.h - AetherionOS C-SDK v1.0
 *
 * The official C library for AetherionOS Ring 3 userspace programs.
 * Provides POSIX-compatible syscall wrappers, string utilities, and
 * AetherionOS-specific extensions (Cognitive Bus, VGA, networking).
 *
 * Usage:
 *   #include <aetherion.h>
 *   Compile: gcc -c -I/path/to/sdk/c myapp.c
 *   Link:    ld -T aetherion.ld -static -o myapp.elf myapp.o -L/path/to/sdk/c -laetherion
 *
 * Targets the AetherionOS kernel syscall ABI (Linux x86_64 compatible).
 *
 * Syscall numbers:
 *   0   sys_read(fd, buf, len)
 *   1   sys_write(fd, buf, len)
 *   9   sys_mmap(addr, len, prot, flags, fd, offset)
 *  20   sys_getpid()
 *  41   sys_socket(domain, type, protocol)
 *  42   sys_connect(fd, ip_packed, port)
 *  44   sys_sendto(fd, buf_encoded, dest_encoded)
 *  47   sys_shutdown(fd)
 *  60   sys_exit(code)
 * 201   sys_bus_publish(intent, priority, data)
 * 202   sys_vga_write(row, col, color_char)
 * 210   sys_net_ping(ip, seq)
 * 211   sys_gethostbyname(name)
 * 212   sys_tcp_read(fd, buf, len)
 */

#ifndef _AETHERION_H
#define _AETHERION_H

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

/* Print hex */
void print_hex(unsigned long val);

/* ========================================
 * Network syscalls (Couche 17+18)
 * ======================================== */

/* Pack IP address into u32: (a<<24 | b<<16 | c<<8 | d) */
static inline unsigned long pack_ip(int a, int b, int c, int d) {
    return ((unsigned long)a << 24) | ((unsigned long)b << 16) |
           ((unsigned long)c << 8) | (unsigned long)d;
}

/* ICMP ping */
long net_ping(int a, int b, int c, int d, int seq);

/* TCP connect: fd, ip octets, port -> 0 on success */
long tcp_connect(int fd, int a, int b, int c, int d, int port);

/* TCP send data */
long tcp_send(int fd, const void *buf, size_t len);

/* TCP read data */
long tcp_read(int fd, void *buf, size_t len);

/* TCP shutdown */
long tcp_shutdown(int fd);

/* DNS resolve */
long gethostbyname(const char *name);

/* ========================================
 * Threading syscalls (Couche 20)
 * ======================================== */

/* sys_clone: create a thread sharing the parent's address space.
 * child_stack: top of allocated stack with function pointer at (stack_top - 8).
 * Returns: child PID to parent. */
long sys_clone(void *child_stack);

/* sys_yield: voluntarily yield CPU to another thread */
long sys_yield(void);

/* sys_wait: wait for any child to terminate.
 * Returns: (child_pid << 16) | exit_code, or negative error. */
long sys_wait(long pid);

/* thread_create: allocate a 64 KiB stack, store fn pointer, call sys_clone.
 * Returns: child PID or negative error. */
long thread_create(void (*start_routine)(void));

/* ========================================
 * POSIX Fork/Exec syscalls (Jalon 25/26)
 * ======================================== */

/* fork(): duplicate the calling process.
 * Returns: 0 to child, child PID to parent, negative on error. */
long fork(void);

/* execve(path): replace current process image with new ELF.
 * path: VFS path (e.g., "/bin/ls.elf").
 * Returns: negative on error (does NOT return on success). */
long execve(const char *path);

/* waitpid(pid): wait for child process to terminate.
 * pid > 0: wait for specific child.
 * pid = 0: wait for any child.
 * Returns: (child_pid << 16) | exit_code, or negative error. */
long waitpid(long pid);

/* open(path, flags): open a file, returns FD or negative error.
 * flags: 0=O_RDONLY, 1=O_WRONLY, 2=O_RDWR */
long open(const char *path, int flags);

/* close(fd): close a file descriptor */
long close(int fd);

/* getdents(fd, buf, bufsize): read directory entries.
 * Returns bytes written to buf (newline-separated names). */
long getdents(int fd, void *buf, size_t bufsize);

#endif /* _AETHERION_H */
