# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/lordmacu/nexo-rs/releases/tag/nexo-plugin-browser-v0.1.1) - 2026-04-25

### Added

- *(browser)* BrowserConfig.args — forward extra flags to Chrome
- *(setup)* guided wizard + google plugin extraction + inline pairing
- agent framework phases 1-14 — runtime, memory, LLMs, plugins, skills, taskflow
- *(1.1)* workspace scaffold — 9 crates, config YAMLs, cargo build clean

### Fixed

- *(ci)* green-up rustfmt + clippy on rust 1.95 toolchain

### Other

- *(release)* per-crate independent versioning
- *(release)* bump workspace 0.1.0 → 0.1.1, add per-crate READMEs
- agent_* crates → nexo_*, agent bin → nexo
