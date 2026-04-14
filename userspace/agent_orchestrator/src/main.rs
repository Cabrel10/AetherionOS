//! AetherionOS Jalon 125 — Agent Orchestrateur (Thalamus + Hippocampe + Pipe Cognitif)
//!
//! Chapitre ACHA 3.2 (Thalamus) + 3.3 (Hippocampe) + 3.5 (World Connection):
//!   - Ecoute INTENT_USER_PROMPT (0x8001) sur le Cognitive Bus
//!   - Memoire Reflexe O(1): dictionnaire hash -> action immediate (<1ms)
//!   - Routage intelligent: questions connues -> reponse directe,
//!     questions complexes -> reveil du LLM via INTENT_LLM_WAKEUP
//!   - Network queries -> delegate to HTTP Agent via INTENT_HTTP_REQUEST (0xB002)
//!   - Consumes INTENT_API_RESPONSE (0xB001) for HTTP results
//!   - Jalon 118: Pipe Cognitif — consumes INTENT_PROCESS_OUTPUT (0xC001)
//!     from child processes and routes to LLM or memory reflex
//!   - Jalon 120: Learned reflexes — stores answer hashes for O(1) recall
//!
//! Intents:
//!   IN:  0x8001 INTENT_USER_PROMPT    — requete utilisateur (hash du prompt)
//!   IN:  0xB001 INTENT_API_RESPONSE   — reponse HTTP de l'Agent HTTP
//!   IN:  0x8010 INTENT_LLM_OUTPUT     — sortie LLM pour synthese
//!   IN:  0xC001 INTENT_PROCESS_OUTPUT — stdout d'un processus enfant (Pipe Cognitif)
//!   IN:  0xC002 INTENT_PROCESS_EXIT   — notification de fin de processus enfant
//!   OUT: 0x8002 INTENT_REFLEX_HIT     — reponse reflexe trouvee
//!   OUT: 0x8003 INTENT_LLM_WAKEUP     — LLM requis pour question complexe
//!   OUT: 0x8004 INTENT_ORCH_READY     — orchestrateur pret
//!   OUT: 0x8005 INTENT_ORCH_RESPONSE  — reponse de l'orchestrateur
//!   OUT: 0xB002 INTENT_HTTP_REQUEST   — requete HTTP a l'Agent HTTP

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

// =====================================================
// Cognitive Bus Intent IDs
// =====================================================
const INTENT_USER_PROMPT: u32     = 0x8001;
const INTENT_REFLEX_HIT: u64      = 0x8002;
const INTENT_LLM_WAKEUP: u64     = 0x8003;
const INTENT_ORCH_READY: u64      = 0x8004;
const INTENT_ORCH_RESPONSE: u64   = 0x8005;
const INTENT_LLM_OUTPUT: u32      = 0x8010;
const INTENT_API_RESPONSE: u32    = 0xB001;
const INTENT_HTTP_REQUEST: u64    = 0xB002;
#[allow(dead_code)]
const INTENT_HTTP_READY: u32      = 0xB003;
#[allow(dead_code)]
const INTENT_NET_QUERY: u64       = 0xB004;

// Jalon 118: Pipe Cognitif intents
const INTENT_PROCESS_OUTPUT: u32  = 0xC001;
const INTENT_PROCESS_EXIT: u32    = 0xC002;

// =====================================================
// Hippocampe: Memoire Reflexe O(1)
// =====================================================

#[derive(Clone, Copy)]
struct ReflexEntry {
    hash: u64,
    action_code: u32,
    confidence: u8,
}

// Action codes — system commands
const ACTION_SYS_TIME: u32       = 1;
const ACTION_SYS_UPTIME: u32     = 2;
const ACTION_SYS_VERSION: u32    = 3;
const ACTION_SYS_HELP: u32       = 4;
const ACTION_SYS_MEMORY: u32     = 5;
const ACTION_NET_STATUS: u32     = 6;
const ACTION_ERROR_404: u32      = 7;
const ACTION_ERROR_OOM: u32      = 8;
const ACTION_HELLO: u32          = 9;
const ACTION_WHO_ARE_YOU: u32    = 10;
const ACTION_REBOOT: u32         = 11;
const ACTION_SHUTDOWN: u32       = 12;
const ACTION_BTC_PRICE: u32      = 20;
const ACTION_WEATHER: u32        = 21;
const ACTION_API_STATUS: u32     = 22;
#[allow(dead_code)]
const ACTION_FETCH_URL: u32      = 23;

