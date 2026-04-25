# Transcripts (FTS + redaction)

Per-session JSONL transcripts under `agents.<id>.transcripts_dir` are
the canonical record of every turn. Two optional layers wrap that
record:

- **FTS5 index** — a SQLite virtual table that mirrors transcript
  content for `MATCH` queries. Backs the `session_logs` tool's
  `search` action when present.
- **Redaction** — a regex pre-processor that rewrites entry content
  before it ever reaches disk. Patterns target common credentials and
  home-directory paths.

Source: `crates/core/src/agent/transcripts_index.rs`,
`crates/core/src/agent/redaction.rs`,
`crates/core/src/agent/transcripts.rs`.

## Configuration

`config/transcripts.yaml` (optional; absent → defaults below):

```yaml
fts:
  enabled: true                       # default
  db_path: ./data/transcripts.db      # default

redaction:
  enabled: false                      # default — opt in
  use_builtins: true                  # only relevant if enabled
  extra_patterns:
    - { regex: "TENANT-[0-9]+", label: "tenant_id" }
```

JSONL is the source of truth. The FTS index is derivable; if the DB
is corrupted or deleted, `agent transcripts reindex` (planned) can
rebuild it from disk.

## FTS schema

```sql
CREATE VIRTUAL TABLE transcripts_fts USING fts5(
    content,
    agent_id        UNINDEXED,
    session_id      UNINDEXED,
    timestamp_unix  UNINDEXED,
    role            UNINDEXED,
    source_plugin   UNINDEXED,
    tokenize = 'unicode61 remove_diacritics 2'
);
```

The DB is shared across agents; isolation is enforced at query time
by `WHERE agent_id = ?`. User queries are escaped as a single FTS5
phrase so operators (`OR`, `NOT`, `:`) in the user input never reach
the engine as syntax.

## `session_logs` integration

When the index is available, the `search` action returns:

```json
{
  "ok": true,
  "query": "reembolso",
  "backend": "fts5",
  "count": 3,
  "hits": [
    {
      "session_id": "…",
      "timestamp": "2026-04-25T18:00:00Z",
      "role": "user",
      "source_plugin": "wa",
      "preview": "...quería un [reembolso] del pedido..."
    }
  ]
}
```

If the index is `None` (FTS disabled or init failed), the action
falls back to the legacy substring scan over JSONL. The shape is the
same minus `backend: "fts5"`.

## Redaction patterns

| Label | Detects | Example match |
|-------|---------|---------------|
| `bearer_jwt` | `Bearer eyJ…` JWT triplets | `Bearer eyJhbGc.eyJzdWI.dGVzdA` |
| `anthropic_key` | Anthropic API keys | `sk-ant-abcdef…` |
| `openai_key` | `sk-` prefix API keys (OpenAI etc.) | `sk-abc123…` |
| `aws_access_key` | AWS access key id | `AKIAIOSFODNN7EXAMPLE` |
| `hex_token_32` | Long hex strings | `5d41402abc4b2a76b9719d911017c592` |
| `home_path` | Linux/macOS home dirs | `/home/familia`, `/Users/alice` |

Each match is replaced with `[REDACTED:<label>]`. Patterns run in the
order above, so more specific shapes (Bearer JWT, Anthropic) win over
generic catch-alls below.

A 40-char base64 pattern targeting AWS *secret* keys was deliberately
omitted — it produces too many false positives on legitimate hashes
and opaque ids. Operators who need it can add it scoped via
`extra_patterns`.

### Custom patterns

```yaml
redaction:
  enabled: true
  extra_patterns:
    - { regex: "TENANT-[0-9]+",   label: "tenant_id" }
    - { regex: "internal\\.acme", label: "internal_host" }
```

Custom patterns run *after* built-ins. Invalid regex aborts boot
with a message naming the offending index and label.

## What redaction does not do

- It does not maintain a reverse map. Once content is redacted on
  disk the original is gone — by design. A reversible mapping would
  recreate the leak surface this feature is meant to close.
- It does not rewrite previously-written JSONL files. New entries
  redact going forward; historical content stays as-is.
- It does not redact `tracing` logs — that's a separate concern.
- The FTS index stores the redacted text, so `search` results never
  surface the original secrets either.

## Operational notes

- The FTS index uses WAL journaling and capped pool size of 4 — it
  shares the same idiom as the long-term memory DB.
- Insert is best-effort. If an FTS write fails (disk full, lock
  contention) the tool logs at `warn` and the JSONL append still
  succeeds. The source of truth is never compromised.
- Boot logs include `transcripts FTS index ready` (or the warn that
  it fell back) and `transcripts redaction active` when the redactor
  has any rule loaded.
