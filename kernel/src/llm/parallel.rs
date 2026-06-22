//! Real in-kernel SMP parallelism for the LLM logit projection.
//!
//! The dominant cost of a forward pass is the final logit projection:
//!   x (dim=576)  ·  W (vocab=49152 × 576, Q8_0/Q4_0)  →  49152 dot products.
//! That is ~28M f32 MACs for a single token — and it was running entirely on the
//! BSP while AP1/AP2/AP3 sat in `pause`/`hlt`. "4 CPU détectés ≠ 4 CPU utilisés".
//!
//! This module fixes that with a lock-free, zero-allocation work queue:
//!   * BSP fills `JOB` with raw pointers to `x`, the quantized weight bytes and
//!     the dimensions, partitions the 49152 vocab rows into `cpu_count` contiguous
//!     ranges, then publishes the job by bumping `JOB_GENERATION`.
//!   * Each AP, in its dispatch loop, calls `try_run_worker(core_id)`. If a new
//!     generation is visible it computes the argmax over *its* row range and
//!     writes the partial `(best_tok, best_val)` into `PARTIAL[core_id]`, then
//!     bumps `WORKERS_DONE`.
//!   * BSP computes its own range inline, spins on `WORKERS_DONE` (PAUSE), then
//!     reduces the `cpu_count` partial results into the global argmax.
//!
//! Safety: the weight bytes and `x` are read-only for the whole job; each core
//! writes only to its own `PARTIAL` slot and a disjoint logical row range, so
//! there is no data race. Pointers stay valid because the BSP blocks inside
//! `argmax_quant_logits_parallel` until every worker has finished.

use core::sync::atomic::{AtomicU64, AtomicU32, Ordering};

/// Maximum cores we can fan out to (matches apic::MAX_CPUS upper bound we use).
pub const MAX_WORKERS: usize = 8;

/// Description of the current parallel argmax job. All fields are plain integers
/// (pointers stored as usize) so the struct can live in static atomics-guarded
/// memory without a lock. Only published/consumed under `JOB_GENERATION`.
#[derive(Clone, Copy)]
struct ArgmaxJob {
    x_ptr: usize,        // *const f32, length = dim
    w_ptr: usize,        // *const u8, quantized weight matrix
    w_len: usize,        // bytes available in w
    dim: usize,
    vocab: usize,
    is_q8: bool,
}

impl ArgmaxJob {
    const EMPTY: ArgmaxJob = ArgmaxJob {
        x_ptr: 0, w_ptr: 0, w_len: 0, dim: 0, vocab: 0, is_q8: false,
    };
}

/// The published job. Guarded by JOB_GENERATION: a worker only reads JOB after it
/// has observed a generation newer than the one it last processed.
static mut JOB: ArgmaxJob = ArgmaxJob::EMPTY;

/// Monotonic counter; incremented by the BSP every time a new job is published.
static JOB_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Number of worker ranges (== cpu_count) for the active job.
static JOB_WORKERS: AtomicU32 = AtomicU32::new(0);

/// Number of workers (BSP + APs) that have finished the active job.
static WORKERS_DONE: AtomicU32 = AtomicU32::new(0);

/// Per-core partial result: packed (best_tok << 32) | best_val.to_bits().
/// best_val is stored as f32 bits so we can compare after a bitwise reload.
static PARTIAL: [AtomicU64; MAX_WORKERS] = {
    const Z: AtomicU64 = AtomicU64::new(0);
    [Z; MAX_WORKERS]
};

/// Last generation each AP has already processed (so it runs a job exactly once).
static LAST_SEEN_GEN: [AtomicU64; MAX_WORKERS] = {
    const Z: AtomicU64 = AtomicU64::new(0);
    [Z; MAX_WORKERS]
};

#[inline]
fn pack_partial(tok: u32, val: f32) -> u64 {
    ((tok as u64) << 32) | (val.to_bits() as u64)
}

#[inline]
fn unpack_partial(p: u64) -> (u32, f32) {
    let tok = (p >> 32) as u32;
    let val = f32::from_bits((p & 0xFFFF_FFFF) as u32);
    (tok, val)
}

