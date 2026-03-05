/*
 * crt0.c - AetherionOS C Runtime Startup (CRT0)
 *
 * Provides a WEAK _start entry point that calls main() and exit().
 * Apps that define their own _start (legacy) override this one.
 * Apps that only define main() get this _start automatically.
 */

/* Declare main (provided by the user's application) */
extern int main(void) __attribute__((weak));

/* Declare exit (provided by libaetherion.a) */
extern void exit(int status);

/* _start: weak entry point — overridden by apps with their own _start */
void _start(void) __attribute__((weak, section(".text.start"), used));

void _start(void) {
    int ret = 0;
    if (main) {
        ret = main();
    }
    exit(ret);
    /* unreachable */
    for (;;) {}
}
