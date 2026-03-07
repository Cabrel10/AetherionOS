//! AetherionOS Jalon 51 - Multithreaded MatMul via sys_clone/sys_wait (Ring 3)
//!
//! Demonstrates cooperative multithreading:
//!   1. Allocates a shared buffer for matrix data and result
//!   2. Creates 2 worker threads via sys_clone, each computing half the result
//!   3. Parent waits for each worker via sys_wait
//!   4. Verifies merged result matches single-threaded reference
//!
//! Threading model: cooperative (clone + wait), shared address space (threads).
//! Workers share the same PML4 — real shared memory, no IPC needed for data.

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

const N: usize = 32; // Matrix dimension NxN
const NUM_WORKERS: usize = 2;
const STACK_SIZE: usize = 4096 * 4; // 16KB per worker stack

/// Shared state (allocated via mmap, visible to all threads)
#[repr(C)]
struct SharedState {
    // Input matrix A (N x N)
    a: [f32; N * N],
    // Input vector x (N)
    x: [f32; N],
    // Output vector y (N) — workers write their portion
    y: [f32; N],
    // Reference output (single-threaded)
    y_ref: [f32; N],
    // Worker status: 0=pending, 1=done
    worker_done: [u64; NUM_WORKERS],
    // Worker parameters: start_row, end_row
    worker_start: [usize; NUM_WORKERS],
    worker_end: [usize; NUM_WORKERS],
}

/// Worker thread entry point for worker 0
#[no_mangle]
pub extern "C" fn worker_0_entry() -> ! {
    // Read shared state from known mmap address
    // The parent stores the shared state pointer at a fixed location
    let shared = unsafe { &mut *(SHARED_PTR as *mut SharedState) };
    let start = shared.worker_start[0];
    let end = shared.worker_end[0];

    // Compute y[start..end] = A[start..end, :] * x
    for i in start..end {
        let mut sum: f32 = 0.0;
        let row_base = i * N;
        for j in 0..N {
            sum += shared.a[row_base + j] * shared.x[j];
        }
        shared.y[i] = sum;
    }

    shared.worker_done[0] = 1;
    sys_exit(0);
    loop {} // unreachable
}

/// Worker thread entry point for worker 1
#[no_mangle]
pub extern "C" fn worker_1_entry() -> ! {
    let shared = unsafe { &mut *(SHARED_PTR as *mut SharedState) };
    let start = shared.worker_start[1];
    let end = shared.worker_end[1];

    for i in start..end {
        let mut sum: f32 = 0.0;
        let row_base = i * N;
        for j in 0..N {
            sum += shared.a[row_base + j] * shared.x[j];
        }
        shared.y[i] = sum;
    }

    shared.worker_done[1] = 1;
    sys_exit(0);
    loop {} // unreachable
}

/// Global shared state pointer (set by parent before cloning)
/// Safe because all threads share the same address space
static mut SHARED_PTR: u64 = 0;

