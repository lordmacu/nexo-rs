# Dependencies — modes and bin versions

A skill that depends on a CLI tool or an environment variable can
declare those needs in `requires`. The runtime resolves the
declarations at load time and decides whether to expose the skill,
hide it, or expose it with a visible warning the LLM can see.

```yaml
---
name: ffmpeg-tools
requires:
  bins: [ffmpeg]
  env:  [TRANSCODE_OUTPUT_DIR]
  bin_versions:
    ffmpeg: ">=4.0"
  mode: strict          # default
---
```

## Modes

| Mode | When deps are missing | LLM sees the skill? |
|------|-----------------------|---------------------|
| `strict` (default) | Skill is dropped | No |
| `warn` | Skill loads with a `> ⚠️ MISSING DEPS …` banner prepended to its body | Yes — with the warning inline |
| `disable` | Skill is always dropped, even when deps are satisfied | No |

### Per-agent override

Operators override a skill's declared mode without editing the skill
file:

```yaml
agents:
  - id: kate
    skills: [ffmpeg-tools]
    skill_overrides:
      ffmpeg-tools: warn
```

Resolution order:

1. `agents.<id>.skill_overrides[<name>]` (operator wins)
2. Skill frontmatter `requires.mode`
3. `strict` (built-in default)

## Bin versions

`requires.bin_versions` adds a semver constraint on top of mere bin
presence. Failing the constraint is treated like a missing dep —
the active mode decides whether to skip or warn.

### Constraint syntax

[semver](https://docs.rs/semver/) request strings:

| Want | Constraint |
|------|-----------|
| At least 4.0 | `">=4.0"` |
| Any 4.x compatible release | `"^4.0"` |
| 4.x but no 5 | `">=4.0, <5.0"` |
| Exact 4.2.1 | `"=4.2.1"` |
| Patch-compatible to 5.1.3 | `"~5.1.3"` |

Versions like `4.2` are normalized to `4.2.0` before comparison so
constraint matching works against partial outputs.

### Custom probe

Defaults: `<bin> --version`, regex `\d+\.\d+(?:\.\d+)?`. Override
when a tool emits something idiosyncratic:

```yaml
requires:
  bin_versions:
    curl:
      constraint: ">=8.0"
      command: "--help"
      regex: 'curl (\d+\.\d+(?:\.\d+)?)'
```

The shorthand form `bin: ">=4.0"` and the long form
`bin: { constraint: …, command: …, regex: … }` are both accepted.

### Probe fail modes

| Reason | When |
|--------|------|
| `bin_not_found` | Binary not on PATH |
| `probe_failed` | Spawn errored or timed out (5 s cap) |
| `parse_failed` | The default regex (or override) didn't match |
| `constraint_unsatisfied` | Found version doesn't match the constraint |
| `invalid_constraint` | Constraint string couldn't be parsed as semver |

Invalid constraints log at `error` level; the skill is treated as
having a missing dep — boot continues so a typo in one skill doesn't
take the whole agent down. Probes are cached process-wide by absolute
path so a bin shared across skills only spawns once.

## Banner format

When `mode: warn` and any dep is missing, the skill body is rendered
to the LLM with this prefix:

```
> ⚠️ MISSING DEPS for skill `ffmpeg-tools`:
>   - bin not found: ffmpeg
>   - env unset: TRANSCODE_OUTPUT_DIR
>   - version mismatch: ffmpeg requires >=4.0 (found 3.4.2)
> Calls into this skill may fail.
```

The LLM treats this like any other markdown context, so it has the
information it needs to either avoid the skill or report a useful
error to the user when a tool call fails.

## Backwards compatibility

Skills without `requires.mode`, `requires.bin_versions`, or
`agents.<id>.skill_overrides` keep the prior behavior (strict, no
version checks). The defaults are chosen so an unmodified skill
catalog and existing agents.yaml continue to work unchanged.