// Jalon 120: Learned reflex action code
const ACTION_LEARNED: u32        = 100;

/// DJB2 hash function
fn djb2_hash(input: &[u8]) -> u64 {
    let mut hash: u64 = 5381;
    for &b in input {
        let c = if b >= b'A' && b <= b'Z' { b + 32 } else { b };
        hash = hash.wrapping_mul(33).wrapping_add(c as u64);
    }
    hash
}

/// The Reflex Memory table (Hippocampe) — expanded for learned reflexes
const REFLEX_TABLE_SIZE: usize = 64;

struct ReflexMemory {
    entries: [ReflexEntry; REFLEX_TABLE_SIZE],
    count: usize,
    /// Jalon 120: Count of dynamically-learned reflexes
    learned_count: usize,
}

impl ReflexMemory {
    fn new() -> Self {
        let empty = ReflexEntry { hash: 0, action_code: 0, confidence: 0 };
        let mut mem = ReflexMemory {
            entries: [empty; REFLEX_TABLE_SIZE],
            count: 0,
            learned_count: 0,
        };

        // ── Static reflexes (system + social + network) ──
        mem.add(b"quelle heure est-il ?", ACTION_SYS_TIME, 99);
        mem.add(b"quelle heure est-il", ACTION_SYS_TIME, 99);
        mem.add(b"heure", ACTION_SYS_TIME, 85);
        mem.add(b"what time is it", ACTION_SYS_TIME, 99);
        mem.add(b"what time is it?", ACTION_SYS_TIME, 99);
        mem.add(b"uptime", ACTION_SYS_UPTIME, 95);
        mem.add(b"version", ACTION_SYS_VERSION, 95);
        mem.add(b"help", ACTION_SYS_HELP, 99);
        mem.add(b"aide", ACTION_SYS_HELP, 99);
        mem.add(b"memory", ACTION_SYS_MEMORY, 90);
        mem.add(b"free", ACTION_SYS_MEMORY, 85);
        mem.add(b"network status", ACTION_NET_STATUS, 90);
        mem.add(b"erreur 404 nginx", ACTION_ERROR_404, 95);
        mem.add(b"error 404", ACTION_ERROR_404, 90);
        mem.add(b"out of memory", ACTION_ERROR_OOM, 95);
        mem.add(b"oom killer", ACTION_ERROR_OOM, 90);
        mem.add(b"hello", ACTION_HELLO, 99);
        mem.add(b"bonjour", ACTION_HELLO, 99);
        mem.add(b"who are you", ACTION_WHO_ARE_YOU, 99);
        mem.add(b"qui es-tu", ACTION_WHO_ARE_YOU, 99);
        mem.add(b"reboot", ACTION_REBOOT, 95);
        mem.add(b"shutdown", ACTION_SHUTDOWN, 95);
        mem.add(b"bitcoin price", ACTION_BTC_PRICE, 95);
        mem.add(b"btc price", ACTION_BTC_PRICE, 95);
        mem.add(b"prix bitcoin", ACTION_BTC_PRICE, 95);
        mem.add(b"crypto", ACTION_BTC_PRICE, 80);
        mem.add(b"weather", ACTION_WEATHER, 90);
        mem.add(b"meteo", ACTION_WEATHER, 90);
        mem.add(b"api status", ACTION_API_STATUS, 95);
        mem.add(b"http test", ACTION_API_STATUS, 90);

        // ── Jalon 120: First learned reflex — 2+2=? → "4" ──
        // This demonstrates O(1) recall of learned answers
        mem.learn(b"2+2=?", b"4");
        mem.learn(b"2+2", b"4");
        mem.learn(b"what is 2+2", b"4");
        mem.learn(b"what is 2+2?", b"4");

        mem
    }

