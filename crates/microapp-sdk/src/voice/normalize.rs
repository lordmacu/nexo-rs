//! Text normalisation for TTS pipelines.
//!
//! Three independent passes a microapp wires in order before
//! handing the body to a [`super::tts::TtsProvider`]:
//!
//! 1. [`normalise_markdown_for_tts`] — turn STRUCTURE (lists,
//!    headings, paragraph breaks, arrows, em/en dashes) into
//!    sentence boundaries so the engine produces natural pauses.
//!    Without this every reply sounds like a single run-on.
//! 2. [`strip_emojis_for_tts`] — drop emoji + dingbats + variation
//!    selectors so Edge doesn't read them aloud (it emits literal
//!    "smiling face" or stays silent for unrecognised glyphs,
//!    which sounds weird either way). Lettters (incl. accents),
//!    digits, whitespace and a short whitelist of punctuation
//!    survive. SSML tags inside `<…>` are preserved verbatim.
//! 3. [`collapse_punctuation`] — squeeze runs of identical
//!    punctuation that the prior passes may produce when several
//!    rules fire in sequence (`". . ."` →  `"."`). XML-aware so
//!    SSML attribute values stay untouched.
//!
//! All three are pure functions — no allocations beyond the
//! returned `String`. Lifted from the agent-creator-microapp's
//! voice_mode/tts.rs without behaviour change.

/// Convert LLM-friendly markdown into TTS-friendly prose. The
/// stripper that runs after this drops the markers themselves;
/// the goal here is to turn STRUCTURE (lists, headings, paragraph
/// breaks, arrows) into sentence boundaries so the engine
/// produces natural pauses.
pub fn normalise_markdown_for_tts(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for raw_line in input.lines() {
        // Strip leading list marker / heading hashes. We keep the
        // text and append a period so each item gets its own
        // intonation curve.
        let line = raw_line.trim_start();
        let body = if let Some(rest) = line.strip_prefix("# ") {
            rest
        } else if let Some(rest) = line.strip_prefix("## ") {
            rest
        } else if let Some(rest) = line.strip_prefix("### ") {
            rest
        } else if let Some(rest) = line.strip_prefix("- ") {
            rest
        } else if let Some(rest) = line.strip_prefix("* ") {
            rest
        } else if let Some(rest) = strip_numbered_marker(line) {
            rest
        } else {
            line
        };
        let body = body.trim();
        if body.is_empty() {
            // Blank line — flush a sentence boundary so paragraph
            // breaks become real pauses.
            if !out.ends_with(' ') && !out.is_empty() {
                if !ends_with_sentence_punct(&out) {
                    out.push('.');
                }
                out.push(' ');
            }
            continue;
        }
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(body);
        // Force a period at end-of-line if the line didn't already
        // end in sentence punctuation — gives us a pause between
        // every list item / paragraph.
        if !ends_with_sentence_punct(&out) {
            out.push('.');
        }
        out.push(' ');
    }
    // Replace common typographic substitutes with their spoken
    // equivalents BEFORE the symbol stripper runs.
    let out = out.replace('→', ",");
    let out = out.replace('←', ",");
    let out = out.replace('•', ",");
    let out = out.replace('▪', ",");
    let out = out.replace('·', ",");
    // Em-dash + en-dash → comma so Edge produces a natural pause.
    // Otherwise the stripper drops them and adjacent words run on.
    let out = out.replace('—', ", ");
    let out = out.replace('–', ", ");
    let out = out.replace("...", "…");
    let out = out.replace('…', ". ");
    out
}

fn strip_numbered_marker(line: &str) -> Option<&str> {
    // Matches `1.`, `12.`, `1)`, `12)` at the start of a line.
    let mut saw_digit = false;
    for (i, ch) in line.char_indices() {
        if ch.is_ascii_digit() {
            saw_digit = true;
            continue;
        }
        if saw_digit && (ch == '.' || ch == ')') {
            let after = i + ch.len_utf8();
            let rest = &line[after..];
            if let Some(stripped) = rest.strip_prefix(' ') {
                return Some(stripped);
            }
        }
        return None;
    }
    None
}

