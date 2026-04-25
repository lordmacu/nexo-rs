# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/lordmacu/nexo-rs/releases/tag/nexo-plugin-whatsapp-v0.1.1) - 2026-04-25

### Added

- *(pairing)* channel-adapter registry + per-channel reply delivery
- *(auth)* per-(channel,instance) circuit breakers
- *(auth)* phase 17 — runtime integration
- *(auth)* phase 17 scaffold — per-agent credential framework
- *(plugins,core)* outbound + delegation read effective policy
- *(setup)* guided wizard + google plugin extraction + inline pairing
- agent framework phases 1-14 — runtime, memory, LLMs, plugins, skills, taskflow
- *(1.1)* workspace scaffold — 9 crates, config YAMLs, cargo build clean

### Fixed

- *(ci)* green-up rustfmt + clippy on rust 1.95 toolchain
- *(audit)* land 18 of 25 findings from AUDIT-2026-04-25-pass2

### Other

- *(release)* bump nexo-pairing + nexo-memory to 0.1.2; sync path-dep pins
- *(release)* per-crate independent versioning
- *(release)* bump workspace 0.1.0 → 0.1.1, add per-crate READMEs
- agent_* crates → nexo_*, agent bin → nexo
- clippy -D warnings pass on the whole workspace [no-docs]
- *(whatsapp)* pull wa-agent 0.1.1 from crates.io
