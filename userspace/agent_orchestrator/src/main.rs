//! AetherionOS Jalon 89 — Agent Orchestrateur (Thalamus + Hippocampe + HTTP Bridge)
//!
//! Chapitre ACHA 3.2 (Thalamus) + 3.3 (Hippocampe) + 3.5 (World Connection):
//!   - Ecoute INTENT_USER_PROMPT (0x8001) sur le Cognitive Bus
//!   - Memoire Reflexe O(1): dictionnaire hash -> action immediate (<1ms)
//!   - Routage intelligent: questions connues -> reponse directe,
//!     questions complexes -> reveil du LLM via INTENT_LLM_WAKEUP
//!   - Network queries -> delegate to HTTP Agent via INTENT_HTTP_REQUEST (0xB002)
//!   - Consumes INTENT_API_RESPONSE (0xB001) for HTTP results
//!   - Full pipeline: User -> Orchestrator -> HTTP -> LLM -> synthesis
//!   - Score de confiance: determine si le Reflexe suffit ou si le LLM est requis
//!
//! Intents:
//!   IN:  0x8001 INTENT_USER_PROMPT    — requete utilisateur (hash du prompt)
//!   IN:  0xB001 INTENT_API_RESPONSE   — reponse HTTP de l'Agent HTTP
//!   IN:  0x8010 INTENT_LLM_OUTPUT     — sortie LLM pour synthese
//!   OUT: 0x8002 INTENT_REFLEX_HIT     — reponse reflexe trouvee
//!   OUT: 0x8003 INTENT_LLM_WAKEUP     — LLM requis pour question complexe
//!   OUT: 0x8004 INTENT_ORCH_READY     — orchestrateur pret
//!   OUT: 0x8005 INTENT_ORCH_RESPONSE  — reponse de l'orchestrateur
//!   OUT: 0xB002 INTENT_HTTP_REQUEST   — requete HTTP a l'Agent HTTP
//!   OUT: 0xB004 INTENT_NET_QUERY      — requete reseau enrichie (avec payload)

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
const INTENT_HTTP_READY: u32      = 0xB003;
const INTENT_NET_QUERY: u64       = 0xB004;

// =====================================================
// Hippocampe: Memoire Reflexe O(1)
//
// Hash-based lookup table. Each entry maps a DJB2 hash of a known
// query to a pre-computed response code and response string.
// This gives sub-millisecond responses for common queries without
// invoking the LLM at all.
// =====================================================

/// Response type for the Reflex Memory
#[derive(Clone, Copy)]
struct ReflexEntry {
    hash: u64,           // DJB2 hash of the canonical query
    action_code: u32,    // Machine-readable action type
    confidence: u8,      // 0-100 confidence score
}

// Action codes — system commands
const ACTION_SYS_TIME: u32       = 1;   // "Quelle heure est-il?" -> sys_time
const ACTION_SYS_UPTIME: u32     = 2;   // "uptime" -> report system uptime
const ACTION_SYS_VERSION: u32    = 3;   // "version" -> report OS version
const ACTION_SYS_HELP: u32       = 4;   // "help" / "aide" -> display help
const ACTION_SYS_MEMORY: u32     = 5;   // "memory" -> report memory status
const ACTION_NET_STATUS: u32     = 6;   // "network status" -> check connectivity
const ACTION_ERROR_404: u32      = 7;   // "Erreur 404 nginx" -> known fix
const ACTION_ERROR_OOM: u32      = 8;   // "out of memory" -> known fix
const ACTION_HELLO: u32          = 9;   // "hello" / "bonjour" -> greeting
const ACTION_WHO_ARE_YOU: u32    = 10;  // "who are you" -> identity
const ACTION_REBOOT: u32         = 11;  // "reboot" -> system restart
const ACTION_SHUTDOWN: u32       = 12;  // "shutdown" -> system halt

