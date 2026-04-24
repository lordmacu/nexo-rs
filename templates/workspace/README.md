# Workspace Templates

Starter files for an agent's workspace. The workspace is the agent's "self":
identity, persona, relationship with the user, recent notes, and curated memory.

## Layout expected by the loader

```
<workspace-root>/
├── IDENTITY.md          ← parsed: name/creature/vibe/emoji/avatar
├── SOUL.md              ← persona, tone, constraints
├── USER.md              ← the human's profile
├── AGENTS.md            ← operating rules for the agent
├── MEMORY.md            ← curated long-term memory (main session only)
└── memory/
    └── YYYY-MM-DD.md    ← daily notes (today + yesterday auto-loaded)
```

## Usage

1. Copy this directory to `<data>/workspace/<agent-id>/` (one per agent).
2. Edit the files to match the agent's persona and the user.
3. Point the agent's config at it:
   ```yaml
   agents:
     - id: "kate"
       workspace: "./data/workspace/kate"
   ```

## Loader rules

- **Missing files are silently skipped.** You only need the ones you want.
- **Template placeholders** (values in `_(...)_` or `(...)`) are treated as unset.
- **Per-file cap:** 12_000 chars. **Total budget:** 60_000 chars. Excess gets truncated with `[truncated]`.
- **`MEMORY.md` privacy boundary:** only loaded when the inbound message is a direct DM. Agent-to-agent delegation (`source_plugin = "agent"`) treats the scope as shared and skips `MEMORY.md`.
- **Config `system_prompt`** (per-agent YAML) is appended *after* the workspace bundle, so workspace persona wins on conflict but inline instructions can sharpen it.

## What NOT to put here

- API keys, credentials, OAuth tokens — use `secrets/` + `${file:/run/secrets/...}` in YAML instead.
- Session transcripts — those live outside the workspace (Phase 10.4).
- Anything you wouldn't commit to a private repo.
