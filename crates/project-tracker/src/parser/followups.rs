//! `FOLLOWUPS.md` parser.
//!
//! Recognised shape (column-0 lines, outside fenced code):
//!
//! ```text
//! ### Phase 26 — Pairing protocol
//!
//! PR-1. ~~**Plugin gate hooks for WhatsApp + Telegram**~~  ✅ shipped (...)
//! - body bullet
//! - body bullet
//!
//! PR-3. **`tunnel.url` integration in URL resolver**
//! - Missing: ...
//! ```
//!
//! Item id pattern: `[A-Z][A-Z0-9-]*\.\d+(?:\.\d+)?`. Title may be
//! wrapped in `~~ ~~` (strikethrough → resolved) and / or carry a
//! `✅` somewhere on the heading line (also resolved).
//!
//! Body = every subsequent non-section, non-item line until the next
//! item heading or section heading, trimmed.

use std::path::Path;

use regex::Regex;

use crate::types::{FollowUp, FollowUpStatus, TrackerError};

pub fn parse_file(path: &Path) -> Result<Vec<FollowUp>, TrackerError> {
    if !path.exists() {
        return Err(TrackerError::NotTracked(path.to_path_buf()));
    }
    let raw = std::fs::read_to_string(path)?;
    parse_str(&raw).map_err(|msg| TrackerError::Parse {
        file: path.to_path_buf(),
        msg,
    })
}

pub fn parse_str(raw: &str) -> Result<Vec<FollowUp>, String> {
    // Section heading: `## Foo` or `### Foo` (must NOT be `## Open
    // items` style top-level "rules" sections — we still capture
    // them, the caller can filter by section name if needed).
    let section_re = Regex::new(r#"^(?P<hash>#{2,3})\s+(?P<title>.+?)\s*$"#)
        .map_err(|e| e.to_string())?;
    // Item heading: `<CODE>. <title>` where code is uppercase letters
    // + digits, possibly with `-` and `.` separators.
    let item_re = Regex::new(
        r#"^(?P<code>[A-Z][A-Z0-9]*-[A-Z0-9]+(?:\.[A-Za-z0-9]+)?)\.\s+(?P<rest>.*?)\s*$"#,
    )
    .map_err(|e| e.to_string())?;

    let mut out: Vec<FollowUp> = Vec::new();
    let mut current_section: String = String::new();
    let mut current_item: Option<(FollowUp, Vec<String>)> = None;
    let mut in_fence = false;
    let mut fence_marker: Option<&'static str> = None;

    for line in raw.lines() {
        if !in_fence {
            if line.trim_start().starts_with("```") {
                in_fence = true;
                fence_marker = Some("```");
            } else if line.trim_start().starts_with("~~~") && !line.contains("~~**") {
                // `~~**title**~~` is strikethrough markdown, NOT a fence.
                in_fence = true;
                fence_marker = Some("~~~");
            }
        } else if let Some(m) = fence_marker {
            if line.trim_start().starts_with(m) {
                in_fence = false;
                fence_marker = None;
            }
            if let Some((_, body)) = current_item.as_mut() {
                body.push(line.to_string());
            }
            continue;
        }

        if in_fence {
            if let Some((_, body)) = current_item.as_mut() {
                body.push(line.to_string());
            }
            continue;
        }

        if let Some(c) = section_re.captures(line) {
            // Flush any open item.
            flush_item(&mut current_item, &mut out);
            current_section = c.name("title").unwrap().as_str().trim().to_string();
            continue;
        }

        if let Some(c) = item_re.captures(line) {
            flush_item(&mut current_item, &mut out);
            let code = c.name("code").unwrap().as_str().to_string();
            let rest = c.name("rest").unwrap().as_str();
            let (title, status) = parse_item_title(rest);
            current_item = Some((
                FollowUp {
                    code,
                    title,
                    section: current_section.clone(),
                    status,
                    body: String::new(),
                },
                Vec::new(),
            ));
            continue;
        }

        if let Some((_, body)) = current_item.as_mut() {
            body.push(line.to_string());
        }
    }
    flush_item(&mut current_item, &mut out);
    Ok(out)
}

/// Strip strikethrough wrappers + ✅ markers. Returns the cleaned
/// title and the resolved/open status.
fn parse_item_title(rest: &str) -> (String, FollowUpStatus) {
    let trimmed = rest.trim();
    let strikethrough = trimmed.starts_with("~~");
    let has_check = trimmed.contains('✅');
    let status = if strikethrough || has_check {
        FollowUpStatus::Resolved
    } else {
        FollowUpStatus::Open
    };

    // Best-effort title cleanup: drop leading `**` / `~~`, drop the
    // trailing "  ✅ ..." section. We only need a sane title for
    // display; the full original line is preserved in `body` if a
    // consumer wants the suffix.
    let mut t = trimmed.to_string();
    if let Some(idx) = t.find('✅') {
        t.truncate(idx);
    }
    let t = t
        .trim()
        .trim_start_matches("~~")
        .trim_end_matches("~~")
        .trim()
        .trim_start_matches("**")
        .trim_end_matches("**")
        .trim()
        .trim_end_matches("~~")
        .trim_end_matches("**")
        .to_string();

    (t, status)
}

fn flush_item(
    current: &mut Option<(FollowUp, Vec<String>)>,
    out: &mut Vec<FollowUp>,
) {
    let Some((mut item, body)) = current.take() else {
        return;
    };
    item.body = body.join("\n").trim().to_string();
    out.push(item);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_and_resolved_items() {
        let md = "\
### Phase 21 — Link understanding

L-1. ~~**Telemetry counters for link fetches**~~  ✅ shipped
- counter A
- counter B

L-2. **`readability`-style extraction**
- Missing: ...
- Why deferred: ...
";
        let items = parse_str(md).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].code, "L-1");
        assert_eq!(items[0].status, FollowUpStatus::Resolved);
        assert_eq!(items[0].section, "Phase 21 — Link understanding");
        assert!(items[0].body.contains("counter A"));
        assert_eq!(items[1].code, "L-2");
        assert_eq!(items[1].status, FollowUpStatus::Open);
        assert!(items[1].title.contains("readability"));
    }

    #[test]
    fn fenced_code_does_not_split_items() {
        let md = "\
### Section

X-1. **Title**
- before fence
```
P-9. fake item inside code
```
- after fence
";
        let items = parse_str(md).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].body.contains("after fence"));
    }

    #[test]
    fn item_with_subcode_is_parsed() {
        let md = "\
### Phase 26 — Pairing

PR-1.1. ~~**Challenge reply through channel adapter**~~  ✅ shipped
- body
";
        let items = parse_str(md).unwrap();
        assert_eq!(items[0].code, "PR-1.1");
        assert_eq!(items[0].status, FollowUpStatus::Resolved);
    }

    #[test]
    fn checkmark_without_strikethrough_still_resolved() {
        let md = "\
### S

A-1. **Title** ✅ done
- body
";
        let items = parse_str(md).unwrap();
        assert_eq!(items[0].status, FollowUpStatus::Resolved);
    }
}
