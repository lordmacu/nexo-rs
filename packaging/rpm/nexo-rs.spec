%global debug_package %{nil}

Name:           nexo-rs
Version:        0.1.1
Release:        1%{?dist}
Summary:        Multi-agent Rust framework with NATS event bus, LLM providers, and channel plugins
License:        MIT OR Apache-2.0
URL:            https://lordmacu.github.io/nexo-rs/
Source0:        %{name}-%{version}.tar.gz
Source1:        nexo-rs.service

# Phase 27.4 — the bundled `nexo` binary is musl-static, so sqlite-libs
# and openssl-libs are linked in and NOT runtime deps. Optional
# channel-plugin runtimes stay under `Recommends:`.
BuildRequires:  systemd-rpm-macros
Requires:       ca-certificates
Requires(pre):  shadow-utils
Requires(post): systemd
Requires(preun): systemd
Requires(postun): systemd

Recommends:     nats-server
Recommends:     git
Recommends:     ffmpeg
Recommends:     tesseract
Recommends:     cloudflared
Recommends:     yt-dlp
Recommends:     python3
Suggests:       chromium

%description
Nexo is a multi-agent Rust framework with a NATS event bus, pluggable
LLM providers (MiniMax, Anthropic, OpenAI-compat, Gemini, DeepSeek),
per-agent credentials, MCP support, and channel plugins for WhatsApp,
Telegram, Email, and Browser.

The package ships a systemd unit (nexo-rs.service) that the operator
enables manually after wiring /etc/nexo-rs/ configs. The 'nexo' system
user is created on first install and owns /var/lib/nexo-rs/.

%prep
%setup -q

%build
# The RPM spec assumes the binary is pre-built and dropped in the
# source tarball under target/release/nexo. The CI release workflow
# (Phase 27.2) builds with cargo + drops the binary into the tarball
# before invoking rpmbuild. For a from-scratch RPM build, replace
# this section with:
#   cargo build --release --bin nexo

%install
install -d -m 0755 %{buildroot}%{_bindir}
install -m 0755 target/release/nexo %{buildroot}%{_bindir}/nexo

install -d -m 0755 %{buildroot}%{_unitdir}
install -m 0644 %{SOURCE1} %{buildroot}%{_unitdir}/nexo-rs.service

install -d -m 0750 %{buildroot}%{_sysconfdir}/nexo-rs
install -d -m 0750 %{buildroot}%{_sharedstatedir}/nexo-rs
install -d -m 0750 %{buildroot}%{_localstatedir}/log/nexo-rs

# README + licenses are packaged via the `%doc`/`%license` macros
# in `%files` (rpm copies them from the build dir into
# /usr/share/doc/%{name}-%{version}/ and /usr/share/licenses/…).
# Don't `install` them manually too — that lands a second copy in
# /usr/share/doc/%{name}/ that no `%files` glob covers, and
# rpmbuild aborts with "Installed (but unpackaged) file(s) found".

%pre
getent group nexo >/dev/null || groupadd --system nexo
getent passwd nexo >/dev/null || \
    useradd --system --gid nexo --no-create-home \
            --home-dir %{_sharedstatedir}/nexo-rs \
            --shell /sbin/nologin \
            --comment "Nexo agent runtime" nexo
exit 0

%post
chown -R nexo:nexo %{_sharedstatedir}/nexo-rs %{_localstatedir}/log/nexo-rs
# Phase 95 — auto-seed sample YAMLs on first install (skip on
# upgrade so operator edits survive). `nexo init` ships
# baked-in templates from the binary itself.
if [ $1 -eq 1 ] && [ ! -f %{_sysconfdir}/nexo-rs/broker.yaml ]; then
    mkdir -p %{_sysconfdir}/nexo-rs
    %{_bindir}/nexo init --output %{_sysconfdir}/nexo-rs >/dev/null 2>&1 || :
    chown -R nexo:nexo %{_sysconfdir}/nexo-rs
fi
%systemd_post nexo-rs.service
cat <<EOF

  nexo-rs installed.

  Quick smoke test (Phase 93 zero-config):
    sudo systemctl enable --now nexo-rs
    sudo journalctl -u nexo-rs -f
  → daemon boots with defaults (0 agents, broker=local,
    no LLM provider). Admin RPCs + health endpoint live.

  To customize:
    A) Edit the auto-seeded YAMLs at /etc/nexo-rs/*.yaml,
       then `sudo systemctl restart nexo-rs`.
    B) Run the interactive wizard:
         sudo -u nexo nexo setup
    C) Use admin RPCs from the operator UI (microapp).

  Common switches:
    sudo -u nexo nexo --config /etc/nexo-rs set-broker nats \\
         --url nats://localhost:4222    # if you run NATS
    sudo -u nexo nexo --config /etc/nexo-rs set-broker local
                                         # stdio bridge (default)

  Docs: https://lordmacu.github.io/nexo-rs/

EOF

%preun
%systemd_preun nexo-rs.service

%postun
%systemd_postun_with_restart nexo-rs.service
if [ $1 -eq 0 ]; then
    # `rpm -e` (full removal): wipe state and the user.
    rm -rf %{_sharedstatedir}/nexo-rs %{_localstatedir}/log/nexo-rs
    userdel nexo 2>/dev/null || :
    groupdel nexo 2>/dev/null || :
fi

%files
%license LICENSE-APACHE LICENSE-MIT
%doc README.md
%{_bindir}/nexo
%{_unitdir}/nexo-rs.service
%dir %attr(0750, nexo, nexo) %{_sysconfdir}/nexo-rs
%dir %attr(0750, nexo, nexo) %{_sharedstatedir}/nexo-rs
%dir %attr(0750, nexo, nexo) %{_localstatedir}/log/nexo-rs

%changelog
* Sat Apr 25 2026 Cristian Garcia <informacion@cristiangarcia.co> - 0.1.1-1
- Initial RPM packaging (Phase 27.4).
- Bundles systemd unit; creates `nexo` system user; owns
  /var/lib/nexo-rs/. Operator enables the unit manually after
  wiring /etc/nexo-rs/.
