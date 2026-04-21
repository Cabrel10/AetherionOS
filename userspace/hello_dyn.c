/* userspace/hello_dyn.c - Dynamic linking test binary for AetherionOS
 *
 * This binary is dynamically linked against musl libc.
 * It requires /lib/ld-musl-x86_64.so.1 to be present in the VFS.
 *
 * Build (in CI):
 *   musl-gcc -o hello_dyn.elf hello_dyn.c
 *
 * Expected output:
 *   Dynamic linking works perfectly on AetherionOS!
 */
#include <stdio.h>

int main(void) {
    puts("Dynamic linking works perfectly on AetherionOS!");
    return 0;
}
