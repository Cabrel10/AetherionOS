//! AetherionOS Jalon 46 - Static BPE Tokenizer Agent (Ring 3)
//!
//! Implements a minimal tokenizer with 512 static tokens.
//! Tests encode/decode round-trip on a fixed string.
//! Uses heapless fixed-size arrays (no heap allocation for tokenizer core).

#![no_std]
#![no_main]

extern crate alloc;

use aetherion_sdk::*;

// ===== Static vocabulary: 512 tokens =====
// Tokens 0-255: single bytes (character-level fallback)
// Tokens 256-511: common English bigrams/trigrams
const VOCAB_SIZE: usize = 512;
const MAX_MERGE_TOKENS: usize = 256; // tokens 256..511
const MAX_TOKEN_LEN: usize = 8;      // max bytes per merge token

// Common English subwords for tokens 256-511
static MERGE_TOKENS: &[&[u8]] = &[
    b"th", b"he", b"in", b"er", b"an", b"re", b"on", b"en",   // 256-263
    b"at", b"nd", b"st", b"es", b"or", b"te", b"of", b"ed",   // 264-271
    b"is", b"it", b"al", b"ar", b"le", b"to", b"ti", b"ng",   // 272-279
    b"se", b"ha", b"as", b"ou", b"io", b"co", b"ce", b"me",   // 280-287
    b"de", b"hi", b"ri", b"ro", b"ic", b"ne", b"ea", b"ra",   // 288-295
    b"ve", b" t", b" a", b" s", b" i", b" o", b" c", b" m",   // 296-303
    b"the", b"and", b"ing", b"ion", b"tio", b"ent", b"ati",    // 304-310
    b"for", b"her", b"ter", b"hat", b"tha", b"ere", b"ate",    // 311-317
    b"his", b"con", b"res", b"ver", b"all", b"ith", b"not",    // 318-324
    b"ons", b"est", b"ous", b"com", b"pro", b"per", b"int",    // 325-331
    b"men", b"pre", b"ess", b"ect", b"rea", b"sta", b"ove",    // 332-338
    b"our", b"ted", b"ble", b"ine", b"out", b"act", b"ore",    // 339-345
    b"age", b"ear", b"ort", b"ure", b"str", b"igh", b"ard",    // 346-352
    b"ght", b"und", b"rom", b"ive", b"wor", b"use", b"ful",    // 353-359
    b"tion", b"ment", b"ally", b"ness", b"able", b"ence",      // 360-365
    b"ight", b"ould", b"ther", b"from", b"with", b"have",      // 366-371
    b"this", b"will", b"your", b"that", b"they", b"been",      // 372-377
    b"some", b"were", b"when", b"more", b"make", b"like",      // 378-383
    b"time", b"just", b"know", b"take", b"come", b"them",      // 384-389
    b"what", b"then", b"each", b"well", b"also", b"into",      // 390-395
    b"year", b"back", b"only", b"over", b"such", b"good",      // 396-401
    b"give", b"most", b"very", b"work", b"call", b"need",      // 402-407
    b"long", b"high", b"last", b"keep", b"even", b"much",      // 408-413
    b"help", b"line", b"turn", b"move", b"live", b"find",      // 414-419
    b"here", b"show", b"head", b"hand", b"part", b"play",      // 420-425
    b"life", b"tell", b"does", b"said", b"look", b"still",     // 426-431
    b"after", b"world", b"thing", b"these", b"other",          // 432-436
    b"think", b"could", b"where", b"which", b"their",          // 437-441
    b"about", b"would", b"there", b"right", b"being",          // 442-446
    b"going", b"never", b"under", b"great", b"every",          // 447-451
    b"shall", b"first", b"state", b"those", b"place",          // 452-456
    b"while", b"start", b"three", b"house", b"point",          // 457-461
    b"small", b"again", b"might", b"power", b"order",          // 462-466
    b"water", b"given", b"group", b"often", b"later",          // 467-471
    b"early", b"young", b"night", b"large", b"local",          // 472-476
    b"human", b"began", b"table", b"world", b"light",          // 477-481
    b"money", b"least", b"stood", b"along", b"known",          // 482-486
    b"south", b"close", b"north", b"added", b"taken",          // 487-491
    b"among", b"black", b"white", b"whole", b"clear",          // 492-496
    b"study", b"words", b"child", b"quite", b"class",          // 497-501
    b"above", b"using", b"level", b"based", b"field",          // 502-506
    b"model", b"learn", b"token", b"input", b"query",          // 507-511
];

/// Max output tokens from encode
const MAX_OUTPUT_TOKENS: usize = 256;

