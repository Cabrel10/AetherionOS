/*
 * test_malloc.c - Jalon 27: Dynamic memory allocation test
 *
 * Tests:
 *  1. sys_brk(0) returns valid heap base
 *  2. malloc returns non-NULL pointers
 *  3. malloc returns 16-byte aligned pointers
 *  4. Multiple allocations don't overlap
 *  5. Write/read-back validates allocated memory
 *  6. free() + re-malloc() reuses freed blocks
 *  7. calloc() returns zeroed memory
 *  8. realloc() preserves data
 *  9. Stress test: 100 allocations + frees
 */

#include "../../sdk/c/aetherion.h"

static int tests_passed = 0;
static int tests_failed = 0;

static void check(int cond, const char *name) {
    if (cond) {
        puts("[J27-OK] ");
        puts(name);
        puts("\n");
        tests_passed++;
    } else {
        puts("[J27-FAIL] ");
        puts(name);
        puts("\n");
        tests_failed++;
    }
}

int main(void) {
    puts("[J27] AetherionOS Jalon 27 - Dynamic Memory Allocator\n");
    puts("[J27] ================================================\n");

    /* Test 1: sys_brk(0) returns valid heap base */
    long brk0 = sys_brk(0);
    check(brk0 >= 0x0000300000000000L, "sys_brk(0) returns valid heap base");
    puts("[J27] Heap base = 0x");
    print_hex((unsigned long)brk0);
    puts("\n");

    /* Test 2: malloc returns non-NULL */
    int *arr1 = (int *)malloc(100 * sizeof(int));
    check(arr1 != NULL, "malloc(400) returns non-NULL");

    /* Test 3: 16-byte alignment */
    check(((unsigned long)arr1 & 0xF) == 0, "malloc returns 16-byte aligned ptr");

    /* Test 4: Write and read-back */
    for (int i = 0; i < 100; i++) arr1[i] = i * 7;
    int sum = 0;
    for (int i = 0; i < 100; i++) sum += arr1[i];
    check(sum == 34650, "malloc write/read: sum(0..99 * 7) = 34650");

    /* Test 5: Second allocation doesn't overlap first */
    char *str = (char *)malloc(256);
    check(str != NULL, "malloc(256) returns non-NULL");
    check((unsigned long)str != (unsigned long)arr1, "Two allocations don't overlap");

    /* Write string */
    const char *hello = "Hello from malloc!";
    int j = 0;
    while (hello[j]) { str[j] = hello[j]; j++; }
    str[j] = '\0';
    check(strcmp(str, "Hello from malloc!") == 0, "String write/read OK");

    /* Test 6: Third allocation */
    int *arr2 = (int *)malloc(50 * sizeof(int));
    check(arr2 != NULL, "malloc(200) third allocation OK");
    for (int i = 0; i < 50; i++) arr2[i] = i + 1000;
    check(arr2[49] == 1049, "Third allocation data integrity");

    /* Test 7: free + re-alloc reuse */
    free(arr1);
    free(str);
    int *reuse = (int *)malloc(100 * sizeof(int));
    check(reuse != NULL, "Reuse after free OK");
    for (int i = 0; i < 100; i++) reuse[i] = 42;
    check(reuse[99] == 42, "Reused memory writeable");
    free(reuse);
    free(arr2);

    /* Test 8: calloc returns zeroed memory */
    int *zeroed = (int *)calloc(64, sizeof(int));
    check(zeroed != NULL, "calloc(64, 4) returns non-NULL");
    int all_zero = 1;
    for (int i = 0; i < 64; i++) {
        if (zeroed[i] != 0) { all_zero = 0; break; }
    }
    check(all_zero, "calloc memory is zeroed");
    free(zeroed);

    /* Test 9: realloc preserves data */
    char *buf = (char *)malloc(32);
    for (int i = 0; i < 32; i++) buf[i] = (char)(i + 65);
    char *buf2 = (char *)realloc(buf, 128);
    check(buf2 != NULL, "realloc(32->128) OK");
    int data_ok = 1;
    for (int i = 0; i < 32; i++) {
        if (buf2[i] != (char)(i + 65)) { data_ok = 0; break; }
    }
    check(data_ok, "realloc preserves original data");
    free(buf2);

    /* Test 10: Stress test - 100 allocations then free */
    puts("[J27] Stress test: 100 malloc/free cycles...\n");
    void *ptrs[100];
    int stress_ok = 1;
    for (int i = 0; i < 100; i++) {
        ptrs[i] = malloc(64 + i * 8);
        if (!ptrs[i]) { stress_ok = 0; break; }
        /* Write a canary */
        *(int *)ptrs[i] = 0xDEAD0000 + i;
    }
    /* Verify canaries */
    for (int i = 0; i < 100 && stress_ok; i++) {
        if (*(int *)ptrs[i] != (int)(0xDEAD0000 + i)) { stress_ok = 0; }
    }
    /* Free all */
    for (int i = 0; i < 100; i++) {
        free(ptrs[i]);
    }
    check(stress_ok, "Stress: 100 malloc/free with canary validation");

    /* Summary */
    puts("[J27] ================================================\n");
    puts("[J27] Results: ");
    print_int(tests_passed);
    puts(" passed, ");
    print_int(tests_failed);
    puts(" failed\n");

    if (tests_failed == 0) {
        puts("[J27] === ALL JALON 27 TESTS PASSED ===\n");
    } else {
        puts("[J27] === SOME TESTS FAILED ===\n");
    }

    exit(0);
    return 0;
}
