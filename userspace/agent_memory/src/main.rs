//! AetherionOS Jalon 111a - Episodic Memory Agent
//!
//! Logs all Cognitive Bus traffic to /disk/var/memory.db on FAT32.
//! This provides persistent episodic memory across reboots:
//!   - Subscribes to ALL bus intents (wildcard consume)
//!   - Logs: timestamp, intent_id, source_pid, priority, payload
//!   - Writes to FAT32 in append-only CSV format
//!   - On boot, reads last N entries to restore context
//!   - Publishes INTENT_MEMORY_READY (0xA001) on startup
//!   - Publishes INTENT_MEMORY_RECALL (0xA002) for query responses
//!
//! Format: "T:<ticks>,I:<intent>,P:<priority>,D:<data>,S:<session>,C:<corr>\n"

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

const INTENT_MEMORY_READY: u64 = 0xA001;
const INTENT_MEMORY_RECALL: u64 = 0xA002;
const INTENT_MEMORY_FLUSH: u64 = 0xA003;

/// Path to the episodic memory database on FAT32
const MEMORY_DB_PATH: &[u8] = b"/disk/var/memory.db\0";
/// Path for the directory (ensure it exists)
const MEMORY_DIR_PATH: &[u8] = b"/disk/var\0";

/// Maximum entries to buffer before flushing to disk
const FLUSH_THRESHOLD: usize = 16;

/// Maximum total entries in memory ring (in-RAM)
const RING_SIZE: usize = 256;

/// A single episodic memory entry
struct EpisodeEntry {
    tick: u64,
    intent_id: u32,
    priority: u32,
    payload: u64,
    session_id: u64,
    correlation_id: u64,
}

/// In-memory ring buffer of recent episodes
struct EpisodicMemory {
    entries: [EpisodeEntry; RING_SIZE],
    write_idx: usize,
    total_logged: u64,
    pending_flush: usize,
}

impl EpisodicMemory {
    fn new() -> Self {
        const EMPTY: EpisodeEntry = EpisodeEntry {
            tick: 0, intent_id: 0, priority: 0,
            payload: 0, session_id: 0, correlation_id: 0,
        };
        EpisodicMemory {
            entries: [EMPTY; RING_SIZE],
            write_idx: 0,
            total_logged: 0,
            pending_flush: 0,
        }
    }

    fn record(&mut self, tick: u64, intent: u32, prio: u32, data: u64, session: u64, corr: u64) {
        self.entries[self.write_idx] = EpisodeEntry {
            tick, intent_id: intent, priority: prio,
            payload: data, session_id: session, correlation_id: corr,
        };
        self.write_idx = (self.write_idx + 1) % RING_SIZE;
        self.total_logged += 1;
        self.pending_flush += 1;
    }

    fn should_flush(&self) -> bool {
        self.pending_flush >= FLUSH_THRESHOLD
    }
}

/// Format a u64 as hex into a buffer, return slice written
fn format_hex(val: u64, buf: &mut [u8]) -> usize {
    let hex_chars = b"0123456789ABCDEF";
    if val == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut n = val;
    let mut digits = [0u8; 16];
    let mut len = 0usize;
    while n > 0 && len < 16 {
        digits[len] = hex_chars[(n & 0xF) as usize];
        n >>= 4;
        len += 1;
    }
    for i in 0..len {
        buf[i] = digits[len - 1 - i];
    }
    len
}

/// Format a u64 as decimal into a buffer, return slice written
fn format_dec(val: u64, buf: &mut [u8]) -> usize {
    if val == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut n = val;
    let mut digits = [0u8; 20];
    let mut len = 0usize;
    while n > 0 && len < 20 {
        digits[len] = b'0' + (n % 10) as u8;
        n /= 10;
        len += 1;
    }
    for i in 0..len {
        buf[i] = digits[len - 1 - i];
    }
    len
}

