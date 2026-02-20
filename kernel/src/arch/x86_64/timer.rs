// arch/x86_64/timer.rs - Time Stamp Counter (TSC) pour benchmarks ACHA

/// Lit le Time Stamp Counter (cycles CPU depuis boot)
/// 
/// # Safety
/// RDTSC est sûr en ring 0 (kernel)
#[inline]
pub fn read_tsc() -> u64 {
    let low: u32;
    let high: u32;
    
    unsafe {
        core::arch::asm!(
            "rdtsc",
            out("eax") low,
            out("edx") high,
            options(nomem, nostack)
        );
    }
    
    ((high as u64) << 32) | (low as u64)
}

/// Attend un nombre de cycles (busy-wait, pour tests)
#[inline]
pub fn wait_cycles(cycles: u64) {
    let start = read_tsc();
    while read_tsc().wrapping_sub(start) < cycles {
        core::hint::spin_loop();
    }
}

/// Convertit cycles en microsecondes (approximation ~3GHz)
/// Note: À calibrer pour le CPU réel
#[inline]
pub fn cycles_to_us(cycles: u64) -> u64 {
    // Approximation: 3 GHz = 3 cycles/ns = 3000 cycles/us
    cycles / 3000
}

/// Mesure le temps d'exécution d'une fonction en cycles
#[inline]
pub fn measure_cycles<F: FnOnce()>(f: F) -> u64 {
    let start = read_tsc();
    f();
    read_tsc().wrapping_sub(start)
}
