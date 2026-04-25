//! `DefaultAcceptanceEvaluator` — runs each criterion in order,
//! accumulates failures, attaches truncated evidence to shell ones.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use nexo_driver_types::{AcceptanceCriterion, AcceptanceFailure, AcceptanceVerdict};
use regex::Regex;
use serde_json::Value;

use crate::acceptance::custom::CustomVerifierRegistry;
use crate::acceptance::file_match::check_file;
use crate::acceptance::shell::{ShellResult, ShellRunner};
use crate::acceptance::trait_def::AcceptanceEvaluator;
use crate::error::DriverError;

pub struct DefaultAcceptanceEvaluator {
    shell: ShellRunner,
    default_shell_timeout: Duration,
    custom: Arc<CustomVerifierRegistry>,
    evidence_byte_limit: usize,
}

impl Default for DefaultAcceptanceEvaluator {
    fn default() -> Self {
        Self {
            shell: ShellRunner::default(),
            default_shell_timeout: Duration::from_secs(300),
            custom: Arc::new(CustomVerifierRegistry::default().with_builtins()),
            evidence_byte_limit: 4 * 1024,
        }
    }
}

impl DefaultAcceptanceEvaluator {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_shell(mut self, s: ShellRunner) -> Self {
        self.shell = s;
        self
    }
    pub fn with_default_shell_timeout(mut self, t: Duration) -> Self {
        self.default_shell_timeout = t;
        self
    }
    pub fn with_custom_verifiers(mut self, r: Arc<CustomVerifierRegistry>) -> Self {
        self.custom = r;
        self
    }
    pub fn with_evidence_byte_limit(mut self, n: usize) -> Self {
        self.evidence_byte_limit = n;
        self
    }

    async fn check_shell(
        &self,
        idx: usize,
        command: &str,
        expected_exit: Option<i32>,
        stdout_must_match: Option<&str>,
        timeout_secs: u64,
        workspace: &Path,
    ) -> Result<Option<AcceptanceFailure>, DriverError> {
        let timeout = if timeout_secs == 0 {
            self.default_shell_timeout
        } else {
            Duration::from_secs(timeout_secs)
        };
        let res = self.shell.run(command, workspace, timeout).await?;
        let want_exit = expected_exit.unwrap_or(0);
        if res.timed_out {
            return Ok(Some(AcceptanceFailure {
                criterion_index: idx,
                criterion_label: format!("shell:{command}"),
                message: format!("timed out after {}s", timeout.as_secs()),
                evidence: Some(self.evidence(&res)),
            }));
        }
        if res.exit_code != Some(want_exit) {
            return Ok(Some(AcceptanceFailure {
                criterion_index: idx,
                criterion_label: format!("shell:{command}"),
                message: format!("exit {:?} != {}", res.exit_code, want_exit),
                evidence: Some(self.evidence(&res)),
            }));
        }
        if let Some(rx) = stdout_must_match {
            let re = Regex::new(rx)
                .map_err(|e| DriverError::Acceptance(format!("bad regex {rx:?}: {e}")))?;
            if !re.is_match(&res.stdout) {
                return Ok(Some(AcceptanceFailure {
                    criterion_index: idx,
                    criterion_label: format!("shell:{command}"),
                    message: format!("stdout did not match {rx}"),
                    evidence: Some(self.evidence(&res)),
                }));
            }
        }
        Ok(None)
    }

    fn evidence(&self, res: &ShellResult) -> String {
        let combined = format!("{}\n--- STDERR ---\n{}", res.stdout, res.stderr);
        truncate_utf8_tail(&combined, self.evidence_byte_limit)
    }

