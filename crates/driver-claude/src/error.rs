//! Errors specific to driving the `claude` CLI.

use nexo_driver_types::HarnessError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ClaudeError {
    #[error("`claude` binary not found in PATH (try `npm i -g @anthropic-ai/claude-code`)")]
    BinaryNotFound,
    #[error("failed to spawn claude: {0}")]
    Spawn(String),
    #[error("missing pipe: {0}")]
    MissingPipe(&'static str),
    #[error("turn cancelled")]
    Cancelled,
    #[error("turn timed out")]
    Timeout,
    #[error("session expired or invalid: {0}")]
    SessionInvalid(String),
    #[error("stream-json parse failure on line {line_no}: raw={raw:?}")]
    ParseLine {
        line_no: u64,
        raw: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("binding error: {0}")]
    Binding(String),
}

impl From<ClaudeError> for HarnessError {
    fn from(e: ClaudeError) -> Self {
        match e {
            ClaudeError::Io(io) => HarnessError::Io(io),
            ClaudeError::SessionInvalid(msg) => HarnessError::SessionInvalid(msg),
            // Spawn / MissingPipe / ParseLine / BinaryNotFound all
            // describe a subprocess failure mode from the harness's POV.
            ClaudeError::Spawn(_)
            | ClaudeError::MissingPipe(_)
            | ClaudeError::ParseLine { .. }
            | ClaudeError::BinaryNotFound => HarnessError::Subprocess(e.to_string()),
            ClaudeError::Cancelled | ClaudeError::Timeout | ClaudeError::Binding(_) => {
                HarnessError::Other(e.to_string())
            }
        }
    }
}
