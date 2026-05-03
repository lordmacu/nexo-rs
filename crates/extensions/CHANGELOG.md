# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/lordmacu/nexo-rs/compare/nexo-extensions-v0.1.1...nexo-extensions-v0.2.0) - 2026-05-03

### Added

- *(82.6.b)* stamp NEXO_EXTENSION_STATE_ROOT onto extension env
- *(83.3)* hook vote-to-block / vote-to-transform wire shapes + 19 tests
- *(82.6)* per-extension state_dir helper + nexo ext state-dir CLI
- *(82.10.h.b.5)* main.rs admin RPC bootstrap wire-path
- *(82.10.h.b.3)* AdminRouter trait + reader_task app: prefix routing
- *(82.3.2)* plugin.toml [outbound_bindings] schema

### Fixed

- *(ci)* green-up rustfmt + clippy on rust 1.95 toolchain

### Other

- *(crates)* expand 4 more READMEs (core, llm, pairing, extensions)
- *(release)* per-crate independent versioning
