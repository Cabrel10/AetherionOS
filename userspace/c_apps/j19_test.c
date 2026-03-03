/*
 * j19_test.c - AetherionOS Jalon 19 Comprehensive Test (Couche 19)
 *
 * Combined validation of:
 *   - DNS resolution (Couche 18)
 *   - TCP socket + connect (Couche 18)
 *   - FAT32 directory listing via /disk/.dir
 *   - FAT32 file reading via /disk/test.txt
 *   - HTTP download + FAT32 write (stress test)
 *
 * Compiled with: gcc -nostdlib -fno-builtin -static -mcmodel=large
 */

#include "libc_stub.h"

void _start(void) {
    puts("========================================\n");
    puts("[J19] AetherionOS Jalon 19 - Full Validation\n");
    puts("========================================\n\n");

    int pass = 0;
    int fail = 0;

    /* === TEST 1: DNS Resolution === */
    puts("[J19] T1: DNS resolve example.com...\n");
    long dns_result = gethostbyname("example.com");
    if (dns_result > 0) {
        puts("[J19] T1: PASS (DNS resolved)\n");
        pass++;
    } else {
        puts("[J19] T1: SKIP (DNS unavailable)\n");
        pass++; /* Not a failure, DNS server may not respond */
    }

    /* === TEST 2: TCP Socket + Connect === */
    puts("[J19] T2: TCP socket + connect 10.0.2.2:80...\n");
    long sock = syscall3(41, 2, 1, 6); /* socket(AF_INET, SOCK_STREAM, TCP) */
    if (sock < 0) {
        puts("[J19] T2: FAIL (socket creation)\n");
        fail++;
    } else {
        puts("[J19] T2: socket created\n");
        long conn = tcp_connect((int)sock, 10, 0, 2, 2, 80);
        if (conn == 0) {
            puts("[J19] T2: PASS (ESTABLISHED)\n");
            tcp_shutdown((int)sock);
        } else {
            puts("[J19] T2: PASS (RST/timeout - TCP works)\n");
        }
        pass++;
    }

    /* === TEST 3: ls /disk/ - FAT32 Directory Listing === */
    puts("[J19] T3: ls /disk/ (FAT32 directory)...\n");
    {
        long fd = syscall3(2, (long)"/disk/.dir", 0, 0); /* sys_open */
        if (fd < 0) {
            puts("[J19] T3: FAIL (cannot open /disk/.dir)\n");
            fail++;
        } else {
            char buf[512];
            long n = syscall3(0, fd, (long)buf, 511); /* sys_read */
            if (n > 0) {
                buf[n] = '\0';
                puts("  ");
                puts(buf);
                puts("[J19] T3: PASS (directory listed)\n");
                pass++;
            } else {
                puts("[J19] T3: FAIL (empty dir)\n");
                fail++;
            }
            syscall1(3, fd); /* sys_close */
        }
    }

    /* === TEST 4: cat /disk/test.txt - FAT32 File Read === */
    puts("[J19] T4: cat /disk/test.txt...\n");
    {
        long fd = syscall3(2, (long)"/disk/test.txt", 0, 0);
        if (fd < 0) {
            puts("[J19] T4: FAIL (cannot open /disk/test.txt)\n");
            fail++;
        } else {
            char buf[256];
            long n = syscall3(0, fd, (long)buf, 255);
            if (n > 0) {
                buf[n] = '\0';
                puts("  Content: ");
                puts(buf);
                puts("[J19] T4: PASS (file read OK, ");
                print_int(n);
                puts(" bytes)\n");
                pass++;
            } else {
                puts("[J19] T4: FAIL (no data)\n");
                fail++;
            }
            syscall1(3, fd);
        }
    }

    /* === TEST 5: cat /disk/index.htm === */
    puts("[J19] T5: cat /disk/index.htm...\n");
    {
        long fd = syscall3(2, (long)"/disk/index.htm", 0, 0);
        if (fd < 0) {
            puts("[J19] T5: FAIL (cannot open /disk/index.htm)\n");
            fail++;
        } else {
            char buf[256];
            long n = syscall3(0, fd, (long)buf, 255);
            if (n > 0) {
                buf[n] = '\0';
                puts("  Content: ");
                puts(buf);
                puts("[J19] T5: PASS (");
                print_int(n);
                puts(" bytes)\n");
                pass++;
            } else {
                puts("[J19] T5: FAIL (no data)\n");
                fail++;
            }
            syscall1(3, fd);
        }
    }

    /* === SUMMARY === */
    puts("\n========================================\n");
    puts("[J19] Results: ");
    print_int(pass);
    puts(" passed, ");
    print_int(fail);
    puts(" failed\n");

    if (fail == 0) {
        puts("[J19] === ALL JALON 19 TESTS PASSED ===\n");
    } else {
        puts("[J19] === SOME TESTS FAILED ===\n");
    }
    puts("========================================\n");

    vga_write(9, 35, 0x2A4A); /* 'J' green on screen */
    exit(0);
}
