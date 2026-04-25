//! Async iterator over the JSONL stream Claude writes to stdout.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, Lines};

use crate::error::ClaudeError;
use crate::event::ClaudeEvent;

pub struct EventStream<R: AsyncBufRead + Unpin + Send> {
    inner: Lines<R>,
    line_count: u64,
}

impl<R: AsyncBufRead + Unpin + Send> EventStream<R> {
    pub fn new(reader: R) -> Self {
        Self {
            inner: reader.lines(),
            line_count: 0,
        }
    }

    /// Pull the next event off the stream.
    ///
    /// - `Ok(None)` — Claude closed stdout cleanly. Caller should
    ///   interpret this as the turn ending; if no `ResultEvent` was
    ///   seen, that's the abort case.
    /// - `Err(ParseLine)` — caller should log + abort the turn.
    pub async fn next(&mut self) -> Result<Option<ClaudeEvent>, ClaudeError> {
        loop {
            let raw = match self.inner.next_line().await? {
                None => return Ok(None),
                Some(s) => s,
            };
            self.line_count += 1;
            if raw.trim().is_empty() {
                continue;
            }
            return match serde_json::from_str::<ClaudeEvent>(&raw) {
                Ok(ev) => Ok(Some(ev)),
                Err(source) => Err(ClaudeError::ParseLine {
                    line_no: self.line_count,
                    raw,
                    source,
                }),
            };
        }
    }

    pub fn line_count(&self) -> u64 {
        self.line_count
    }
}
