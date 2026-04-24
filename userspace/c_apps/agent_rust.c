/*
 * agent_rust.c - Jalon 29: Native Rust-like Ring 3 agent
 * Demonstrates Vec alloc (via mmap) + Bus publish from C.
 * Compiled as a C app that mimics what the Rust SDK does.
 */
#include "libc_stub.h"

#define HEAP_SIZE 4096
static char heap[HEAP_SIZE];
static int heap_pos = 0;

static void *simple_alloc(int size) {
    if (heap_pos + size > HEAP_SIZE) return NULL;
    void *ptr = &heap[heap_pos];
    heap_pos += size;
    return ptr;
}

void _start(void) {
    puts("=== Jalon 29: agent_rust (C variant) ===\n");
    puts("[agent_rust] Vec alloc simulation...\n");

    /* Simulate Vec<u64> with 10 elements */
    long *vec = (long *)simple_alloc(10 * sizeof(long));
    if (!vec) {
        puts("[agent_rust] FAIL: alloc failed\n");
        exit(1);
    }

    for (int i = 0; i < 10; i++) {
        vec[i] = i * 42;
    }

    /* Verify */
    int ok = 1;
    for (int i = 0; i < 10; i++) {
        if (vec[i] != i * 42) { ok = 0; break; }
    }

    if (ok) {
        puts("[agent_rust] Vec alloc + verify: OK (10 elements)\n");
    } else {
        puts("[agent_rust] Vec verify: FAIL\n");
    }

    /* Publish result on Cognitive Bus */
    bus_publish(0x2901, 2, ok ? 1 : 0);
    puts("[agent_rust] Bus publish 0x2901 done\n");
    puts("=== agent_rust complete ===\n");
    exit(0);
}