fn ends_with_sentence_punct(s: &str) -> bool {
    matches!(
        s.chars().last(),
        Some('.' | ',' | '?' | '!' | ';' | ':' | '…')
    )
}

/// Squeeze runs of identical punctuation that the normaliser may
/// produce when several rules fire in sequence. `". . ."` →  `"."`
/// without losing the spacing we depend on for prosody. XML-aware
/// — bytes inside `<…>` (SSML tag attributes) pass through
/// untouched.
pub fn collapse_punctuation(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_punct: Option<char> = None;
    let mut last_was_space = false;
    let mut depth = 0i32; // depth>0 ⇒ inside an SSML tag; passthrough.
    for ch in input.chars() {
        if depth > 0 {
            out.push(ch);
            if ch == '>' {
                depth -= 1;
                last_punct = None;
                last_was_space = false;
            }
            continue;
        }
        if ch == '<' {
            depth = 1;
            out.push(ch);
            continue;
        }
        if matches!(ch, '.' | ',' | '!' | '?' | ';') {
            if last_punct == Some(ch) {
                continue;
            }
            last_punct = Some(ch);
            last_was_space = false;
            out.push(ch);
            continue;
        }
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
            continue;
        }
        last_punct = None;
        last_was_space = false;
        out.push(ch);
    }
    out.trim().to_string()
}

/// Drop emoji + dingbats + variation selectors so Edge TTS doesn't
/// read them aloud. Keeps every letter (including accented
/// Spanish/Portuguese), digit, whitespace and a SHORT whitelist
/// of punctuation. SSML tags inside `<…>` survive intact —
/// bytes are sacred there because the synthesizer parses them as
/// XML.
///
/// Codepoint-by-codepoint instead of pulling a regex: the set of
/// "bad for TTS" glyphs is well-defined Unicode blocks. Runs of
/// stripped chars collapse to a single space so "hola 👋" becomes
/// "hola " (spoken as "hola"), not "hola" jammed against the next
/// word.
pub fn strip_emojis_for_tts(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_space = false;
    let mut in_tag = false;
    for ch in input.chars() {
        if in_tag {
            out.push(ch);
            if ch == '>' {
                in_tag = false;
                last_was_space = false;
            }
            continue;
        }
        if ch == '<' {
            in_tag = true;
            out.push(ch);
            last_was_space = false;
            continue;
        }
        if is_tts_safe(ch) {
            // Collapse runs of whitespace too — otherwise stripping
            // an emoji surrounded by real spaces leaves a double
            // space behind.
            if ch.is_whitespace() {
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
                continue;
            }
            out.push(ch);
            last_was_space = false;
            continue;
        }
        // Stripped char — emit a space so word boundaries survive.
        if !last_was_space {
            out.push(' ');
            last_was_space = true;
        }
    }
    out
}

fn is_tts_safe(ch: char) -> bool {
    // Letters (incl. accented), digits and a SHORT whitelist of
    // punctuation pass through. Everything else — markdown markers
    // (`*` `_` `` ` `` `#`), brackets, currency, math symbols,
    // emoji — gets stripped. The Unicode database flags pictographs
    // in the dedicated blocks listed below.
    match ch as u32 {
        // Variation selectors (FE0F = "show as emoji") + ZWJ chains.
        0xFE00..=0xFE0F => return false,
        0x200D => return false,
        // Dingbats.
        0x2700..=0x27BF => return false,
        // Misc symbols + arrows that mostly render as emoji on iOS.
        0x2600..=0x26FF => return false,
        0x2300..=0x23FF => return false,
        0x2B00..=0x2BFF => return false,
        // Enclosed alphanumerics that emoji-render on phones.
        0x2460..=0x24FF => return false,
        // CJK pictographs / regional indicator base ranges.
        0x1F000..=0x1FAFF => return false,
        // Tag characters (used inside flag sequences).
        0xE0000..=0xE007F => return false,
        _ => {}
    }
    ch.is_alphanumeric() || ch.is_whitespace() || is_safe_punct(ch)
}

