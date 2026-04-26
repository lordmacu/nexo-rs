# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/lordmacu/nexo-rs/compare/nexo-setup-v0.1.1...nexo-setup-v0.2.0) - 2026-04-26

### Added

- *(setup)* per-agent wizard submenu + yaml_patch helpers
- *(setup)* linear channel link flow (canal → agente → reauth/vincular)
- *(setup)* channel dashboard inside `nexo setup` step 3
- *(setup)* web-search wizard entry [W-3]
- *(project-tracker)* Phase 67.A.5 — config YAML + capabilities entry

### Fixed

- *(setup)* single-shot link flow with optional reauth + telegram chat-link
- *(setup)* wizard enumerates agents.d/ drop-ins, not just agents.yaml

### Other

- cargo fmt --all
- *(crates)* expand 6 more READMEs (setup, taskflow, config, mcp, memory, broker)
