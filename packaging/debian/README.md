# Debian / Ubuntu packaging

Recipe for `.deb` packages targeting Debian 12+ and Ubuntu 22.04+.
Runs the `nexo` binary as a systemd service under a dedicated
`nexo` system user.

## Files

- `build.sh` — builds `dist/nexo-rs_<version>_<arch>.deb`
- `nexo-rs.service` — systemd unit (also reused by the RPM spec)

## Local build

```bash
# Native amd64:
packaging/debian/build.sh

# Cross-compile for arm64 (Raspberry Pi, Graviton, etc.):
cargo install cargo-zigbuild
pip install ziglang
packaging/debian/build.sh --arch arm64

# Use a pre-built binary (skip cargo entirely):
packaging/debian/build.sh --binary target/release/nexo
```

## Install on a target host

```bash
sudo apt install ./dist/nexo-rs_0.1.1_amd64.deb
```

What the install does:
1. Creates the `nexo` system user (`adduser --system`).
2. Drops the binary at `/usr/bin/nexo`.
3. Drops the systemd unit at `/lib/systemd/system/nexo-rs.service`.
4. Creates `/etc/nexo-rs/`, `/var/lib/nexo-rs/`, `/var/log/nexo-rs/`
   owned by `nexo:nexo`, mode `0750`.
5. Reloads systemd. **Does NOT auto-start** — operator runs
   `systemctl enable --now nexo-rs` after wiring configs.

## First-run configuration

```bash
sudo -u nexo nexo setup        # writes /etc/nexo-rs/{agents,broker,llm,memory}.yaml
sudo systemctl enable --now nexo-rs
sudo journalctl -u nexo-rs -f  # tail logs
```

## Hardening shipped with the unit

The `nexo-rs.service` unit ships with these systemd sandboxing
options enabled by default (see `nexo-rs.service`):

- `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`
- `PrivateTmp`, `PrivateDevices`
- `ProtectKernelTunables`, `ProtectKernelModules`,
  `ProtectControlGroups`
- `RestrictNamespaces`, `LockPersonality`, `RestrictRealtime`,
  `RestrictSUIDSGID`
- `RemoveIPC`
- `ReadWritePaths=/var/lib/nexo-rs /var/log/nexo-rs`
- `LimitNOFILE=65536`, `TasksMax=4096`

If you need the browser plugin (Chrome) or `cloudflared` to
launch sub-processes, you may have to relax `PrivateDevices` and
`RestrictNamespaces` — adjust via a drop-in:
`/etc/systemd/system/nexo-rs.service.d/local.conf`.

## Removal

```bash
sudo apt remove nexo-rs    # keeps /var/lib/nexo-rs (state)
sudo apt purge  nexo-rs    # also wipes state + drops the user
```

## Apt repo (deferred)

The `build.sh` produces a single .deb. The release pipeline (Phase
27.2) will publish each new build to a Debian apt repo at
`https://lordmacu.github.io/nexo-rs/apt/` so users can:

```bash
curl -fsSL https://lordmacu.github.io/nexo-rs/apt/key.asc \
  | sudo tee /etc/apt/trusted.gpg.d/nexo-rs.asc
echo "deb https://lordmacu.github.io/nexo-rs/apt stable main" \
  | sudo tee /etc/apt/sources.list.d/nexo-rs.list
sudo apt update && sudo apt install nexo-rs
```

That side is still pending (needs GPG key, signed Release file,
GitHub Pages publish job).
