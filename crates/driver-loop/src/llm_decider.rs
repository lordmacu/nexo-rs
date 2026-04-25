//! `LlmDecider` — production `PermissionDecider` that consults
//! a `LlmClient` (typically MiniMax). 67.4 wires this; 67.7 will
//! plug a real `DecisionMemory` for semantic recall.

use std::sync::Arc;

use async_trait::async_trait;
use nexo_driver_permission::{
    PermissionDecider, PermissionError, PermissionOutcome, PermissionRequest, PermissionResponse,
};
use nexo_driver_types::Decision;
use nexo_llm::{ChatMessage, ChatRequest, LlmClient, ResponseContent};
use serde::Deserialize;

use crate::memory::{DecisionMemory, NoopDecisionMemory};

const DEFAULT_SYSTEM_PROMPT: &str = "\
You are nexo-driver, the human-in-the-loop for a Claude Code agent.\n\
For each tool call Claude proposes, decide allow / allow_session / deny.\n\
Respond with JSON only, matching this schema:\n\
{ \"outcome\": \"allow_once\" | \"allow_session\" | \"deny\",\n\
  \"scope\":   \"turn\" | \"session\"   (required iff outcome == allow_session),\n\
  \"message\": \"<= 200 chars\"        (required iff outcome == deny),\n\
  \"rationale\": \"<= 200 chars\" }\n\
\n\
Safety rules:\n\
- Reject destructive shell (rm -rf, dd, mkfs, kill -9 1, format).\n\
- Reject network mutations outside the goal scope.\n\
- Reject reads of common secret paths (~/.ssh, /etc/shadow, .env).\n\
- Reject Edit/Write that touches paths outside the workspace.\n\
- Prefer denying when uncertain.\n";

pub struct LlmDecider {
    llm: Arc<dyn LlmClient>,
    model: String,
    max_tokens: u32,
    system_prompt: String,
    memory: Arc<dyn DecisionMemory>,
    recall_k: usize,
}

pub struct LlmDeciderBuilder {
    llm: Option<Arc<dyn LlmClient>>,
    model: Option<String>,
    max_tokens: u32,
    system_prompt: Option<String>,
    memory: Arc<dyn DecisionMemory>,
    recall_k: usize,
}

fn default_memory() -> Arc<dyn DecisionMemory> {
    Arc::new(NoopDecisionMemory)
}

impl Default for LlmDeciderBuilder {
    fn default() -> Self {
        Self {
            llm: None,
            model: None,
            max_tokens: 256,
            system_prompt: None,
            memory: default_memory(),
            recall_k: 5,
        }
    }
}

impl LlmDecider {
    pub fn builder() -> LlmDeciderBuilder {
        LlmDeciderBuilder::default()
    }
}

impl LlmDeciderBuilder {
    pub fn llm(mut self, llm: Arc<dyn LlmClient>) -> Self {
        self.llm = Some(llm);
        self
    }
    pub fn model(mut self, m: impl Into<String>) -> Self {
        self.model = Some(m.into());
        self
    }
    pub fn max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }
    pub fn system_prompt(mut self, s: impl Into<String>) -> Self {
        self.system_prompt = Some(s.into());
        self
    }
    pub fn memory(mut self, m: Arc<dyn DecisionMemory>) -> Self {
        self.memory = m;
        self
    }
    pub fn recall_k(mut self, k: usize) -> Self {
        self.recall_k = k;
        self
    }
    pub fn build(self) -> Result<LlmDecider, &'static str> {
        Ok(LlmDecider {
            llm: self.llm.ok_or("llm client is required")?,
            model: self.model.ok_or("model id is required")?,
            max_tokens: self.max_tokens,
            system_prompt: self
                .system_prompt
                .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string()),
            memory: self.memory,
            recall_k: self.recall_k,
        })
    }
}

#[derive(Debug, Deserialize)]
struct DeciderJson {
    outcome: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    rationale: Option<String>,
    #[serde(default)]
    updated_input: Option<serde_json::Value>,
}

fn parse_outcome(json: &str) -> PermissionOutcome {
    let stripped = strip_markdown_fences(json);
    let parsed: Result<DeciderJson, _> = serde_json::from_str(stripped.trim());
    let Ok(d) = parsed else {
        return PermissionOutcome::Deny {
            message: "decider returned invalid response".into(),
        };
    };
    match d.outcome.as_str() {
        "allow_once" => PermissionOutcome::AllowOnce {
            updated_input: d.updated_input,
        },
        "allow_session" => {
            let scope = match d.scope.as_deref() {
                Some("session") => nexo_driver_permission::AllowScope::Session,
                _ => nexo_driver_permission::AllowScope::Turn,
            };
            PermissionOutcome::AllowSession {
                scope,
                updated_input: d.updated_input,
            }
        }
        "deny" => PermissionOutcome::Deny {
            message: d
                .message
                .unwrap_or_else(|| "denied (no reason given)".into()),
        },
        _ => PermissionOutcome::Deny {
            message: "decider returned unknown outcome".into(),
        },
    }
}

fn strip_markdown_fences(s: &str) -> String {
    // Look for ```<lang>\n...\n``` block; return the inner content.
    let trimmed = s.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        // Skip optional language hint up to newline.
        let after_lang = match rest.find('\n') {
            Some(i) => &rest[i + 1..],
            None => rest,
        };
        if let Some(end) = after_lang.rfind("```") {
            return after_lang[..end].trim().to_string();
        }
    }
    trimmed.to_string()
}

fn rationale_from(d: &DeciderJson) -> String {
    d.rationale.clone().unwrap_or_default()
}