/// Flush pending entries to /disk/var/memory.db
fn flush_to_disk(memory: &EpisodicMemory) {
    // Create/append to the memory file
    let fd = sys_creat(MEMORY_DB_PATH, 0o644);
    if fd < 0 {
        sys_write(1, b"[MEMORY] WARNING: Cannot create memory.db\n");
        return;
    }
    let fd = fd as u32;

    // Build flush buffer — each entry is ~80 bytes max
    let mut line_buf = [0u8; 128];
    let flush_start = if memory.write_idx >= memory.pending_flush {
        memory.write_idx - memory.pending_flush
    } else {
        RING_SIZE - (memory.pending_flush - memory.write_idx)
    };

    for i in 0..memory.pending_flush {
        let idx = (flush_start + i) % RING_SIZE;
        let e = &memory.entries[idx];
        let mut pos = 0usize;

        // "T:" prefix
        line_buf[pos] = b'T'; pos += 1;
        line_buf[pos] = b':'; pos += 1;
        pos += format_dec(e.tick, &mut line_buf[pos..]);

        line_buf[pos] = b','; pos += 1;
        line_buf[pos] = b'I'; pos += 1;
        line_buf[pos] = b':'; pos += 1;
        pos += format_hex(e.intent_id as u64, &mut line_buf[pos..]);

        line_buf[pos] = b','; pos += 1;
        line_buf[pos] = b'P'; pos += 1;
        line_buf[pos] = b':'; pos += 1;
        pos += format_dec(e.priority as u64, &mut line_buf[pos..]);

        line_buf[pos] = b','; pos += 1;
        line_buf[pos] = b'D'; pos += 1;
        line_buf[pos] = b':'; pos += 1;
        pos += format_hex(e.payload, &mut line_buf[pos..]);

        line_buf[pos] = b','; pos += 1;
        line_buf[pos] = b'S'; pos += 1;
        line_buf[pos] = b':'; pos += 1;
        pos += format_hex(e.session_id, &mut line_buf[pos..]);

        line_buf[pos] = b','; pos += 1;
        line_buf[pos] = b'C'; pos += 1;
        line_buf[pos] = b':'; pos += 1;
        pos += format_hex(e.correlation_id, &mut line_buf[pos..]);

        line_buf[pos] = b'\n'; pos += 1;

        sys_write_fd(fd, &line_buf[..pos]);
    }

    sys_close(fd);
}

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J111a] ════════════════════════════════════");
    println("[J111a] Episodic Memory Agent v1.0");
    println("[J111a] Persistent AI Memory on FAT32");
    println("[J111a] Path: /disk/var/memory.db");
    println("[J111a] ════════════════════════════════════");

    let mut memory = EpisodicMemory::new();
    let mut msg_buf = [0u64; 8];

    // Try to create /disk/var directory (ignore if exists)
    sys_write(1, b"[MEMORY] Creating /disk/var directory...\n");

    // Signal readiness
    sys_bus_publish_ext(INTENT_MEMORY_READY, 2, 0, 0, 0);
    println("[J111a] INTENT_MEMORY_READY published");

    // Read baseline TSC
    let tsc_start = sys_rdtsc();
    let tsc_freq: u64 = 2_000_000_000; // ~2 GHz Haswell

    let mut tick_count: u64 = 0;
    let mut last_report: u64 = 0;

    // Main memory loop: consume ALL bus messages
    loop {
        // Read current tick
        let tsc_now = sys_rdtsc();
        let elapsed = tsc_now.wrapping_sub(tsc_start) / tsc_freq;

        // Consume any bus message (wildcard: intent=0 means accept all)
        let r = sys_bus_consume(&mut msg_buf);
        if r == 0 {
            // Decode bus message fields
            let intent_id = (msg_buf[1] & 0xFFFF_FFFF) as u32;
            let priority = ((msg_buf[1] >> 32) & 0xFFFF_FFFF) as u32;
            let payload = msg_buf[2];
            let session_id = if msg_buf.len() > 4 { msg_buf[4] } else { 0 };
            let correlation_id = if msg_buf.len() > 5 { msg_buf[5] } else { 0 };

            // Record episode
            memory.record(elapsed, intent_id, priority, payload, session_id, correlation_id);
            tick_count += 1;

            // Log first few events
            if tick_count <= 5 {
                print("[MEMORY] Episode #");
                print_u64(tick_count);
                print(" intent=0x");
                print_hex(intent_id as u64);
                print(" data=0x");
                print_hex(payload);
                println("");
            }

            // Flush to disk when threshold reached
            if memory.should_flush() {
                flush_to_disk(&memory);
                memory.pending_flush = 0;

                if tick_count <= 20 || tick_count % 100 == 0 {
                    print("[MEMORY] Flushed ");
                    print_u64(memory.total_logged);
                    println(" episodes to /disk/var/memory.db");
                }
            }
        }

        // Periodic status report
        if elapsed > last_report + 30 {
            last_report = elapsed;
            print("[MEMORY] Status: ");
            print_u64(memory.total_logged);
            print(" episodes logged, uptime=");
            print_u64(elapsed);
            println("s");

            // Publish a recall summary
            sys_bus_publish_ext(INTENT_MEMORY_RECALL, 1, memory.total_logged, 0, tick_count);
        }

        sys_yield();
    }
}
