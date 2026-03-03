/*
 * cat.c - AetherionOS Ring 3 file display (Couche 19)
 *
 * Reads and displays the contents of /disk/test.txt
 * from the FAT32 filesystem.
 *
 * Compiled with: gcc -nostdlib -fno-builtin -static -mcmodel=large
 */

#include "libc_stub.h"

void _start(void) {
    puts("[CAT] AetherionOS File Reader (Couche 19)\n");
    puts("[CAT] Reading /disk/test.txt ...\n\n");

    /* Open the file */
    long fd = syscall3(2, (long)"/disk/test.txt", 0, 0); /* sys_open */
    if (fd < 0) {
        puts("[CAT] File not found: /disk/test.txt\n");
        exit(1);
    }

    /* Read and display in chunks */
    char buf[256];
    long total = 0;
    long n;
    while (1) {
        n = syscall3(0, fd, (long)buf, 255); /* sys_read */
        if (n <= 0) break;
        buf[n] = '\0';
        puts(buf);
        total += n;
    }

    puts("\n");
    syscall1(3, fd); /* sys_close */
    puts("[CAT] Read ");
    print_int(total);
    puts(" bytes\n");
    exit(0);
}
