---
name: Bug report
about: Something in nexo-rs behaves incorrectly
title: "[bug] "
labels: bug
assignees: ''
---

## Summary

<!-- One sentence describing what's broken. -->

## Environment

- **nexo-rs commit / tag:** <!-- `git rev-parse HEAD` or tag -->
- **Rust version:** <!-- `rustc -V` -->
- **OS / arch:** <!-- e.g. Ubuntu 24.04 x86_64, macOS 14 arm64, Termux arm64 -->
- **Deployment:** <!-- native / Docker compose / Termux / systemd / other -->
- **Broker mode:** <!-- nats / local -->

## Reproduction

Minimal steps to trigger the bug. Include the smallest config that
reproduces it (redact API keys and phone numbers).

```yaml
# config/agents.yaml relevant excerpt
```

```bash
# commands you ran
```

## Expected behavior

<!-- What you thought would happen. -->

## Actual behavior

<!-- What actually happened. -->

## Logs

<details>
<summary>Relevant log lines (redact secrets)</summary>

```
<!-- paste here -->
```

</details>

## Additional context

<!-- Anything else: upstream changes, recent config edits, etc. -->

## Confirmation

- [ ] I searched open and closed issues for duplicates
- [ ] I redacted secrets (API keys, tokens, phone numbers) from logs
- [ ] I tried reproducing on the tip of `main`
