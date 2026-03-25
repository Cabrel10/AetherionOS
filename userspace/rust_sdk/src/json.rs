// ============================================================================
// Level 8 — Zero-Allocation no_std JSON Parser for MCP Contracts
// ============================================================================
//
// A minimal JSON extractor that operates on &[u8] slices without any heap
// allocation. Designed for parsing ACHA action contracts of the form:
//
//   {"action": "gen_driver", "params": {"vendor": 4660, "device": 4369}}
//
// This parser does NOT validate full JSON syntax. It extracts values for
// known keys using byte-level scanning. This is intentional: in a bare-metal
// Ring 3 environment, we want speed and zero dependencies, not completeness.
//
// Security: All operations are bounds-checked. No panics, no OOB reads.
// ============================================================================

/// Extract a string value for a given key from a JSON byte slice.
///
/// Given `json = b'{"action": "gen_driver", "params": {...}}'` and `key = "action"`,
/// returns `Some(b"gen_driver")`.
///
/// Returns `None` if the key is not found or the value is not a string.
///
/// # Algorithm
/// 1. Search for `"key"` pattern in the byte slice
/// 2. Skip whitespace and `:` after the key
/// 3. If next non-whitespace char is `"`, extract the string value
pub fn extract_json_str<'a>(json: &'a [u8], key: &str) -> Option<&'a [u8]> {
    let key_bytes = key.as_bytes();
    let json_len = json.len();

    // Search for "key" pattern
    let mut i = 0;
    while i < json_len {
        // Look for opening quote of key
        if json[i] == b'"' {
            let key_start = i + 1;
            // Check if we have enough bytes for the key + closing quote
            if key_start + key_bytes.len() >= json_len {
                i += 1;
                continue;
            }
            // Compare key bytes
            let mut matched = true;
            for k in 0..key_bytes.len() {
                if json[key_start + k] != key_bytes[k] {
                    matched = false;
                    break;
                }
            }
            if matched && json[key_start + key_bytes.len()] == b'"' {
                // Found the key! Now skip to the value
                let mut j = key_start + key_bytes.len() + 1; // past closing quote
                // Skip whitespace
                while j < json_len && is_ws(json[j]) {
                    j += 1;
                }
                // Expect ':'
                if j < json_len && json[j] == b':' {
                    j += 1;
                } else {
                    i += 1;
                    continue;
                }
                // Skip whitespace
                while j < json_len && is_ws(json[j]) {
                    j += 1;
                }
                // If value starts with '"', extract string
                if j < json_len && json[j] == b'"' {
                    let val_start = j + 1;
                    let mut val_end = val_start;
                    while val_end < json_len && json[val_end] != b'"' {
                        // Handle escaped quotes
                        if json[val_end] == b'\\' && val_end + 1 < json_len {
                            val_end += 2;
                        } else {
                            val_end += 1;
                        }
                    }
                    if val_end <= json_len {
                        return Some(&json[val_start..val_end]);
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Extract an unsigned 32-bit integer value for a given key.
///
/// Given `json = b'{"vendor": 4660}'` and `key = "vendor"`,
/// returns `Some(4660)`.
///
/// Handles both decimal (`4660`) and hex-prefixed (`0x1234`) values.
/// Returns `None` if the key is not found or the value is not numeric.
pub fn extract_json_u32(json: &[u8], key: &str) -> Option<u32> {
    let key_bytes = key.as_bytes();
    let json_len = json.len();

    let mut i = 0;
    while i < json_len {
        if json[i] == b'"' {
            let key_start = i + 1;
            if key_start + key_bytes.len() >= json_len {
                i += 1;
                continue;
            }
            let mut matched = true;
            for k in 0..key_bytes.len() {
                if json[key_start + k] != key_bytes[k] {
                    matched = false;
                    break;
                }
            }
            if matched && json[key_start + key_bytes.len()] == b'"' {
                let mut j = key_start + key_bytes.len() + 1;
                while j < json_len && is_ws(json[j]) {
                    j += 1;
                }
                if j < json_len && json[j] == b':' {
                    j += 1;
                } else {
                    i += 1;
                    continue;
                }
                while j < json_len && is_ws(json[j]) {
                    j += 1;
                }
                // Parse number (decimal or hex)
                if j < json_len && (json[j] >= b'0' && json[j] <= b'9') {
                    // Check for hex prefix 0x
                    if j + 2 < json_len && json[j] == b'0' && (json[j + 1] == b'x' || json[j + 1] == b'X') {
                        return parse_hex(&json[j + 2..]);
                    }
                    return parse_decimal(&json[j..]);
                }
                // Also handle string-encoded numbers: "4660"
                if j < json_len && json[j] == b'"' {
                    let val_start = j + 1;
                    if val_start < json_len && json[val_start] >= b'0' && json[val_start] <= b'9' {
                        // Check hex in string: "0x1234"
                        if val_start + 2 < json_len && json[val_start] == b'0'
                            && (json[val_start + 1] == b'x' || json[val_start + 1] == b'X')
                        {
                            return parse_hex(&json[val_start + 2..]);
                        }
                        return parse_decimal(&json[val_start..]);
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Extract a nested JSON object for a given key.
///
/// Given `json = b'{"params": {"vendor": 4660, "device": 4369}}'` and `key = "params"`,
/// returns `Some(b'{"vendor": 4660, "device": 4369}')`.
pub fn extract_json_object<'a>(json: &'a [u8], key: &str) -> Option<&'a [u8]> {
    let key_bytes = key.as_bytes();
    let json_len = json.len();

    let mut i = 0;
    while i < json_len {
        if json[i] == b'"' {
            let key_start = i + 1;
            if key_start + key_bytes.len() >= json_len {
                i += 1;
                continue;
            }
            let mut matched = true;
            for k in 0..key_bytes.len() {
                if json[key_start + k] != key_bytes[k] {
                    matched = false;
                    break;
                }
            }
            if matched && json[key_start + key_bytes.len()] == b'"' {
                let mut j = key_start + key_bytes.len() + 1;
                while j < json_len && is_ws(json[j]) {
                    j += 1;
                }
                if j < json_len && json[j] == b':' {
                    j += 1;
                } else {
                    i += 1;
                    continue;
                }
                while j < json_len && is_ws(json[j]) {
                    j += 1;
                }
                // If value starts with '{', find matching '}'
                if j < json_len && json[j] == b'{' {
                    let obj_start = j;
                    let mut depth: u32 = 0;
                    let mut k = j;
                    while k < json_len {
                        if json[k] == b'{' {
                            depth += 1;
                        } else if json[k] == b'}' {
                            depth -= 1;
                            if depth == 0 {
                                return Some(&json[obj_start..k + 1]);
                            }
                        } else if json[k] == b'"' {
                            // Skip strings (may contain braces)
                            k += 1;
                            while k < json_len && json[k] != b'"' {
                                if json[k] == b'\\' {
                                    k += 1;
                                }
                                k += 1;
                            }
                        }
                        k += 1;
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// Compare a byte slice with a string literal (helper for action matching).
pub fn json_str_eq(value: &[u8], expected: &str) -> bool {
    let exp = expected.as_bytes();
    if value.len() != exp.len() {
        return false;
    }
    for i in 0..value.len() {
        if value[i] != exp[i] {
            return false;
        }
    }
    true
}

// ── Internal helpers ──

fn is_ws(b: u8) -> bool {
    b == b' ' || b == b'\t' || b == b'\n' || b == b'\r'
}

fn parse_decimal(data: &[u8]) -> Option<u32> {
    let mut result: u32 = 0;
    let mut found_digit = false;
    for &b in data {
        if b >= b'0' && b <= b'9' {
            result = result.wrapping_mul(10).wrapping_add((b - b'0') as u32);
            found_digit = true;
        } else {
            break;
        }
    }
    if found_digit { Some(result) } else { None }
}

fn parse_hex(data: &[u8]) -> Option<u32> {
    let mut result: u32 = 0;
    let mut found_digit = false;
    for &b in data {
        let nibble = match b {
            b'0'..=b'9' => (b - b'0') as u32,
            b'a'..=b'f' => (b - b'a' + 10) as u32,
            b'A'..=b'F' => (b - b'A' + 10) as u32,
            _ => break,
        };
        result = (result << 4) | nibble;
        found_digit = true;
    }
    if found_digit { Some(result) } else { None }
}
