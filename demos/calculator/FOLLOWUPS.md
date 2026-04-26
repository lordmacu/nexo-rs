# Follow-ups

Active backlog for the calculator demo. Same shape the project
tracker parses for the real `proyecto/FOLLOWUPS.md`.

## Open items

### Hardening

H-1. **Fuzz the parser**
- Missing: a `cargo-fuzz` target that feeds random bytes into
  the lexer + parser and asserts no panic. We currently rely on
  hand-written negative tests (Phase 5.1) which don't catch
  pathological grouping like `((((((((1))))))))`.
- Why deferred: parser surface is small enough that fuzzing
  feels heavy until the variable feature lands. Worth doing
  after Phase 4.
- Target: post Phase 4.

H-2. **Reject NaN / inf at the AST evaluator**
- Missing: `eval` happily returns `f64::NAN` for `0.0 / 0.0`
  and `f64::INFINITY` for `1.0 / 0.0`. The REPL prints them as
  `NaN` / `inf`. Ideally we surface them as `EvalError`s so the
  caller sees a structured failure instead of a magical string.
- Why deferred: bikeshed on what the right semantics are
  (calculator users expect `1 / 0 = error`, but a programming
  REPL might want `inf`). Decide after we add a `--strict`
  flag.

### Phase 4 — Variables

V-1. ~~**Variable lookup spec'd**~~  ✅ shipped 2026-04-26
- Names follow `[A-Za-z_][A-Za-z0-9_]*`. Reserved: `let`, `quit`,
  `pi`, `e`. Pre-defined `pi` and `e` constants.
- Re-assignment overwrites without warning. We considered
  forbidding overwrite of pre-defined constants but agreed the
  whole point of a calculator is being able to scratch
  numbers, including over `pi` for a quick estimate.

V-2. **Multi-character operators**
- Missing: `**` for exponentiation, `//` for integer division.
  Parser currently only knows `+ - * /`.
- Why deferred: needs a precedence rework + careful tokeniser
  to distinguish `**` from `* *`. Punted to a Phase 6.

V-3. **Tab-completion of variable names in the REPL**
- Missing: `rustyline` integration so `<Tab>` cycles through
  defined names. Today the REPL is line-based with no editing.
- Why deferred: third-party dep doubles the binary size budget
  (Phase 5.3). Worth revisiting if we add a `--rich` mode.

### Phase 5 — Polish

P-1. **Dynamic prompt showing last result**
- Missing: prompt today is always `> `. Would be nice to show
  `[ans=42] > ` after a previous result so the user can refer
  back without scrolling.
- Why deferred: surface choice (env-var / flag / config file)
  not decided yet.
- Target: when a real user complains.

P-2. **Windows release pipeline**
- Missing: CI release workflow only builds Linux + macOS. The
  binary builds fine on Windows when invoked manually but the
  release-plz job doesn't push a `.exe`.
- Why deferred: no Windows users yet for the demo.

## Resolved (recent highlights)

- 2026-04-25 — Lexer scanner pinned to a regex-free
  hand-written DFA so the binary doesn't pull in `regex`. Saved
  ~80 KB on the release build.
- 2026-04-22 — Parser AST nodes drop `Box<Expr>` in favour of an
  index into a `Vec<Expr>` arena. Faster + simpler clone story.
