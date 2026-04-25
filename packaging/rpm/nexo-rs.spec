%global debug_package %{nil}

Name:           nexo-rs
Version:        0.1.1
Release:        1%{?dist}
Summary:        Multi-agent Rust framework with NATS event bus, LLM providers, and channel plugins
License:        MIT OR Apache-2.0
URL:            https://lordmacu.github.io/nexo-rs/
Source0:        %{name}-%{version}.tar.gz
Source1:        nexo-rs.service

BuildRequires:  systemd-rpm-macros
Requires:       sqlite-libs
Requires:       openssl-libs
Requires:       ca-certificates
Requires(pre):  shadow-utils
Requires(post): systemd
Requires(preun): systemd
Requires(postun): systemd

Recommends:     nats-server
Recommends:     git
Recommends:     ffmpeg
Recommends:     tesseract
Suggests:       cloudflared
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

install -d -m 0755 %{buildroot}%{_defaultdocdir}/%{name}
install -m 0644 README.md %{buildroot}%{_defaultdocdir}/%{name}/
install -m 0644 LICENSE-APACHE %{buildroot}%{_defaultdocdir}/%{name}/copyright-apache
install -m 0644 LICENSE-MIT %{buildroot}%{_defaultdocdir}/%{name}/copyright-mit

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
%systemd_post nexo-rs.service
cat <<EOF

  nexo-rs installed.

  Next steps:
    1. Wire your config:    sudo -u nexo nexo setup
    2. Enable + start:      sudo systemctl enable --now nexo-rs
    3. Tail logs:           sudo journalctl -u nexo-rs -f

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
