import { useCallback, useEffect, useState } from "react";
import { Wizard } from "./wizard/Wizard";
import { AgentsSection } from "./dashboard/Agents";
import { ChannelsSection } from "./dashboard/Channels";
import { McpServersSection } from "./dashboard/McpServers";

type DebugEnv = { debug: boolean; build: string };
type Bootstrap = { needs_wizard: boolean; agent_count: number };

export default function App() {
  const [loggingOut, setLoggingOut] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const [debugEnv, setDebugEnv] = useState<DebugEnv | null>(null);
  const [bootstrap, setBootstrap] = useState<Bootstrap | null>(null);
  const [resetBusy, setResetBusy] = useState(false);
  const [resetReport, setResetReport] = useState<string | null>(null);

  useEffect(() => {
    document.title = "nexo-rs admin";
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);

  const refreshBootstrap = useCallback(() => {
    fetch("/api/bootstrap")
      .then((r) => (r.ok ? r.json() : null))
      .then((d: Bootstrap | null) => setBootstrap(d))
      .catch(() => setBootstrap(null));
  }, []);

  useEffect(() => {
    refreshBootstrap();
    fetch("/api/debug/env")
      .then((r) => (r.ok ? r.json() : null))
      .then((d: DebugEnv | null) => setDebugEnv(d))
      .catch(() => setDebugEnv(null));
  }, [refreshBootstrap]);

  const handleLogout = async () => {
    setLoggingOut(true);
    try {
      await fetch("/api/logout", { method: "POST" });
    } catch {
      /* cookies rotate on daemon restart anyway */
    }
    window.location.replace("/login");
  };

  const handleReset = useCallback(async () => {
    const ok = window.confirm(
      "This will DELETE every agent definition, plugin session, workspace, memory DB, transcripts, DLQ, and every file under ./data/**.\n\n" +
        "API keys under ./secrets/** and your hand-edited top-level config/*.yaml are preserved.\n\n" +
        "Continue?",
    );
    if (!ok) return;
    setResetBusy(true);
    setResetReport(null);
    try {
      const r = await fetch("/api/debug/reset", { method: "POST" });
      const data = await r.json();
      if (data.ok) {
        setResetReport(
          `Cleared ${data.cleared.length} path(s)${
            data.errors.length ? `, ${data.errors.length} error(s)` : ""
          }. Reloading wizard state…`,
        );
        refreshBootstrap();
      } else {
        setResetReport(`Reset failed: ${data.error ?? "unknown error"}`);
      }
    } catch (e) {
      setResetReport(
        "Reset failed: " + (e instanceof Error ? e.message : String(e)),
      );
    } finally {
      setResetBusy(false);
    }
  }, [refreshBootstrap]);

  const debugOn = debugEnv?.debug === true;

  // Loading gate — waits for /api/bootstrap so we don't flash the
  // dashboard first and then redirect into the wizard.
  if (bootstrap === null) {
    return (
      <div className="min-h-screen grid place-items-center bg-neutral-50 dark:bg-neutral-950 text-neutral-500">
        <span className="text-sm font-mono">loading…</span>
      </div>
    );
  }

  if (bootstrap.needs_wizard) {
    return <Wizard onFinish={refreshBootstrap} />;
  }

  return (
    <div className="min-h-screen bg-neutral-50 dark:bg-neutral-950 text-neutral-900 dark:text-neutral-100 font-sans">
      <header className="border-b border-neutral-200 dark:border-neutral-800">
        <div className="max-w-3xl mx-auto px-4 sm:px-6 py-3 sm:py-4 flex flex-wrap items-center justify-between gap-2">
          <h1 className="text-base sm:text-lg font-semibold tracking-tight">
            nexo-rs <span className="text-neutral-400 font-normal">admin</span>
          </h1>
          <div className="flex items-center gap-2 sm:gap-3">
            <span className="text-xs font-mono bg-neutral-200 dark:bg-neutral-800 px-2 py-1 rounded">
              {debugEnv?.build ?? "…"}
            </span>
            <button
              type="button"
              onClick={handleLogout}
              disabled={loggingOut}
              className="text-xs min-h-[2.5rem] px-3 rounded border border-neutral-300 dark:border-neutral-700 hover:bg-neutral-200 dark:hover:bg-neutral-800 disabled:opacity-60"
            >
              {loggingOut ? "Signing out…" : "Sign out"}
            </button>
          </div>
        </div>
      </header>

      <main className="max-w-3xl mx-auto px-4 sm:px-6 py-8 sm:py-12 space-y-6 sm:space-y-8">
        <section>
          <h2 className="text-2xl font-semibold mb-1">Dashboard</h2>
          <p className="text-neutral-600 dark:text-neutral-400 text-sm">
            {bootstrap.agent_count === 1
              ? "1 agent registered."
              : `${bootstrap.agent_count} agents registered.`}{" "}
            Per-agent edit + channel pairing flows coming next; see{" "}
            <code className="font-mono text-xs">admin-ui/PHASES.md</code>.
          </p>
        </section>

        <AgentsSection />
        <ChannelsSection />
        <McpServersSection />

        <section className="rounded-lg border border-neutral-200 dark:border-neutral-800 p-4">
          <h3 className="text-sm uppercase tracking-wide text-neutral-500 mb-2">
            Session
          </h3>
          <dl className="space-y-1 text-sm">
            <div className="flex justify-between">
              <dt className="text-neutral-500">status</dt>
              <dd className="font-mono">authenticated</dd>
            </div>
            <div className="flex justify-between">
              <dt className="text-neutral-500">server time</dt>
              <dd className="font-mono">{new Date(now).toISOString()}</dd>
            </div>
          </dl>
        </section>

        {debugOn && (
          <section className="rounded-lg border border-red-300 dark:border-red-900 bg-red-50 dark:bg-red-950/30 p-4">
            <h3 className="text-sm uppercase tracking-wide text-red-700 dark:text-red-400 mb-2">
              Debug — danger zone
            </h3>
            <p className="text-sm text-red-900 dark:text-red-200 mb-3">
              Reset deletes every agent definition (except{" "}
              <code className="font-mono">*.example.yaml</code>), plugin
              sessions, workspaces, transcripts, and every runtime database.
              API keys under <code className="font-mono">./secrets</code> are
              preserved. Next load fires the wizard again.
            </p>
            <button
              type="button"
              onClick={handleReset}
              disabled={resetBusy}
              className="text-sm px-3 py-2 rounded bg-red-600 text-white hover:bg-red-700 disabled:opacity-60"
            >
              {resetBusy ? "Resetting…" : "Reset everything"}
            </button>
            {resetReport && (
              <p className="mt-3 text-sm font-mono whitespace-pre-line">
                {resetReport}
              </p>
            )}
          </section>
        )}

        <section className="text-sm text-neutral-600 dark:text-neutral-400">
          <p>
            Docs:{" "}
            <a
              href="https://lordmacu.github.io/nexo-rs/"
              className="underline text-blue-600 dark:text-blue-400"
              rel="noreferrer"
            >
              lordmacu.github.io/nexo-rs
            </a>
          </p>
        </section>
      </main>
    </div>
  );
}