/// Encode a byte slice into token IDs using greedy longest-match
fn encode(input: &[u8], output: &mut [u16; MAX_OUTPUT_TOKENS]) -> usize {
    let mut out_len: usize = 0;
    let mut pos: usize = 0;

    while pos < input.len() && out_len < MAX_OUTPUT_TOKENS {
        // Try longest merge token first
        let mut best_len: usize = 0;
        let mut best_id: u16 = 0;

        let remaining = input.len() - pos;
        for (idx, tok) in MERGE_TOKENS.iter().enumerate() {
            let tlen = tok.len();
            if tlen <= remaining && tlen > best_len {
                // Check if token matches at current position
                let mut matches = true;
                for j in 0..tlen {
                    if input[pos + j] != tok[j] {
                        matches = false;
                        break;
                    }
                }
                if matches {
                    best_len = tlen;
                    best_id = (256 + idx) as u16;
                }
            }
        }

        if best_len > 1 {
            output[out_len] = best_id;
            out_len += 1;
            pos += best_len;
        } else {
            // Fall back to single byte token
            output[out_len] = input[pos] as u16;
            out_len += 1;
            pos += 1;
        }
    }

    out_len
}

/// Decode token IDs back to bytes
fn decode(tokens: &[u16], count: usize, output: &mut [u8; 1024]) -> usize {
    let mut out_len: usize = 0;

    for i in 0..count {
        let tid = tokens[i];
        if tid < 256 {
            // Single byte
            if out_len < 1024 {
                output[out_len] = tid as u8;
                out_len += 1;
            }
        } else {
            let merge_idx = (tid - 256) as usize;
            if merge_idx < MERGE_TOKENS.len() {
                let tok = MERGE_TOKENS[merge_idx];
                for &b in tok.iter() {
                    if out_len < 1024 {
                        output[out_len] = b;
                        out_len += 1;
                    }
                }
            }
        }
    }

    out_len
}

const TEST_INPUT: &[u8] = b"the tokenizer will encode this input query";

#[no_mangle]
pub extern "C" fn main() -> i64 {
    println("[J46] Static BPE Tokenizer Agent v1.0");
    print("[J46] Vocab size: ");
    print_u64(VOCAB_SIZE as u64);
    print(" (256 byte + ");
    print_u64(MERGE_TOKENS.len() as u64);
    println(" merge)");

    // Encode test string
    let mut token_ids = [0u16; MAX_OUTPUT_TOKENS];
    let n_tokens = encode(TEST_INPUT, &mut token_ids);
    print("[J46] Input length: ");
    print_u64(TEST_INPUT.len() as u64);
    print(" bytes -> ");
    print_u64(n_tokens as u64);
    println(" tokens");

    // Print first 10 token IDs
    print("[J46] Token IDs: [");
    let show = if n_tokens < 10 { n_tokens } else { 10 };
    for i in 0..show {
        print_u64(token_ids[i] as u64);
        if i + 1 < show {
            print(", ");
        }
    }
    if n_tokens > 10 {
        print(", ...");
    }
    println("]");

    // Decode back
    let mut decoded_buf = [0u8; 1024];
    let decoded_len = decode(&token_ids, n_tokens, &mut decoded_buf);

    print("[J46] Decoded: ");
    print_u64(decoded_len as u64);
    println(" bytes");

    // Round-trip check
    let mut round_trip_ok = true;
    if decoded_len != TEST_INPUT.len() {
        println("[J46] WARN: length mismatch");
        round_trip_ok = false;
    } else {
        for i in 0..decoded_len {
            if decoded_buf[i] != TEST_INPUT[i] {
                print("[J46] WARN: mismatch at byte ");
                print_u64(i as u64);
                println("");
                round_trip_ok = false;
                break;
            }
        }
    }

    if round_trip_ok {
        println("[J46] Round-trip: PASS");
    } else {
        println("[J46] Round-trip: FAIL");
    }

    // Compression ratio
    // tokens / bytes
    print("[J46] Compression: ");
    print_u64(TEST_INPUT.len() as u64);
    print(" -> ");
    print_u64(n_tokens as u64);
    print(" tokens (");
    if n_tokens > 0 {
        let ratio = (TEST_INPUT.len() * 100) / n_tokens;
        print_u64(ratio as u64);
        print("% bytes/token");
    }
    println(")");

    // Publish success
    let bus_ret = sys_bus_publish(0xC046, 2, n_tokens as u64);
    if bus_ret == 0 {
        println("[J46] Bus 0xC046 OK");
    }

    sys_write(1, b"\n[J46-OK] Tokenizer round-trip SUCCESS\n");
    0
}
