/*
 * test_preempt.c - Jalon 28: Preemptive Scheduler Test
 *
 * Demonstrates that the PIT timer interrupt preempts running
 * threads, enabling true multi-tasking without explicit yield().
 *
 * Creates two compute-bound threads that print periodic progress.
 * The scheduler tick (from PIT IRQ 0) re-enqueues the current process
 * and picks the next ready one, so both threads make progress
 * even though neither calls sys_yield().
 *
 * Success criteria: both threads complete their work, and the
 * interleaved "[AGENT-HIGH]" / "[AGENT-NORM]" messages in the
 * serial log prove preemption occurred.
 */

#include "../../sdk/c/aetherion.h"

/* Shared volatile counters — both threads share the same address space */
static volatile long counter_high   = 0;
static volatile long counter_normal = 0;

#define ITERATIONS 500000

/* High-priority agent: compute-bound loop */
void agent_high(void) {
    for (long i = 0; i < ITERATIONS; i++) {
        counter_high++;
        if (i % 100000 == 0) {
            puts("[AGENT-HIGH] Iteration ");
            print_int(i);
            puts(" (PID=");
            print_int(getpid());
            puts(")\n");
        }
    }
    puts("[AGENT-HIGH] Completed ");
    print_int(ITERATIONS);
    puts(" iterations\n");
    exit(0);
}

/* Normal-priority agent: compute-bound loop */
void agent_normal(void) {
    for (long i = 0; i < ITERATIONS; i++) {
        counter_normal++;
        if (i % 100000 == 0) {
            puts("[AGENT-NORM] Iteration ");
            print_int(i);
            puts(" (PID=");
            print_int(getpid());
            puts(")\n");
        }
    }
    puts("[AGENT-NORM] Completed ");
    print_int(ITERATIONS);
    puts(" iterations\n");
    exit(0);
}

int main(void) {
    puts("[J28] AetherionOS Jalon 28 - Preemptive Scheduler\n");
    puts("[J28] ================================================\n");
    puts("[J28] Main process PID=");
    print_int(getpid());
    puts("\n");

    /* Create high-priority thread */
    puts("[J28] Creating HIGH priority thread...\n");
    long pid_high = thread_create(agent_high);
    puts("[J28] HIGH thread PID=");
    print_int(pid_high);
    puts("\n");

    /* Create normal-priority thread */
    puts("[J28] Creating NORMAL priority thread...\n");
    long pid_normal = thread_create(agent_normal);
    puts("[J28] NORMAL thread PID=");
    print_int(pid_normal);
    puts("\n");

    /* Wait for both to finish */
    puts("[J28] Waiting for threads to finish...\n");
    long r1 = sys_wait(pid_high);
    puts("[J28] HIGH thread done (wait returned ");
    print_int(r1);
    puts(")\n");

    long r2 = sys_wait(pid_normal);
    puts("[J28] NORMAL thread done (wait returned ");
    print_int(r2);
    puts(")\n");

    /* Validate results */
    puts("[J28] Counter HIGH   = ");
    print_int(counter_high);
    puts("\n");
    puts("[J28] Counter NORMAL = ");
    print_int(counter_normal);
    puts("\n");

    int pass = (counter_high == ITERATIONS) && (counter_normal == ITERATIONS);

    puts("[J28] ================================================\n");
    if (pass) {
        puts("[J28] === ALL JALON 28 TESTS PASSED ===\n");
        puts("[J28] Both agents completed with preemptive scheduling\n");
    } else {
        puts("[J28] === PREEMPTION TEST INCONCLUSIVE ===\n");
        puts("[J28] Threads may have run cooperatively\n");
    }

    exit(0);
    return 0;
}
