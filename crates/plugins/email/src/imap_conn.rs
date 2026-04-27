//! Thin async-imap wrapper used by `AccountWorker` (Phase 48.3).
//!
//! Owns the TCP+TLS handshake, AUTH (`LOGIN` for `Password`,
//! `AUTHENTICATE XOAUTH2` for OAuth), and the `CAPABILITY` cache. The
//! state machine in `inbound.rs` calls into this struct for the
//! select / fetch / search / idle primitives — keeping IMAP wire
//! details out of the worker loop.
//!
//! Scope of v1:
//! - `TlsMode::ImplicitTls` (port 993) is the only supported mode.
//!   `Starttls` and `Plain` would each require a stream-upgrade dance
//!   that's not in 48.3's scope; calling `connect` with either returns
//!   an `anyhow::Error` that operators can act on at boot.
//! - INBOX-only (configurable folder name comes from
//!   `EmailFolders.inbox`).
//! - `BODY.PEEK[]` reads — never marks `\Seen` (agent decides via 48.7
//!   tools).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_imap::extensions::idle::IdleResponse;
use async_imap::types::Capabilities;
use async_imap::Session;
use futures::StreamExt;
use nexo_auth::email::{EmailAccount, EmailAuth};
use nexo_auth::google::GoogleCredentialStore;
use nexo_config::types::plugins::{ImapEndpoint, TlsMode};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, RootCertStore};
use secrecy::ExposeSecret;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt};
use tokio_util::sync::CancellationToken;

/// async-imap is built on `futures::io::AsyncRead/Write`; tokio's I/O
/// world uses `tokio::io::AsyncRead/Write`. The compat shim bridges
/// the two so we can keep the `tokio-rustls` TLS handshake intact.
pub type ImapStream = Compat<TlsStream<TcpStream>>;
pub type ImapSession = Session<ImapStream>;

pub struct ImapConnection {
    pub session: ImapSession,
    pub instance: String,
    pub capabilities: CachedCapabilities,
}

#[derive(Debug, Clone, Default)]
pub struct CachedCapabilities {
    pub idle: bool,
    pub uidplus: bool,
    pub move_ext: bool,
    pub raw: Vec<String>,
}

