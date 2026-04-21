/* userspace/linux-test/hello-linux.c - Linuxulator test binary
 *
 * This is a standard Linux static binary compiled with musl-gcc.
 * It tests the Linuxulator's ability to execute Linux binaries
 * on AetherionOS.
 *
 * Build (in CI):
 *   musl-gcc -static -o hello-linux hello-linux.c
 *
 * Expected output:
 *   Hello from Linux
 */
#include <stdio.h>

int main(void) {
    puts("Hello from Linux");
    return 0;
}
