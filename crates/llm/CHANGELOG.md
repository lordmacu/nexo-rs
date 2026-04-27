# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2](https://github.com/lordmacu/nexo-rs/compare/nexo-llm-v0.1.1...nexo-llm-v0.1.2) - 2026-04-27

### Added

- *(llm)* SSE parser benches [Phase 35.3]

### Fixed

- *(llm/telemetry)* pure renderer kills test/test global race [Phase 38.x.1]
- *(ci)* cross arm64 jammy image + ignore 2 known concurrency-flake tests
- *(ci)* green-up rustfmt + clippy on rust 1.95 toolchain
- *(llm/anthropic)* drop needless as_deref on Option<&'static str>

### Other

- Phase 27.1: cargo-dist baseline + bundled WIP
- cargo fmt --all
- *(crates)* expand 4 more READMEs (core, llm, pairing, extensions)
- *(release)* per-crate independent versioning
