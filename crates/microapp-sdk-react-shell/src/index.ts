// Phase 83.18 — `@nexo/microapp-ui-shell-react`.
//
// Public entry. Today exports only the contract types
// (zero runtime dep). Subsequent commits in the lift will
// migrate the runtime primitives (Rail, ShellRoot,
// ModuleRegistry, hooks) from
// `agent-creator-microapp/frontend/src/shell/*` into this
// package.

export * from "./types";
