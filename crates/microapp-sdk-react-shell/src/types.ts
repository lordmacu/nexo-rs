// Shell contract — every workspace module exports a manifest
// matching `ModuleManifest`. The shell discovers manifests via
// `import.meta.glob('../modules/*/manifest.tsx', { eager: true })`
// at boot, validates each through `ModuleManifestSchema`, and
// uses the result to drive the rail, sidebars, routes, and
// command palette.
//
// Versioning + migrations follow the same shape as scoped
// settings registries: each module declares a semver `version`
// and a `migrations` map keyed by source version. When the
// persisted state's version is older than the manifest's, the
// shell runs the migration chain synchronously before mounting.

import type { ComponentType, ReactNode } from "react";
import type { RouteObject } from "react-router-dom";
import { z } from "zod";

// ── Manifest contract ───────────────────────────────────────────

/** Public contract every module must export from
 *  `frontend/src/modules/<id>/manifest.tsx`. */
export interface ModuleManifest {
  /** Stable kebab-case id. Becomes the URL segment
   *  (`/m/<id>/...`) and the localStorage namespace
   *  (`module:<id>:*`). Matches `[a-z][a-z0-9-]{1,30}`. */
  readonly id: string;

  /** Semver triple. Bumped when the module's persisted state
   *  schema changes; the shell invokes `migrations[from]`
   *  on load if the persisted schema is older. */
  readonly version: string;

  /** Rail entry. The icon renders inside a 32 px square
   *  with a hover tooltip + ARIA label. The optional
   *  `badge` is recomputed on each shell render and shown
   *  as a notification dot or count. */
  readonly rail: ModuleRailEntry;

  /** React Router route objects mounted under `/m/<id>`. */
  readonly routes: readonly RouteObject[];

  /** Optional secondary sidebar component. Receives the
   *  active route path so it can highlight selection. */
  readonly sidebar?: ComponentType<SidebarSlotProps>;

  /** Optional context panel component. The shell only
   *  renders it when the active route's `meta.contextPanel`
   *  is truthy. */
  readonly contextPanel?: ComponentType<ContextSlotProps>;

  /** Cmd+K entries this module contributes. Recomputed
   *  each time the palette opens; cap at ~50 entries to
   *  keep the modal usable. */
  readonly cmdK?: (ctx: CmdKContext) => readonly CmdkActionLike[];

  /** Declarative capabilities the shell uses to gate
   *  features. */
  readonly capabilities?: ModuleCapabilities;

  /** Lifecycle. Returns optional teardown. Runs on the
   *  active module's mount; never runs for inactive
   *  modules. */
  readonly onMount?: (ctx: ShellContext) => void | (() => void);

  /** Module-state migration registry. Keyed by source
   *  version — i.e. `migrations["1.0.0"]` upgrades a
   *  `1.0.0` blob to whatever comes next. The shell
   *  chains them until the manifest version is reached. */
  readonly migrations?: Readonly<Record<string, ModuleMigration>>;
}

export interface ModuleRailEntry {
  readonly icon: ReactNode;
  readonly label: string;
  readonly tooltip?: string;
  /** Recomputed each render; return null for no badge. */
  readonly badge?: () => ReactNode | null;
  /** Sort key (lower = higher in the rail). 100..900
   *  reserved for first-party modules; user / plugin
   *  modules use 1000+. */
  readonly order: number;
}

export interface ModuleCapabilities {
  /** Re-mount the module (drop component state) when the
   *  active tenant switches. Default `true`. */
  readonly tenantSwitch?: boolean;
  /** The shell keeps an SSE firehose subscription alive
   *  for this module. Default `false`. */
  readonly firehose?: boolean;
  /** Module wants the browser-notifications opt-in
   *  banner. Default `false`. */
  readonly notificationsOptIn?: boolean;
}

export type ModuleMigration = (state: unknown) => unknown;

export interface SidebarSlotProps {
  readonly activePath: string;
  readonly tenantId: string;
}

export interface ContextSlotProps {
  readonly activePath: string;
  readonly tenantId: string;
}

// ── Shell-side context handed to modules ─────────────────────────

export interface ShellContext {
  readonly tenantId: string;
  readonly switchTenant: (id: string) => void;
  readonly navigate: (to: string) => void;
  readonly invalidateModule: (id: string) => void;
}

export interface CmdKContext extends ShellContext {
  readonly activeModuleId: string;
}

/** Loose action shape; the existing `lib/cmdk.ts`
 *  `CmdkAction` extends this. We keep the manifest contract
 *  decoupled from the modal's internal type so modules can
 *  ship without importing `lib/cmdk.ts`. */
export interface CmdkActionLike {
  readonly id: string;
  readonly label: string;
  readonly group?: string;
  readonly hotkey?: string;
  readonly run: (ctx: ShellContext) => Promise<void> | void;
}

// ── Typed errors ────────────────────────────────────────────────

export type ShellError =
  | { kind: "manifest_invalid"; module: string; cause: unknown }
  | { kind: "module_not_found"; module: string }
  | {
      kind: "migration_failed";
      module: string;
      from: string;
      to: string;
      cause: unknown;
    }
  | { kind: "tenant_unauthorised"; tenantId: string }
  | { kind: "storage_corrupt"; key: string };

// ── Zod schemas — runtime validation of imported manifests ───────
//
// The shape mirrors the TypeScript interfaces above. We
// validate at `ModuleRegistry.discover()` time so a malformed
// manifest never reaches the rendering tree — it just
// disappears from the rail with a console warning (and a dev
// toast).

const semverRegex = /^\d+\.\d+\.\d+$/;
const idRegex = /^[a-z][a-z0-9-]{1,30}$/;

export const ModuleRailEntrySchema = z.object({
  // ReactNode + functions can't be Zod-checked statically;
  // validate at boundary only.
  icon: z.unknown(),
  label: z.string().min(1).max(40),
  tooltip: z.string().max(120).optional(),
  badge: z.function().optional(),
  order: z.number().int().min(0).max(9999),
});

export const ModuleCapabilitiesSchema = z.object({
  tenantSwitch: z.boolean().optional(),
  firehose: z.boolean().optional(),
  notificationsOptIn: z.boolean().optional(),
});

export const ModuleManifestSchema = z.object({
  id: z.string().regex(idRegex, "id must be kebab-case 2-31 chars"),
  version: z.string().regex(semverRegex, "version must be semver triple"),
  rail: ModuleRailEntrySchema,
  routes: z.array(z.unknown()),
  sidebar: z.unknown().optional(),
  contextPanel: z.unknown().optional(),
  cmdK: z.function().optional(),
  capabilities: ModuleCapabilitiesSchema.optional(),
  onMount: z.function().optional(),
  migrations: z.record(z.function()).optional(),
});

// ── Convenience guards ──────────────────────────────────────────

/** Narrow `unknown` → `ShellError` without importing the
 *  union. Useful for catch blocks that re-throw or log. */
export function isShellError(value: unknown): value is ShellError {
  return (
    typeof value === "object" &&
    value !== null &&
    "kind" in value &&
    typeof (value as { kind: unknown }).kind === "string"
  );
}
