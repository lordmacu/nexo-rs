# Memdir scanner

`memdir` scanner support is currently documented through the MCP server
extension flow and OpenClaw-parity references.

Current status:
- scanner-style memory path logic is referenced in
  `docs/src/extensions/mcp-server.md` (`teamMemPaths` parity notes)
- there is no standalone operator CLI page yet for a dedicated
  `memdir scan` command

## What operators should do today

1. Use the MCP server extension docs as the canonical path for memory
   directory layout and exposure behavior.
2. Rely on existing memory docs for storage/runtime semantics:
   - [Long-term memory (SQLite)](../memory/long-term.md)
   - [Vector search](../memory/vector.md)
3. Track roadmap follow-ups in `PHASES.md` / `FOLLOWUPS.md` for an
   explicit scanner command surface.
