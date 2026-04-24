/*
 * agent_saga.c - Jalon 30/31: Persistence agent (FAT32 write + Sagas/Almanach)
 * Tests file I/O to /disk (FAT32 partition).
 */
#include "libc_stub.h"

void _start(void) {
    puts("=== Jalon 30/31: agent_saga (Persistence Agent) ===\n");

    /* Test 1: Write to FAT32 */
    puts("[saga] Writing to /disk/var/saga_test.txt...\n");
    long fd = open("/disk/var/saga_test.txt", 0x41);  /* O_WRONLY | O_CREAT (mode ignored) */
    if (fd >= 0) {
        const char *data = "AetherionOS Saga Checkpoint v1\n";
        long written = write(fd, data, 31);
        if (written > 0) {
            puts("[saga] Write OK (");
            /* print count */
            char buf[8];
            buf[0] = '0' + (written / 10);
            buf[1] = '0' + (written % 10);
            buf[2] = ' ';
            buf[3] = 'b';
            buf[4] = ')';
            buf[5] = '\n';
            buf[6] = 0;
            puts(buf);
        } else {
            puts("[saga] Write FAIL\n");
        }
        /* close via syscall 3 */
        syscall1(3, fd);
    } else {
        puts("[saga] open() failed (no /disk mount?)\n");
    }

    /* Test 2: Read back */
    puts("[saga] Reading /disk/var/saga_test.txt...\n");
    fd = open("/disk/var/saga_test.txt", 0);  /* O_RDONLY (mode ignored) */
    if (fd >= 0) {
        char rbuf[64];
        long n = read(fd, rbuf, 63);
        if (n > 0) {
            rbuf[n] = 0;
            puts("[saga] Read OK: ");
            puts(rbuf);
        } else {
            puts("[saga] Read returned 0\n");
        }
        syscall1(3, fd);
    } else {
        puts("[saga] read open() failed\n");
    }

    /* Publish saga checkpoint on Cognitive Bus */
    bus_publish(0x3001, 2, 1);
    puts("[saga] Checkpoint published on Cognitive Bus\n");
    puts("=== agent_saga complete ===\n");
    exit(0);
}