fn is_safe_punct(ch: char) -> bool {
    matches!(
        ch,
        '.' | ',' | '?' | '¿' | '!' | '¡' | ';' | ':' | '\''
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_drops_emoji_keeps_accents() {
        assert_eq!(strip_emojis_for_tts("hola 👋 cómo estás"), "hola cómo estás");
        assert_eq!(strip_emojis_for_tts("✅ ok"), " ok");
        assert_eq!(strip_emojis_for_tts("100%"), "100 ");
        assert_eq!(strip_emojis_for_tts("¿qué tal?"), "¿qué tal?");
    }

    #[test]
    fn strip_collapses_runs_of_emoji() {
        assert_eq!(strip_emojis_for_tts("ok 👨‍💻 ya"), "ok ya");
    }

    #[test]
    fn strip_drops_currency_and_brackets() {
        assert_eq!(
            strip_emojis_for_tts("Pague $50.000 — gracias!"),
            "Pague 50.000 gracias!"
        );
    }

    #[test]
    fn strip_drops_markdown_emphasis() {
        assert_eq!(
            strip_emojis_for_tts("Hola **mundo** _de_ `prueba` # título"),
            "Hola mundo de prueba título"
        );
    }

    #[test]
    fn strip_idempotent_on_clean_text() {
        let s = "Hola, ¿cómo estás? Todo bien.";
        assert_eq!(strip_emojis_for_tts(s), s);
    }

    #[test]
    fn empty_after_strip_is_empty() {
        assert_eq!(strip_emojis_for_tts("👋👋👋").trim(), "");
    }

    #[test]
    fn normalise_lists_become_sentences() {
        let md = "Cosas:\n- alpha\n- beta\n- gamma";
        let out = normalise_markdown_for_tts(md);
        assert!(out.contains("Cosas:"));
        assert!(out.contains("alpha."));
        assert!(out.contains("beta."));
        assert!(out.contains("gamma."));
    }

    #[test]
    fn normalise_numbered_lists() {
        let md = "1. Denunciar\n2. Contactar\n3. Consultar";
        let out = normalise_markdown_for_tts(md);
        assert!(out.contains("Denunciar."));
        assert!(out.contains("Contactar."));
        assert!(out.contains("Consultar."));
        assert!(!out.contains("1."));
    }

    #[test]
    fn normalise_headings_drop_hashes() {
        let out = normalise_markdown_for_tts("# Título\nCuerpo");
        assert!(!out.contains('#'));
        assert!(out.contains("Título."));
        assert!(out.contains("Cuerpo."));
    }

    #[test]
    fn normalise_arrows_become_commas() {
        let out = normalise_markdown_for_tts("Robo de datos → delito penal");
        assert!(out.contains("Robo de datos , delito penal"));
    }

    #[test]
    fn normalise_paragraph_break_inserts_pause() {
        let out = normalise_markdown_for_tts("Hola.\n\nQué tal?");
        assert!(out.starts_with("Hola."));
        assert!(out.contains("Qué tal?"));
    }

    #[test]
    fn collapse_squeezes_repeated_punct() {
        assert_eq!(collapse_punctuation("hola.. mundo"), "hola. mundo");
        assert_eq!(collapse_punctuation("pero,, no"), "pero, no");
        assert_eq!(collapse_punctuation("a   b"), "a b");
    }

    #[test]
    fn full_pipeline_real_example() {
        let raw = "Entiendo que sospechas que un hacker robó tu correo. Eso es **muy serio**, pero aquí entramos en territorio que **va más allá del Artículo 15**:\n\n- **Robo de datos** → delito penal\n- **Acceso no autorizado** → también delito\n\n¿Hay algo del Artículo 15?";
        let normalised = normalise_markdown_for_tts(raw);
        let stripped = strip_emojis_for_tts(&normalised);
        let final_text = collapse_punctuation(stripped.trim());
        assert!(!final_text.contains('*'));
        assert!(!final_text.contains('#'));
        assert!(!final_text.contains('→'));
        assert!(final_text.contains("Robo de datos"));
        assert!(final_text.contains("Acceso no autorizado"));
        assert!(final_text.contains("¿Hay algo del Artículo 15?"));
    }
}
