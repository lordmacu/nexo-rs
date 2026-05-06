//! SSML hint pipeline.
//!
//! Edge's TTS endpoint accepts a subset of SSML inside its
//! `<prosody>` body — `<break>`, `<say-as>`, `<emphasis>`,
//! `<phoneme>`, `<sub>`, etc. The crate already wraps our text in
//! `<speak><voice><prosody>…</prosody></voice></speak>` so any
//! tags we leave inside the body land verbatim on the synthesizer.
//!
//! Two kinds of inputs become SSML:
//!   1. Operator-friendly markers an LLM can emit (`[pause=400ms]`,
//!      `[em]palabra[/em]`, `[spell]SAT[/spell]`, `[slow]…[/slow]`)
//!      — translated 1:1.
//!   2. Auto-detected patterns we wrap silently (currency, ISO
//!      dates, big numbers, ALL-CAPS acronyms) — operator does
//!      nothing; the synthesizer reads them naturally.
//!
//! Both kinds run BEFORE the symbol stripper, so the raw `$` /
//! `-` characters are still available for pattern detection. The
//! stripper learns to skip everything inside `<…>` so our tags
//! survive intact.

use once_cell::sync::Lazy;
use regex::{Captures, Regex};

/// Translate operator-friendly markers + auto-detect frequent
/// patterns into SSML. Idempotent on already-tagged text — the
/// auto-detect regexes deliberately reject content that already
/// sits inside an XML tag.
pub fn apply_ssml_hints(input: &str) -> String {
    let after_markers = translate_markers(input);
    auto_detect(&after_markers)
}

/// Strip every voice-mode marker from a string. Used to produce
/// the transcript field of `OutboundReplyKind::VoiceNote` so the
/// operator dashboard / firehose audit sees the clean prose
/// without `[pause=400ms]`-style noise. Idempotent — clean text
/// passes through unchanged.
pub fn strip_voice_markers(input: &str) -> String {
    let mut s = input.to_string();
    s = RE_PAUSE.replace_all(&s, "").into_owned();
    s = RE_EM
        .replace_all(&s, |c: &Captures<'_>| c[1].to_string())
        .into_owned();
    s = RE_STRONG
        .replace_all(&s, |c: &Captures<'_>| c[1].to_string())
        .into_owned();
    s = RE_SPELL
        .replace_all(&s, |c: &Captures<'_>| c[1].to_string())
        .into_owned();
    s = RE_SLOW
        .replace_all(&s, |c: &Captures<'_>| c[1].to_string())
        .into_owned();
    s = RE_FAST
        .replace_all(&s, |c: &Captures<'_>| c[1].to_string())
        .into_owned();
    // Squeeze any double spaces created by removing standalone
    // `[pause]` markers.
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
}

// ── 1. Operator markers ────────────────────────────────────────

static RE_PAUSE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[pause=(\d{1,5})ms\]").expect("re_pause"));
static RE_EM: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[em\](.*?)\[/em\]").expect("re_em"));
static RE_STRONG: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[strong\](.*?)\[/strong\]").expect("re_strong"));
static RE_SPELL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[spell\](.*?)\[/spell\]").expect("re_spell"));
static RE_SLOW: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[slow\](.*?)\[/slow\]").expect("re_slow"));
static RE_FAST: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[fast\](.*?)\[/fast\]").expect("re_fast"));
// Markdown emphasis the LLM emits out of habit — mapped to SSML
// so the audio actually reflects it. Bold (`**X**`, `__X__`) →
// strong; italic (`*X*`, `_X_`) → moderate. Bold runs first so
// the italic regex doesn't grab one of the `**` pairs.
//
// Constraints:
//   - At least one non-whitespace char inside, ≤200 chars (avoid
//     runaway matches across paragraphs).
//   - No newline inside — markdown emphasis is line-local.
//   - Italic single-underscore requires non-underscore boundaries
//     so we don't mangle `snake_case_idents`.
static RE_MD_BOLD_STARS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\*\*([^*\n][^*\n]{0,198}?[^*\n\s]|[^*\n\s])\*\*").expect("re_md_bold_stars"));
static RE_MD_BOLD_UNDER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"__([^_\n][^_\n]{0,198}?[^_\n\s]|[^_\n\s])__").expect("re_md_bold_under"));
static RE_MD_ITAL_STARS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?:^|[\s(¡¿])\*([^*\s][^*\n]{0,198}?[^*\s])\*(?:[\s.,;:!?)¡¿]|$)")
        .expect("re_md_ital_stars"));
static RE_MD_ITAL_UNDER: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?:^|[\s(¡¿])_([^_\s][^_\n]{0,198}?[^_\s])_(?:[\s.,;:!?)¡¿]|$)")
        .expect("re_md_ital_under"));

