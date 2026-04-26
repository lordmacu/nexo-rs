//! `PHASES.md` parser.
//!
//! Recognised shapes (column-0 lines, outside fenced code):
//!
//! ```text
//! ## Phase 67 — Driver subsystem
//! ### Phase 25 — Auto-fetch web pages and search   ✅
//! #### 67.9 — Compact opportunista                  ✅
//! ### 26.x — Pairing challenge reply via adapter    ✅
//! ```
//!
//! Status is detected from the trailing token on the heading line:
//! `✅` -> Done, `🔄` -> InProgress, `⬜`/missing -> Pending.
//! Everything between a sub-phase heading and the next heading at
//! depth `<= sub-phase depth` becomes its `body` (trimmed). Code
//! fences (``` and ~~~) are honoured so an embedded `#### ...` inside
//! a code block doesn't get parsed as a heading.

use std::path::Path;

use regex::Regex;

use crate::types::{Phase, PhaseStatus, SubPhase, TrackerError};

/// Top-level entry point. Reads the file, parses, returns phases in
/// document order.
pub fn parse_file(path: &Path) -> Result<Vec<Phase>, TrackerError> {
    if !path.exists() {
        return Err(TrackerError::NotTracked(path.to_path_buf()));
    }
    let raw = std::fs::read_to_string(path)?;
    parse_str(&raw).map_err(|msg| TrackerError::Parse {
        file: path.to_path_buf(),
        msg,
    })
}

/// Parse a string in-memory. Returned `Vec<Phase>` is in document
/// order; sub-phases preserve their declaration order too.
pub fn parse_str(raw: &str) -> Result<Vec<Phase>, String> {
    let phase_heading = Regex::new(
        r#"^(?P<hash>#{2,3})\s+Phase\s+(?P<id>\d+[a-zA-Z0-9.]*)\s+(?:—|-)\s+(?P<rest>.*?)\s*$"#,
    )
    .map_err(|e| e.to_string())?;
    let sub_heading = Regex::new(
        r#"^(?P<hash>#{3,4})\s+(?P<id>\d+\.[A-Za-z0-9.]+)\s+(?:—|-)\s+(?P<rest>.*?)\s*$"#,
    )
    .map_err(|e| e.to_string())?;

    let mut out: Vec<Phase> = Vec::new();
    let mut current_phase: Option<Phase> = None;
    let mut current_sub: Option<(SubPhase, Vec<String>)> = None;
    let mut in_fence = false;
    let mut fence_marker: Option<&'static str> = None;

    for line in raw.lines() {
        // Fence tracking — ``` or ~~~. Only the *opening* marker is
        // remembered so a `~~~` cannot accidentally close a ``` block.
        if !in_fence {
            if line.trim_start().starts_with("```") {
                in_fence = true;
                fence_marker = Some("```");
            } else if line.trim_start().starts_with("~~~") {
                in_fence = true;
                fence_marker = Some("~~~");
            }
        } else if let Some(m) = fence_marker {
            if line.trim_start().starts_with(m) {
                in_fence = false;
                fence_marker = None;
            }
            // Inside a fence, accumulate as body if we have an open
            // sub-phase, otherwise skip.
            if let Some((_, body)) = current_sub.as_mut() {
                body.push(line.to_string());
            }
            continue;
        }

        if in_fence {
            // Opening fence line itself.
            if let Some((_, body)) = current_sub.as_mut() {
                body.push(line.to_string());
            }
            continue;
        }

        // Phase heading?
        if let Some(c) = phase_heading.captures(line) {
            // Flush any open sub-phase into its phase.
            flush_sub(&mut current_phase, &mut current_sub);
            // Flush any open phase into output.
            if let Some(p) = current_phase.take() {
                out.push(p);
            }
            let id = c.name("id").unwrap().as_str().to_string();
            let rest = c.name("rest").unwrap().as_str();
            let (title, _status) = split_status(rest);
            current_phase = Some(Phase {
                id,
                title: title.trim().to_string(),
                sub_phases: Vec::new(),
            });
            continue;
        }

        // Sub-phase heading?
        if let Some(c) = sub_heading.captures(line) {
            flush_sub(&mut current_phase, &mut current_sub);
            let id = c.name("id").unwrap().as_str().to_string();
            let rest = c.name("rest").unwrap().as_str();
            let (title, status) = split_status(rest);

            // Auto-create a synthetic Phase if the sub-phase appears
            // before any `## Phase` heading — keeps misc top-of-file
            // sub-fases grouped instead of dropped.
            if current_phase.is_none() {
                let phase_id = id.split('.').next().unwrap_or(&id).to_string();
                current_phase = Some(Phase {
                    id: phase_id.clone(),
                    title: format!("Phase {phase_id}"),
                    sub_phases: Vec::new(),
                });
            }

            current_sub = Some((
                SubPhase {
                    id,
                    title: title.trim().to_string(),
                    status,
                    body: None,
                    acceptance: None,
                },
                Vec::new(),
            ));
            continue;
        }

        // Otherwise, accumulate as body for the current sub-phase.
        if let Some((_, body)) = current_sub.as_mut() {
            body.push(line.to_string());
        }
    }

    // Final flush.
    flush_sub(&mut current_phase, &mut current_sub);
    if let Some(p) = current_phase.take() {
        out.push(p);
    }

    Ok(out)
}

