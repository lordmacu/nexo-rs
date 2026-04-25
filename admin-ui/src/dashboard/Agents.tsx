import { useCallback, useEffect, useMemo, useState } from "react";

type Channel = { plugin: string; instance: string };
type Agent = {
  id: string;
  description: string;
  model: string;
  channels: Channel[];
};

type ChannelOption = { plugin: string; instance: string };

export function AgentsSection() {
  const [data, setData] = useState<Agent[] | null>(null);
  const [channels, setChannels] = useState<ChannelOption[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [expandedId, setExpandedId] = useState<string | null>(null);

  const load = useCallback(async () => {
    setError(null);
    try {
      const [ra, rc] = await Promise.all([
        fetch("/api/agents"),
        fetch("/api/channels"),
      ]);
      if (!ra.ok) throw new Error(`agents HTTP ${ra.status}`);
      if (!rc.ok) throw new Error(`channels HTTP ${rc.status}`);
      const bodyA = (await ra.json()) as { agents: Agent[] };
      const bodyC = (await rc.json()) as { channels: ChannelOption[] };
      setData(bodyA.agents);
      setChannels(bodyC.channels.filter((c) => c.instance.length > 0));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setData([]);
      setChannels([]);
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
          {data.map((a) => {
            const isExpanded = expandedId === a.id;
            return (
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
                  <div className="flex items-center gap-2 flex-wrap justify-end">
                    <div className="text-xs font-mono bg-neutral-100 dark:bg-neutral-900 px-2 py-1 rounded">
                      {a.model}
                    </div>
                    <button
                      type="button"
                      onClick={() =>
                        setExpandedId(isExpanded ? null : a.id)
                      }
                      className="text-xs min-h-[2rem] px-2 rounded border border-neutral-300 dark:border-neutral-700 hover:bg-neutral-200 dark:hover:bg-neutral-800"
                    >
                      {isExpanded ? "Close" : "Credentials"}
                    </button>
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
                {isExpanded && (
                  <CredentialsPanel
                    agentId={a.id}
                    channels={channels ?? []}
                    onSaved={async () => {
                      setExpandedId(null);
                      await load();
                    }}
                    onError={setError}
                  />
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

function CredentialsPanel({
  agentId,
  channels,
  onSaved,
  onError,
}: {
  agentId: string;
  channels: ChannelOption[];
  onSaved: () => void | Promise<void>;
  onError: (msg: string | null) => void;
}) {
  const [telegram, setTelegram] = useState("");
  const [whatsapp, setWhatsapp] = useState("");
  const [google, setGoogle] = useState("");
  const [submitting, setSubmitting] = useState(false);

  const telegramOpts = useMemo(
    () => channels.filter((c) => c.plugin === "telegram"),
    [channels],
  );
  const whatsappOpts = useMemo(
    () => channels.filter((c) => c.plugin === "whatsapp"),
    [channels],
  );

  const submit = useCallback(async () => {
    setSubmitting(true);
    onError(null);
    try {
      // Only include keys the operator actually set. Empty string is
      // the explicit "unset" sentinel understood by the backend.
      const body: Record<string, string> = {};
      if (telegram !== "") body.telegram = telegram;
      if (whatsapp !== "") body.whatsapp = whatsapp;
      if (google !== "") body.google = google;
      if (Object.keys(body).length === 0) {
        onError("pick at least one channel to pin or clear");
        setSubmitting(false);
        return;
      }
      const r = await fetch(
        `/api/agents/${encodeURIComponent(agentId)}/credentials`,
        {
          method: "PATCH",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        },
      );
      const d = await r.json();
      if (r.ok && d.ok) {
        await onSaved();
      } else {
        onError(d.error ?? `HTTP ${r.status}`);
      }
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  }, [agentId, telegram, whatsapp, google, onSaved, onError]);

  return (
    <div className="mt-3 rounded-md border border-neutral-300 dark:border-neutral-700 bg-neutral-50 dark:bg-neutral-900 p-3 space-y-3">
      <h4 className="text-sm font-semibold">
        Pin <span className="font-mono">{agentId}</span> to channel instances
      </h4>
      <p className="text-xs text-neutral-500">
        Writes the <code className="font-mono">credentials:</code> block in
        the agent's YAML. Pick a channel instance from the dropdown (or
        choose <em>— unset —</em> to remove the pin). Channels without
        instances don't appear here; add them from the Channels section.
      </p>

      <CredentialRow
        label="Telegram"
        options={telegramOpts}
        value={telegram}
        onChange={setTelegram}
      />
      <CredentialRow
        label="WhatsApp"
        options={whatsappOpts}
        value={whatsapp}
        onChange={setWhatsapp}
      />
      <label className="block">
        <span className="text-xs uppercase tracking-wide text-neutral-500">
          Google account id (free-form; must match google-auth.yaml)
        </span>
        <input
          value={google}
          onChange={(e) => setGoogle(e.target.value)}
          placeholder="you@gmail.com (empty = untouched; '  ' = unset)"
          className="w-full min-h-11 px-3 py-2 mt-1 rounded-md border border-neutral-300 dark:border-neutral-700 bg-white dark:bg-neutral-900 text-neutral-900 dark:text-neutral-100 focus:outline-none focus:ring-2 focus:ring-blue-500"
        />
      </label>

      <button
        type="button"
        onClick={submit}
        disabled={submitting}
        className="text-sm min-h-11 px-3 rounded bg-blue-600 text-white hover:bg-blue-700 disabled:opacity-60"
      >
        {submitting ? "Saving…" : "Save credentials"}
      </button>
    </div>
  );
}

function CredentialRow({
  label,
  options,
  value,
  onChange,
}: {
  label: string;
  options: ChannelOption[];
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <label className="block">
      <span className="text-xs uppercase tracking-wide text-neutral-500">
        {label}
      </span>
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="w-full min-h-11 px-3 py-2 mt-1 rounded-md border border-neutral-300 dark:border-neutral-700 bg-white dark:bg-neutral-900 text-neutral-900 dark:text-neutral-100 focus:outline-none focus:ring-2 focus:ring-blue-500"
      >
        <option value="">— untouched —</option>
        <option value=" ">— unset —</option>
        {options.map((o) => (
          <option key={o.instance} value={o.instance}>
            {o.instance}
          </option>
        ))}
      </select>
    </label>
  );
}