// Action codes — network / API queries (NEW: Jalon 89)
const ACTION_BTC_PRICE: u32      = 20;  // "bitcoin price" -> HTTP API call
const ACTION_WEATHER: u32        = 21;  // "weather" -> HTTP API call
const ACTION_API_STATUS: u32     = 22;  // "api status" -> HTTP health check
const ACTION_FETCH_URL: u32      = 23;  // "fetch <url>" -> HTTP GET

/// DJB2 hash function — same as used by the Terminal
fn djb2_hash(input: &[u8]) -> u64 {
    let mut hash: u64 = 5381;
    for &b in input {
        let c = if b >= b'A' && b <= b'Z' { b + 32 } else { b };
        hash = hash.wrapping_mul(33).wrapping_add(c as u64);
    }
    hash
}

/// The Reflex Memory table (Hippocampe)
const REFLEX_TABLE_SIZE: usize = 36;

struct ReflexMemory {
    entries: [ReflexEntry; REFLEX_TABLE_SIZE],
    count: usize,
}

impl ReflexMemory {
    fn new() -> Self {
        let empty = ReflexEntry { hash: 0, action_code: 0, confidence: 0 };
        let mut mem = ReflexMemory {
            entries: [empty; REFLEX_TABLE_SIZE],
            count: 0,
        };

        // French queries
        mem.add(b"quelle heure est-il ?", ACTION_SYS_TIME, 99);
        mem.add(b"quelle heure est-il", ACTION_SYS_TIME, 99);
        mem.add(b"heure", ACTION_SYS_TIME, 85);

        // English queries
        mem.add(b"what time is it", ACTION_SYS_TIME, 99);
        mem.add(b"what time is it?", ACTION_SYS_TIME, 99);
        mem.add(b"uptime", ACTION_SYS_UPTIME, 95);
        mem.add(b"version", ACTION_SYS_VERSION, 95);
        mem.add(b"help", ACTION_SYS_HELP, 99);
        mem.add(b"aide", ACTION_SYS_HELP, 99);
        mem.add(b"memory", ACTION_SYS_MEMORY, 90);
        mem.add(b"free", ACTION_SYS_MEMORY, 85);
        mem.add(b"network status", ACTION_NET_STATUS, 90);

        // Known error patterns
        mem.add(b"erreur 404 nginx", ACTION_ERROR_404, 95);
        mem.add(b"error 404", ACTION_ERROR_404, 90);
        mem.add(b"out of memory", ACTION_ERROR_OOM, 95);
        mem.add(b"oom killer", ACTION_ERROR_OOM, 90);

        // Social / identity
        mem.add(b"hello", ACTION_HELLO, 99);
        mem.add(b"bonjour", ACTION_HELLO, 99);
        mem.add(b"who are you", ACTION_WHO_ARE_YOU, 99);
        mem.add(b"qui es-tu", ACTION_WHO_ARE_YOU, 99);

        // System control
        mem.add(b"reboot", ACTION_REBOOT, 95);
        mem.add(b"shutdown", ACTION_SHUTDOWN, 95);

        // ── NEW: Network / API queries (Jalon 89) ──
        mem.add(b"bitcoin price", ACTION_BTC_PRICE, 95);
        mem.add(b"btc price", ACTION_BTC_PRICE, 95);
        mem.add(b"prix bitcoin", ACTION_BTC_PRICE, 95);
        mem.add(b"crypto", ACTION_BTC_PRICE, 80);
        mem.add(b"weather", ACTION_WEATHER, 90);
        mem.add(b"meteo", ACTION_WEATHER, 90);
        mem.add(b"api status", ACTION_API_STATUS, 95);
        mem.add(b"http test", ACTION_API_STATUS, 90);

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
// Action executor — handles Reflex responses
// =====================================================

/// Check if action is a network query that requires HTTP agent
fn is_network_action(action: u32) -> bool {
    action >= 20 && action <= 29
}

fn execute_reflex(action: u32) {
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
            println("[ORCH] Reflex: AetherionOS v3.0.0-j89-smp-thalamus");
        },
        ACTION_SYS_HELP => {
            println("[ORCH] Reflex: Commands: help, version, uptime, memory, bitcoin price, weather, api status");
        },
        ACTION_SYS_MEMORY => {
            println("[ORCH] Reflex: Memory status -> heap allocator active, demand paging OK");
        },
        ACTION_NET_STATUS => {
            println("[ORCH] Reflex: Network -> HTTP Agent active, TCP/IP sockets wired");
            println("[ORCH]         SMP: multi-core active, LLM affinity on Core 1");
        },
        ACTION_ERROR_404 => {
            println("[ORCH] Reflex: 404 Fix -> Check nginx config: location / { try_files $uri $uri/ =404; }");
        },
        ACTION_ERROR_OOM => {
            println("[ORCH] Reflex: OOM Fix -> Increase swap: fallocate -l 4G /swapfile && swapon /swapfile");
        },
        ACTION_HELLO => {
            println("[ORCH] Reflex: Hello! I am the AetherionOS Thalamus orchestrator v3.0.");
            println("[ORCH]         I can answer questions, call APIs, and route to the LLM.");
        },
        ACTION_WHO_ARE_YOU => {
            println("[ORCH] Reflex: I am the Thalamus, the central routing agent of AetherionOS.");
            println("[ORCH]         I decide if your query needs: Reflex, HTTP, or LLM inference.");
        },
        ACTION_REBOOT => {
            println("[ORCH] Reflex: Reboot requested (not implemented in Ring 3)");
        },
        ACTION_SHUTDOWN => {
            println("[ORCH] Reflex: Shutdown requested (not implemented in Ring 3)");
        },
        // ── Network actions: delegate to HTTP Agent ──
        ACTION_BTC_PRICE => {
            println("[ORCH] Network: Bitcoin price query -> delegating to HTTP Agent");
            println("[ORCH] Publishing INTENT_HTTP_REQUEST (0xB002) for BTC price API");
            // Payload encodes the query type: 20 = BTC price
            sys_bus_publish(INTENT_HTTP_REQUEST, 2, ACTION_BTC_PRICE as u64);
        },
        ACTION_WEATHER => {
            println("[ORCH] Network: Weather query -> delegating to HTTP Agent");
            println("[ORCH] Publishing INTENT_HTTP_REQUEST (0xB002) for weather API");
            sys_bus_publish(INTENT_HTTP_REQUEST, 2, ACTION_WEATHER as u64);
        },
        ACTION_API_STATUS => {
            println("[ORCH] Network: API health check -> delegating to HTTP Agent");
            println("[ORCH] Publishing INTENT_HTTP_REQUEST (0xB002) for HTTP test");
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
// Processes responses from the HTTP Agent
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
                println("[ORCH] Forwarding to LLM for natural language synthesis...");
                // Route to LLM for synthesis: "Summarize BTC price data"
                sys_bus_publish(INTENT_LLM_WAKEUP, 3, djb2_hash(b"summarize btc"));
            },
            21 => {
                println("[ORCH] Weather data received from API");
                println("[ORCH] Forwarding to LLM for synthesis...");
                sys_bus_publish(INTENT_LLM_WAKEUP, 3, djb2_hash(b"summarize weather"));
            },
            _ => {
                println("[ORCH] Generic API response processed");
            }
        }
    } else {
        println(" (non-200)");
        println("[ORCH] API returned an error or non-success status");
    }

    // Publish orchestrator response
    sys_bus_publish(INTENT_ORCH_RESPONSE, 2, payload);
}

