# OpenClaw Skills Research And Implementation Plan

## Goal

Bring the most useful parts of the OpenClaw "skills" model into `proyecto`
without forcing a large runtime rewrite.

The first implementation target is intentionally small:

1. add a minimal skill layer to agent config and prompt assembly
2. load skill documents from disk
3. validate the flow with one first skill: `weather`

This keeps the first pass low-risk and compatible with the extension + MCP
architecture that `proyecto` already has.

## Research Summary

### 1. What OpenClaw calls a "skill"

After reviewing the OpenClaw reference tree in `research/`, the main finding is
that a large part of the skill system is not "magic runtime behavior".

Most skills are one of these:

- instruction bundles in `SKILL.md`
- wrappers around existing CLIs or services
- operational runbooks that tell the agent when and how to use a tool
- internal maintenance workflows for the OpenClaw repo itself

That distinction matters because `proyecto` already has strong support for
runtime tools, but it does not yet have a first-class prompt-layer for skills.

### 2. Skills inventory reviewed

I reviewed the OpenClaw skill tree in three groups:

- `research/skills`: 53 base skills
- `research/extensions`: 14 extension-provided skills
- `research/.agents/skills`: 12 internal maintainer skills

Total reviewed: 79 skills

The total under `research/` alone appears lower if hidden paths are excluded,
because `.agents/skills` is a hidden directory.

### 3. Skills inspected in detail

I inspected these skills directly because they are the best candidates for
adoption in `proyecto`:

- `research/skills/weather/SKILL.md`
- `research/skills/openai-whisper-api/SKILL.md`
- `research/skills/summarize/SKILL.md`
- `research/skills/goplaces/SKILL.md`
- `research/skills/github/SKILL.md`
- `research/skills/taskflow/SKILL.md`

### 4. Most useful candidates for `proyecto`

#### `weather`

Why it is useful:

- immediate user value
- very small surface area
- no API key required in the OpenClaw version
- good first skill to validate config + loading + prompt injection

Recommended shape in `proyecto`:

- first as a prompt-layer skill
- later optionally as a Rust extension if we want explicit weather tools

#### `openai-whisper-api`

Why it is useful:

- complements the Rust `msedge-tts` extension that is already in progress
- gives us a clean voice input + voice output story
- practical and broadly reusable

Recommended shape in `proyecto`:

- Rust extension or MCP-backed tool
- optional skill doc explaining when to use it

#### `summarize`

Why it is useful:

- high leverage for URLs, PDFs, YouTube, and local files
- broad utility for everyday agent work
- a good example of a capability plus a usage policy

Recommended shape in `proyecto`:

- MCP integration if an existing server is available
- or Rust extension if we want full local control
- plus an instruction-layer skill

#### `goplaces`

Why it is useful:

- practical real-world place lookup
- maps well to structured tool inputs and outputs

Tradeoff:

- requires `GOOGLE_PLACES_API_KEY`

Recommended shape in `proyecto`:

- Rust extension
- optional skill doc

#### `github`

Why it is useful:

- strong value for development workflows
- very relevant for this repo's own usage pattern

Recommended shape in `proyecto`:

- MCP first if available
- otherwise extension
- instruction doc can be thin because the tool itself carries most of the value

#### `taskflow`

Why it is useful:

- the highest long-term value for durable, multi-step, resumable work

Why it is not a first-pass skill:

- it is not just a markdown instruction layer
- it wants runtime semantics: identity, persistence, wait state, resume,
  child-task linkage, revision-safe mutations

Recommended shape in `proyecto`:

- native core feature later
- optional skill on top of it after the runtime exists

### 5. Skills I do not recommend starting with

- macOS-specific skills such as `apple-notes`, `bear-notes`, `things-mac`,
  `peekaboo`
- repo-maintainer skills from `research/.agents/skills`
- Feishu / QQ-specific skills unless those channels become product goals

These are either too ecosystem-specific or too tied to OpenClaw's own repo
operations.

## Current State Of `proyecto`

### Runtime capability layer already exists

`proyecto` already has a strong base for executable capabilities:

- extension discovery starts in `src/main.rs`
- extension tools and hooks are registered in `src/main.rs`
- MCP runtime bootstrap also lives in `src/main.rs`
- Rust extension scaffolding already exists in `extensions/template-rust/`

Relevant files:

- `src/main.rs`
- `config/extensions.yaml`
- `config/mcp.yaml`
- `extensions/template-rust/README.md`

This means we do not need to invent a new runtime mechanism for tool-backed
skills.

### Prompt-layer skill support does not exist yet

The agent currently builds its system prompt from:

- workspace bundle
- inline `system_prompt`

There is no dedicated skill layer between those two.

Relevant files:

- `crates/core/src/agent/llm_behavior.rs`
- `crates/core/src/agent/workspace.rs`
- `crates/config/src/types/agents.rs`
- `config/agents.yaml`

### Important architectural conclusion

OpenClaw-style skills should be split into two categories in `proyecto`:

