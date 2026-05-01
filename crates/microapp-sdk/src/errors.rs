//! Typed error surface — `ToolError` for tool/hook failures,
//! `Error` for SDK runtime / dispatch loop failures.

use thiserror::Error;

/// Result alias used by `run_stdio` / `run_with` and other
/// dispatch-loop entry points.
pub type Result<T> = std::result::Result<T, Error>;

/// SDK runtime / dispatch-loop errors. Distinct from
/// [`ToolError`] (which is the per-tool / per-hook failure
/// surface). Operators see this when the loop itself fails to
/// boot or read/write — `ToolError` flows back over JSON-RPC to
/// the daemon.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum Error {
    /// I/O on stdin / stdout failed (closed, permission denied,
    /// etc.). Usually fatal to the loop.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Generic boxed error escape hatch — used by handlers that
    /// don't fit one of the above and don't have their own typed
    /// shape.
    #[error("internal: {0}")]
    Internal(String),
}

/// Per-tool / per-hook failure. Returned by handler bodies and
/// translated into JSON-RPC error frames by the dispatch loop.
///
/// Operator-facing diagnostic — `#[non_exhaustive]` so a future
/// failure mode (e.g. `RateLimited`) lands as semver-minor without
/// breaking downstream pattern matches.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum ToolError {
    /// The handler couldn't parse / validate its arguments.
    /// Translates to JSON-RPC code `-32602` (Invalid params).
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),

    /// The handler isn't implemented yet (scaffold stage).
    /// Translates to JSON-RPC code `-32601` (Method not found).
    #[error("not implemented")]
    NotImplemented,

    /// Catch-all for handler-internal failures (DB error, network,
    /// unexpected state). Translates to JSON-RPC code `-32000`.
    #[error("internal: {0}")]
    Internal(String),

    /// The caller doesn't have permission to invoke this tool with
    /// these arguments. Translates to JSON-RPC code `-32000` with
    /// `data.code = "unauthorized"`.
    #[error("unauthorized: {0}")]
    Unauthorized(String),
}

impl ToolError {
    /// JSON-RPC error code for this variant.
    pub fn code(&self) -> i32 {
        match self {
            ToolError::InvalidArguments(_) => -32602,
            ToolError::NotImplemented => -32601,
            ToolError::Internal(_) => -32000,
            ToolError::Unauthorized(_) => -32000,
        }
    }

    /// Symbolic code surfaced under JSON-RPC error `data.code` so
    /// callers can pattern-match without parsing the message.
    pub fn symbolic(&self) -> &'static str {
        match self {
            ToolError::InvalidArguments(_) => "invalid_arguments",
            ToolError::NotImplemented => "not_implemented",
            ToolError::Internal(_) => "internal",
            ToolError::Unauthorized(_) => "unauthorized",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_arguments_maps_to_minus_32602() {
        let e = ToolError::InvalidArguments("missing field".into());
        assert_eq!(e.code(), -32602);
        assert_eq!(e.symbolic(), "invalid_arguments");
    }

    #[test]
    fn not_implemented_maps_to_minus_32601() {
        assert_eq!(ToolError::NotImplemented.code(), -32601);
        assert_eq!(ToolError::NotImplemented.symbolic(), "not_implemented");
    }

    #[test]
    fn internal_maps_to_minus_32000() {
        assert_eq!(ToolError::Internal("x".into()).code(), -32000);
        assert_eq!(ToolError::Internal("x".into()).symbolic(), "internal");
    }

    #[test]
    fn unauthorized_maps_to_minus_32000_with_symbolic() {
        let e = ToolError::Unauthorized("no binding".into());
        assert_eq!(e.code(), -32000);
        assert_eq!(e.symbolic(), "unauthorized");
    }

    #[test]
    fn error_io_round_trip() {
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe closed");
        let e: Error = io_err.into();
        assert!(format!("{e}").contains("pipe closed"));
    }
}
