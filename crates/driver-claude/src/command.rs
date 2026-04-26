//! `ClaudeCommand` — type-safe builder over the `claude` CLI's flag
//! surface. Consume-self setters mark the field as caller-set so
//! [`apply_defaults`] can fill in only what hasn't been touched.

use std::collections::HashMap;
use std::path::PathBuf;

use tokio::process::Command;

use crate::config::{ClaudeDefaultArgs, OutputFormat};
use crate::error::ClaudeError;

/// Bitset of fields the caller has set. Used by `apply_defaults` so
/// caller-set values win over `ClaudeDefaultArgs`.
#[derive(Clone, Copy, Default)]
struct SetMask(u32);

impl SetMask {
    const OUTPUT_FORMAT: u32 = 1 << 0;
    const PERMISSION_PROMPT_TOOL: u32 = 1 << 1;
    const ALLOWED_TOOLS: u32 = 1 << 2;
    const DISALLOWED_TOOLS: u32 = 1 << 3;
    const MODEL: u32 = 1 << 4;

    fn mark(&mut self, bit: u32) {
        self.0 |= bit;
    }
    fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }
}

#[derive(Clone)]
pub struct ClaudeCommand {
    binary: PathBuf,
    prompt: String,
    output_format: OutputFormat,
    resume: Option<String>,
    set_session_id: Option<String>,
    additional_dirs: Vec<PathBuf>,
    allowed_tools: Vec<String>,
    disallowed_tools: Vec<String>,
    permission_prompt_tool: Option<String>,
    mcp_config_path: Option<PathBuf>,
    model: Option<String>,
    extra_env: HashMap<String, String>,
    cwd: Option<PathBuf>,
    set_mask: SetMask,
}