    fn add(&mut self, query: &[u8], action: u32, confidence: u8) {
        if self.count >= REFLEX_TABLE_SIZE { return; }
        self.entries[self.count] = ReflexEntry {
            hash: djb2_hash(query),
            action_code: action,
            confidence,
        };
        self.count += 1;
    }

    /// Jalon 120: Learn a new reflex mapping query_hash → action
    /// Stores the answer hash in the entry's action_code field.
    fn learn(&mut self, query: &[u8], _answer: &[u8]) {
        if self.count >= REFLEX_TABLE_SIZE { return; }
        self.entries[self.count] = ReflexEntry {
            hash: djb2_hash(query),
            action_code: ACTION_LEARNED,
            confidence: 100,
        };
        self.count += 1;
        self.learned_count += 1;
    }

    fn lookup(&self, query_hash: u64, threshold: u8) -> Option<&ReflexEntry> {
        for i in 0..self.count {
            if self.entries[i].hash == query_hash && self.entries[i].confidence >= threshold {
                return Some(&self.entries[i]);
            }
        }
        None
    }
}

// =====================================================
// Jalon 120: Learned Answer Store — O(1) recall
// =====================================================
// Maps hash(question) → answer string (up to 32 bytes)
// This is the first "memory" of the AI: observed answers
// are stored for instant recall without LLM invocation.

const MAX_LEARNED_ANSWERS: usize = 32;

struct LearnedAnswerStore {
    keys: [u64; MAX_LEARNED_ANSWERS],
    values: [[u8; 32]; MAX_LEARNED_ANSWERS],
    lengths: [u8; MAX_LEARNED_ANSWERS],
    count: usize,
}

impl LearnedAnswerStore {
    fn new() -> Self {
        LearnedAnswerStore {
            keys: [0u64; MAX_LEARNED_ANSWERS],
            values: [[0u8; 32]; MAX_LEARNED_ANSWERS],
            lengths: [0u8; MAX_LEARNED_ANSWERS],
            count: 0,
        }
    }

    fn store(&mut self, key_hash: u64, answer: &[u8]) {
        // Check if already stored
        for i in 0..self.count {
            if self.keys[i] == key_hash { return; } // Already learned
        }
        if self.count >= MAX_LEARNED_ANSWERS { return; }

        self.keys[self.count] = key_hash;
        let len = if answer.len() > 31 { 31 } else { answer.len() };
        let mut j = 0;
        while j < len {
            self.values[self.count][j] = answer[j];
            j += 1;
        }
        self.lengths[self.count] = len as u8;
        self.count += 1;
    }

    fn recall(&self, key_hash: u64) -> Option<&[u8]> {
        for i in 0..self.count {
            if self.keys[i] == key_hash {
                let len = self.lengths[i] as usize;
                return Some(&self.values[i][..len]);
            }
        }
        None
    }
}

// =====================================================
// Action executor — handles Reflex responses
// =====================================================

fn is_network_action(action: u32) -> bool {
    action >= 20 && action <= 29
}