/// Compute the argmax dot-product over the vocab range [row_start, row_end).
/// This is the exact scalar kernel from inference::argmax_quant_logits, factored
/// out so both the BSP and the APs run identical math (bit-for-bit reduction).
fn argmax_range(
    x: &[f32], raw_data: &[u8], dim: usize,
    row_start: usize, row_end: usize, is_q8: bool,
) -> (u32, f32) {
    use super::matmul::f16_to_f32;
    let blocks_per_row = dim / 32;
    let bytes_per_block: usize = if is_q8 { 34 } else { 18 };
    let bytes_per_row = blocks_per_row * bytes_per_block;

    let mut best_tok: u32 = row_start as u32;
    let mut best_val: f32 = f32::NEG_INFINITY;

    for tok in row_start..row_end {
        let row_off = tok * bytes_per_row;
        if row_off + bytes_per_row > raw_data.len() {
            continue;
        }
        let mut dot = 0.0f32;
        if is_q8 {
            for b in 0..blocks_per_row {
                let off = row_off + b * 34;
                let raw_scale = f16_to_f32(u16::from_le_bytes([raw_data[off], raw_data[off + 1]]));
                let scale = if !raw_scale.is_finite() || raw_scale.abs() > 1000.0 { 0.0 } else { raw_scale };
                let base_idx = b * 32;
                let mut block_acc = 0.0f32;
                let mut i = 0;
                while i + 3 < 32 {
                    let v0 = raw_data[off + 2 + i] as i8;
                    let v1 = raw_data[off + 2 + i + 1] as i8;
                    let v2 = raw_data[off + 2 + i + 2] as i8;
                    let v3 = raw_data[off + 2 + i + 3] as i8;
                    block_acc += v0 as f32 * x[base_idx + i]
                              + v1 as f32 * x[base_idx + i + 1]
                              + v2 as f32 * x[base_idx + i + 2]
                              + v3 as f32 * x[base_idx + i + 3];
                    i += 4;
                }
                dot += scale * block_acc;
            }
        } else {
            for b in 0..blocks_per_row {
                let off = row_off + b * 18;
                let raw_scale = f16_to_f32(u16::from_le_bytes([raw_data[off], raw_data[off + 1]]));
                let scale = if !raw_scale.is_finite() || raw_scale.abs() > 1000.0 { 0.0 } else { raw_scale };
                let base_idx = b * 32;
                let mut block_acc = 0.0f32;
                for i in 0..16 {
                    let byte_val = raw_data[off + 2 + i];
                    let lo = (byte_val & 0x0F) as i8 - 8;
                    let hi = ((byte_val >> 4) & 0x0F) as i8 - 8;
                    let idx = i * 2;
                    block_acc += lo as f32 * x[base_idx + idx]
                               + hi as f32 * x[base_idx + idx + 1];
                }
                dot += scale * block_acc;
            }
        }
        if dot.is_finite() && dot > best_val {
            best_val = dot;
            best_tok = tok as u32;
        }
    }
    (best_tok, best_val)
}

/// Contiguous [start, end) row range for `worker` out of `workers` total,
/// splitting `vocab` rows as evenly as possible (last worker takes the remainder).
#[inline]
fn worker_range(worker: usize, workers: usize, vocab: usize) -> (usize, usize) {
    let per = vocab / workers;
    let start = worker * per;
    let end = if worker == workers - 1 { vocab } else { start + per };
    (start, end)
}

/// AP entry point — called from the dispatch loop in apic::ap_main.
/// Runs the current argmax job's slice for `core_id` exactly once per generation.
/// Returns true if it actually processed a job (useful for logging/back-off).
pub fn try_run_worker(core_id: usize) -> bool {
    if core_id == 0 || core_id >= MAX_WORKERS {
        return false; // BSP runs its slice inline; out-of-range cores skip.
    }
    let gen = JOB_GENERATION.load(Ordering::Acquire);
    if gen == 0 {
        return false; // no job ever published
    }
    if LAST_SEEN_GEN[core_id].load(Ordering::Acquire) == gen {
        return false; // already did this generation
    }
    let workers = JOB_WORKERS.load(Ordering::Acquire) as usize;
    if core_id >= workers {
        // This core isn't part of the current partition; still mark it seen so we
        // don't spin re-checking the same generation.
        LAST_SEEN_GEN[core_id].store(gen, Ordering::Release);
        return false;
    }

    // Snapshot the job (published-before JOB_GENERATION bump → Acquire makes it visible).
    let job = unsafe { JOB };
    if job.x_ptr == 0 || job.w_ptr == 0 {
        LAST_SEEN_GEN[core_id].store(gen, Ordering::Release);
        return false;
    }

    let x = unsafe { core::slice::from_raw_parts(job.x_ptr as *const f32, job.dim) };
    let w = unsafe { core::slice::from_raw_parts(job.w_ptr as *const u8, job.w_len) };
    let (start, end) = worker_range(core_id, workers, job.vocab);
    let (tok, val) = argmax_range(x, w, job.dim, start, end, job.is_q8);

    PARTIAL[core_id].store(pack_partial(tok, val), Ordering::Release);
    LAST_SEEN_GEN[core_id].store(gen, Ordering::Release);
    WORKERS_DONE.fetch_add(1, Ordering::AcqRel);
    true
}

