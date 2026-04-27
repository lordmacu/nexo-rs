# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/lordmacu/nexo-rs/compare/nexo-config-v0.1.1...nexo-config-v0.2.0) - 2026-04-27

### Added

- *(setup)* per-agent wizard submenu + yaml_patch helpers
- *(config)* pairing.yaml schema + loader + boot wiring [PR-6 partial]
- *(config,core)* Phase 67.D.1 — DispatchPolicy on agent + per-binding override

### Fixed

- *(clippy)* pairing.rs doc list overindent
- *(ci)* green-up rustfmt + clippy on rust 1.95 toolchain

### Other

- Phase 27.1: cargo-dist baseline + bundled WIP
- cargo fmt --all
- *(crates)* expand 6 more READMEs (setup, taskflow, config, mcp, memory, broker)
- *(release)* per-crate independent versioning
