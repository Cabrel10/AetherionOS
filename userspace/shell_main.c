/*
 * shell_main.c - AetherionOS Ring 3 Interactive Shell
 */
#include "libc_stub.h"

void _start(void) {
    puts("AetherionOS Shell v1.0 (Ring 3)\n");
    puts("Type 'help' for commands, 'exit' to quit.\n");
    puts("shell> ");
    puts("[shell] No interactive TTY - exiting cleanly\n");
    exit(0);
}
