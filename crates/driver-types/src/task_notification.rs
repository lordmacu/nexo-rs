//! Phase 84.2 — `<task-notification>` XML envelope for worker results.
//!
//! When a forked subagent (`nexo-fork`) or a TeamCreate worker
//! completes, the coordinator receives the result. Pre-Phase 84.2,
//! that result was either appended as plain text or returned via the
//! tool-call response path. Phase 84.2 standardizes a single XML
//! envelope so the coordinator's parser (LLM or downstream tool) can
//! match deterministically and ignore free-form chatter.
//!
//! # Schema
//!
//! ```xml
//! <task-notification>
//! <task-id>{worker_goal_id}</task-id>
//! <status>completed|failed|killed|timeout</status>
//! <summary>{one-line outcome}</summary>
//! <result>{worker's final assistant text — optional}</result>
//! <usage>
//!   <total_tokens>{N}</total_tokens>
//!   <tool_uses>{N}</tool_uses>
//!   <duration_ms>{N}</duration_ms>
//! </usage>
//! </task-notification>
//! ```
//!
//! Optional fields (`<result>`, `<usage>`) are omitted when the
//! corresponding source data is absent — the coordinator persona
//! treats absent elements as "no data", not as zero.
//!
//! # Backwards compatibility
//!
//! [`TaskNotification::parse_block`] returns `None` when the input
//! lacks a `<task-notification>` opening tag; the caller falls back
//! to treating the input as plain text. This means legacy fork
//! consumers that read the raw final assistant text keep working
//! during the rollout window.

use serde::{Deserialize, Serialize};

/// Finite-state outcome of a worker run. The variant set is stable —
/// new states require both an XML-vocabulary version bump and a
/// coordinator persona prompt update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    /// Worker reached its goal and produced a final assistant text.
    Completed,
    /// Worker hit an unrecoverable error before producing a final
    /// answer. `result` may still carry partial output.
    Failed,
    /// External cancellation (user, operator, or coordinator
    /// `TaskStop`).
    Killed,
    /// Worker exceeded its budget (turns / tokens / wall clock).
    Timeout,
}

impl TaskStatus {
    /// Wire form used inside `<status>` element body.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Killed => "killed",
            TaskStatus::Timeout => "timeout",
        }
    }

    /// Reverse of [`as_wire_str`]. Case-insensitive on parse to
    /// tolerate model-side typos in any future hand-rolled emitter.
    pub fn from_wire_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "completed" => Some(TaskStatus::Completed),
            "failed" => Some(TaskStatus::Failed),
            "killed" => Some(TaskStatus::Killed),
            "timeout" => Some(TaskStatus::Timeout),
            _ => None,
        }
    }
}

/// Usage telemetry rolled up from the worker's turn loop.
///
/// Fields are non-optional because the producer (fork / TeamCreate
/// completion) always knows them. To omit usage entirely from the
/// rendered envelope, set [`TaskNotification::usage`] to `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskUsage {
    pub total_tokens: u64,
    pub tool_uses: u64,
    pub duration_ms: u64,
}

/// One worker → coordinator notification block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskNotification {
    /// Worker goal id — same value the coordinator passes to
    /// `SendMessageToWorker` (Phase 84.3) when continuing.
    pub task_id: String,
    pub status: TaskStatus,
    /// One-line outcome for the coordinator's synthesis. Always
    /// present; producers default to `"<status_word>"` when no
    /// richer summary is available.
    pub summary: String,
    /// Worker's final assistant text. `None` when the worker
    /// produced no text (failure before first output, or killed
    /// mid-tool-call).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Optional rolled-up usage. `None` collapses the entire
    /// `<usage>` element out of the rendered XML.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TaskUsage>,
}

impl TaskNotification {
    /// Render the notification as the canonical `<task-notification>`
    /// XML block. Inner text is XML-escaped; the rendered string is
    /// safe to embed verbatim in an LLM message.
    pub fn to_xml(&self) -> String {
        let mut out = String::with_capacity(256);
        out.push_str("<task-notification>\n");
        push_element(&mut out, "task-id", &self.task_id);
        push_element(&mut out, "status", self.status.as_wire_str());
        push_element(&mut out, "summary", &self.summary);
        if let Some(result) = self.result.as_deref() {
            push_element(&mut out, "result", result);
        }
        if let Some(usage) = &self.usage {
            out.push_str("<usage>\n");
            push_element(&mut out, "  total_tokens", &usage.total_tokens.to_string());
            push_element(&mut out, "  tool_uses", &usage.tool_uses.to_string());
            push_element(&mut out, "  duration_ms", &usage.duration_ms.to_string());
            out.push_str("</usage>\n");
        }
        out.push_str("</task-notification>");
        out
    }