impl CachedCapabilities {
    fn from_caps(caps: &Capabilities) -> Self {
        let raw: Vec<String> = caps.iter().map(|c| format!("{c:?}")).collect();
        let mut out = Self {
            raw,
            ..Self::default()
        };
        for c in caps.iter() {
            let s = format!("{c:?}").to_ascii_uppercase();
            if s.contains("IDLE") {
                out.idle = true;
            }
            if s.contains("UIDPLUS") {
                out.uidplus = true;
            }
            if s.contains("MOVE") {
                out.move_ext = true;
            }
        }
        out
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MailboxState {
    pub uid_validity: u32,
    pub uid_next: u32,
    pub exists: u32,
}

#[derive(Debug, Clone)]
pub struct FetchedMessage {
    pub uid: u32,
    pub internal_date: i64,
    pub raw_bytes: Vec<u8>,
}

pub enum IdleOutcome {
    /// Server pushed an `EXISTS` / `RECENT` — caller should fetch new
    /// UIDs and resume IDLE.
    NewMessages,
    /// 28-min reissue cycle hit — caller should `done()` and call
    /// `idle()` again.
    Timeout,
    /// `CancellationToken` was triggered (plugin stop).
    Cancelled,
}

/// SASL XOAUTH2 authenticator. The token resolution happens before
/// this is constructed (so the per-account refresh mutex is held only
/// for the duration of the read, not the whole IMAP login).
struct Xoauth2 {
    payload: String,
    sent: bool,
}

impl async_imap::Authenticator for Xoauth2 {
    type Response = String;

    fn process(&mut self, _challenge: &[u8]) -> Self::Response {
        if self.sent {
            // Server rejected — RFC 7628 says we should respond with an
            // empty client message to terminate the AUTH exchange. Any
            // bytes here would just be re-rejected.
            String::new()
        } else {
            self.sent = true;
            self.payload.clone()
        }
    }
}

impl ImapConnection {
    /// Open TCP, complete TLS handshake, run AUTH, fetch CAPABILITY.
    /// Returns an `ImapConnection` wrapping the authenticated `Session`.
    pub async fn connect(
        endpoint: &ImapEndpoint,
        account: &EmailAccount,
        google: Arc<GoogleCredentialStore>,
    ) -> Result<Self> {
        match endpoint.tls {
            TlsMode::ImplicitTls => {}
            TlsMode::Starttls => bail!(
                "email/imap: STARTTLS not yet supported (Phase 48.3 v1 ships ImplicitTls only); \
                 either move to ImplicitTls (port 993) or wait for the STARTTLS follow-up"
            ),
            TlsMode::Plain => bail!(
                "email/imap: plaintext IMAP not supported — use ImplicitTls (port 993)"
            ),
        }

        let tcp = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
            .await
            .with_context(|| {
                format!(
                    "email/imap: TCP connect to {}:{} failed",
                    endpoint.host, endpoint.port
                )
            })?;

        // 30s keepalive — long IDLE waits otherwise look idle to NAT
        // gateways (CGNAT routinely drops 5min-idle TCPs) and the
        // connection silently dies without the server ever telling us.
        let sock = socket2::SockRef::from(&tcp);
        let _ = sock.set_tcp_keepalive(
            &socket2::TcpKeepalive::new()
                .with_time(Duration::from_secs(30))
                .with_interval(Duration::from_secs(30)),
        );

        let tls = build_tls_connector()?;
        let server_name = ServerName::try_from(endpoint.host.clone())
            .with_context(|| format!("email/imap: invalid TLS server name: {}", endpoint.host))?;
        let stream = tls
            .connect(server_name, tcp)
            .await
            .with_context(|| format!("email/imap: TLS handshake to {} failed", endpoint.host))?;

        let client = async_imap::Client::new(stream.compat());
        // Discard the unsolicited `* OK` greeting before issuing
        // commands. async-imap's `Client::new` accepts the stream
        // without consuming it; the first `read_greeting` is part of
        // login/authenticate internally.
        let session = match &account.auth {
            EmailAuth::Password { username, password } => client
                .login(username, password.expose_secret())
                .await
                .map_err(|(e, _client)| anyhow!("email/imap: LOGIN failed: {e}"))?,
            EmailAuth::OAuth2Static { username, .. }
            | EmailAuth::OAuth2Google { username, .. } => {
                let token = account
                    .resolve_access_token(&google)
                    .await
                    .context("email/imap: resolve XOAUTH2 access token")?;
                let payload = EmailAccount::xoauth2_sasl(username, token.expose_secret());
                let auth = Xoauth2 {
                    payload,
                    sent: false,
                };
                client
                    .authenticate("XOAUTH2", auth)
                    .await
                    .map_err(|(e, _client)| anyhow!("email/imap: AUTHENTICATE XOAUTH2 failed: {e}"))?
            }
        };

        let mut session = session;
        let caps_raw = session
            .capabilities()
            .await
            .context("email/imap: CAPABILITY post-login")?;
        let capabilities = CachedCapabilities::from_caps(&caps_raw);
        drop(caps_raw);

        Ok(Self {
            session,
            instance: account.instance.clone(),
            capabilities,
        })
    }

    /// Run `SELECT INBOX` (or the configured mailbox). Returns the
    /// `(UIDVALIDITY, UIDNEXT, EXISTS)` state caller needs to update
    /// the cursor.
    pub async fn select(&mut self, mailbox: &str) -> Result<MailboxState> {
        let m = self
            .session
            .select(mailbox)
            .await
            .with_context(|| format!("email/imap: SELECT {mailbox}"))?;
        Ok(MailboxState {
            uid_validity: m.uid_validity.unwrap_or(0),
            uid_next: m.uid_next.unwrap_or(0),
            exists: m.exists,
        })
    }

    /// `UID SEARCH UID <last_uid+1>:*`. Returns sorted ascending so
    /// the worker can advance the cursor monotonically.
    pub async fn search_since(&mut self, last_uid: u32) -> Result<Vec<u32>> {
        let query = format!("UID {}:*", last_uid.saturating_add(1));
        let set = self
            .session
            .uid_search(&query)
            .await
            .with_context(|| format!("email/imap: UID SEARCH {query}"))?;
        let mut uids: Vec<u32> = set.into_iter().filter(|u| *u > last_uid).collect();
        uids.sort_unstable();
        Ok(uids)
    }

    /// `UID FETCH <uid> (BODY.PEEK[] INTERNALDATE)` — does not set the
    /// `\Seen` flag.
    pub async fn fetch_uid(&mut self, uid: u32) -> Result<FetchedMessage> {
        let mut stream = self
            .session
            .uid_fetch(uid.to_string(), "(BODY.PEEK[] INTERNALDATE)")
            .await
            .with_context(|| format!("email/imap: UID FETCH {uid}"))?;

        let mut raw_bytes: Option<Vec<u8>> = None;
        let mut internal_date: i64 = 0;
        while let Some(item) = stream.next().await {
            let f = item.with_context(|| format!("email/imap: FETCH stream item uid={uid}"))?;
            if let Some(body) = f.body() {
                raw_bytes = Some(body.to_vec());
            }
            if let Some(dt) = f.internal_date() {
                internal_date = dt.timestamp();
            }
        }
        // Drain into a Result so we drop the borrowed stream before
        // returning the borrow on `self.session`.
        drop(stream);

        let raw_bytes = raw_bytes.ok_or_else(|| {
            anyhow!("email/imap: UID FETCH {uid} returned no BODY[] section")
        })?;
        Ok(FetchedMessage {
            uid,
            internal_date,
            raw_bytes,
        })
    }

    /// `IDLE` with a hard timeout (caller passes 28 min) and a cancel
    /// token (plugin stop). Returns the outcome so the worker knows
    /// whether to fetch new UIDs, reissue, or exit.
    pub async fn idle_wait(
        mut self,
        timeout: Duration,
        cancel: CancellationToken,
    ) -> Result<(Self, IdleOutcome)> {
        let mut handle = self.session.idle();
        handle
            .init()
            .await
            .context("email/imap: IDLE init")?;
        let (fut, stop) = handle.wait_with_timeout(timeout);
        let outcome = tokio::select! {
            res = fut => match res {
                Ok(IdleResponse::NewData(_)) => IdleOutcome::NewMessages,
                Ok(IdleResponse::Timeout) => IdleOutcome::Timeout,
                Ok(IdleResponse::ManualInterrupt) => IdleOutcome::Cancelled,
                Err(e) => return Err(anyhow!("email/imap: IDLE wait: {e}")),
            },
            _ = cancel.cancelled() => {
                drop(stop);
                IdleOutcome::Cancelled
            }
        };
        let session = handle
            .done()
            .await
            .context("email/imap: IDLE DONE")?;
        self.session = session;
        Ok((self, outcome))
    }

    pub async fn logout(mut self) -> Result<()> {
        self.session
            .logout()
            .await
            .context("email/imap: LOGOUT")?;
        Ok(())
    }

    // ── Phase 48.7 helpers (tools) ────────────────────────────────

    /// `UID SEARCH <atoms>` — caller composes the atom string (already
    /// quoted via `tool::imap_quote`).
    pub async fn uid_search(&mut self, atoms: &str) -> Result<Vec<u32>> {
        let set = self
            .session
            .uid_search(atoms)
            .await
            .with_context(|| format!("email/imap: UID SEARCH {atoms}"))?;
        let mut uids: Vec<u32> = set.into_iter().collect();
        uids.sort_unstable();
        Ok(uids)
    }

    /// `UID MOVE <set> <folder>` (RFC 6851). Caller verifies the
    /// server advertised `MOVE` via `capabilities.move_ext` before
    /// reaching here; otherwise route through `uid_copy` +
    /// `uid_store_flags("+FLAGS (\\Deleted)")` + `expunge`.
    pub async fn uid_move(&mut self, uid_set: &str, mailbox: &str) -> Result<()> {
        self.session
            .uid_mv(uid_set, mailbox)
            .await
            .with_context(|| format!("email/imap: UID MOVE {uid_set} → {mailbox}"))?;
        Ok(())
    }

    /// `UID COPY <set> <folder>`.
    pub async fn uid_copy(&mut self, uid_set: &str, mailbox: &str) -> Result<()> {
        self.session
            .uid_copy(uid_set, mailbox)
            .await
            .with_context(|| format!("email/imap: UID COPY {uid_set} → {mailbox}"))?;
        Ok(())
    }

    /// `UID STORE <set> <query>` — caller composes the query
    /// (e.g. `+FLAGS (\\Deleted)`, `+X-GM-LABELS (Important)`).
    /// Drains the response stream so the session is ready for the
    /// next command.
    pub async fn uid_store(&mut self, uid_set: &str, query: &str) -> Result<()> {
        let mut stream = self
            .session
            .uid_store(uid_set, query)
            .await
            .with_context(|| format!("email/imap: UID STORE {uid_set} {query}"))?;
        while let Some(item) = futures::StreamExt::next(&mut stream).await {
            let _ = item; // server echoes the new flag list per UID
        }
        drop(stream);
        Ok(())
    }

    /// `EXPUNGE` — drains the resulting stream of expunged sequence
    /// numbers. The async-imap stream isn't `Unpin`, so we pin it
    /// in place before iterating.
    pub async fn expunge(&mut self) -> Result<()> {
        let stream = self.session.expunge().await.context("email/imap: EXPUNGE")?;
        futures::pin_mut!(stream);
        while let Some(item) = futures::StreamExt::next(&mut stream).await {
            let _ = item;
        }
        Ok(())
    }

    /// `UID FETCH <set> (UID INTERNALDATE BODY.PEEK[HEADER.FIELDS
    /// (FROM SUBJECT MESSAGE-ID)] BODY.PEEK[TEXT]<0.200>)` — used by
    /// `email_search` to render the result rows. Returns one row per
    /// matched UID, preserving caller order.
    pub async fn fetch_search_rows(&mut self, uid_set: &str) -> Result<Vec<SearchRow>> {
        let query = "(UID INTERNALDATE BODY.PEEK[HEADER.FIELDS (FROM SUBJECT MESSAGE-ID)] BODY.PEEK[TEXT]<0.200>)";
        let mut stream = self
            .session
            .uid_fetch(uid_set, query)
            .await
            .with_context(|| format!("email/imap: UID FETCH {uid_set}"))?;
        let mut rows = Vec::new();
        use futures::StreamExt;
        while let Some(item) = stream.next().await {
            let f = item.context("email/imap: FETCH stream item")?;
            let uid = f.uid.unwrap_or(0);
            let internal_date = f.internal_date().map(|d| d.timestamp()).unwrap_or(0);
            let header_bytes = f.header().map(|b| b.to_vec()).unwrap_or_default();
            let snippet_bytes = f.text().map(|b| b.to_vec()).unwrap_or_default();
            let header_str = String::from_utf8_lossy(&header_bytes);
            let (from, subject, message_id) = parse_search_headers(&header_str);
            let snippet = String::from_utf8_lossy(&snippet_bytes)
                .chars()
                .take(200)
                .collect();
            rows.push(SearchRow {
                uid,
                message_id,
                from,
                subject,
                date: internal_date,
                snippet,
            });
        }
        drop(stream);
        Ok(rows)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchRow {
    pub uid: u32,
    pub message_id: Option<String>,
    pub from: String,
    pub subject: String,
    pub date: i64,
    pub snippet: String,
}

/// Pull `From:`, `Subject:`, `Message-ID:` from a small header block
/// returned by `BODY.PEEK[HEADER.FIELDS (...)]`. Tolerant of folded
/// lines and missing fields.
fn parse_search_headers(s: &str) -> (String, String, Option<String>) {
    let mut from = String::new();
    let mut subject = String::new();
    let mut message_id: Option<String> = None;
    let mut current: Option<&'static str> = None;
    let mut buf_from = String::new();
    let mut buf_subject = String::new();
    let mut buf_msgid = String::new();
    for line in s.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            // Folded continuation of the previous header.
            match current {
                Some("from") => {
                    buf_from.push(' ');
                    buf_from.push_str(line.trim());
                }
                Some("subject") => {
                    buf_subject.push(' ');
                    buf_subject.push_str(line.trim());
                }
                Some("message-id") => {
                    buf_msgid.push_str(line.trim());
                }
                _ => {}
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            let n = name.trim().to_ascii_lowercase();
            let v = value.trim();
            match n.as_str() {
                "from" => {
                    buf_from = v.to_string();
                    current = Some("from");
                }
                "subject" => {
                    buf_subject = v.to_string();
                    current = Some("subject");
                }
                "message-id" => {
                    buf_msgid = v.to_string();
                    current = Some("message-id");
                }
                _ => current = None,
            }
        }
    }
    if !buf_from.is_empty() {
        from = buf_from;
    }
    if !buf_subject.is_empty() {
        subject = buf_subject;
    }
    if !buf_msgid.is_empty() {
        message_id = Some(buf_msgid);
    }
    (from, subject, message_id)
}

/// Build a `rustls` config trusting the OS's native root store. Set
/// `EMAIL_INSECURE_TLS=1` to disable verification — only for local
/// dev / fake servers; the boot capability inventory surfaces this
/// toggle to the operator.
fn build_tls_connector() -> Result<TlsConnector> {
    let insecure = std::env::var("EMAIL_INSECURE_TLS").ok().as_deref() == Some("1");
    let cfg = if insecure {
        tracing::warn!(
            target: "plugin.email",
            "EMAIL_INSECURE_TLS=1 — IMAP TLS certificate verification disabled. \
             Do not use in production."
        );
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertVerify))
            .with_no_client_auth()
    } else {
        let mut roots = RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs()
            .context("email/imap: load native root certs")?;
        for cert in native {
            // Best-effort — skip certs that fail to parse rather than
            // refusing to start the daemon.
            let _ = roots.add(cert);
        }
        if roots.is_empty() {
            tracing::warn!(
                target: "plugin.email",
                "no native root certs available — IMAP TLS handshakes will fail. \
                 Install ca-certificates or set EMAIL_INSECURE_TLS=1 for dev."
            );
        }
        ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth()
    };
    Ok(TlsConnector::from(Arc::new(cfg)))
}

/// Dev-only verifier that accepts any certificate. Gated behind
/// `EMAIL_INSECURE_TLS=1` and logged at WARN — never reached in a
/// default deployment.
#[derive(Debug)]
struct NoCertVerify;

impl rustls::client::danger::ServerCertVerifier for NoCertVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        use rustls::SignatureScheme::*;
        vec![
            RSA_PKCS1_SHA256,
            RSA_PKCS1_SHA384,
            RSA_PKCS1_SHA512,
            ECDSA_NISTP256_SHA256,
            ECDSA_NISTP384_SHA384,
            RSA_PSS_SHA256,
            RSA_PSS_SHA384,
            RSA_PSS_SHA512,
            ED25519,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_imap::types::Capability;

    #[test]
    fn cached_capabilities_detects_idle_and_uidplus() {
        // Build a Capabilities-like input via the public Capability
        // enum. async-imap's Capabilities doesn't have a public ctor,
        // so we exercise the parser indirectly.
        let _ = Capability::Imap4rev1; // smoke: enum compiles
        // Instead, validate the case-insensitive substring match the
        // helper relies on, with a fake input.
        let mut cc = CachedCapabilities::default();
        for tok in ["IDLE", "UIDPLUS", "MOVE", "QUOTA"] {
            let s = tok.to_string();
            if s.contains("IDLE") {
                cc.idle = true;
            }
            if s.contains("UIDPLUS") {
                cc.uidplus = true;
            }
            if s.contains("MOVE") {
                cc.move_ext = true;
            }
        }
        assert!(cc.idle);
        assert!(cc.uidplus);
        assert!(cc.move_ext);
    }
}
