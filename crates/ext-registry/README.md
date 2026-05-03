# nexo-ext-registry

Phase 31.0 — types + parser + validation for the nexo plugin
**ext-registry**: the catalog of installable out-of-tree
subprocess plugins (authored against
[`nexo-plugin-contract.md`](../../nexo-plugin-contract.md)).

A single JSON document at a well-known URL (production:
`https://nexo-rs.dev/ext-index.json`) lists every plugin
available for `nexo ext install <id>` (CLI ships in Phase 31.1).

## Wire shape

```json
{
  "schema_version": "1.0.0",
  "generated_at": "2026-05-01T00:00:00Z",
  "entries": [
    {
      "id": "slack",
      "version": "0.2.0",
      "name": "Slack Channel",
      "description": "Slack bot integration",
      "homepage": "https://github.com/nexo-rs/plugin-slack",
      "tier": "verified",
      "min_nexo_version": ">=0.1.0",
      "downloads": [
        {
          "target": "x86_64-unknown-linux-gnu",
          "url": "https://example.com/slack-0.2.0-x86_64-linux.tar.gz",
          "sha256": "abcdef0123...",
          "size_bytes": 1234567
        }
      ],
      "manifest_url": "https://example.com/slack-0.2.0/nexo-plugin.toml",
      "signing": {
        "cosign_signature_url": "https://example.com/slack-0.2.0.sig",
        "cosign_certificate_url": "https://example.com/slack-0.2.0.cert"
      },
      "authors": ["Author Name <email@example.com>"]
    }
  ]
}
```

## Validation rules

- `schema_version` major MUST match this crate's `SCHEMA_VERSION`.
- Every entry's `id` MUST match `^[a-z][a-z0-9_]{0,31}$`.
- `name` + `description` MUST be non-empty after trim.
- `homepage`, `manifest_url`, every `download.url`, and signing
  URLs MUST be valid HTTPS URLs.
- `downloads` MUST be non-empty per entry.
- Every download's `sha256` MUST be exactly 64 lowercase hex
  chars.
- `tier == verified` entries MUST carry a `signing` block.
- `tier == community` entries MAY omit `signing` (operator
  decides via `config/extensions/trusted_keys.toml` in Phase 31.3).

## Roadmap

- **31.0 (this crate)** — index types + parser + validation.
- **31.1** — `nexo ext install <id>` CLI consumes this crate.
- **31.2** — per-plugin CI publish workflow generates entries.
- **31.3** — cosign verification + trusted_keys.toml config.
- **31.6** — `nexo plugin new --lang <rust|python|ts>` scaffolder.

See `proyecto/PHASES-curated.md` for the full Phase 31 plan.