/// Split a heading's "rest" portion into (title, status). The status
/// is whatever trailing emoji marker we recognise; everything else is
/// treated as the title (stripped of trailing whitespace).
fn split_status(rest: &str) -> (String, PhaseStatus) {
    let trimmed = rest.trim_end();
    // Look at the last "word" — take everything after the last run of
    // whitespace.
    let last_ws = trimmed.rfind(|c: char| c.is_whitespace());
    let (title, tail) = match last_ws {
        Some(i) => (&trimmed[..i], trimmed[i + 1..].trim()),
        None => (trimmed, ""),
    };
    let status = match tail {
        "✅" => PhaseStatus::Done,
        "🔄" => PhaseStatus::InProgress,
        "⬜" => PhaseStatus::Pending,
        _ => return (trimmed.to_string(), PhaseStatus::Pending),
    };
    (title.trim_end().to_string(), status)
}

fn flush_sub(current_phase: &mut Option<Phase>, current_sub: &mut Option<(SubPhase, Vec<String>)>) {
    let Some((mut sub, body)) = current_sub.take() else {
        return;
    };
    let body_text = body.join("\n").trim().to_string();
    sub.acceptance = parse_acceptance_block(&body_text);
    sub.body = if body_text.is_empty() {
        None
    } else {
        Some(body_text)
    };
    if let Some(phase) = current_phase.as_mut() {
        phase.sub_phases.push(sub);
    }
}

