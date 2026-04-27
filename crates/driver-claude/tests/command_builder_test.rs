use nexo_driver_claude::{ClaudeCommand, ClaudeDefaultArgs, OutputFormat};

fn base(prompt: &str) -> ClaudeCommand {
    ClaudeCommand::new("claude", prompt)
}

#[test]
fn default_only() {
    let args = base("hi").debug_args();
    // Phase 73 — `--verbose` is mandatory whenever
    // `--print` + `--output-format=stream-json` combine; without
    // it the Claude CLI bails with "stream-json requires
    // --verbose" and the driver loop spins on phantom turns.
    assert_eq!(
        args,
        vec![
            "-p".to_string(),
            "hi".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
        ]
    );
}

#[test]
fn with_resume() {
    let args = base("go").resume("01HZX").debug_args();
    assert!(args.windows(2).any(|w| w == ["--resume", "01HZX"]));
    assert!(!args.iter().any(|a| a == "--session-id"));
}

#[test]
fn with_set_session_id_overrides_resume() {
    let args = base("go")
        .resume("01HZX")
        .set_session_id("FRESH")
        .debug_args();
    assert!(args.windows(2).any(|w| w == ["--session-id", "FRESH"]));
    assert!(!args.iter().any(|a| a == "--resume"));
}

#[test]
fn with_allowed_tools_joins_csv() {
    let args = base("go")
        .allowed_tools(["Read", "Grep", "Glob"])
        .debug_args();
    assert!(args
        .windows(2)
        .any(|w| w == ["--allowedTools", "Read,Grep,Glob"]));
}

#[test]
fn with_mcp_config() {
    let args = base("go").mcp_config("/etc/mcp.json").debug_args();
    assert!(args
        .windows(2)
        .any(|w| w == ["--mcp-config", "/etc/mcp.json"]));
    // Phase 73 — strict flag follows so Claude does not merge
    // with the user's ~/.claude.json (which silently drops the
    // driver's nexo-driver server).
    assert!(args.iter().any(|a| a == "--strict-mcp-config"));
}

#[test]
fn full_bundle() {
    let args = base("go")
        .resume("S1")
        .additional_dir("/tmp/a")
        .additional_dir("/tmp/b")
        .allowed_tools(["Read"])
        .disallowed_tools(["Bash"])
        .permission_prompt_tool("mcp__nexo-driver__permission_prompt")
        .mcp_config("/etc/mcp.json")
        .model("claude-sonnet-4-6")
        .debug_args();
    let joined = args.join(" ");
    assert!(joined.contains("--resume S1"));
    assert!(joined.contains("--add-dir /tmp/a"));
    assert!(joined.contains("--add-dir /tmp/b"));
    assert!(joined.contains("--allowedTools Read"));
    assert!(joined.contains("--disallowedTools Bash"));
    assert!(joined.contains("--permission-prompt-tool mcp__nexo-driver__permission_prompt"));
    assert!(joined.contains("--mcp-config /etc/mcp.json"));
    assert!(joined.contains("--model claude-sonnet-4-6"));
}

#[test]
fn apply_defaults_caller_set_wins() {
    let defaults = ClaudeDefaultArgs {
        output_format: OutputFormat::Json,
        permission_prompt_tool: Some("mcp__defaults__perm".into()),
        allowed_tools: vec!["Read".into(), "Grep".into()],
        disallowed_tools: vec!["Bash".into()],
        model: Some("claude-haiku-4-5".into()),
    };
    let args = base("go")
        .allowed_tools(["MyOnlyTool"]) // caller-set
        .model("claude-sonnet-4-6") // caller-set
        .apply_defaults(&defaults)
        .debug_args();
    let joined = args.join(" ");
    // caller-set wins for model + allowed_tools…
    assert!(joined.contains("--allowedTools MyOnlyTool"));
    assert!(!joined.contains("Read,Grep"));
    assert!(joined.contains("--model claude-sonnet-4-6"));
    assert!(!joined.contains("claude-haiku-4-5"));
    // …defaults fill in for everything else.
    assert!(joined.contains("--output-format json"));
    assert!(joined.contains("--disallowedTools Bash"));
    assert!(joined.contains("--permission-prompt-tool mcp__defaults__perm"));
}
