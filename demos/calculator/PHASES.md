# Calculator — Implementation phases

Demo project for the Phase 67 driver subsystem. The shape of this
file matches what the project tracker expects (`## Phase NN — Title`
plus `#### NN.M — Sub-phase title  STATUS`).

## Status

Single-binary terminal calculator written in Rust. Target: REPL
that takes infix expressions, prints results, exits on `quit`.

## Phase 1 — Project skeleton

#### 1.1 — Cargo project + binary entry point   ✅

`cargo new calc --bin`. Single `main.rs` with a hello-world
println. Just enough to verify `cargo build` runs end-to-end.

#### 1.2 — README + LICENSE files                ✅

Short README describing the binary. Apache-2.0 LICENSE file.

#### 1.3 — Workspace lints + edition pin         ⬜

Add `[workspace.lints.rust]` with `unused_imports = "deny"` and
pin `edition = "2021"`. Keeps the demo from drifting on the
toolchain bump.

## Phase 2 — Lexer + parser

#### 2.1 — Tokeniser for numbers + operators     ⬜

Hand-written scanner that produces a `Vec<Token>` from a `&str`.
Tokens: `Number(f64)`, `Plus`, `Minus`, `Star`, `Slash`,
`LParen`, `RParen`. No floats with exponent yet, no scientific
notation. Errors carry the column offset.

Acceptance: `cargo test -p calc tokeniser`.

#### 2.2 — Recursive descent parser              ⬜

Pratt-style parser that respects `+`/`-` < `*`/`/` precedence.
Returns an `Expr` AST: `Num | BinOp(Box<Expr>, Op, Box<Expr>)`.
Handles parentheses. Bails on trailing tokens with a clear
error.

Acceptance: `cargo test -p calc parser`.

#### 2.3 — AST evaluator                          ⬜

Single recursive `eval(&Expr) -> Result<f64, EvalError>`. Maps
divide-by-zero to `EvalError::DivByZero`. Returns the same f64
the parser produced for atomic numbers.

Acceptance: `cargo test -p calc eval`.

## Phase 3 — REPL

#### 3.1 — Read-eval-print loop                  ⬜

Main loop reads `stdin` line-by-line, runs the lexer →
parser → evaluator pipeline, prints result. Exits on EOF or
literal `quit`.

Acceptance: `cargo build && echo '2+3' | cargo run` prints `5`.

#### 3.2 — `:help` + `:vars` meta commands       ⬜

Lines starting with `:` are control commands instead of
expressions. `:help` prints the command list. `:vars` prints
the current variable bindings (Phase 4).

Acceptance: smoke test via `expect`-style script.

## Phase 4 — Variables

#### 4.1 — Assignment syntax                      ⬜

Parser learns `let <name> = <expr>` and stores the value in a
`HashMap<String, f64>`. Re-assignment overwrites. Bare names in
expressions are looked up; missing names raise
`EvalError::UnknownName`.

Acceptance: `cargo test -p calc variables`.

#### 4.2 — Persistent history file                ⬜

REPL appends every assignment line to `~/.calc_history` in
plaintext. On boot, replays the file so previous sessions
resume. Skip silently when the file isn't writable.

Acceptance: integration test using a tempdir.

## Phase 5 — Polish

#### 5.1 — Error messages with carets            ⬜

When the lexer or parser fails, print the original input plus a
caret line pointing at the column. Inspired by `rustc`'s error
format.

Acceptance: golden-file test on three malformed inputs.

#### 5.2 — `--no-color` flag                     ⬜

Default output uses ANSI colour for prompt + errors. The flag
suppresses every escape sequence. Honours `NO_COLOR` env var
the same way.

Acceptance: snapshot test with TERM=dumb.

#### 5.3 — Release build + binary size budget    ⬜

`cargo build --release` produces a stripped binary under 600 KB
on x86_64 Linux. CI fails if the budget breaks.

Acceptance: `du -b target/release/calc` ≤ 614400.
