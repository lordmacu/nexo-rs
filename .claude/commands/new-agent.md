# New Agent Definition

Add a new agent entry to `config/agents.yaml` named $ARGUMENTS.

Required fields:
- `id` — unique string
- `model.provider` — minimax | openai | anthropic | ollama
- `model.model` — model name
- `plugins` — list of enabled plugins
- `heartbeat.enabled` + `heartbeat.interval` — optional

Also create:
- `agents/$ARGUMENTS/IDENTITY.md` — agent persona/instructions
- `agents/$ARGUMENTS/MEMORY.md` — initial long-term memory seed
