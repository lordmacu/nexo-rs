# `@nexo/microapp-ui-shell-react`

Reusable workspace shell primitives for [nexo-rs] microapps:
vertical icon rail + secondary sidebar + main content area +
optional context panel + module manifest contract.

**Phase 83.18** of the nexo-rs microapp framework.

## Status

🚧 **Scaffolded — not feature-complete.**

The `agent-creator-microapp` ships the canonical implementation
under `frontend/src/shell/*`. This package currently exports only
the **contract types** (`ModuleManifest`, `ShellError`, slot
prop shapes, Zod schema). The runtime primitives (`<Rail>`,
`<ShellRoot>`, `<ModuleRegistry>`, hooks) are scheduled to lift
into this package in a multi-commit follow-up — gated on a
second consumer microapp landing so the lift validates against
real reuse, not a synthetic second consumer.

See `proyecto/PHASES-microapps.md` Phase 83.18 for the full
scope, done criteria, and effort estimate.

## Sibling: `@nexo/microapp-ui-react` (Phase 83.13)

83.13 is the chat-specific component library (WhatsApp Web
3-column layout). 83.18 is the multi-module shell that *hosts*
that chat library inside one of its panels:

- A microapp with no shell concept (single-page batch dashboard)
  ignores 83.18 entirely.
- A microapp with a chat surface but no other modules ignores
  83.18 and consumes 83.13 directly.
- A microapp with both imports both — they compose because 83.18
  doesn't know what 83.13 renders inside the main panel.

## Contract types (today)

```ts
import type {
  ModuleManifest,
  ModuleRailEntry,
  ModuleCapabilities,
  SidebarSlotProps,
  ContextSlotProps,
  ShellContext,
  CmdKContext,
  CmdkActionLike,
  ShellError,
} from "@nexo/microapp-ui-shell-react/types";

import { ModuleManifestSchema, isShellError } from "@nexo/microapp-ui-shell-react";
```

## Planned exports (after the runtime lift)

```ts
import {
  Rail,
  SecondarySidebar,
  ContextPanel,
  ShellRoot,
  ModuleErrorBoundary,
  TenantSwitcher,
  ModuleRegistry,
  useShellContext,
  useTenant,
  useUrlState,
  useViewport,
  usePersistedWidth,
  useRegistry,
} from "@nexo/microapp-ui-shell-react";
```

## Dependencies (peer)

- `react` ^18
- `react-dom` ^18
- `react-router-dom` ^6
- `react-resizable-panels` ^2
- `zustand` ^5
- `zod` ^3

## License

Dual-licensed under MIT or Apache-2.0, at your option. Mirrors
the rest of the nexo-rs SDK posture.

[nexo-rs]: https://github.com/lordmacu/nexo-rs