fn translate_markers(input: &str) -> String {
    let mut s = input.to_string();
    s = RE_PAUSE
        .replace_all(&s, |c: &Captures<'_>| {
            format!(r#"<break time="{}ms"/>"#, &c[1])
        })
        .into_owned();
    // We intentionally render emphasis as `<prosody>` rather than
    // `<emphasis>`. Microsoft Edge's Read-Aloud endpoint rejects
    // SSML bodies that contain `<emphasis>` inside the
    // `<prosody>` wrapper that the `msedge_tts` crate hardcodes
    // around our text — the websocket closes with 0 audio frames.
    // Nested `<prosody>` is accepted, so volume + rate deltas
    // give us the same audible "boost" without tripping the
    // server's parser.
    s = RE_EM
        .replace_all(&s, |c: &Captures<'_>| {
            format!(r#"<prosody volume="+15%" rate="-3%">{}</prosody>"#, &c[1])
        })
        .into_owned();
    s = RE_STRONG
        .replace_all(&s, |c: &Captures<'_>| {
            format!(
                r#"<prosody volume="+25%" rate="-7%" pitch="+8%">{}</prosody>"#,
                &c[1]
            )
        })
        .into_owned();
    s = RE_SPELL
        .replace_all(&s, |c: &Captures<'_>| {
            format!(
                r#"<say-as interpret-as="characters">{}</say-as>"#,
                &c[1]
            )
        })
        .into_owned();
    s = RE_SLOW
        .replace_all(&s, |c: &Captures<'_>| {
            format!(r#"<prosody rate="-15%">{}</prosody>"#, &c[1])
        })
        .into_owned();
    s = RE_FAST
        .replace_all(&s, |c: &Captures<'_>| {
            format!(r#"<prosody rate="+12%">{}</prosody>"#, &c[1])
        })
        .into_owned();
    // Markdown bold/italic → SSML prosody (NOT emphasis — see the
    // RE_EM/RE_STRONG note above). Bold first so the italic regex
    // doesn't see leftover `*` from a `**` pair.
    s = RE_MD_BOLD_STARS
        .replace_all(&s, |c: &Captures<'_>| {
            format!(
                r#"<prosody volume="+25%" rate="-7%" pitch="+8%">{}</prosody>"#,
                &c[1]
            )
        })
        .into_owned();
    s = RE_MD_BOLD_UNDER
        .replace_all(&s, |c: &Captures<'_>| {
            format!(
                r#"<prosody volume="+25%" rate="-7%" pitch="+8%">{}</prosody>"#,
                &c[1]
            )
        })
        .into_owned();
    s = RE_MD_ITAL_STARS
        .replace_all(&s, |c: &Captures<'_>| {
            let full = &c[0];
            let inner = &c[1];
            let lead = full.chars().next().unwrap_or(' ');
            let trail = full.chars().last().unwrap_or(' ');
            format!(
                r#"{lead}<prosody volume="+15%" rate="-3%">{inner}</prosody>{trail}"#
            )
        })
        .into_owned();
    s = RE_MD_ITAL_UNDER
        .replace_all(&s, |c: &Captures<'_>| {
            let full = &c[0];
            let inner = &c[1];
            let lead = full.chars().next().unwrap_or(' ');
            let trail = full.chars().last().unwrap_or(' ');
            format!(
                r#"{lead}<prosody volume="+15%" rate="-3%">{inner}</prosody>{trail}"#
            )
        })
        .into_owned();
    s
}

// ── 2. Auto-detection ──────────────────────────────────────────

// ISO date `2026-05-05` — must be flanked by non-alphanumeric so
// we don't grab IDs or hashes.
static RE_DATE_ISO: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b(\d{4}-\d{2}-\d{2})\b").expect("re_date_iso")
});
// `dd/mm/yyyy` — Spanish convention.
static RE_DATE_SLASH: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b(\d{1,2}/\d{1,2}/\d{4})\b").expect("re_date_slash")
});
// Currency with $ / € prefix and optional thousands separators.
static RE_CURRENCY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(\$|€|£|US\$|COP\s|USD\s)\s*(\d{1,3}(?:[.,]\d{3})*(?:[.,]\d{1,2})?)")
        .expect("re_currency")
});
// 4+ digit cardinal — small numbers Edge already reads fine.
static RE_BIG_NUMBER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b(\d{4,})\b").expect("re_big_number")
});
// 3+ uppercase ASCII letters in a row, surrounded by non-letter.
// Catches `SIC`, `DIJIN`, `SAT`, but skips inflected words like
// `BANCO` (we exempt it via the explicit denylist below).
static RE_ACRONYM: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\b([A-Z]{3,6})\b").expect("re_acronym")
});

