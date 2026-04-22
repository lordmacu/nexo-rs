# New Plugin Scaffold

Create a new plugin crate under `crates/plugins/$ARGUMENTS/`.

Steps:
1. Copy structure from `crates/plugins/template/`
2. Implement `Plugin` trait in `src/lib.rs`
3. Add to workspace `Cargo.toml`
4. Add config file `config/plugins/$ARGUMENTS.yaml`
5. Register in `config/agents.yaml` for relevant agents
