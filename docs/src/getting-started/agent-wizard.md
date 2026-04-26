# Agent-centric setup wizard

The hub menu's `Configurar agente (canal, modelo, idioma, skills)`
entry drops the operator into a per-agent submenu. Where the rest of
the wizard groups actions by *service* (Telegram, OpenAI, the
browser plugin), this submenu groups them by *agent*: pick one agent
up front, then mutate its model, language, channels, and skills from
a single dashboard. Every action reuses the existing channel / LLM /
skill flows underneath, so behavior stays in lockstep with the rest
of the wizard.

```bash
./target/release/agent setup
# → Configurar agente (canal, modelo, idioma, skills)
```

## Dashboard

```text
Agente: kate
  Modelo:   anthropic / claude-haiku-4-5  [creds ✔]
  Idioma:   es
  Canales:  ✔ telegram:default  (bound)
            ✗ whatsapp:default  (unbound)
  Skills:   8 / 24 attached
```

The dashboard is recomputed from disk on every loop iteration, so
the screen always reflects the most recent YAML state.

## Action menu

After the dashboard renders, the operator picks one of:

| Action     | Effect                                                                                              |
|------------|-----------------------------------------------------------------------------------------------------|
| `Modelo`   | Attach / detach / change the LLM provider + model name. Re-uses the LLM credential form when secrets are missing. |
| `Idioma`   | Pick from `es / en / pt / fr / it / de`, or clear the directive.                                    |
| `Canales`  | Auth/Reauth, Bind, or Unbind a channel for this agent. Auth flows are the same `services_imperative` dispatchers the legacy menu uses. |
| `Skills`   | Multi-select against the skill catalog. Newly added skills with required secrets prompt for creds.  |
| `← volver` | Exit the submenu, return to the hub.                                                                |

## YAML mutations

| Action               | YAML path                                                  | Operation                              |
|----------------------|-------------------------------------------------------------|----------------------------------------|
| Attach model         | `agents[<id>].model.provider`, `…model.model`               | `upsert_agent_field`                   |
| Detach model         | `agents[<id>].model`                                         | `remove_agent_field`                   |
| Set language         | `agents[<id>].language`                                      | `upsert_agent_field`                   |
| Clear language       | `agents[<id>].language`                                      | `remove_agent_field`                   |
| Bind channel         | `agents[<id>].plugins[]`, `agents[<id>].inbound_bindings[]`  | `append_agent_list_item` (idempotent)  |
| Unbind channel       | `agents[<id>].plugins[]`, `agents[<id>].inbound_bindings[]`  | `remove_agent_list_item` by predicate  |
| Replace skills       | `agents[<id>].skills`                                        | `upsert_agent_field` (full sequence)   |

All mutations land atomically (tempfile + rename) and are gated by
the same process-wide YAML mutex the legacy upsert path uses, so
concurrent wizard sessions don't corrupt the file.

## Hot-reload

After every successful mutation, the wizard fires a best-effort
`nexo --config <dir> reload` so a running daemon picks up the YAML
edit without a manual restart. The call is fire-and-forget: when
the binary isn't on `PATH` or the daemon isn't running, the wizard
keeps going silently.

## Where the code lives

* `crates/setup/src/agent_wizard.rs` — submenu + dashboard.
* `crates/setup/src/yaml_patch.rs` — `read_agent_field`,
  `upsert_agent_field`, `remove_agent_field`,
  `append_agent_list_item`, `remove_agent_list_item`.
* `crates/setup/tests/agent_wizard_yaml.rs` — schema-roundtrip tests
  that re-parse the mutated YAML through `nexo_config::AgentsConfig`.
