/*
 * threads.c - AetherionOS Jalon 20: Multi-Threading Ring 3 Validation
 *
 * Spawns two threads that increment a shared volatile counter to 200
 * using a basic spinlock mutex with __sync_bool_compare_and_swap and
 * sys_yield on contention.
 *
 * Expected output: "Counter final value = 200"
 *
 * Compiled with: gcc -nostdlib -fno-builtin -static -mcmodel=large
 */

#include "libc_stub.h"

/* ========================================
 * Shared state (volatile for visibility)
 * ======================================== */

static volatile long shared_counter = 0;
static volatile long mutex_lock = 0;

/* ========================================
 * Spinlock mutex using GCC atomic builtins
 * ======================================== */

static void mutex_acquire(void) {
    while (!__sync_bool_compare_and_swap(&mutex_lock, 0, 1)) {
        /* Contention: yield CPU to the other thread */
        sys_yield();
    }
}

static void mutex_release(void) {
    __sync_lock_release(&mutex_lock);
}

/* ========================================
 * Thread function: increment counter 100x
 * ======================================== */

static void thread_func(void) {
    long my_pid = getpid();
    puts("[THREAD] Started, PID=");
    print_int(my_pid);
    puts("\n");

    int i;
    for (i = 0; i < 100; i++) {
        mutex_acquire();
        shared_counter++;
        mutex_release();
    }

    puts("[THREAD] PID=");
    print_int(my_pid);
    puts(" done (incremented 100 times)\n");

    exit(0);
}

/* ========================================
 * Main entry point
 * ======================================== */

void _start(void) {
    puts("========================================\n");
    puts("[J20] AetherionOS Jalon 20 - Multi-Threading Ring 3\n");
    puts("========================================\n\n");

    long my_pid = getpid();
    puts("[J20] Main process PID=");
    print_int(my_pid);
    puts("\n");

    /* Create two threads */
    puts("[J20] Creating thread 1...\n");
    long t1 = thread_create(thread_func);
    if (t1 < 0) {
        puts("[J20] FAIL: thread_create returned ");
        print_int(t1);
        puts("\n");
        exit(1);
    }
    puts("[J20] Thread 1 PID=");
    print_int(t1);
    puts("\n");

    puts("[J20] Creating thread 2...\n");
    long t2 = thread_create(thread_func);
    if (t2 < 0) {
        puts("[J20] FAIL: thread_create returned ");
        print_int(t2);
        puts("\n");
        exit(1);
    }
    puts("[J20] Thread 2 PID=");
    print_int(t2);
    puts("\n");

    puts("[J20] Waiting for threads to finish...\n");

    /* Wait for both threads */
    long w1 = sys_wait(0);
    puts("[J20] wait returned: ");
    print_int(w1);
    puts("\n");

    long w2 = sys_wait(0);
    puts("[J20] wait returned: ");
    print_int(w2);
    puts("\n");

    /* Print final counter value */
    puts("\n========================================\n");
    puts("[J20] Counter final value = ");
    print_int(shared_counter);
    puts("\n");

    if (shared_counter == 200) {
        puts("[J20] === ALL JALON 20 TESTS PASSED ===\n");
    } else {
        puts("[J20] FAIL: expected 200, got ");
        print_int(shared_counter);
        puts("\n");
    }
    puts("========================================\n");

    exit(0);
}