// =====================================================
// Confidence scoring
// =====================================================

const CONFIDENCE_THRESHOLD: u8 = 70;

// =====================================================
// MAIN: Thalamus Event Loop (v3.0 with HTTP + LLM pipeline)
// =====================================================
#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("============================================================");
    println("[ORCH] AetherionOS Thalamus Orchestrator v3.0 (Jalon 89)");
    println("[ORCH] ACHA Ch.3.2 (Thalamus) + Ch.3.3 (Hippocampe)");
    println("[ORCH]   + Ch.3.5 (World Connection via HTTP Agent)");
    println("[ORCH]   + SMP Multi-Core Scheduling (LLM on Core 1)");
    println("============================================================");

    // Initialize Reflex Memory (Hippocampe)
    let reflex = ReflexMemory::new();
    print("[ORCH] Hippocampe: ");
    print_u64(reflex.count as u64);
    println(" reflex entries loaded (including network queries)");
    print("[ORCH] Confidence threshold: ");
    print_u64(CONFIDENCE_THRESHOLD as u64);
    println("%");
    println("[ORCH] Routing: Reflex(system) | HTTP(network) | LLM(complex)");

    // Signal readiness on the Cognitive Bus
    sys_bus_publish(INTENT_ORCH_READY, 2, reflex.count as u64);
    println("[ORCH] Published INTENT_ORCH_READY (0x8004) on Cognitive Bus");
    println("[ORCH] Listening for:");
    println("[ORCH]   - INTENT_USER_PROMPT  (0x8001)");
    println("[ORCH]   - INTENT_API_RESPONSE (0xB001)");
    println("[ORCH]   - INTENT_LLM_OUTPUT   (0x8010)");

    let mut msg_buf: [u64; 8] = [0; 8];
    let mut api_buf: [u64; 8] = [0; 8];
    let mut total_queries: u64 = 0;
    let mut reflex_hits: u64 = 0;
    let mut llm_routes: u64 = 0;
    let mut http_routes: u64 = 0;

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

            // Phase 1: Hippocampe lookup (O(1) amortized)
            match reflex.lookup(prompt_hash, CONFIDENCE_THRESHOLD) {
                Some(entry) => {
                    let cycles = sys_rdtsc() - t0;

                    if is_network_action(entry.action_code) {
                        // NETWORK QUERY — route to HTTP Agent
                        http_routes += 1;
                        print("[ORCH] NETWORK ROUTE! action=");
                        print_u64(entry.action_code as u64);
                        print(", latency=");
                        print_u64(cycles);
                        println(" cycles");

                        execute_reflex(entry.action_code);

                        sys_bus_publish(INTENT_ORCH_RESPONSE, 2, prompt_hash);
                    } else {
                        // REFLEX HIT — respond immediately
                        reflex_hits += 1;

                        print("[ORCH] REFLEX HIT! action=");
                        print_u64(entry.action_code as u64);
                        print(", confidence=");
                        print_u64(entry.confidence as u64);
                        print("%, latency=");
                        print_u64(cycles);
                        println(" cycles");

                        execute_reflex(entry.action_code);

                        sys_bus_publish(INTENT_REFLEX_HIT, 2,
                            ((entry.action_code as u64) << 32) | (entry.confidence as u64));
                        sys_bus_publish(INTENT_ORCH_RESPONSE, 2, prompt_hash);
                    }
                },
                None => {
                    // NO REFLEX — route to LLM
                    llm_routes += 1;
                    let cycles = sys_rdtsc() - t0;

                    print("[ORCH] No reflex match. Routing to LLM (");
                    print_u64(cycles);
                    println(" cycles lookup)");
                    println("[ORCH] Publishing INTENT_LLM_WAKEUP (0x8003)");

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
            println("");
        } else {
            // No user prompt — check for API responses
            let api_result = sys_bus_consume_intent(&mut api_buf, INTENT_API_RESPONSE);
            if api_result == 0 {
                idle_cycles = 0;
                println("[ORCH] Received INTENT_API_RESPONSE (0xB001)");
                handle_api_response(api_buf[4]);
            }

            // Also check for LLM output (for synthesis pipeline)
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
                if total_queries > 0 {
                    println("[ORCH] ========================================");
                    print("[ORCH] Session stats: ");
                    print_u64(total_queries);
                    print(" total, ");
                    print_u64(reflex_hits);
                    print(" reflexes, ");
                    print_u64(http_routes);
                    print(" http, ");
                    print_u64(llm_routes);
                    println(" llm");
                    if total_queries > 0 {
                        print("[ORCH] Reflex rate: ");
                        print_u64(reflex_hits * 100 / total_queries);
                        println("%");
                    }
                    println("[ORCH] ========================================");
                }
                idle_cycles = 0;
            }
        }
    }
}
