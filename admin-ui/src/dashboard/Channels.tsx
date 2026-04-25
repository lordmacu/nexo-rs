import { useCallback, useEffect, useState } from "react";

type Channel = {
  plugin: string;
  instance: string;
  source_file: string;
  allow_agents: string[];
};
type ChannelsResponse = { channels: Channel[] };

type ProbeState =
  | { state: "idle" }
  | { state: "probing" }
  | { state: "ok"; first_name: string; username: string }
  | { state: "err"; msg: string };

export function ChannelsSection() {
  const [data, setData] = useState<Channel[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showForm, setShowForm] = useState(false);
  const [instance, setInstance] = useState("");
  const [token, setToken] = useState("");
  const [allowAgents, setAllowAgents] = useState("");
  const [probe, setProbe] = useState<ProbeState>({ state: "idle" });
  const [submitting, setSubmitting] = useState(false);

  const load = useCallback(async () => {
    setError(null);
    try {
      const r = await fetch("/api/channels");
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const body = (await r.json()) as ChannelsResponse;
      setData(body.channels);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setData([]);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  // Reset probe state on token edits so a stale ✓ doesn't carry over.
  useEffect(() => {
    setProbe({ state: "idle" });
  }, [token]);

  const verifyToken = useCallback(async () => {
    const t = token.trim();
    if (!t) return;
    setProbe({ state: "probing" });
    try {
      const r = await fetch(
        `https://api.telegram.org/bot${encodeURIComponent(t)}/getMe`,
      );
      const d = await r.json();
      if (d.ok && d.result) {
        setProbe({
          state: "ok",
          first_name: d.result.first_name ?? "",
          username: d.result.username ?? "",
        });
      } else {
        setProbe({ state: "err", msg: d.description ?? "token rejected" });
      }
    } catch (e) {
      setProbe({
        state: "err",
        msg: e instanceof Error ? e.message : String(e),
      });
    }
  }, [token]);

  const submit = useCallback(async () => {
    setSubmitting(true);
    setError(null);
    try {
      const body: { instance: string; token: string; allow_agents?: string[] } = {
        instance: instance.trim(),
        token: token.trim(),
      };
      const agents = allowAgents
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean);
      if (agents.length > 0) body.allow_agents = agents;
      const r = await fetch("/api/channels/telegram", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      const d = await r.json();
      if (r.ok && d.ok) {
        setShowForm(false);
        setInstance("");
        setToken("");
        setAllowAgents("");
        setProbe({ state: "idle" });
        await load();
      } else {
        setError(d.error ?? `HTTP ${r.status}`);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  }, [instance, token, allowAgents, load]);

  return (
    <section className="rounded-lg border border-neutral-200 dark:border-neutral-800 p-4">
      <header className="flex items-center justify-between mb-3 gap-2 flex-wrap">
        <h3 className="text-sm uppercase tracking-wide text-neutral-500">
          Channels {data ? `(${data.length})` : ""}
        </h3>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={load}
            className="text-xs min-h-[2rem] px-2 rounded border border-neutral-300 dark:border-neutral-700 hover:bg-neutral-200 dark:hover:bg-neutral-800"
          >
            Refresh
          </button>
          <button
            type="button"
            onClick={() => setShowForm((s) => !s)}
            className="text-xs min-h-[2rem] px-2 rounded bg-blue-600 text-white hover:bg-blue-700"
          >
            {showForm ? "Cancel" : "+ Add Telegram"}
          </button>
        </div>
      </header>

      {error && (
        <p className="text-sm text-red-700 dark:text-red-400 mb-2">{error}</p>
      )}

      {showForm && (
        <div className="rounded-md border border-neutral-300 dark:border-neutral-700 bg-neutral-50 dark:bg-neutral-900 p-3 mb-3 space-y-3">
          <h4 className="text-sm font-semibold">New Telegram bot</h4>
          <label className="block">
            <span className="text-xs uppercase tracking-wide text-neutral-500">
              Instance label (kebab-snake, no spaces)
            </span>
            <input
              value={instance}
              onChange={(e) =>
                setInstance(
                  e.target.value
                    .toLowerCase()
                    .replace(/[^a-z0-9_-]/g, "-")
                    .replace(/--+/g, "-"),
                )
              }
              placeholder="sales-bot"
              className={inputCls + " mt-1"}
            />
          </label>
          <label className="block">
            <span className="text-xs uppercase tracking-wide text-neutral-500">
              Bot token
            </span>
            <input
              type="password"
              value={token}
              onChange={(e) => setToken(e.target.value)}
              placeholder="1234567:ABC-..."
              className={inputCls + " mt-1 font-mono"}
              autoComplete="off"
            />
          </label>
          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              onClick={verifyToken}
              disabled={probe.state === "probing" || !token.trim()}
              className="text-sm min-h-11 px-3 rounded-md border border-neutral-300 dark:border-neutral-700 hover:bg-neutral-200 dark:hover:bg-neutral-800 disabled:opacity-60"
            >
              {probe.state === "probing" ? "Verifying…" : "Verify token"}
            </button>
            {probe.state === "ok" && (
              <span className="text-sm text-green-700 dark:text-green-400">
                ✓ {probe.first_name} @{probe.username}
              </span>
            )}
            {probe.state === "err" && (
              <span className="text-sm text-red-700 dark:text-red-400">
                ✗ {probe.msg}
              </span>
            )}
          </div>
          <label className="block">
            <span className="text-xs uppercase tracking-wide text-neutral-500">
              Allowed agents (comma-separated, optional)
            </span>
            <input
              value={allowAgents}
              onChange={(e) => setAllowAgents(e.target.value)}
              placeholder="kate, ana"
              className={inputCls + " mt-1"}
            />
          </label>
          <button
            type="button"
            onClick={submit}
            disabled={submitting || !instance.trim() || !token.trim()}
            className="text-sm min-h-11 px-3 rounded bg-blue-600 text-white hover:bg-blue-700 disabled:opacity-60"
          >
            {submitting ? "Adding…" : "Add bot"}
          </button>
        </div>
      )}

      {data === null ? (
        <p className="text-sm text-neutral-500">loading…</p>
      ) : data.length === 0 ? (
        <p className="text-sm text-neutral-500">
          No channel instances yet. Click{" "}
          <span className="font-mono">+ Add Telegram</span> above to register
          a bot.
        </p>
      ) : (
        <ul className="space-y-2">
          {data.map((c, i) => (
            <li
              key={i}
              className="rounded-md border border-neutral-200 dark:border-neutral-800 px-3 py-2"
            >
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div>
                  <div className="font-semibold">
                    {c.plugin}
                    {c.instance ? (
                      <span className="text-neutral-500"> · {c.instance}</span>
                    ) : (
                      <span className="text-neutral-400"> · (unlabelled)</span>
                    )}
                  </div>
                  <div className="text-xs text-neutral-500 font-mono truncate max-w-md">
                    {c.source_file}
                  </div>
                </div>
                {c.allow_agents.length > 0 && (
                  <div className="flex flex-wrap gap-1">
                    {c.allow_agents.map((a, j) => (
                      <span
                        key={j}
                        className="text-xs bg-neutral-200 dark:bg-neutral-800 px-2 py-0.5 rounded"
                      >
                        {a}
                      </span>
                    ))}
                  </div>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

const inputCls =
  "w-full min-h-11 px-3 py-2 rounded-md border border-neutral-300 dark:border-neutral-700 bg-white dark:bg-neutral-900 text-neutral-900 dark:text-neutral-100 focus:outline-none focus:ring-2 focus:ring-blue-500";
