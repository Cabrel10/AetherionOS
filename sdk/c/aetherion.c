/*
 * aetherion.c - AetherionOS C-SDK v1.0 Implementation
 *
 * Compiled into libaetherion.a -- the static library that all
 * AetherionOS Ring 3 C programs link against.
 *
 * Copyright (c) 2024-2026 MORNINGSTAR / AetherionOS Project
 */

#include "aetherion.h"

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
        : "rcx", "r11", "r8", "r9", "r10", "rsi", "rdx", "memory");
    return ret;
}

long syscall2(long n, long a1, long a2) {
    long ret;
    asm volatile("syscall"
        : "=a"(ret)
        : "a"(n), "D"(a1), "S"(a2)
        : "rcx", "r11", "r8", "r9", "r10", "rdx", "memory");
    return ret;
}

long syscall3(long n, long a1, long a2, long a3) {
    long ret;
    asm volatile("syscall"
        : "=a"(ret)
        : "a"(n), "D"(a1), "S"(a2), "d"(a3)
        : "rcx", "r11", "r8", "r9", "r10", "memory");
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
    /* r10, r8, r9 are inputs so already handled */
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

/* ========================================
 * POSIX Fork/Exec syscalls (Jalon 25/26)
 * ======================================== */

/* fork(): syscall 57 - duplicate the calling process.
 * Returns: 0 to child, child PID to parent, negative on error. */
long fork(void) {
    long ret;
    asm volatile("syscall"
        : "=a"(ret)
        : "a"(57)
        : "rcx", "r11", "r8", "r9", "r10", "rdi", "rsi", "rdx", "memory");
    return ret;
}

/* execve(path): syscall 59(path_addr) - replace process image.
 * Does NOT return on success; returns negative on error. */
long execve(const char *path) {
    return syscall1(59, (long)path);
}

/* waitpid(pid): syscall 61(pid) - wait for child to terminate.
 * Same as sys_wait but named POSIX-style. */
long waitpid(long pid) {
    return syscall1(61, pid);
}

/* open(path, flags): syscall 2(path_addr, flags) - open a file */
long open(const char *path, int flags) {
    return syscall2(2, (long)path, (long)flags);
}

/* close(fd): syscall 3(fd) - close a file descriptor */
long close(int fd) {
    return syscall1(3, (long)fd);
}

/* getdents(fd, buf, bufsize): syscall 78(fd, buf, len) - read directory entries */
long getdents(int fd, void *buf, size_t bufsize) {
    return syscall3(78, (long)fd, (long)buf, (long)bufsize);
}

/* ========================================
 * Dynamic Memory Allocation (Jalon 27)
 *
 * First-fit allocator using sys_brk for heap growth.
 * Block header: size + free flag + next pointer.
 * Coalescing on free() merges adjacent free blocks.
 * Thread-unsafe (single-threaded userspace assumed).
 * ======================================== */

#define SYS_BRK 12

/* sys_brk: set/get program break */
long sys_brk(long addr) {
    return syscall1(SYS_BRK, addr);
}

/* Block header for malloc free-list */
typedef struct block_header {
    size_t size;                 /* Usable size (excluding header) */
    int    free;                 /* 1 = free, 0 = allocated */
    struct block_header *next;   /* Next block in linked list */
} block_header_t;

#define BLOCK_HDR_SIZE  ((sizeof(block_header_t) + 15) & ~((size_t)15))  /* Rounded up to 16 */
#define ALIGN_16(x)     (((x) + 15) & ~((size_t)15))  /* 16-byte alignment */
#define INITIAL_HEAP    (128 * 1024)  /* 128 KiB initial heap expansion */

static block_header_t *heap_head = NULL;
static int heap_initialized = 0;

/* Initialize the heap: call brk(0) to get base, then expand by INITIAL_HEAP */
static int heap_init(void) {
    long base = sys_brk(0);
    if (base <= 0) return -1;

    long end = sys_brk(base + INITIAL_HEAP);
    if (end <= base) return -1;

    /* Set up the first free block spanning the whole initial heap */
    heap_head = (block_header_t *)(unsigned long)base;
    heap_head->size = INITIAL_HEAP - BLOCK_HDR_SIZE;
    heap_head->free = 1;
    heap_head->next = NULL;
    heap_initialized = 1;
    return 0;
}

/* Extend the heap by at least `min_bytes` beyond the current break */
static block_header_t *heap_extend(size_t min_bytes) {
    size_t total = ALIGN_16(min_bytes + BLOCK_HDR_SIZE);
    if (total < 64 * 1024) total = 64 * 1024;  /* Minimum 64 KiB growth */

    long cur = sys_brk(0);
    if (cur <= 0) return NULL;

    long new_end = sys_brk(cur + (long)total);
    if (new_end <= cur) return NULL;

    block_header_t *blk = (block_header_t *)(unsigned long)cur;
    blk->size = total - BLOCK_HDR_SIZE;
    blk->free = 1;
    blk->next = NULL;

    /* Link to end of existing list */
    block_header_t *p = heap_head;
    while (p->next) p = p->next;
    p->next = blk;

    return blk;
}

/* Split a block if it has enough extra space */
static void block_split(block_header_t *blk, size_t needed) {
    size_t remaining = blk->size - needed;
    if (remaining > BLOCK_HDR_SIZE + 16) {
        /* Create a new free block after the allocated region */
        block_header_t *new_blk = (block_header_t *)((char *)blk + BLOCK_HDR_SIZE + needed);
        new_blk->size = remaining - BLOCK_HDR_SIZE;
        new_blk->free = 1;
        new_blk->next = blk->next;
        blk->size = needed;
        blk->next = new_blk;
    }
}

void *malloc(size_t size) {
    if (size == 0) return NULL;

    size = ALIGN_16(size);  /* Ensure 16-byte alignment */

    if (!heap_initialized) {
        if (heap_init() < 0) return NULL;
    }

    /* First-fit search */
    block_header_t *blk = heap_head;
    while (blk) {
        if (blk->free && blk->size >= size) {
            block_split(blk, size);
            blk->free = 0;
            return (void *)((char *)blk + BLOCK_HDR_SIZE);
        }
        blk = blk->next;
    }

    /* No suitable block found — extend heap */
    blk = heap_extend(size);
    if (!blk) return NULL;

    block_split(blk, size);
    blk->free = 0;
    return (void *)((char *)blk + BLOCK_HDR_SIZE);
}

void free(void *ptr) {
    if (!ptr) return;

    block_header_t *blk = (block_header_t *)((char *)ptr - BLOCK_HDR_SIZE);
    blk->free = 1;

    /* Coalesce with next block if also free */
    if (blk->next && blk->next->free) {
        blk->size += BLOCK_HDR_SIZE + blk->next->size;
        blk->next = blk->next->next;
    }

    /* Coalesce from head: scan for adjacent free pairs */
    block_header_t *p = heap_head;
    while (p && p->next) {
        if (p->free && p->next->free) {
            p->size += BLOCK_HDR_SIZE + p->next->size;
            p->next = p->next->next;
            /* Don't advance — check again in case of triple merge */
        } else {
            p = p->next;
        }
    }
}

void *calloc(size_t nmemb, size_t size) {
    size_t total = nmemb * size;
    if (total == 0) return NULL;
    void *p = malloc(total);
    if (p) memset(p, 0, total);
    return p;
}

void *realloc(void *ptr, size_t new_size) {
    if (!ptr) return malloc(new_size);
    if (new_size == 0) { free(ptr); return NULL; }

    block_header_t *blk = (block_header_t *)((char *)ptr - BLOCK_HDR_SIZE);
    if (blk->size >= new_size) return ptr;  /* Already large enough */

    void *new_ptr = malloc(new_size);
    if (!new_ptr) return NULL;
    memcpy(new_ptr, ptr, blk->size);
    free(ptr);
    return new_ptr;
}

/* ========================================
 * Additional String Utilities (Jalon 27+)
 * ======================================== */

char *strncpy(char *dest, const char *src, size_t n) {
    size_t i;
    for (i = 0; i < n && src[i]; i++)
        dest[i] = src[i];
    for (; i < n; i++)
        dest[i] = '\0';
    return dest;
}

int strncmp(const char *s1, const char *s2, size_t n) {
    for (size_t i = 0; i < n; i++) {
        if (s1[i] != s2[i]) return (unsigned char)s1[i] - (unsigned char)s2[i];
        if (s1[i] == '\0') return 0;
    }
    return 0;
}

void print_dec(long val) {
    print_int(val);
}

char *strcpy(char *dest, const char *src) {
    char *d = dest;
    while ((*d++ = *src++));
    return dest;
}

char *strcat(char *dest, const char *src) {
    char *d = dest;
    while (*d) d++;
    while ((*d++ = *src++));
    return dest;
}

int atoi(const char *s) {
    int result = 0;
    int sign = 1;
    while (*s == ' ' || *s == '\t') s++;
    if (*s == '-') { sign = -1; s++; }
    else if (*s == '+') { s++; }
    while (*s >= '0' && *s <= '9') {
        result = result * 10 + (*s - '0');
        s++;
    }
    return sign * result;
}