fn f32_abs(x: f32) -> f32 {
    if x < 0.0 { -x } else { x }
}

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J51] Multithreaded MatMul Agent v1.0");
    print("[J51] Matrix: ");
    print_u64(N as u64);
    print("x");
    print_u64(N as u64);
    print(", workers=");
    print_u64(NUM_WORKERS as u64);
    println("");

    // Allocate shared state
    let shared_size = core::mem::size_of::<SharedState>();
    print("[J51] SharedState size: ");
    print_u64(shared_size as u64);
    println(" bytes");

    let shared_addr = sys_mmap(shared_size);
    if shared_addr == 0 || shared_addr > 0xFFFF_FFFF_FFFF {
        println("[J51] FAIL: mmap shared state");
        return 1;
    }
    unsafe { SHARED_PTR = shared_addr; }

    let shared = unsafe { &mut *(shared_addr as *mut SharedState) };

    // Initialize matrix A and vector x
    for i in 0..N {
        for j in 0..N {
            // Simple deterministic values
            shared.a[i * N + j] = ((i * 7 + j * 3 + 1) % 100) as f32 * 0.01;
        }
        shared.x[i] = (i + 1) as f32 * 0.1;
        shared.y[i] = 0.0;
        shared.y_ref[i] = 0.0;
    }

    // Single-threaded reference computation
    let t0 = sys_rdtsc();
    for i in 0..N {
        let mut sum: f32 = 0.0;
        let row_base = i * N;
        for j in 0..N {
            sum += shared.a[row_base + j] * shared.x[j];
        }
        shared.y_ref[i] = sum;
    }
    let t1 = sys_rdtsc();
    print("[J51] Reference matmul: ");
    print_u64(t1 - t0);
    println(" cycles");

    // Set worker parameters: split rows evenly
    let rows_per_worker = N / NUM_WORKERS;
    for w in 0..NUM_WORKERS {
        shared.worker_start[w] = w * rows_per_worker;
        shared.worker_end[w] = if w == NUM_WORKERS - 1 { N } else { (w + 1) * rows_per_worker };
        shared.worker_done[w] = 0;
        print("[J51] Worker ");
        print_u64(w as u64);
        print(": rows ");
        print_u64(shared.worker_start[w] as u64);
        print("..");
        print_u64(shared.worker_end[w] as u64);
        println("");
    }

    // Allocate stacks for workers
    let stacks_addr = sys_mmap(STACK_SIZE * NUM_WORKERS);
    if stacks_addr == 0 {
        println("[J51] FAIL: mmap stacks");
        return 1;
    }

    // Worker function pointers
    let worker_fns: [u64; 2] = [
        worker_0_entry as u64,
        worker_1_entry as u64,
    ];

    let t2 = sys_rdtsc();

    // Clone workers
    let mut child_pids = [0i64; NUM_WORKERS];
    for w in 0..NUM_WORKERS {
        let stack_top = stacks_addr + ((w + 1) * STACK_SIZE) as u64;
        // Write function pointer at (stack_top - 8) as expected by sys_clone
        unsafe {
            let fn_slot = (stack_top - 8) as *mut u64;
            core::ptr::write_volatile(fn_slot, worker_fns[w]);
        }

        print("[J51] Cloning worker ");
        print_u64(w as u64);
        print(" stack=");
        print_hex(stack_top);
        print(" fn=");
        print_hex(worker_fns[w]);
        println("");

        let pid = sys_clone(stack_top);
        if pid <= 0 {
            // We are the child (pid == 0) — should not happen since
            // the child will jump to the fn_ptr directly
            // If it does, the child entry function handles it
            println("[J51] WARN: clone returned 0 (child path)");
        }
        child_pids[w] = pid;
        print("[J51] Worker ");
        print_u64(w as u64);
        print(" PID=");
        print_u64(pid as u64);
        println("");
    }

    // Wait for workers
    for w in 0..NUM_WORKERS {
        if child_pids[w] > 0 {
            print("[J51] Waiting for worker ");
            print_u64(w as u64);
            println("...");
            let ret = sys_wait(child_pids[w] as u64);
            print("[J51] Worker ");
            print_u64(w as u64);
            print(" exited (ret=");
            print_u64(ret as u64);
            println(")");
        }
    }

    let t3 = sys_rdtsc();
    print("[J51] MT matmul: ");
    print_u64(t3 - t2);
    println(" cycles");

    // Verify result matches reference
    let mut max_err: f32 = 0.0;
    let mut mismatches: u64 = 0;
    for i in 0..N {
        let err = f32_abs(shared.y[i] - shared.y_ref[i]);
        if err > max_err { max_err = err; }
        if err > 0.001 { mismatches += 1; }
    }

    print("[J51] Mismatches: ");
    print_u64(mismatches);
    println("");

    if mismatches == 0 {
        println("[J51] Verification: PASS (results match reference)");
    } else {
        println("[J51] Verification: FAIL");
        return 1;
    }

    // Publish result
    sys_bus_publish(0xC051, 2, (N * N) as u64);
    println("[J51] Bus 0xC051 OK");

    sys_write(1, b"\n[J51-OK] MT MatMul 2 workers SUCCESS\n");
    0
}
