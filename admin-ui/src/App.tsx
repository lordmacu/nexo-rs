import { useEffect, useState } from "react";

type Health = {
  status: string;
  ts_ms: number;
};

export default function App() {
  // Placeholder state — a real probe against /api/health lands when
  // the admin HTTP router ships API routes in a follow-up commit. For
  // now we just prove the React bundle is alive inside the agent
  // binary, with Basic Auth already validated by the Rust side.
  const [hello] = useState<Health>(() => ({
    status: "hello, world",
    ts_ms: Date.now(),
  }));

  useEffect(() => {
    document.title = "nexo-rs admin";
  }, []);

  return (
    <div className="min-h-screen bg-neutral-50 dark:bg-neutral-950 text-neutral-900 dark:text-neutral-100 font-sans">
      <header className="border-b border-neutral-200 dark:border-neutral-800">
        <div className="max-w-3xl mx-auto px-6 py-4 flex items-center justify-between">
          <h1 className="text-lg font-semibold tracking-tight">
            nexo-rs <span className="text-neutral-400 font-normal">admin</span>
          </h1>
          <span className="text-xs font-mono bg-neutral-200 dark:bg-neutral-800 px-2 py-1 rounded">
            dev
          </span>
        </div>
      </header>

      <main className="max-w-3xl mx-auto px-6 py-12 space-y-8">
        <section>
          <h2 className="text-2xl font-semibold mb-2">
            Hello, world.
          </h2>
          <p className="text-neutral-600 dark:text-neutral-400">
            If you are reading this, the Cloudflare quick tunnel reached the
            embedded React bundle inside your local{" "}
            <code className="text-sm font-mono bg-neutral-200 dark:bg-neutral-800 px-1 py-0.5 rounded">
              agent
            </code>{" "}
            binary. The authentication gate already passed.
          </p>
        </section>

        <section className="rounded-lg border border-neutral-200 dark:border-neutral-800 p-4">
          <h3 className="text-sm uppercase tracking-wide text-neutral-500 mb-2">
            Session
          </h3>
          <dl className="space-y-1 text-sm">
            <div className="flex justify-between">
              <dt className="text-neutral-500">status</dt>
              <dd className="font-mono">{hello.status}</dd>
            </div>
            <div className="flex justify-between">
              <dt className="text-neutral-500">loaded at</dt>
              <dd className="font-mono">
                {new Date(hello.ts_ms).toISOString()}
              </dd>
            </div>
          </dl>
        </section>

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
          <p className="mt-2">
            Real admin routes (agent directory, sessions, DLQ, live config
            reload) land in follow-up commits.
          </p>
        </section>
      </main>
    </div>
  );
}