/// BSP entry point — parallel replacement for inference::argmax_quant_logits.
/// Falls back to single-core when only the BSP is online.
pub fn argmax_quant_logits_parallel(
    x: &[f32], raw_data: &[u8], dim: usize, vocab_size: usize, is_q8: bool,
) -> (u32, f32) {
    let cpus = crate::arch::x86_64::apic::cpu_count() as usize;
    let workers = cpus.clamp(1, MAX_WORKERS);

    if workers <= 1 || raw_data.is_empty() {
        // Single-core path: identical math, no synchronization overhead.
        let (s, e) = (0usize, vocab_size);
        return argmax_range(x, raw_data, dim, s, e, is_q8);
    }

    crate::serial_println!(
        "[PLOGIT] Parallel argmax: vocab={} dim={} across {} cores",
        vocab_size, dim, workers
    );

    // Publish the job for the APs. Reset accounting first.
    WORKERS_DONE.store(0, Ordering::SeqCst);
    JOB_WORKERS.store(workers as u32, Ordering::SeqCst);
    for slot in PARTIAL.iter().take(workers) {
        slot.store(pack_partial(0, f32::NEG_INFINITY), Ordering::SeqCst);
    }
    unsafe {
        JOB = ArgmaxJob {
            x_ptr: x.as_ptr() as usize,
            w_ptr: raw_data.as_ptr() as usize,
            w_len: raw_data.len(),
            dim,
            vocab: vocab_size,
            is_q8,
        };
    }
    // Release publish: bump generation so APs observe the fully-written JOB.
    JOB_GENERATION.fetch_add(1, Ordering::Release);

    // BSP computes worker 0's slice inline (no IPI needed — APs poll the queue).
    let (start0, end0) = worker_range(0, workers, vocab_size);
    let (tok0, val0) = argmax_range(x, raw_data, dim, start0, end0, is_q8);
    PARTIAL[0].store(pack_partial(tok0, val0), Ordering::Release);
    WORKERS_DONE.fetch_add(1, Ordering::AcqRel);

    // Lock-free barrier: wait for all APs to finish their slices.
    // Bounded spin so a wedged AP can never hang the whole forward pass; if we
    // time out we fold the missing ranges back onto the BSP (correctness > speed).
    let mut spins: u64 = 0;
    const SPIN_LIMIT: u64 = 2_000_000_000;
    while WORKERS_DONE.load(Ordering::Acquire) < workers as u32 {
        core::hint::spin_loop();
        spins += 1;
        if spins >= SPIN_LIMIT {
            crate::serial_println!(
                "[PLOGIT] WARN barrier timeout: {}/{} workers done — BSP folds remainder",
                WORKERS_DONE.load(Ordering::Acquire), workers
            );
            // Recompute any range whose partial is still NEG_INFINITY on the BSP.
            for w in 1..workers {
                let (_, v) = unpack_partial(PARTIAL[w].load(Ordering::Acquire));
                if !v.is_finite() {
                    let (s, e) = worker_range(w, workers, vocab_size);
                    let (t, val) = argmax_range(x, raw_data, dim, s, e, is_q8);
                    PARTIAL[w].store(pack_partial(t, val), Ordering::Release);
                }
            }
            break;
        }
    }

    // Reduce the per-core partials into the global argmax.
    let mut best_tok: u32 = tok0;
    let mut best_val: f32 = val0;
    for w in 0..workers {
        let (t, v) = unpack_partial(PARTIAL[w].load(Ordering::Acquire));
        if v.is_finite() && v > best_val {
            best_val = v;
            best_tok = t;
        }
    }

    crate::serial_println!(
        "[PLOGIT-DONE] argmax=tok{} val_i64={} (parallel, {} cores)",
        best_tok, best_val as i64, workers
    );
    (best_tok, best_val)
}