#[async_trait]
impl PermissionDecider for LlmDecider {
    async fn decide(
        &self,
        request: PermissionRequest,
    ) -> Result<PermissionResponse, PermissionError> {
        let recalled: Vec<Decision> = self.memory.recall(&request, self.recall_k).await;
        let user_prompt = build_user_prompt(&request, &recalled);
        let req = ChatRequest {
            system_prompt: Some(self.system_prompt.clone()),
            ..ChatRequest::new(self.model.clone(), vec![ChatMessage::user(user_prompt)])
        };
        let mut req = req;
        req.max_tokens = self.max_tokens;
        req.temperature = 0.0;
        let resp = self
            .llm
            .chat(req)
            .await
            .map_err(|e| PermissionError::Decider(e.to_string()))?;
        let text = match resp.content {
            ResponseContent::Text(t) => t,
            ResponseContent::ToolCalls(_) => {
                return Ok(PermissionResponse {
                    tool_use_id: request.tool_use_id,
                    outcome: PermissionOutcome::Deny {
                        message: "decider tried to call a tool".into(),
                    },
                    rationale: String::new(),
                });
            }
        };
        let outcome = parse_outcome(&text);
        // Best-effort rationale: re-parse the JSON to recover it.
        let rationale = serde_json::from_str::<DeciderJson>(strip_markdown_fences(&text).trim())
            .map(|d| rationale_from(&d))
            .unwrap_or_default();

        // Phase 67.7 — record this decision into long-term memory so
        // future similar requests can recall it. Best-effort.
        let decision = Decision {
            id: nexo_driver_types::DecisionId::new(),
            goal_id: request.goal_id,
            turn_index: 0,
            tool: request.tool_name.clone(),
            input: request.input.clone(),
            choice: outcome_to_choice(&outcome),
            rationale: rationale.clone(),
            decided_at: chrono::Utc::now(),
        };
        if let Err(e) = self.memory.record(&decision).await {
            tracing::warn!(target: "llm-decider", "record decision failed: {e}");
        }

        Ok(PermissionResponse {
            tool_use_id: request.tool_use_id,
            outcome,
            rationale,
        })
    }
}

fn outcome_to_choice(o: &PermissionOutcome) -> nexo_driver_types::DecisionChoice {
    use nexo_driver_types::DecisionChoice;
    match o {
        PermissionOutcome::AllowOnce { .. } | PermissionOutcome::AllowSession { .. } => {
            DecisionChoice::Allow
        }
        PermissionOutcome::Deny { message } => DecisionChoice::Deny {
            message: message.clone(),
        },
        PermissionOutcome::Unavailable { reason } => DecisionChoice::Deny {
            message: reason.clone(),
        },
        PermissionOutcome::Cancelled => DecisionChoice::Deny {
            message: "cancelled".into(),
        },
    }
}

fn build_user_prompt(req: &PermissionRequest, recalled: &[Decision]) -> String {
    let mut s = String::new();
    if !recalled.is_empty() {
        s.push_str("Past decisions (most recent first):\n");
        for d in recalled {
            s.push_str(&format!("- {}: {:?} ({})\n", d.tool, d.choice, d.rationale));
        }
        s.push('\n');
    }
    s.push_str(&format!("Tool: {}\n", req.tool_name));
    s.push_str(&format!(
        "Input: {}\n",
        serde_json::to_string_pretty(&req.input).unwrap_or_else(|_| "<invalid>".into())
    ));
    if let Some(goal) = req
        .metadata
        .get("goal_description")
        .and_then(|v| v.as_str())
    {
        s.push_str(&format!("Goal: {goal}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_allow_once() {
        let raw = r#"{"outcome":"allow_once","rationale":"safe"}"#;
        match parse_outcome(raw) {
            PermissionOutcome::AllowOnce { .. } => {}
            o => panic!("expected AllowOnce, got {o:?}"),
        }
    }

    #[test]
    fn parses_allow_session_with_scope() {
        let raw = r#"{"outcome":"allow_session","scope":"session"}"#;
        match parse_outcome(raw) {
            PermissionOutcome::AllowSession {
                scope: nexo_driver_permission::AllowScope::Session,
                ..
            } => {}
            o => panic!("expected AllowSession{{scope=session}}, got {o:?}"),
        }
    }

    #[test]
    fn parses_deny_with_message() {
        let raw = r#"{"outcome":"deny","message":"too risky"}"#;
        match parse_outcome(raw) {
            PermissionOutcome::Deny { message } => assert_eq!(message, "too risky"),
            o => panic!("expected Deny, got {o:?}"),
        }
    }

    #[test]
    fn invalid_json_falls_back_to_deny() {
        let raw = "definitely not json";
        match parse_outcome(raw) {
            PermissionOutcome::Deny { message } => {
                assert!(message.contains("invalid"));
            }
            o => panic!("expected Deny fallback, got {o:?}"),
        }
    }

    #[test]
    fn strips_markdown_fence() {
        let raw = "```json\n{\"outcome\":\"allow_once\"}\n```";
        match parse_outcome(raw) {
            PermissionOutcome::AllowOnce { .. } => {}
            o => panic!("expected AllowOnce after fence strip, got {o:?}"),
        }
    }

    #[test]
    fn unknown_outcome_falls_back_to_deny() {
        let raw = r#"{"outcome":"escalate"}"#;
        match parse_outcome(raw) {
            PermissionOutcome::Deny { message } => {
                assert!(message.contains("unknown"));
            }
            o => panic!("expected Deny fallback, got {o:?}"),
        }
    }
}
