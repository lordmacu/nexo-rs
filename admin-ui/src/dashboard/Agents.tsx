import { useCallback, useEffect, useState } from "react";

type Channel = { plugin: string; instance: string };
type Agent = {
  id: string;
  description: string;
  model: string;
  channels: Channel[];
};
type AgentsResponse = { agents: Agent[] };

export function AgentsSection() {
  const [data, setData] = useState<Agent[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setError(null);
    try {
      const r = await fetch("/api/agents");
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const body = (await r.json()) as AgentsResponse;
      setData(body.agents);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setData([]);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  return (
    <section className="rounded-lg border border-neutral-200 dark:border-neutral-800 p-4">
      <header className="flex items-center justify-between mb-3 gap-2">
        <h3 className="text-sm uppercase tracking-wide text-neutral-500">
          Agents {data ? `(${data.length})` : ""}
        </h3>
        <button
          type="button"
          onClick={load}
          className="text-xs min-h-[2rem] px-2 rounded border border-neutral-300 dark:border-neutral-700 hover:bg-neutral-200 dark:hover:bg-neutral-800"
        >
          Refresh
        </button>
      </header>

      {error && (
        <p className="text-sm text-red-700 dark:text-red-400 mb-2">{error}</p>
      )}

      {data === null ? (
        <p className="text-sm text-neutral-500">loading…</p>
      ) : data.length === 0 ? (
        <p className="text-sm text-neutral-500">
          No agents yet. Run the first-run wizard to create one.
        </p>
      ) : (
        <ul className="space-y-2">
          {data.map((a) => (
            <li
              key={a.id}
              className="rounded-md border border-neutral-200 dark:border-neutral-800 px-3 py-2"
            >
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div>
                  <div className="font-semibold">{a.id}</div>
                  {a.description && (
                    <div className="text-xs text-neutral-500 truncate max-w-xs">
                      {a.description}
                    </div>
                  )}
                </div>
                <div className="text-xs font-mono bg-neutral-100 dark:bg-neutral-900 px-2 py-1 rounded">
                  {a.model}
                </div>
              </div>
              {a.channels.length > 0 && (
                <div className="mt-2 flex flex-wrap gap-1">
                  {a.channels.map((c, i) => (
                    <span
                      key={i}
                      className="text-xs bg-blue-100 dark:bg-blue-950/40 text-blue-900 dark:text-blue-200 px-2 py-0.5 rounded"
                    >
                      {c.plugin}
                      {c.instance ? ` · ${c.instance}` : ""}
                    </span>
                  ))}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
