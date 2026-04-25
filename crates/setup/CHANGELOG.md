# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/lordmacu/nexo-rs/releases/tag/nexo-setup-v0.1.1) - 2026-04-25

### Added

- *(setup)* wizard entries for DeepSeek + generic OpenAI-compatible
- *(admin/channels)* Telegram edit (PATCH) + delete [no-docs]
- *(setup)* device-code OAuth inline en agent setup google
- *(setup)* phase 17 — multi-instance WA/TG + google-auth.yaml flows
- *(setup)* run credential gauntlet inside the wizard
- Ana sales agent + per-agent outbound allowlist + setup polish
- *(setup)* guided wizard + google plugin extraction + inline pairing
- agent framework phases 1-14 — runtime, memory, LLMs, plugins, skills, taskflow

### Other

- *(release)* per-crate independent versioning
- *(release)* bump workspace 0.1.0 → 0.1.1, add per-crate READMEs
- agent_* crates → nexo_*, agent bin → nexo
- clippy -D warnings pass on the whole workspace [no-docs]
- pre-release prep — CI, dual license, ext gating, Ana→MiniMax
