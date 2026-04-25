//! Line-based parsers for `PHASES.md` and `FOLLOWUPS.md`.
//!
//! We deliberately avoid a full markdown AST: the files we parse use
//! a constrained, well-formed dialect (#-prefixed headings at column
//! zero, status emoji at end of line, optional prose body), and the
//! parser only needs to skip fenced code blocks to be robust. That
//! beats pulling in `pulldown-cmark` for ~30 lines of state machine.

pub mod phases;
