/*
 * ui.c - AetherionOS Jalon 21: GUI Framebuffer Validation
 *
 * Maps the framebuffer via sys_mmap_fb (syscall 10), reads width/height/stride,
 * and draws a solid red 100x100 pixel square centered on the screen.
 *
 * Pixel format: 32-bit BGRA (Blue, Green, Red, Alpha)
 *   Red = 0x00FF0000
 *
 * Expected output: "[J21] Red square drawn at center of screen"
 *
 * Compiled with: gcc -nostdlib -fno-builtin -static -mcmodel=large
 */

#include "libc_stub.h"

/* sys_mmap_fb: syscall 10
 * a1 = pointer to info buffer (4 x u64):
 *   [0] = framebuffer virtual address
 *   [1] = width
 *   [2] = height
 *   [3] = stride (bytes per row)
 * Returns: framebuffer virtual address, or negative error
 */
static long sys_mmap_fb(unsigned long *info_buf) {
    return syscall1(10, (long)info_buf);
}

void _start(void) {
    puts("========================================\n");
    puts("[J21] AetherionOS Jalon 21 - GUI Framebuffer\n");
    puts("========================================\n\n");

    /* Query framebuffer info */
    unsigned long fb_info[4];
    long ret = sys_mmap_fb(fb_info);

    if (ret < 0 || ret == 0) {
        puts("[J21] FAIL: sys_mmap_fb returned ");
        print_int(ret);
        puts("\n");
        puts("[J21] Framebuffer not available (running without -vga std?)\n");
        exit(1);
    }

    unsigned long fb_addr  = fb_info[0];
    unsigned long fb_width  = fb_info[1];
    unsigned long fb_height = fb_info[2];
    unsigned long fb_stride = fb_info[3];

    puts("[J21] Framebuffer mapped at 0x");
    print_hex(fb_addr);
    puts("\n");
    puts("[J21] Resolution: ");
    print_int((long)fb_width);
    puts("x");
    print_int((long)fb_height);
    puts(", stride=");
    print_int((long)fb_stride);
    puts("\n");

    /* Draw a red 100x100 square centered on the screen */
    unsigned long sq_size = 100;
    unsigned long x_start = (fb_width - sq_size) / 2;
    unsigned long y_start = (fb_height - sq_size) / 2;

    puts("[J21] Drawing red ");
    print_int((long)sq_size);
    puts("x");
    print_int((long)sq_size);
    puts(" square at (");
    print_int((long)x_start);
    puts(",");
    print_int((long)y_start);
    puts(")\n");

    /* Pixel format: 32-bit BGRA
     * Red pixel: B=0x00, G=0x00, R=0xFF, A=0x00 -> 0x00FF0000 */
    unsigned int red_pixel = 0x00FF0000;
    unsigned char *fb = (unsigned char *)fb_addr;

    unsigned long y, x;
    for (y = y_start; y < y_start + sq_size; y++) {
        unsigned int *row = (unsigned int *)(fb + y * fb_stride);
        for (x = x_start; x < x_start + sq_size; x++) {
            row[x] = red_pixel;
        }
    }

    puts("\n========================================\n");
    puts("[J21] Red square drawn at center of screen\n");
    puts("[J21] === JALON 21 GUI TEST PASSED ===\n");
    puts("========================================\n");

    exit(0);
}
