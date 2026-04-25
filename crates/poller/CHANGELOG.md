# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/lordmacu/nexo-rs/releases/tag/nexo-poller-v0.1.1) - 2026-04-25

### Added

- *(poller)* kind: agent_turn — scheduled LLM turns from YAML
- *(poller/gmail)* retire legacy crate + ship six gmail_* LLM tools
- *(poller/gmail)* seen-id dedup cache + sample pollers.yaml
- *(poller)* per-kind custom tools — Poller::custom_tools()
- *(poller)* four V1 built-ins + register_all
- *(poller)* admin HTTP + CLI client
- *(poller)* runner core + backoff + hot-reload
- *(poller)* telemetry + dispatch + config schemas
- *(poller)* scaffold + types + schedule + sqlite state

### Fixed

- *(ci)* green-up rustfmt + clippy on rust 1.95 toolchain

### Other

- *(release)* per-crate independent versioning
- *(release)* bump workspace 0.1.0 → 0.1.1, add per-crate READMEs
- agent_* crates → nexo_*, agent bin → nexo
