/*
 * ls.c - AetherionOS Ring 3 directory listing (Couche 19)
 *
 * Lists files on the FAT32 disk mounted at /disk/
 * Uses sys_open + sys_read to read directory listing from /disk/.dir
 *
 * The kernel populates /disk/.dir with a text listing of the FAT32 root.
 *
 * Compiled with: gcc -nostdlib -fno-builtin -static -mcmodel=large
 */

#include "libc_stub.h"

void _start(void) {
    puts("[LS] AetherionOS File Listing (Couche 19)\n");
    puts("[LS] Listing /disk/ ...\n\n");

    /* Open the directory listing pseudo-file */
    long fd = syscall3(2, (long)"/disk/.dir", 0, 0); /* sys_open */
    if (fd < 0) {
        puts("[LS] /disk/ not mounted or empty\n");
        exit(1);
    }

    /* Read and display */
    char buf[512];
    long n = syscall3(0, fd, (long)buf, 511); /* sys_read */
    if (n > 0) {
        buf[n] = '\0';
        puts(buf);
    } else {
        puts("[LS] (empty)\n");
    }

    syscall1(3, fd); /* sys_close */
    puts("\n[LS] Done\n");
    exit(0);
}