impl ClaudeCommand {
    pub fn new(binary: impl Into<PathBuf>, prompt: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            prompt: prompt.into(),
            output_format: OutputFormat::default(),
            resume: None,
            set_session_id: None,
            additional_dirs: Vec::new(),
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            permission_prompt_tool: None,
            mcp_config_path: None,
            model: None,
            extra_env: HashMap::new(),
            cwd: None,
            set_mask: SetMask::default(),
        }
    }

    /// Convenience constructor — runs `which::which("claude")` and
    /// returns `BinaryNotFound` if the CLI isn't on `$PATH`.
    pub fn discover(prompt: impl Into<String>) -> Result<Self, ClaudeError> {
        let path = which::which("claude").map_err(|_| ClaudeError::BinaryNotFound)?;
        Ok(Self::new(path, prompt))
    }

    pub fn output_format(mut self, fmt: OutputFormat) -> Self {
        self.output_format = fmt;
        self.set_mask.mark(SetMask::OUTPUT_FORMAT);
        self
    }
    /// Set `--resume <id>`. Last of `resume` / `set_session_id` wins.
    pub fn resume(mut self, session_id: impl Into<String>) -> Self {
        self.resume = Some(session_id.into());
        self.set_session_id = None;
        self
    }
    /// Set `--session-id <id>`. Use on the first turn only.
    pub fn set_session_id(mut self, id: impl Into<String>) -> Self {
        self.set_session_id = Some(id.into());
        self.resume = None;
        self
    }
    pub fn additional_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.additional_dirs.push(path.into());
        self
    }
    pub fn allowed_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_tools = tools.into_iter().map(Into::into).collect();
        self.set_mask.mark(SetMask::ALLOWED_TOOLS);
        self
    }
    pub fn disallowed_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.disallowed_tools = tools.into_iter().map(Into::into).collect();
        self.set_mask.mark(SetMask::DISALLOWED_TOOLS);
        self
    }
    pub fn permission_prompt_tool(mut self, name: impl Into<String>) -> Self {
        self.permission_prompt_tool = Some(name.into());
        self.set_mask.mark(SetMask::PERMISSION_PROMPT_TOOL);
        self
    }
    pub fn mcp_config(mut self, path: impl Into<PathBuf>) -> Self {
        self.mcp_config_path = Some(path.into());
        self
    }
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self.set_mask.mark(SetMask::MODEL);
        self
    }
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.insert(key.into(), value.into());
        self
    }
    pub fn cwd(mut self, path: impl Into<PathBuf>) -> Self {
        self.cwd = Some(path.into());
        self
    }

    /// Fill in fields the caller hasn't explicitly set from a
    /// `ClaudeDefaultArgs` block. Caller-set values win.
    pub fn apply_defaults(mut self, defaults: &ClaudeDefaultArgs) -> Self {
        if !self.set_mask.has(SetMask::OUTPUT_FORMAT) {
            self.output_format = defaults.output_format;
        }
        if !self.set_mask.has(SetMask::PERMISSION_PROMPT_TOOL)
            && self.permission_prompt_tool.is_none()
        {
            self.permission_prompt_tool = defaults.permission_prompt_tool.clone();
        }
        if !self.set_mask.has(SetMask::ALLOWED_TOOLS) && self.allowed_tools.is_empty() {
            self.allowed_tools = defaults.allowed_tools.clone();
        }
        if !self.set_mask.has(SetMask::DISALLOWED_TOOLS) && self.disallowed_tools.is_empty() {
            self.disallowed_tools = defaults.disallowed_tools.clone();
        }
        if !self.set_mask.has(SetMask::MODEL) && self.model.is_none() {
            self.model = defaults.model.clone();
        }
        self
    }

    /// Build the actual `tokio::process::Command`. Sets stdio piped
    /// for stdin/stdout/stderr; the caller's `spawn_turn` consumes the
    /// pipes.
    pub fn into_command(self) -> Command {
        let args = self.debug_args();
        // First arg is the binary itself; skip it for `Command::new`.
        let mut cmd = Command::new(&self.binary);
        cmd.args(&args);
        for (k, v) in &self.extra_env {
            cmd.env(k, v);
        }
        if let Some(cwd) = &self.cwd {
            cmd.current_dir(cwd);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        cmd
    }

    /// Test helper — produce the exact arg vector the spawn would use.
    /// Hidden from rustdoc; consumers outside the crate must not rely
    /// on the exact ordering.
    #[doc(hidden)]
    pub fn debug_args(&self) -> Vec<String> {
        let mut args = Vec::with_capacity(16);
        args.push("-p".into());
        args.push(self.prompt.clone());
        args.push("--output-format".into());
        args.push(self.output_format.as_cli().into());
        // Phase 73 — Claude CLI requires `--verbose` whenever
        // `--print` is combined with `--output-format=stream-json`.
        // Without it the CLI prints
        // "Error: When using --print, --output-format=stream-json
        // requires --verbose" to stderr and exits with status 0
        // stdout-empty, which the driver loop then mis-classifies
        // as a `Continue` turn — the entire 40-turn budget burns
        // on phantom checkpoints. Always pass `--verbose` for
        // stream-json so the harness gets the JSON it expects.
        if matches!(
            self.output_format,
            crate::config::OutputFormat::StreamJson
        ) {
            args.push("--verbose".into());
        }
        if let Some(id) = &self.resume {
            args.push("--resume".into());
            args.push(id.clone());
        }
        if let Some(id) = &self.set_session_id {
            args.push("--session-id".into());
            args.push(id.clone());
        }
        for d in &self.additional_dirs {
            args.push("--add-dir".into());
            args.push(d.display().to_string());
        }
        if !self.allowed_tools.is_empty() {
            args.push("--allowedTools".into());
            args.push(self.allowed_tools.join(","));
        }
        if !self.disallowed_tools.is_empty() {
            args.push("--disallowedTools".into());
            args.push(self.disallowed_tools.join(","));
        }
        if let Some(name) = &self.permission_prompt_tool {
            args.push("--permission-prompt-tool".into());
            args.push(name.clone());
        }
        if let Some(p) = &self.mcp_config_path {
            args.push("--mcp-config".into());
            args.push(p.display().to_string());
        }
        if let Some(m) = &self.model {
            args.push("--model".into());
            args.push(m.clone());
        }
        args
    }
}