    /// Try to parse a `<task-notification>` block out of the given
    /// text. Returns `None` when the opening tag is missing — that
    /// signals the caller to fall back to treating the text as plain
    /// (legacy / non-envelope) output.
    ///
    /// Best-effort lenient parser: tolerates leading/trailing
    /// whitespace and out-of-order child elements. Element bodies
    /// are XML-unescaped.
    pub fn parse_block(text: &str) -> Option<Self> {
        let open = text.find("<task-notification>")?;
        let after_open = open + "<task-notification>".len();
        let close = text[after_open..].find("</task-notification>")?;
        let body = &text[after_open..after_open + close];

        let task_id = extract_element(body, "task-id")?;
        let status_str = extract_element(body, "status")?;
        let status = TaskStatus::from_wire_str(&status_str)?;
        let summary = extract_element(body, "summary").unwrap_or_default();
        let result = extract_element(body, "result");
        let usage = parse_usage(body);

        Some(TaskNotification {
            task_id,
            status,
            summary,
            result,
            usage,
        })
    }
}

fn push_element(out: &mut String, name: &str, body: &str) {
    out.push('<');
    out.push_str(name.trim_start());
    out.push('>');
    out.push_str(&xml_escape(body));
    out.push_str("</");
    out.push_str(name.trim_start());
    out.push_str(">\n");
}

/// Conservative XML escape — handles the five canonical entities so
/// the renderer is safe regardless of body contents (model output
/// commonly contains `<`, `>`, `&` in code fragments).
fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Reverse of `xml_escape` for the five canonical entities. Other
/// numeric / hex / named entities are passed through unchanged —
/// our renderer never produces them, so encountering one means
/// either pre-existing user content (preserve as-is) or a
/// non-conforming external producer (best-effort).
fn xml_unescape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some(end) = input[i..].find(';') {
                let entity = &input[i..=i + end];
                let replacement = match entity {
                    "&lt;" => Some('<'),
                    "&gt;" => Some('>'),
                    "&amp;" => Some('&'),
                    "&quot;" => Some('"'),
                    "&apos;" => Some('\''),
                    _ => None,
                };
                if let Some(c) = replacement {
                    out.push(c);
                    i += end + 1;
                    continue;
                }
            }
        }
        out.push(input[i..].chars().next().unwrap());
        i += input[i..].chars().next().unwrap().len_utf8();
    }
    out
}

fn extract_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end_rel = body[start..].find(&close)?;
    Some(xml_unescape(body[start..start + end_rel].trim()))
}

