# admin-ui

React + Vite + TS + Tailwind bundle served by `agent admin`.

The built output under `dist/` is embedded into the agent binary via
`rust-embed` (see `src/main.rs::run_admin_web`). During `agent admin`
startup the Rust side reads the embedded files and serves them
behind HTTP Basic Auth.

## Dev loop

```bash
cd admin-ui
npm install
npm run dev        # hot-reload Vite on :5173 (uses mocked data)
```

Vite dev server is only for UI work — it does NOT talk to a running
agent. When you need to test against the real tunnel + Basic Auth:

```bash
npm run build
cargo build --release --bin agent
./target/release/agent admin
```

`rust-embed` picks up whatever's in `dist/` at Rust compile time.
**You must rebuild the agent binary after `npm run build`** — the
bundle is baked in, not served live.

## Build flow in CI

The release workflow and `scripts/bootstrap.sh` both call
`npm install && npm run build` when `admin-ui/package.json` is
present and `dist/` is stale. Shipped agent binaries always contain
the current `admin-ui/` bundle.

## Layout

```
admin-ui/
├── index.html            Vite entry
├── vite.config.ts        base: "./" so the bundle is path-agnostic
├── tailwind.config.ts    Tailwind 3; system-font stack
├── tsconfig.json         strict TS, noUnused*, ES2022 target
├── src/
│   ├── main.tsx          React root + StrictMode
│   ├── App.tsx           top-level layout + first page
│   └── index.css         Tailwind directives only
└── dist/                 gitignored build output
```

See `docs/src/cli/reference.md#admin` for how the binary serves
this bundle.