    async fn check_custom(
        &self,
        idx: usize,
        name: &str,
        args: &Value,
        workspace: &Path,
    ) -> Result<Option<AcceptanceFailure>, DriverError> {
        let Some(verifier) = self.custom.get(name) else {
            return Ok(Some(AcceptanceFailure {
                criterion_index: idx,
                criterion_label: format!("custom:{name}"),
                message: format!("unknown custom verifier: {name}"),
                evidence: None,
            }));
        };
        match verifier.verify(args, workspace).await? {
            None => Ok(None),
            Some(message) => Ok(Some(AcceptanceFailure {
                criterion_index: idx,
                criterion_label: format!("custom:{name}"),
                message,
                evidence: None,
            })),
        }
    }
}

#[async_trait]
impl AcceptanceEvaluator for DefaultAcceptanceEvaluator {
    async fn evaluate(
        &self,
        criteria: &[AcceptanceCriterion],
        workspace: &Path,
    ) -> Result<AcceptanceVerdict, DriverError> {
        let started = Instant::now();
        let mut failures = Vec::new();
        for (idx, c) in criteria.iter().enumerate() {
            let outcome = match c {
                AcceptanceCriterion::ShellCommand {
                    command,
                    expected_exit,
                    stdout_must_match,
                    timeout_secs,
                } => {
                    self.check_shell(
                        idx,
                        command,
                        *expected_exit,
                        stdout_must_match.as_deref(),
                        *timeout_secs,
                        workspace,
                    )
                    .await?
                }
                AcceptanceCriterion::FileMatches {
                    path,
                    regex,
                    required,
                } => check_file(Path::new(path), regex, *required, workspace)
                    .await?
                    .map(|message| AcceptanceFailure {
                        criterion_index: idx,
                        criterion_label: format!("file:{path}"),
                        message,
                        evidence: None,
                    }),
                AcceptanceCriterion::Custom { name, args } => {
                    self.check_custom(idx, name, args, workspace).await?
                }
            };
            if let Some(f) = outcome {
                failures.push(f);
            }
        }
        Ok(AcceptanceVerdict {
            met: failures.is_empty(),
            failures,
            evaluated_at: Utc::now(),
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }
}

/// Keep the last `limit` bytes of `s`, but trim back to a UTF-8 char
/// boundary so we never emit a half-codepoint.
fn truncate_utf8_tail(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let mut start = s.len() - limit;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_driver_types::AcceptanceCriterion;

    #[tokio::test]
    async fn empty_criteria_returns_met() {
        let dir = tempfile::tempdir().unwrap();
        let e = DefaultAcceptanceEvaluator::new();
        let v = e.evaluate(&[], dir.path()).await.unwrap();
        assert!(v.met);
        assert!(v.failures.is_empty());
    }

    #[tokio::test]
    async fn order_preserved_with_index() {
        let dir = tempfile::tempdir().unwrap();
        let e = DefaultAcceptanceEvaluator::new();
        let criteria = vec![
            AcceptanceCriterion::shell("true"),
            AcceptanceCriterion::shell("false"),
            AcceptanceCriterion::shell("true"),
        ];
        let v = e.evaluate(&criteria, dir.path()).await.unwrap();
        assert!(!v.met);
        assert_eq!(v.failures.len(), 1);
        assert_eq!(v.failures[0].criterion_index, 1);
    }

    #[tokio::test]
    async fn custom_unknown_returns_failure() {
        let dir = tempfile::tempdir().unwrap();
        let e = DefaultAcceptanceEvaluator::new();
        let criteria = vec![AcceptanceCriterion::Custom {
            name: "definitely_invented".into(),
            args: Value::Null,
        }];
        let v = e.evaluate(&criteria, dir.path()).await.unwrap();
        assert!(!v.met);
        assert!(v.failures[0].message.contains("unknown"));
    }

    #[tokio::test]
    async fn truncate_utf8_tail_preserves_char_boundaries() {
        // Multi-byte UTF-8: 'é' is 2 bytes.
        let s = "aaéééééééé";
        let cut = truncate_utf8_tail(s, 5);
        assert!(cut.is_char_boundary(0));
        assert!(s.ends_with(&cut));
    }
}