const ACRONYM_DENYLIST: &[&str] = &[
    // Common Spanish all-caps words that aren't acronyms.
    "BANCO", "BANCOS", "PARA", "ESTO", "ESTA", "ESTAR", "DIJO",
    "TODO", "TODOS", "PERO", "AHORA", "DESDE", "HASTA",
];

fn auto_detect(input: &str) -> String {
    // Skip everything between `<` and `>` so we don't double-wrap
    // content already inside an SSML tag (from marker translation).
    let mut out = String::with_capacity(input.len() + 32);
    let mut depth = 0i32;
    let mut buf = String::new();
    for ch in input.chars() {
        if ch == '<' {
            // Flush accumulated outside-tag text through the
            // detectors before opening the tag.
            if !buf.is_empty() {
                out.push_str(&detect_in_span(&buf));
                buf.clear();
            }
            depth += 1;
            out.push(ch);
            continue;
        }
        if ch == '>' && depth > 0 {
            depth -= 1;
            out.push(ch);
            continue;
        }
        if depth > 0 {
            out.push(ch);
        } else {
            buf.push(ch);
        }
    }
    if !buf.is_empty() {
        out.push_str(&detect_in_span(&buf));
    }
    out
}

fn detect_in_span(span: &str) -> String {
    // Each detector wraps matches in `<say-as>`. The subsequent
    // detector MUST NOT match inside those tags — otherwise we
    // produce nested `<say-as>` (e.g. ISO date `2026-05-05` would
    // also match the big-cardinal `\b\d{4,}\b` on `2026`).
    //
    // `replace_outside_tags` splits the haystack into tag / non-tag
    // chunks and only runs the regex against non-tag chunks.
    let s = replace_outside_tags(span, &RE_DATE_ISO, |c| {
        format!(
            r#"<say-as interpret-as="date" format="ymd">{}</say-as>"#,
            &c[1]
        )
    });
    let s = replace_outside_tags(&s, &RE_DATE_SLASH, |c| {
        format!(
            r#"<say-as interpret-as="date" format="dmy">{}</say-as>"#,
            &c[1]
        )
    });
    let s = replace_outside_tags(&s, &RE_CURRENCY, |c| {
        format!(r#"<say-as interpret-as="currency">{}</say-as>"#, &c[0])
    });
    let s = replace_outside_tags(&s, &RE_BIG_NUMBER, |c| {
        format!(r#"<say-as interpret-as="cardinal">{}</say-as>"#, &c[1])
    });
    replace_outside_tags(&s, &RE_ACRONYM, |c| {
        let word = &c[1];
        if ACRONYM_DENYLIST.contains(&word) {
            return word.to_string();
        }
        format!(r#"<say-as interpret-as="characters">{}</say-as>"#, word)
    })
}

/// Apply `re.replace_all(replacer)` to every chunk of `input`
/// that lives at element depth 0 — i.e. NOT inside any open tag.
/// Tag bytes (`<…>`) and text inside open elements pass through
/// verbatim. Keeps each detector's wrapping intact when a later
/// detector runs.
///
/// Tag classification:
///   - `<X>` → depth++
///   - `</X>` → depth--
///   - `<X/>` (self-closing) → depth unchanged
fn replace_outside_tags<F>(input: &str, re: &Regex, mut replacer: F) -> String
where
    F: FnMut(&Captures<'_>) -> String,
{
    let mut out = String::with_capacity(input.len());
    let mut buf = String::new();
    let mut depth = 0i32;
    let mut chars = input.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '<' {
            if depth == 0 && !buf.is_empty() {
                out.push_str(&re.replace_all(&buf, |c: &Captures<'_>| replacer(c)));
                buf.clear();
            }
            let end = match input[i..].find('>') {
                Some(p) => i + p + 1,
                None => {
                    out.push_str(&input[i..]);
                    return out;
                }
            };
            let tag = &input[i..end];
            out.push_str(tag);
            let inner = &tag[1..tag.len() - 1];
            if inner.starts_with('/') {
                if depth > 0 {
                    depth -= 1;
                }
            } else if !inner.ends_with('/') {
                depth += 1;
            }
            // Advance the char iterator past the tag.
            while let Some(&(j, _)) = chars.peek() {
                if j >= end {
                    break;
                }
                chars.next();
            }
            continue;
        }
        if depth == 0 {
            buf.push(ch);
        } else {
            out.push(ch);
        }
    }
    if !buf.is_empty() {
        out.push_str(&re.replace_all(&buf, |c: &Captures<'_>| replacer(c)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_pause_translates() {
        assert_eq!(
            translate_markers("hola[pause=400ms]mundo"),
            r#"hola<break time="400ms"/>mundo"#
        );
    }

    #[test]
    fn marker_emphasis_translates() {
        // Emphasis is rendered as nested `<prosody>` rather than
        // `<emphasis>` because Edge's endpoint rejects the latter
        // when our text already sits inside the crate's prosody
        // wrapper.
        assert_eq!(
            translate_markers("[em]importante[/em] dato"),
            r#"<prosody volume="+15%" rate="-3%">importante</prosody> dato"#
        );
    }

    #[test]
    fn marker_spell_translates() {
        assert_eq!(
            translate_markers("la sigla [spell]SIC[/spell] es"),
            r#"la sigla <say-as interpret-as="characters">SIC</say-as> es"#
        );
    }

    #[test]
    fn auto_iso_date() {
        let out = auto_detect("La fecha es 2026-05-05 y listo.");
        assert!(out.contains(r#"<say-as interpret-as="date" format="ymd">2026-05-05</say-as>"#));
        // No nested say-as: the year 2026 must NOT also be wrapped
        // by the big-cardinal detector.
        assert!(!out.contains("<say-as interpret-as=\"cardinal\">2026"));
    }

    #[test]
    fn auto_currency_dollar_pesos() {
        let out = auto_detect("Cuesta $50.000 pesos");
        assert!(out.contains(r#"<say-as interpret-as="currency">$50.000</say-as>"#));
    }

    #[test]
    fn auto_big_cardinal() {
        let out = auto_detect("hubo 12345 visitas");
        assert!(out.contains(r#"<say-as interpret-as="cardinal">12345</say-as>"#));
        // 3-digit numbers stay alone — Edge reads "ciento veintitrés" fine.
        assert_eq!(auto_detect("hubo 123 visitas"), "hubo 123 visitas");
    }

    #[test]
    fn auto_acronym() {
        let out = auto_detect("contactá la SIC");
        assert!(out.contains(r#"<say-as interpret-as="characters">SIC</say-as>"#));
    }

    #[test]
    fn acronym_denylist_skips_common_words() {
        let out = auto_detect("BANCO de Bogotá");
        assert!(!out.contains("<say-as"));
    }

    #[test]
    fn auto_detect_skips_inside_existing_tags() {
        // Content between `<…>` should pass through untouched —
        // a marker that already produced SSML must not get
        // double-wrapped.
        let pre = r#"hola <break time="200ms"/> 2026-05-05 chau"#;
        let out = auto_detect(pre);
        // The break tag survives intact.
        assert!(out.contains(r#"<break time="200ms"/>"#));
        // The date outside the tag still gets wrapped.
        assert!(out.contains(r#"interpret-as="date""#));
    }

    #[test]
    fn markdown_bold_translates_to_strong_prosody() {
        let out = translate_markers("hola **mundo** chau");
        assert!(out.contains(r#"<prosody volume="+25%" rate="-7%" pitch="+8%">mundo</prosody>"#));
    }

    #[test]
    fn markdown_underline_bold_translates_to_strong_prosody() {
        let out = translate_markers("__importante__ aquí");
        assert!(out.contains(
            r#"<prosody volume="+25%" rate="-7%" pitch="+8%">importante</prosody>"#
        ));
    }

    #[test]
    fn markdown_italic_translates_to_moderate_prosody() {
        let out = translate_markers("hola *importante* chau");
        assert!(out.contains(r#"<prosody volume="+15%" rate="-3%">importante</prosody>"#));
    }

    #[test]
    fn markdown_does_not_match_inside_bold() {
        // `**X**` must produce exactly one wrapping prosody, not
        // also a leftover italic match.
        let out = translate_markers("**Buen nombre**");
        assert_eq!(out.matches("<prosody").count(), 1);
    }

    #[test]
    fn end_to_end_pipeline_ok() {
        let raw = "Pago $50.000 el 2026-05-05. [em]Importante[/em]: contactá la SIC.";
        let out = apply_ssml_hints(raw);
        assert!(out.contains("interpret-as=\"currency\""));
        assert!(out.contains("interpret-as=\"date\""));
        // `[em]` now renders as `<prosody volume="+15%" …>` to dodge
        // Edge's rejection of `<emphasis>` inside its hardcoded
        // prosody wrapper.
        assert!(out.contains(r#"<prosody volume="+15%""#));
        assert!(out.contains("interpret-as=\"characters\""));
    }
}
