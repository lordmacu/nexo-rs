# tmux Remote Extension (Rust)

Drive tmux sessions from the agent: create, send keys, capture output, kill.
Uses a dedicated socket (`TMUX_REMOTE_SOCKET` or `$TMPDIR/agent-rs-tmux.sock`)
isolated from the operator's tmux.

## Tools

- `status` — bin + socket path + tools
- `new_session` — `name` (1..64 `[a-zA-Z0-9_-]`), optional `command`
- `send_keys` — `session`, `keys`, optional `enter: true`
- `capture_pane` — `session`, optional `lines` (1..2000, default 200)
- `list_sessions` — returns `[{name, created_unix, windows}, ...]`
- `kill_session` — `session`

## Security

Session names are regex-restricted to `[A-Za-z0-9_-]{1,64}`. That's the
only user-controlled string that reaches `tmux -t <session>`; it cannot
break out into shell metacharacters. Commands inside `new_session` /
`send_keys` are passed as a single argv argument — tmux quotes them for
its own parser, not the caller's shell.

## Error codes

-32050 missing tmux · -32051 tmux non-zero · -32602 bad input · -32031
spawn failed · -32034 io

## Tests

11 tests (3 unit + 8 integration) using a throwaway socket per case.
