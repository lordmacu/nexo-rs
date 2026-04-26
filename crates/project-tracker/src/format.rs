//! Markdown rendering helpers shared by the read tools. Outputs are
//! capped to a configurable byte budget so a Telegram / WhatsApp
//! adapter never has to truncate mid-line.

use crate::types::{FollowUp, FollowUpStatus, Phase, PhaseStatus, SubPhase};

/// Default cap — Telegram and WhatsApp both bound text messages near
/// 4096 chars. Leaving headroom for adapter wrappers.
pub const DEFAULT_BYTE_CAP: usize = 3_500;

/// Truncate `s` to at most `cap` bytes on a UTF-8 boundary, appending
/// `…` if truncation actually happened.
pub fn cap_to(mut s: String, cap: usize) -> String {
    if s.len() <= cap {
        return s;
    }
    let mut end = cap.saturating_sub(3);
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push('…');
    s
}

pub fn status_glyph(s: PhaseStatus) -> &'static str {
    match s {
        PhaseStatus::Done => "✅",
        PhaseStatus::InProgress => "🔄",
        PhaseStatus::Pending => "⬜",
    }
}

/// Single-sub-phase one-liner. Useful as a row inside a markdown
/// table or as a stand-alone reply for `current_phase`.
pub fn render_subphase_line(s: &SubPhase) -> String {
    format!("{} **{}** — {}", status_glyph(s.status), s.id, s.title)
}

pub fn render_subphase_detail(s: &SubPhase) -> String {
    let mut out = format!(
        "{} **{}** — {}\n",
        status_glyph(s.status),
        s.id,
        s.title
    );
    if let Some(body) = &s.body {
        out.push('\n');
        out.push_str(body);
    }
    cap_to(out, DEFAULT_BYTE_CAP)
}

pub fn render_phases_table(phases: &[Phase], filter: Option<PhaseStatus>) -> String {
    let mut out = String::from("| id | status | title |\n|---|---|---|\n");
    for p in phases {
        for s in &p.sub_phases {
            if let Some(f) = filter {
                if s.status != f {
                    continue;
                }
            }
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                s.id,
                status_glyph(s.status),
                s.title.replace('|', "\\|"),
            ));
        }
    }
    cap_to(out, DEFAULT_BYTE_CAP)
}

pub fn render_followups_open(items: &[FollowUp]) -> String {
    let open: Vec<&FollowUp> = items
        .iter()
        .filter(|i| i.status == FollowUpStatus::Open)
        .collect();
    if open.is_empty() {
        return "no open follow-ups".into();
    }
    let mut out = String::new();
    for i in open {
        out.push_str(&format!("- **{}** [{}] — {}\n", i.code, i.section, i.title));
    }
    cap_to(out, DEFAULT_BYTE_CAP)
}

pub fn render_followup_detail(item: &FollowUp) -> String {
    let header = match item.status {
        FollowUpStatus::Open => "Open",
        FollowUpStatus::Resolved => "Resolved",
    };
    let mut out = format!(
        "**{}** ({}) — {}\n_{}_\n\n{}",
        item.code, header, item.title, item.section, item.body
    );
    if out.is_empty() {
        out = "(empty)".into();
    }
    cap_to(out, DEFAULT_BYTE_CAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_truncates_on_utf8_boundary() {
        let s = "ñ".repeat(20); // 40 bytes
        let capped = cap_to(s.clone(), 10);
        assert!(capped.len() <= 10);
        assert!(capped.ends_with('…'));
        // Round-trip parse must not panic.
        let _ = capped.chars().count();
    }

    #[test]
    fn cap_passthrough_when_under() {
        let s = "short";
        let capped = cap_to(s.into(), 100);
        assert_eq!(capped, "short");
    }

    #[test]
    fn render_phases_table_has_header() {
        let p = Phase {
            id: "67".into(),
            title: "X".into(),
            sub_phases: vec![SubPhase {
                id: "67.1".into(),
                title: "T".into(),
                status: PhaseStatus::Done,
                body: None,
            acceptance: None,
            }],
        };
        let out = render_phases_table(std::slice::from_ref(&p), None);
        assert!(out.starts_with("| id |"));
        assert!(out.contains("67.1"));
        assert!(out.contains("✅"));
    }

    #[test]
    fn render_followups_open_skips_resolved() {
        let items = vec![
            FollowUp {
                code: "A-1".into(),
                title: "Open one".into(),
                section: "S".into(),
                status: FollowUpStatus::Open,
                body: "b".into(),
            },
            FollowUp {
                code: "A-2".into(),
                title: "Resolved one".into(),
                section: "S".into(),
                status: FollowUpStatus::Resolved,
                body: "b".into(),
            },
        ];
        let out = render_followups_open(&items);
        assert!(out.contains("A-1"));
        assert!(!out.contains("A-2"));
    }
}
