//! Defense-in-depth scrubber for SERP text before it lands in the
//! prompt. Identical philosophy to Phase 19 (`language` directive) and
//! Phase 21 (`# LINK CONTEXT`): SERPs are attacker-controlled input.

/// Strips control characters, normalises CR/LF to spaces, collapses
/// runs of whitespace, and hard-caps the byte length. Returns an empty
/// string when input is all whitespace or all control characters.
pub fn sanitise_for_prompt(input: &str, max_bytes: usize) -> String {
    let mut out = String::with_capacity(input.len().min(max_bytes));
    let mut last_was_space = true; // suppress leading whitespace
    for ch in input.chars() {
        let mapped = if ch == '\r' || ch == '\n' || ch == '\t' {
            ' '
        } else if ch.is_control() {
            continue;
        } else {
            ch
        };
        if mapped == ' ' {
            if last_was_space {
                continue;
            }
            last_was_space = true;
        } else {
            last_was_space = false;
        }
        if out.len() + mapped.len_utf8() > max_bytes {
            break;
        }
        out.push(mapped);
    }
    let trimmed_end = out.trim_end().len();
    out.truncate(trimmed_end);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_newlines() {
        assert_eq!(sanitise_for_prompt("a\nb\r\nc", 64), "a b c");
    }

    #[test]
    fn drops_other_control_chars() {
        assert_eq!(sanitise_for_prompt("a\x07b\x1bc", 64), "abc");
    }

    #[test]
    fn collapses_runs_of_whitespace() {
        assert_eq!(sanitise_for_prompt("a   \t  b", 64), "a b");
    }

    #[test]
    fn caps_byte_length_at_char_boundary() {
        let out = sanitise_for_prompt("ñññññ", 5); // 'ñ' is 2 bytes
        assert!(out.len() <= 5);
        // Must not split a char.
        assert!(out.chars().all(|c| c == 'ñ'));
    }

    #[test]
    fn trims_trailing_whitespace_after_cap() {
        // Cap at 6 bytes lands "hello " — the trailing space gets trimmed.
        let out = sanitise_for_prompt("hello world goodbye", 6);
        assert_eq!(out, "hello");
    }

    #[test]
    fn empty_on_pure_control() {
        assert_eq!(sanitise_for_prompt("\x00\x01\x02", 64), "");
    }
}
