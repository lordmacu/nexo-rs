import { useCallback, useEffect, useState } from "react";

type Channel = {
  plugin: string;
  instance: string;
  source_file: string;
  allow_agents: string[];
  allowlist_chat_ids?: number[];
  auto_transcribe?: { enabled: boolean; command: string; language: string };
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
  const [editingInstance, setEditingInstance] = useState<string | null>(null);

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

  const handleDelete = useCallback(
    async (plugin: string, instance: string) => {
      const ok = window.confirm(
        `Remove ${plugin} instance "${instance}"?\n\n` +
          "The YAML entry is dropped; the secret token file stays on disk so " +
          "you can re-add the same instance without re-pasting the token. " +
          "To fully wipe, delete ./secrets/<instance>_telegram_token.txt by hand.",
      );
      if (!ok) return;
      setError(null);
      try {
        const r = await fetch(
          `/api/channels/${encodeURIComponent(plugin)}/${encodeURIComponent(instance)}`,
          { method: "DELETE" },
        );
        const d = await r.json();
        if (r.ok && d.ok) {
          await load();
        } else {
          setError(d.error ?? `HTTP ${r.status}`);
        }
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
            onClick={() => {
              setShowForm((s) => !s);
              setEditingInstance(null);
            }}
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
        <TelegramForm
          mode="create"
          onDone={async () => {
            setShowForm(false);
            await load();
          }}
          onError={setError}
        />
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
          {data.map((c, i) => {
            const isEditing =
              editingInstance === `${c.plugin}:${c.instance}` &&
              c.plugin === "telegram";
            return (
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
                  <div className="flex items-center gap-2 flex-wrap justify-end">
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
                    {c.plugin === "telegram" && c.instance && (
                      <button
                        type="button"
                        onClick={() =>
                          setEditingInstance(
                            isEditing ? null : `${c.plugin}:${c.instance}`,
                          )
                        }
                        className="text-xs min-h-[2rem] px-2 rounded border border-neutral-300 dark:border-neutral-700 hover:bg-neutral-200 dark:hover:bg-neutral-800"
                      >
                        {isEditing ? "Close" : "Edit"}
                      </button>
                    )}
                    {c.instance && (
                      <button
                        type="button"
                        onClick={() => handleDelete(c.plugin, c.instance)}
                        className="text-xs min-h-[2rem] px-2 rounded border border-red-300 dark:border-red-800 text-red-700 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-950/30"
                      >
                        Delete
                      </button>
                    )}
                  </div>
                </div>
                {isEditing && (
                  <div className="mt-3">
                    <TelegramForm
                      mode="edit"
                      initialInstance={c.instance}
                      initialAllowAgents={c.allow_agents}
                      initialChatIds={c.allowlist_chat_ids ?? []}
                      initialAutoTranscribe={
                        c.auto_transcribe ?? {
                          enabled: false,
                          command: "",
                          language: "",
                        }
                      }
                      onDone={async () => {
                        setEditingInstance(null);
                        await load();
                      }}
                      onError={setError}
                    />
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

interface FormProps {
  mode: "create" | "edit";
  initialInstance?: string;
  initialAllowAgents?: string[];
  initialChatIds?: number[];
  initialAutoTranscribe?: { enabled: boolean; command: string; language: string };
  onDone: () => void | Promise<void>;
  onError: (msg: string | null) => void;
}

function TelegramForm({
  mode,
  initialInstance,
  initialAllowAgents,
  initialChatIds,
  initialAutoTranscribe,
  onDone,
  onError,
}: FormProps) {
  const [instance, setInstance] = useState(initialInstance ?? "");
  const [token, setToken] = useState("");
  const [allowAgents, setAllowAgents] = useState(
    (initialAllowAgents ?? []).join(", "),
  );
  const [chatIdsText, setChatIdsText] = useState(
    (initialChatIds ?? []).join(", "),
  );
  const [atEnabled, setAtEnabled] = useState(
    initialAutoTranscribe?.enabled ?? false,
  );
  const [atCommand, setAtCommand] = useState(
    initialAutoTranscribe?.command ?? "",
  );
  const [atLanguage, setAtLanguage] = useState(
    initialAutoTranscribe?.language ?? "",
  );
  const [probe, setProbe] = useState<ProbeState>({ state: "idle" });
  const [submitting, setSubmitting] = useState(false);

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
    onError(null);
    try {
      const agents = allowAgents
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean);

      if (mode === "create") {
        const body: { instance: string; token: string; allow_agents?: string[] } = {
          instance: instance.trim(),
          token: token.trim(),
        };
        if (agents.length > 0) body.allow_agents = agents;
        const r = await fetch("/api/channels/telegram", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        });
        const d = await r.json();
        if (r.ok && d.ok) {
          await onDone();
        } else {
          onError(d.error ?? `HTTP ${r.status}`);
        }
      } else {
        const body: {
          token?: string;
          allow_agents: string[];
          allowlist_chat_ids?: number[];
          auto_transcribe?: { enabled: boolean; command?: string; language?: string };
        } = {
          allow_agents: agents,
        };
        if (token.trim()) body.token = token.trim();
        const parsedIds = chatIdsText
          .split(/[\s,\n]+/)
          .map((s) => s.trim())
          .filter(Boolean)
          .map((s) => Number(s))
          .filter((n) => Number.isFinite(n));
        body.allowlist_chat_ids = parsedIds;
        body.auto_transcribe = {
          enabled: atEnabled,
          command: atCommand.trim(),
          language: atLanguage.trim(),
        };
        const r = await fetch(
          `/api/channels/telegram/${encodeURIComponent(initialInstance ?? "")}`,
          {
            method: "PATCH",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(body),
          },
        );
        const d = await r.json();
        if (r.ok && d.ok) {
          await onDone();
        } else {
          onError(d.error ?? `HTTP ${r.status}`);
        }
      }
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  }, [
    mode,
    instance,
    token,
    allowAgents,
    chatIdsText,
    atEnabled,
    atCommand,
    atLanguage,
    initialInstance,
    onDone,
    onError,
  ]);

  const isCreate = mode === "create";
  return (
    <div className="rounded-md border border-neutral-300 dark:border-neutral-700 bg-neutral-50 dark:bg-neutral-900 p-3 space-y-3">
      <h4 className="text-sm font-semibold">
        {isCreate ? "New Telegram bot" : `Edit Telegram bot · ${initialInstance}`}
      </h4>
      {isCreate && (
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
      )}
      <label className="block">
        <span className="text-xs uppercase tracking-wide text-neutral-500">
          Bot token {!isCreate && <em>(leave empty to keep the current one)</em>}
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
          Allowed agents (comma-separated{!isCreate && ", empty to clear"})
        </span>
        <input
          value={allowAgents}
          onChange={(e) => setAllowAgents(e.target.value)}
          placeholder="kate, ana"
          className={inputCls + " mt-1"}
        />
      </label>
      {!isCreate && (
        <>
          <label className="block">
            <span className="text-xs uppercase tracking-wide text-neutral-500">
              Chat-id allowlist (comma- or newline-separated ints; empty = accept all)
            </span>
            <textarea
              value={chatIdsText}
              onChange={(e) => setChatIdsText(e.target.value)}
              placeholder="1194292426, -1001234567890"
              rows={2}
              className={inputCls + " mt-1 font-mono text-sm"}
            />
          </label>
          <div className="rounded-md border border-neutral-300 dark:border-neutral-700 p-3 space-y-2">
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={atEnabled}
                onChange={(e) => setAtEnabled(e.target.checked)}
                className="h-4 w-4"
              />
              <span className="font-medium">Auto-transcribe voice messages</span>
            </label>
            {atEnabled && (
              <>
                <label className="block">
                  <span className="text-xs uppercase tracking-wide text-neutral-500">
                    Transcriber command (path to whisper binary; default uses
                    the shipped openai-whisper extension)
                  </span>
                  <input
                    value={atCommand}
                    onChange={(e) => setAtCommand(e.target.value)}
                    placeholder="./extensions/openai-whisper/target/release/openai-whisper"
                    className={inputCls + " mt-1 font-mono text-sm"}
                  />
                </label>
                <label className="block">
                  <span className="text-xs uppercase tracking-wide text-neutral-500">
                    Force language (ISO code, optional)
                  </span>
                  <input
                    value={atLanguage}
                    onChange={(e) => setAtLanguage(e.target.value)}
                    placeholder="es"
                    className={inputCls + " mt-1 w-24"}
                  />
                </label>
              </>
            )}
          </div>
        </>
      )}
      <button
        type="button"
        onClick={submit}
        disabled={
          submitting || (isCreate && (!instance.trim() || !token.trim()))
        }
        className="text-sm min-h-11 px-3 rounded bg-blue-600 text-white hover:bg-blue-700 disabled:opacity-60"
      >
        {submitting
          ? isCreate
            ? "Adding…"
            : "Saving…"
          : isCreate
          ? "Add bot"
          : "Save changes"}
      </button>
    </div>
  );
}

const inputCls =
  "w-full min-h-11 px-3 py-2 rounded-md border border-neutral-300 dark:border-neutral-700 bg-white dark:bg-neutral-900 text-neutral-900 dark:text-neutral-100 focus:outline-none focus:ring-2 focus:ring-blue-500";