fn parse_usage(body: &str) -> Option<TaskUsage> {
    let usage_open = body.find("<usage>")? + "<usage>".len();
    let usage_close_rel = body[usage_open..].find("</usage>")?;
    let inner = &body[usage_open..usage_open + usage_close_rel];
    Some(TaskUsage {
        total_tokens: extract_element(inner, "total_tokens")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        tool_uses: extract_element(inner, "tool_uses")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        duration_ms: extract_element(inner, "duration_ms")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_completed() -> TaskNotification {
        TaskNotification {
            task_id: "goal-9f3a".into(),
            status: TaskStatus::Completed,
            summary: "Found 3 candidate fixes".into(),
            result: Some("See `crates/auth.rs:142`.".into()),
            usage: Some(TaskUsage {
                total_tokens: 1280,
                tool_uses: 4,
                duration_ms: 12_400,
            }),
        }
    }

    #[test]
    fn completed_with_result_round_trips() {
        let n = sample_completed();
        let xml = n.to_xml();
        assert!(xml.contains("<task-notification>"));
        assert!(xml.contains("<task-id>goal-9f3a</task-id>"));
        assert!(xml.contains("<status>completed</status>"));
        assert!(xml.contains("<summary>Found 3 candidate fixes</summary>"));
        assert!(xml.contains("<result>See `crates/auth.rs:142`.</result>"));
        assert!(xml.contains("<usage>"));
        assert!(xml.contains("<total_tokens>1280</total_tokens>"));
        assert!(xml.contains("<tool_uses>4</tool_uses>"));
        assert!(xml.contains("<duration_ms>12400</duration_ms>"));

        let parsed = TaskNotification::parse_block(&xml).expect("round-trip");
        assert_eq!(parsed, n);
    }

    #[test]
    fn failed_no_result_omits_result_element() {
        let n = TaskNotification {
            task_id: "goal-bad".into(),
            status: TaskStatus::Failed,
            summary: "DB connection refused".into(),
            result: None,
            usage: None,
        };
        let xml = n.to_xml();
        assert!(xml.contains("<status>failed</status>"));
        assert!(!xml.contains("<result>"));
        assert!(!xml.contains("<usage>"));
        let parsed = TaskNotification::parse_block(&xml).expect("round-trip");
        assert_eq!(parsed, n);
    }

    #[test]
    fn killed_mid_run_with_xml_special_chars_escaped() {
        let n = TaskNotification {
            task_id: "goal-kill".into(),
            status: TaskStatus::Killed,
            // Special chars in summary AND result must be escaped so
            // the XML stays well-formed.
            summary: "Stopped at <BashTool> & <FileEdit> calls".into(),
            result: Some("if x < 0 && y > 1 { panic!() }".into()),
            usage: None,
        };
        let xml = n.to_xml();
        assert!(xml.contains("<status>killed</status>"));
        // Special chars must be escaped in body text.
        assert!(xml.contains("&lt;BashTool&gt;"));
        assert!(xml.contains("&amp;"));
        assert!(xml.contains("if x &lt; 0 &amp;&amp; y &gt; 1"));
        // Outer envelope tags must not be escaped.
        assert!(xml.starts_with("<task-notification>"));
        assert!(xml.ends_with("</task-notification>"));
        // Round-trip restores the original strings.
        let parsed = TaskNotification::parse_block(&xml).expect("round-trip");
        assert_eq!(parsed, n);
    }

    #[test]
    fn timeout_status_round_trips() {
        let n = TaskNotification {
            task_id: "goal-slow".into(),
            status: TaskStatus::Timeout,
            summary: "Wall-clock budget exceeded".into(),
            result: None,
            usage: Some(TaskUsage {
                total_tokens: 8_192,
                tool_uses: 12,
                duration_ms: 600_000,
            }),
        };
        let xml = n.to_xml();
        assert!(xml.contains("<status>timeout</status>"));
        let parsed = TaskNotification::parse_block(&xml).expect("round-trip");
        assert_eq!(parsed, n);
    }

    #[test]
    fn parse_block_returns_none_for_plain_text() {
        // Backwards-compat: legacy fork output without the envelope
        // returns None so callers fall back to raw text.
        let plain = "I finished the research, here are the results...";
        assert!(TaskNotification::parse_block(plain).is_none());
    }

    #[test]
    fn parse_block_extracts_from_surrounding_text() {
        let n = sample_completed();
        let wrapped = format!(
            "Some preamble.\n\n{}\n\nTrailing chatter.",
            n.to_xml()
        );
        let parsed = TaskNotification::parse_block(&wrapped).expect("inline parse");
        assert_eq!(parsed, n);
    }

    #[test]
    fn parse_block_tolerates_unknown_status() {
        // A future status word ("crashed") rendered by an
        // out-of-version producer should fail soft (None), not
        // panic. Caller treats it as legacy text.
        let xml = "<task-notification>\
<task-id>x</task-id>\
<status>crashed</status>\
<summary>?</summary>\
</task-notification>";
        assert!(TaskNotification::parse_block(xml).is_none());
    }

    #[test]
    fn xml_escape_handles_all_five_entities() {
        let raw = r#"<&>"'"#;
        let escaped = xml_escape(raw);
        assert_eq!(escaped, "&lt;&amp;&gt;&quot;&apos;");
        assert_eq!(xml_unescape(&escaped), raw);
    }

    #[test]
    fn task_status_wire_round_trip() {
        for s in [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Killed,
            TaskStatus::Timeout,
        ] {
            let wire = s.as_wire_str();
            assert_eq!(TaskStatus::from_wire_str(wire), Some(s));
            // Case insensitivity.
            assert_eq!(
                TaskStatus::from_wire_str(&wire.to_ascii_uppercase()),
                Some(s)
            );
        }
        assert_eq!(TaskStatus::from_wire_str("nonsense"), None);
    }

    #[test]
    fn serde_round_trip_via_json() {
        let n = sample_completed();
        let json = serde_json::to_string(&n).expect("serialize");
        let back: TaskNotification =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, n);
    }

    #[test]
    fn serde_skips_none_optionals() {
        let n = TaskNotification {
            task_id: "x".into(),
            status: TaskStatus::Failed,
            summary: "boom".into(),
            result: None,
            usage: None,
        };
        let json = serde_json::to_string(&n).expect("serialize");
        assert!(!json.contains("\"result\""));
        assert!(!json.contains("\"usage\""));
    }
}