/// Recognises three shapes of acceptance section in a sub-phase
/// body and returns each bullet as a shell command:
///
/// ```text
/// **Acceptance:**
/// - cargo build --workspace
/// - cargo test -p calc parser
/// ```
///
/// or
///
/// ```text
/// ### Acceptance
/// - cargo build
/// - cargo test
/// ```
///
/// or
///
/// ```text
/// Acceptance:
/// - cargo build
/// ```
///
/// Lines without a leading `-` / `*` after the marker stop the
/// block. Returns `None` when no marker was found OR no bullets
/// followed it.
pub(crate) fn parse_acceptance_block(body: &str) -> Option<Vec<String>> {
    // B21 — concatenate every Acceptance block in the body.
    // A sub-phase can ship multiple `Acceptance:` sections
    // (happy-path + edge-case) and all bullets count toward the
    // goal's verdict.
    let mut out: Vec<String> = Vec::new();
    let mut iter = body.lines().peekable();
    loop {
        let mut found_marker = false;
        while let Some(line) = iter.next() {
            let trimmed = line.trim();
            let lower = trimmed.to_ascii_lowercase();
            if lower == "**acceptance:**"
                || lower == "### acceptance"
                || lower == "## acceptance"
                || lower == "acceptance:"
                || lower.starts_with("**acceptance:**")
                || lower.starts_with("### acceptance")
            {
                found_marker = true;
                break;
            }
        }
        if !found_marker {
            break;
        }
        // Drain bullets that follow the marker.
        let mut block_collected_any = false;
        while let Some(line) = iter.peek() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                iter.next();
                if block_collected_any {
                    break;
                }
                continue;
            }
            let cmd = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
                .or_else(|| trimmed.strip_prefix("+ "));
            match cmd {
                Some(c) => {
                    let cleaned = c.trim().trim_matches('`').to_string();
                    if !cleaned.is_empty() {
                        out.push(cleaned);
                        block_collected_any = true;
                    }
                    iter.next();
                }
                None => {
                    // Non-bullet ends the current block; outer
                    // loop continues hunting for more Acceptance
                    // markers so additional sections still land.
                    if block_collected_any {
                        break;
                    }
                    iter.next();
                }
            }
        }
    }
    if !out.is_empty() {
        return Some(out);
    }
    // Inline shorthand fallback: `Acceptance: <one-line cmd>`.
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("Acceptance:")
            .or_else(|| trimmed.strip_prefix("acceptance:"))
        {
            let cmd = rest.trim().trim_matches('`').to_string();
            if !cmd.is_empty() {
                return Some(vec![cmd]);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_phase_with_subphases() {
        let md = "\
## Phase 67 — Driver subsystem

#### 67.0 — Foundational types   ✅
Body line one.
Body line two.

#### 67.1 — Spawn skill   ✅
#### 67.2 — Pending work   ⬜
";
        let phases = parse_str(md).unwrap();
        assert_eq!(phases.len(), 1);
        let p = &phases[0];
        assert_eq!(p.id, "67");
        assert_eq!(p.title, "Driver subsystem");
        assert_eq!(p.sub_phases.len(), 3);
        assert_eq!(p.sub_phases[0].id, "67.0");
        assert_eq!(p.sub_phases[0].status, PhaseStatus::Done);
        assert!(p.sub_phases[0]
            .body
            .as_deref()
            .unwrap()
            .contains("Body line one."));
        assert_eq!(p.sub_phases[2].status, PhaseStatus::Pending);
    }

    #[test]
    fn fenced_code_blocks_dont_create_phantom_subphases() {
        let md = "\
## Phase 1 — Test

#### 1.1 — Real sub   ✅

```rust
#### 1.2 — Fake sub inside code   ✅
```

#### 1.3 — Another real sub   ⬜
";
        let phases = parse_str(md).unwrap();
        assert_eq!(phases[0].sub_phases.len(), 2);
        assert_eq!(phases[0].sub_phases[0].id, "1.1");
        assert_eq!(phases[0].sub_phases[1].id, "1.3");
    }

    #[test]
    fn level_3_phase_heading_works_too() {
        let md = "\
### Phase 25 — Web search   ✅

#### 25.1 — Tavily   ✅
";
        let phases = parse_str(md).unwrap();
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].id, "25");
        assert_eq!(phases[0].sub_phases[0].status, PhaseStatus::Done);
    }

    #[test]
    fn subphase_before_any_phase_heading_gets_synthetic_parent() {
        let md = "\
#### 26.x — Misc subphase   ✅
";
        let phases = parse_str(md).unwrap();
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].id, "26");
        assert_eq!(phases[0].sub_phases.len(), 1);
    }

    #[test]
    fn missing_status_marker_is_pending() {
        let md = "\
## Phase 99 — X

#### 99.1 — No marker
";
        let phases = parse_str(md).unwrap();
        assert_eq!(phases[0].sub_phases[0].status, PhaseStatus::Pending);
    }

    #[test]
    fn acceptance_block_with_bold_marker() {
        let md = "\
## Phase 1 — X

#### 1.1 — Foo   ⬜

Body lorem ipsum.

**Acceptance:**
- cargo build --workspace
- cargo test -p calc parser
";
        let phases = parse_str(md).unwrap();
        let acc = phases[0].sub_phases[0].acceptance.as_ref().unwrap();
        assert_eq!(acc.len(), 2);
        assert_eq!(acc[0], "cargo build --workspace");
        assert_eq!(acc[1], "cargo test -p calc parser");
    }

    #[test]
    fn acceptance_block_with_heading_marker() {
        let md = "\
## Phase 1 — X

#### 1.1 — Foo   ⬜

Body.

### Acceptance

- cargo build
- ./scripts/smoke.sh
";
        let phases = parse_str(md).unwrap();
        let acc = phases[0].sub_phases[0].acceptance.as_ref().unwrap();
        assert_eq!(acc, &vec!["cargo build", "./scripts/smoke.sh"]);
    }

    #[test]
    fn acceptance_inline_one_liner() {
        let md = "\
## Phase 1 — X

#### 1.1 — Foo   ⬜

Body.

Acceptance: `cargo test -p calc`.
";
        let phases = parse_str(md).unwrap();
        let acc = phases[0].sub_phases[0].acceptance.as_ref().unwrap();
        assert_eq!(acc.len(), 1);
        assert!(acc[0].starts_with("cargo test -p calc"));
    }

    #[test]
    fn no_acceptance_block_returns_none() {
        let md = "\
## Phase 1 — X

#### 1.1 — Foo   ⬜

Body without an acceptance section.
";
        let phases = parse_str(md).unwrap();
        assert!(phases[0].sub_phases[0].acceptance.is_none());
    }

    #[test]
    fn acceptance_strips_trailing_period_from_inline() {
        // The inline shorthand keeps the trailing period today —
        // operators write `Acceptance: cargo test.` and we'd get
        // `cargo test.` which is still a runnable shell command.
        // Document the behaviour so the parser stays predictable.
        let md = "\
## Phase 1 — X

#### 1.1 — Foo   ⬜

Acceptance: cargo test.
";
        let phases = parse_str(md).unwrap();
        let acc = phases[0].sub_phases[0].acceptance.as_ref().unwrap();
        assert!(acc[0].starts_with("cargo test"));
    }
}
