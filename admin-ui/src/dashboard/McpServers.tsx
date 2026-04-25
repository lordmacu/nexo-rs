import { FormEvent, useCallback, useEffect, useState } from "react";

type Transport = "stdio" | "streamable_http" | "sse";

type McpServer = {
  name: string;
  transport: Transport;
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  url?: string;
  headers?: Record<string, string>;
  log_level?: string;
  context_passthrough: boolean | null;
};

type ListResponse = { servers: McpServer[] };

type FormMode =
  | { kind: "create" }
  | { kind: "edit"; original: McpServer };

export function McpServersSection() {
  const [data, setData] = useState<McpServer[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [form, setForm] = useState<FormMode | null>(null);

  const load = useCallback(async () => {
    setError(null);
    try {
      const r = await fetch("/api/mcp/servers");
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const body = (await r.json()) as ListResponse;
      setData(body.servers);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setData([]);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const handleDelete = useCallback(
    async (name: string) => {
      const ok = window.confirm(
        `Remove MCP server "${name}"?\n\n` +
          "The YAML entry is dropped. Reload daemon for changes to take effect " +
          "unless mcp.watch.enabled is true.",
      );
      if (!ok) return;
      setError(null);
      try {
        const r = await fetch(`/api/mcp/servers/${encodeURIComponent(name)}`, {
          method: "DELETE",
        });
        const d = await r.json();
        if (r.ok && d.ok) await load();
        else setError(d.error ?? `HTTP ${r.status}`);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [load],
  );

  return (
    <section className="rounded-lg border border-neutral-200 dark:border-neutral-800 p-4">
      <header className="flex items-center justify-between mb-3 gap-2 flex-wrap">
        <h3 className="text-sm uppercase tracking-wide text-neutral-500">
          MCP servers {data ? `(${data.length})` : ""}
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
            onClick={() =>
              setForm((f) => (f === null ? { kind: "create" } : null))
            }
            className="text-xs min-h-[2rem] px-2 rounded bg-blue-600 text-white hover:bg-blue-700"
          >
            {form ? "Cancel" : "+ Add MCP server"}
          </button>
        </div>
      </header>

      {error && (
        <p className="text-sm text-red-700 dark:text-red-400 mb-2">{error}</p>
      )}

      {form && (
        <McpServerForm
          mode={form}
          onDone={async () => {
            setForm(null);
            await load();
          }}
          onError={setError}
        />
      )}

      {data === null ? (
        <p className="text-sm text-neutral-500">loading…</p>
      ) : data.length === 0 ? (
        <p className="text-sm text-neutral-500">
          No MCP servers configured yet. Click{" "}
          <span className="font-mono">+ Add MCP server</span> to register one.
        </p>
      ) : (
        <ul className="space-y-2">
          {data.map((s) => (
            <li
              key={s.name}
              className="rounded border border-neutral-200 dark:border-neutral-800 p-3 text-sm"
            >
              <div className="flex justify-between items-start gap-2 flex-wrap">
                <div className="min-w-0">
                  <p className="font-mono font-semibold truncate">{s.name}</p>
                  <p className="text-xs text-neutral-500">
                    transport: {s.transport}
                    {s.transport === "stdio"
                      ? ` · command: ${s.command ?? ""}`
                      : ` · url: ${s.url ?? ""}`}
                  </p>
                  {s.log_level && (
                    <p className="text-xs text-neutral-500">
                      log_level: {s.log_level}
                    </p>
                  )}
                  {s.context_passthrough !== null && (
                    <p className="text-xs text-neutral-500">
                      context_passthrough:{" "}
                      {String(s.context_passthrough)}
                    </p>
                  )}
                </div>
                <div className="flex gap-2 flex-shrink-0">
                  <button
                    type="button"
                    onClick={() => setForm({ kind: "edit", original: s })}
                    className="text-xs min-h-[2rem] px-2 rounded border border-neutral-300 dark:border-neutral-700 hover:bg-neutral-200 dark:hover:bg-neutral-800"
                  >
                    Edit
                  </button>
                  <button
                    type="button"
                    onClick={() => handleDelete(s.name)}
                    className="text-xs min-h-[2rem] px-2 rounded border border-red-300 dark:border-red-800 text-red-700 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-950/40"
                  >
                    Delete
                  </button>
                </div>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function McpServerForm(props: {
  mode: FormMode;
  onDone: () => Promise<void> | void;
  onError: (msg: string | null) => void;
}) {
  const initial = props.mode.kind === "edit" ? props.mode.original : null;
  const editing = initial !== null;
  const [name, setName] = useState(initial?.name ?? "");
  const [transport, setTransport] = useState<Transport>(
    initial?.transport ?? "stdio",
  );
  const [command, setCommand] = useState(initial?.command ?? "");
  const [argsText, setArgsText] = useState(
    (initial?.args ?? []).join("\n"),
  );
  const [envText, setEnvText] = useState(
    Object.entries(initial?.env ?? {})
      .map(([k, v]) => `${k}=${v}`)
      .join("\n"),
  );
  const [url, setUrl] = useState(initial?.url ?? "");
  const [headersText, setHeadersText] = useState(
    Object.entries(initial?.headers ?? {})
      .map(([k, v]) => `${k}: ${v}`)
      .join("\n"),
  );
  const [logLevel, setLogLevel] = useState(initial?.log_level ?? "");
  const [ctxOverride, setCtxOverride] = useState<string>(
    initial?.context_passthrough === null ||
      initial?.context_passthrough === undefined
      ? ""
      : initial.context_passthrough
        ? "true"
        : "false",
  );
  const [busy, setBusy] = useState(false);

  const submit = useCallback(
    async (e: FormEvent) => {
      e.preventDefault();
      props.onError(null);
      const trimmedName = name.trim();
      if (!editing && !trimmedName) {
        props.onError("name is required");
        return;
      }
      const body: Record<string, unknown> = {
        name: trimmedName,
        transport,
      };
      if (transport === "stdio") {
        body.command = command.trim();
        body.args = argsText
          .split("\n")
          .map((s) => s.trim())
          .filter(Boolean);
        const env: Record<string, string> = {};
        for (const line of envText.split("\n")) {
          const t = line.trim();
          if (!t) continue;
          const i = t.indexOf("=");
          if (i <= 0) continue;
          env[t.slice(0, i).trim()] = t.slice(i + 1).trim();
        }
        body.env = env;
      } else {
        body.url = url.trim();
        const headers: Record<string, string> = {};
        for (const line of headersText.split("\n")) {
          const t = line.trim();
          if (!t) continue;
          const i = t.indexOf(":");
          if (i <= 0) continue;
          headers[t.slice(0, i).trim()] = t.slice(i + 1).trim();
        }
        body.headers = headers;
      }
      if (logLevel.trim()) body.log_level = logLevel.trim();
      if (ctxOverride === "true") body.context_passthrough = true;
      else if (ctxOverride === "false") body.context_passthrough = false;

      setBusy(true);
      try {
        const target = editing
          ? `/api/mcp/servers/${encodeURIComponent(initial!.name)}`
          : "/api/mcp/servers";
        const method = editing ? "PATCH" : "POST";
        const r = await fetch(target, {
          method,
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        });
        const d = await r.json();
        if (r.ok && d.ok) {
          await props.onDone();
        } else {
          props.onError(d.error ?? `HTTP ${r.status}`);
        }
      } catch (e) {
        props.onError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(false);
      }
    },
    [
      argsText,
      command,
      ctxOverride,
      editing,
      envText,
      headersText,
      initial,
      logLevel,
      name,
      props,
      transport,
      url,
    ],
  );

  return (
    <form onSubmit={submit} className="space-y-3 mb-4">
      <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
        <label className="block">
          <span className="block text-xs text-neutral-500 mb-1">name</span>
          <input
            type="text"
            value={name}
            disabled={editing}
            onChange={(e) => setName(e.target.value)}
            placeholder="filesystem"
            className="w-full px-2 py-2 rounded border border-neutral-300 dark:border-neutral-700 bg-white dark:bg-neutral-900 font-mono text-sm disabled:opacity-60"
            required
          />
        </label>
        <label className="block">
          <span className="block text-xs text-neutral-500 mb-1">
            transport
          </span>
          <select
            value={transport}
            onChange={(e) => setTransport(e.target.value as Transport)}
            className="w-full px-2 py-2 rounded border border-neutral-300 dark:border-neutral-700 bg-white dark:bg-neutral-900 font-mono text-sm"
          >
            <option value="stdio">stdio</option>
            <option value="streamable_http">streamable_http</option>
            <option value="sse">sse</option>
          </select>
        </label>
      </div>

      {transport === "stdio" ? (
        <>
          <label className="block">
            <span className="block text-xs text-neutral-500 mb-1">command</span>
            <input
              type="text"
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              placeholder="npx"
              className="w-full px-2 py-2 rounded border border-neutral-300 dark:border-neutral-700 bg-white dark:bg-neutral-900 font-mono text-sm"
              required
            />
          </label>
          <label className="block">
            <span className="block text-xs text-neutral-500 mb-1">
              args (one per line)
            </span>
            <textarea
              value={argsText}
              onChange={(e) => setArgsText(e.target.value)}
              rows={3}
              placeholder={"-y\n@modelcontextprotocol/server-filesystem\n/home/familia"}
              className="w-full px-2 py-2 rounded border border-neutral-300 dark:border-neutral-700 bg-white dark:bg-neutral-900 font-mono text-xs"
            />
          </label>
          <label className="block">
            <span className="block text-xs text-neutral-500 mb-1">
              env (KEY=VALUE per line)
            </span>
            <textarea
              value={envText}
              onChange={(e) => setEnvText(e.target.value)}
              rows={2}
              placeholder="DEBUG=1"
              className="w-full px-2 py-2 rounded border border-neutral-300 dark:border-neutral-700 bg-white dark:bg-neutral-900 font-mono text-xs"
            />
          </label>
        </>
      ) : (
        <>
          <label className="block">
            <span className="block text-xs text-neutral-500 mb-1">url</span>
            <input
              type="url"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://api.example.com/mcp"
              className="w-full px-2 py-2 rounded border border-neutral-300 dark:border-neutral-700 bg-white dark:bg-neutral-900 font-mono text-sm"
              required
            />
          </label>
          <label className="block">
            <span className="block text-xs text-neutral-500 mb-1">
              headers (Header: value per line)
            </span>
            <textarea
              value={headersText}
              onChange={(e) => setHeadersText(e.target.value)}
              rows={3}
              placeholder={"Authorization: Bearer ${TOKEN}\nX-Client: agent-rs"}
              className="w-full px-2 py-2 rounded border border-neutral-300 dark:border-neutral-700 bg-white dark:bg-neutral-900 font-mono text-xs"
            />
          </label>
        </>
      )}

      <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
        <label className="block">
          <span className="block text-xs text-neutral-500 mb-1">
            log_level (optional)
          </span>
          <select
            value={logLevel}
            onChange={(e) => setLogLevel(e.target.value)}
            className="w-full px-2 py-2 rounded border border-neutral-300 dark:border-neutral-700 bg-white dark:bg-neutral-900 font-mono text-sm"
          >
            <option value="">(server default)</option>
            <option value="debug">debug</option>
            <option value="info">info</option>
            <option value="notice">notice</option>
            <option value="warning">warning</option>
            <option value="error">error</option>
            <option value="critical">critical</option>
            <option value="alert">alert</option>
            <option value="emergency">emergency</option>
          </select>
        </label>
        <label className="block">
          <span className="block text-xs text-neutral-500 mb-1">
            context_passthrough override
          </span>
          <select
            value={ctxOverride}
            onChange={(e) => setCtxOverride(e.target.value)}
            className="w-full px-2 py-2 rounded border border-neutral-300 dark:border-neutral-700 bg-white dark:bg-neutral-900 font-mono text-sm"
          >
            <option value="">(use global)</option>
            <option value="true">force on</option>
            <option value="false">force off</option>
          </select>
        </label>
      </div>

      <button
        type="submit"
        disabled={busy}
        className="text-sm min-h-[2.5rem] px-3 rounded bg-blue-600 text-white hover:bg-blue-700 disabled:opacity-60"
      >
        {busy ? "Saving…" : editing ? "Save changes" : "Create server"}
      </button>
    </form>
  );
}