fn execute_reflex(action: u32, answers: &LearnedAnswerStore, query_hash: u64) {
    match action {
        ACTION_SYS_TIME => {
            let tsc = sys_rdtsc();
            print("[ORCH] Reflex: sys_time -> TSC=");
            print_u64(tsc);
            println("");
        },
        ACTION_SYS_UPTIME => {
            let tsc = sys_rdtsc();
            print("[ORCH] Reflex: uptime -> ");
            print_u64(tsc / 1_000_000);
            println(" Mcycles since boot");
        },
        ACTION_SYS_VERSION => {
            println("[ORCH] Reflex: AetherionOS v4.0.0-j125-python-ready");
        },
        ACTION_SYS_HELP => {
            println("[ORCH] Reflex: Commands: help, version, uptime, memory, python <script>");
        },
        ACTION_SYS_MEMORY => {
            println("[ORCH] Reflex: Memory status -> heap allocator active, demand paging OK");
        },
        ACTION_NET_STATUS => {
            println("[ORCH] Reflex: Network -> HTTP Agent active, TCP/IP sockets wired");
        },
        ACTION_ERROR_404 => {
            println("[ORCH] Reflex: 404 Fix -> Check nginx config");
        },
        ACTION_ERROR_OOM => {
            println("[ORCH] Reflex: OOM Fix -> Increase swap: fallocate -l 4G /swapfile");
        },
        ACTION_HELLO => {
            println("[ORCH] Reflex: Hello! I am the AetherionOS Thalamus v4.0.");
            println("[ORCH]         Pipe Cognitif active: child stdout -> AI pipeline.");
        },
        ACTION_WHO_ARE_YOU => {
            println("[ORCH] Reflex: I am the Thalamus, the central routing agent of AetherionOS.");
        },
        ACTION_REBOOT => {
            println("[ORCH] Reflex: Reboot requested (not implemented in Ring 3)");
        },
        ACTION_SHUTDOWN => {
            println("[ORCH] Reflex: Shutdown requested (not implemented in Ring 3)");
        },
        // Jalon 120: Learned reflex — recall answer from memory
        ACTION_LEARNED => {
            if let Some(answer) = answers.recall(query_hash) {
                print("[ORCH] LEARNED REFLEX O(1): answer = \"");
                sys_write(1, answer);
                println("\"");
            } else {
                println("[ORCH] LEARNED REFLEX: entry found but answer not in store");
            }
        },
        // Network actions
        ACTION_BTC_PRICE => {
            println("[ORCH] Network: Bitcoin price -> delegating to HTTP Agent");
            sys_bus_publish(INTENT_HTTP_REQUEST, 2, ACTION_BTC_PRICE as u64);
        },
        ACTION_WEATHER => {
            println("[ORCH] Network: Weather -> delegating to HTTP Agent");
            sys_bus_publish(INTENT_HTTP_REQUEST, 2, ACTION_WEATHER as u64);
        },
        ACTION_API_STATUS => {
            println("[ORCH] Network: API health check -> delegating to HTTP Agent");
            sys_bus_publish(INTENT_HTTP_REQUEST, 2, ACTION_API_STATUS as u64);
        },
        _ => {
            print("[ORCH] Reflex: Unknown action code ");
            print_u64(action as u64);
            println("");
        }
    }
}

// =====================================================
// HTTP Response Handler
// =====================================================

fn handle_api_response(payload: u64) {
    let status_code = (payload & 0xFFFF) as u32;
    let query_type = ((payload >> 16) & 0xFFFF) as u32;

    print("[ORCH] API Response: HTTP ");
    print_u64(status_code as u64);

    if status_code == 200 {
        println(" OK");
        match query_type {
            20 => {
                println("[ORCH] BTC price data received from API");
                sys_bus_publish(INTENT_LLM_WAKEUP, 3, djb2_hash(b"summarize btc"));
            },
            21 => {
                println("[ORCH] Weather data received from API");
                sys_bus_publish(INTENT_LLM_WAKEUP, 3, djb2_hash(b"summarize weather"));
            },
            _ => println("[ORCH] Generic API response processed"),
        }
    } else {
        println(" (non-200)");
    }
    sys_bus_publish(INTENT_ORCH_RESPONSE, 2, payload);
}

// =====================================================
// Jalon 118: Pipe Cognitif Handler
// =====================================================

fn handle_process_output(payload: u64, proc_output_count: &mut u64) {
    let child_pid = payload >> 32;
    let hash_lo = payload & 0xFFFF_FFFF;
    *proc_output_count += 1;

    // Log only every 500 events to avoid serial flood
    if *proc_output_count % 500 == 1 {
        print("[PIPE-COGNITIF] PID=");
        print_u64(child_pid);
        print(" events=");
        print_u64(*proc_output_count);
        print(" hash=0x");
        print_u64(hash_lo);
        println("");
    }

    // Route to LLM for analysis if this is a substantial output
    if *proc_output_count <= 3 {
        sys_bus_publish(INTENT_LLM_WAKEUP, 2, payload);
    }
}

