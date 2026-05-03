# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/lordmacu/nexo-rs/releases/tag/nexo-fork-v0.1.1) - 2026-05-03

### Added

- *(83.8.12.1)* empresa wire shapes + BindingContext + AgentConfig empresa_id
- *(83.1)* AgentConfig.extensions_config field + 2 YAML round-trip tests
- *(84.2.2)* nexo-fork producers — ForkResult / ForkError → TaskNotification
- *(80.1.b.b.b)* AgentToolDispatcher bridge in nexo-fork
- *(82.4.4)* EventSubscriberBinding schema + AgentConfig field
- *(config,driver-loop,main)* extract_memories boot wire (M4.a.b)

### Other

- *(83.8.12.1.fix)* rename empresa → tenant for English code identifiers
- *(wip)* checkpoint mid-refactor + split microapp PHASES into dedicated file