1. tool-backed capabilities
2. instruction-layer skills

If we do not separate them, we will end up mixing runtime behavior,
configuration, and prompt policy in one feature.

## Proposed Phase 1 Scope

### Objective

Add the smallest useful skill system that works with the current agent prompt
pipeline and proves the concept with one real skill.

### Planned behavior

Each agent can declare a list of active skills in config. For each declared
skill, the runtime will load a markdown file from disk and inject its contents
into the system prompt.

Planned prompt order:

1. workspace bundle
2. loaded skill documents
3. inline `system_prompt`

That order is intentional:

- workspace remains the strongest identity and continuity layer
- skills supply reusable operating instructions
- `system_prompt` remains the local override for that specific agent

### Planned file layout

Proposed new directory:

- `skills/<skill-name>/SKILL.md`

Example:

- `skills/weather/SKILL.md`

This mirrors the OpenClaw layout closely enough to keep migration easy.

## Files Planned For The First Implementation

### Config surface

Update:

- `crates/config/src/types/agents.rs`
- `config/agents.yaml`

Planned change:

- add `skills: Vec<String>` to `AgentConfig`
- default to empty
- document the new YAML field in the sample config

### Skill loading

Add a small loader module in `nexo_core`.

Proposed new file:

- `crates/core/src/agent/skills.rs`

Responsibilities:

- resolve configured skill names to `skills/<name>/SKILL.md`
- read markdown safely
- skip missing or unreadable skills with warnings instead of hard failure
- return a rendered block ready for prompt injection

Non-goals for phase 1:

- no recursive skill dependency graph
- no frontmatter parsing
- no auto-discovery based on user intent
- no remote skill marketplace

### Prompt assembly

Update:

- `crates/core/src/agent/llm_behavior.rs`
- `crates/core/src/agent/mod.rs`

Planned change:

- load configured skill docs during system prompt assembly
- inject them after workspace blocks and before `system_prompt`
- keep the final result as one `system` message, matching the current caching
  strategy

### First skill content

Add:

- `skills/weather/SKILL.md`

This skill will document:

- when to use weather lookup
- when not to use it
- expected location specificity
- the intended source/tool strategy for the first iteration

For phase 1, the weather skill is a prompt-layer validation artifact. It does
not require a dedicated Rust extension on day one.

## Validation Plan

### Tests to add or update

Targeted updates are expected in:

- `crates/core/tests/llm_behavior_test.rs`

Planned test coverage:

- configured skills are injected into the system message
- order is preserved: workspace -> skills -> system prompt
- missing skill files do not crash the agent
- empty `skills` produces current behavior unchanged

### Build and regression checks

Planned verification sequence:

1. targeted tests for config and llm behavior
2. `cargo build --workspace`
3. broader test pass if the tree is stable enough

Because `AgentConfig` is constructed explicitly in multiple tests, adding a new
field will likely require touching several test fixtures.

## Risks And Tradeoffs

### 1. Prompt bloat

If skill docs become large, prompt size will grow quickly.

Mitigation:

- keep skill docs focused
- start with manual selection only
- avoid loading all skills automatically

### 2. Ambiguity between skills and tools

If users expect "a skill" to mean "a callable tool", the system becomes
confusing.

Mitigation:

- document the split clearly
- keep runtime capabilities in extensions or MCP
- keep usage guidance in `SKILL.md`

### 3. Missing or renamed skills

A strict failure mode would make agent startup fragile.

Mitigation:

- warn and continue for missing skill files in phase 1

### 4. Overreaching first implementation

Trying to clone all of OpenClaw at once would add too much surface area.

Mitigation:

- first pass only adds local skill loading
- future capabilities stay incremental

## Follow-Up Phases After Phase 1

### Phase 2: add real tool-backed capabilities

Recommended order:

1. `weather` tool or extension
2. `openai-whisper-api`
3. `summarize`
4. `goplaces`
5. `github`

These can use:

- Rust extensions under `extensions/`
- MCP servers configured in `config/mcp.yaml`

### Phase 3: richer skill metadata

Possible later additions:

- frontmatter fields such as `name`, `description`, `requires`
- optional per-skill char limits
- operator-visible warnings for missing dependencies

### Phase 4: durable workflow runtime

This is where `taskflow`-like behavior belongs.

That work should be designed as a native runtime feature, not as a markdown
skill hack.

## Out Of Scope For The First Pass

- automatic skill triggering from natural language
- GUI installer / marketplace
- OpenClaw maintainer skills
- macOS-only operational skills
- taskflow runtime semantics
- extension generation from `SKILL.md`

## Recommended Next Step

After this document is approved, the next implementation step should be:

1. add `skills: []` to agent config
2. add the local skill loader
3. inject skill content into the system prompt
4. add `skills/weather/SKILL.md`
5. validate with tests and build

This gives us a clean foundation before we add more expensive capabilities like
Whisper, Summarize, GitHub integration, or durable flows.