fn handle_process_exit(payload: u64) {
    let child_pid = payload >> 32;
    let exit_code = (payload & 0xFFFF) as i32;
    print("[PIPE-COGNITIF] Process PID ");
    print_u64(child_pid);
    print(" exited with code ");
    print_u64(exit_code as u64);
    println("");

    if exit_code == 0 {
        println("[PIPE-COGNITIF] Child process completed successfully");
    } else {
        println("[PIPE-COGNITIF] Child process failed — routing to LLM for error analysis");
        sys_bus_publish(INTENT_LLM_WAKEUP, 3, payload);
    }
}

// =====================================================
// Confidence scoring
// =====================================================
const CONFIDENCE_THRESHOLD: u8 = 70;

// =====================================================
// MAIN: Thalamus Event Loop v4.0 (with Pipe Cognitif + Learned Reflexes)
// =====================================================
#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("============================================================");
    println("[ORCH] AetherionOS Thalamus Orchestrator v4.0 (Jalon 125)");
    println("[ORCH] ACHA Ch.3.2 (Thalamus) + Ch.3.3 (Hippocampe)");
    println("[ORCH]   + Jalon 118: Pipe Cognitif (child stdout -> AI)");
    println("[ORCH]   + Jalon 120: Learned Reflexes O(1)");
    println("[ORCH]   + Jalon 125: Python/Node Interpreter Support");
    println("============================================================");

    // Initialize Reflex Memory (Hippocampe)
    let reflex = ReflexMemory::new();
    print("[ORCH] Hippocampe: ");
    print_u64(reflex.count as u64);
    print(" reflex entries (");
    print_u64(reflex.learned_count as u64);
    println(" learned)");

    // Initialize Learned Answer Store (Jalon 120)
    let mut answers = LearnedAnswerStore::new();
    // Pre-populate the first learned reflex: 2+2=? → "4"
    answers.store(djb2_hash(b"2+2=?"), b"4");
    answers.store(djb2_hash(b"2+2"), b"4");
    answers.store(djb2_hash(b"what is 2+2"), b"4");
    answers.store(djb2_hash(b"what is 2+2?"), b"4");
    print("[ORCH] Learned Answer Store: ");
    print_u64(answers.count as u64);
    println(" entries (Jalon 120: 2+2=? -> 4)");

    print("[ORCH] Confidence threshold: ");
    print_u64(CONFIDENCE_THRESHOLD as u64);
    println("%");
    println("[ORCH] Routing: Reflex | HTTP | LLM | PipeCognitif");

    // Signal readiness
    sys_bus_publish(INTENT_ORCH_READY, 2, reflex.count as u64);
    println("[ORCH] Published INTENT_ORCH_READY (0x8004) on Cognitive Bus");
    println("[ORCH] Listening for:");
    println("[ORCH]   - INTENT_USER_PROMPT     (0x8001)");
    println("[ORCH]   - INTENT_API_RESPONSE    (0xB001)");
    println("[ORCH]   - INTENT_LLM_OUTPUT      (0x8010)");
    println("[ORCH]   - INTENT_PROCESS_OUTPUT  (0xC001) [Pipe Cognitif]");
    println("[ORCH]   - INTENT_PROCESS_EXIT    (0xC002)");

    let mut msg_buf: [u64; 8] = [0; 8];
    let mut api_buf: [u64; 8] = [0; 8];
    let mut total_queries: u64 = 0;
    let mut reflex_hits: u64 = 0;
    let mut llm_routes: u64 = 0;
    let mut http_routes: u64 = 0;
    let mut pipe_outputs: u64 = 0;

    let mut idle_cycles: u64 = 0;
    let max_idle: u64 = 5_000_000;

    loop {
        // ── Check for user prompts ──
        let result = sys_bus_consume_intent(&mut msg_buf, INTENT_USER_PROMPT);

        if result == 0 {
            let prompt_hash = msg_buf[4];
            idle_cycles = 0;
            total_queries += 1;

            print("[ORCH] Received INTENT_USER_PROMPT #");
            print_u64(total_queries);
            print(", hash=0x");
            print_u64(prompt_hash);
            println("");

            let t0 = sys_rdtsc();

            match reflex.lookup(prompt_hash, CONFIDENCE_THRESHOLD) {
                Some(entry) => {
                    let cycles = sys_rdtsc() - t0;

                    if is_network_action(entry.action_code) {
                        http_routes += 1;
                        print("[ORCH] NETWORK ROUTE! action=");
                        print_u64(entry.action_code as u64);
                        print(", latency=");
                        print_u64(cycles);
                        println(" cycles");
                        execute_reflex(entry.action_code, &answers, prompt_hash);
                        sys_bus_publish(INTENT_ORCH_RESPONSE, 2, prompt_hash);
                    } else {
                        reflex_hits += 1;
                        print("[ORCH] REFLEX HIT! action=");
                        print_u64(entry.action_code as u64);
                        print(", confidence=");
                        print_u64(entry.confidence as u64);
                        print("%, latency=");
                        print_u64(cycles);
                        println(" cycles");
                        execute_reflex(entry.action_code, &answers, prompt_hash);
                        sys_bus_publish(INTENT_REFLEX_HIT, 2,
                            ((entry.action_code as u64) << 32) | (entry.confidence as u64));
                        sys_bus_publish(INTENT_ORCH_RESPONSE, 2, prompt_hash);
                    }
                },
                None => {
                    llm_routes += 1;
                    let cycles = sys_rdtsc() - t0;
                    print("[ORCH] No reflex match. Routing to LLM (");
                    print_u64(cycles);
                    println(" cycles lookup)");
                    sys_bus_publish(INTENT_LLM_WAKEUP, 3, prompt_hash);
                }
            }

            // Stats
            print("[ORCH] Stats: total=");
            print_u64(total_queries);
            print(", reflexes=");
            print_u64(reflex_hits);
            print(", http=");
            print_u64(http_routes);
            print(", llm=");
            print_u64(llm_routes);
            print(", pipe=");
            print_u64(pipe_outputs);
            println("");
        } else {
            // ── Jalon 118: Pipe Cognitif — check for child process output ──
            let pipe_result = sys_bus_consume_intent(&mut api_buf, INTENT_PROCESS_OUTPUT);
            if pipe_result == 0 {
                idle_cycles = 0;
                handle_process_output(api_buf[4], &mut pipe_outputs);
            }

            // ── Jalon 118: Check for child process exit ──
            let exit_result = sys_bus_consume_intent(&mut api_buf, INTENT_PROCESS_EXIT);
            if exit_result == 0 {
                idle_cycles = 0;
                handle_process_exit(api_buf[4]);
            }

            // ── Check for API responses ──
            let api_result = sys_bus_consume_intent(&mut api_buf, INTENT_API_RESPONSE);
            if api_result == 0 {
                idle_cycles = 0;
                println("[ORCH] Received INTENT_API_RESPONSE (0xB001)");
                handle_api_response(api_buf[4]);
            }

            // ── Check for LLM output ──
            let llm_result = sys_bus_consume_intent(&mut api_buf, INTENT_LLM_OUTPUT);
            if llm_result == 0 {
                idle_cycles = 0;
                println("[ORCH] Received INTENT_LLM_OUTPUT (0x8010)");
                println("[ORCH] LLM synthesis complete, publishing final response");
                sys_bus_publish(INTENT_ORCH_RESPONSE, 1, api_buf[4]);
            }

            idle_cycles += 1;
            sys_yield();

            if idle_cycles >= max_idle {
                if total_queries > 0 || pipe_outputs > 0 {
                    println("[ORCH] ========================================");
                    print("[ORCH] Session: ");
                    print_u64(total_queries);
                    print(" queries, ");
                    print_u64(reflex_hits);
                    print(" reflexes, ");
                    print_u64(pipe_outputs);
                    println(" pipe outputs");
                    println("[ORCH] ========================================");
                }
                idle_cycles = 0;
            }
        }
    }
}
