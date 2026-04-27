//! Lettre `AsyncSmtpTransport` wrapper used by `OutboundWorker` (Phase 48.4).
//!
//! Owns the TLS handshake + AUTH negotiation and exposes a single
//! `send_raw` that the worker calls with the bytes that came out of
//! `mime_text::build_text_mime`. The wrapper picks `Mechanism::Plain`
//! for `EmailAuth::Password` and `Mechanism::Xoauth2` for the two
//! OAuth variants — token resolution still goes through 48.2's
//! `resolve_access_token` so the per-account refresh mutex is honoured
//! for `OAuth2Google`.

use std::sync::Arc;

use anyhow::{Context, Result};
use lettre::address::Address;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Tokio1Executor};
use nexo_auth::email::{EmailAccount, EmailAuth};
use nexo_auth::google::GoogleCredentialStore;
use nexo_config::types::plugins::{SmtpEndpoint, TlsMode};
use secrecy::ExposeSecret;

use crate::outbound_queue::SmtpEnvelope;

pub struct SmtpClient {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    pub instance: String,
}

#[derive(Debug, Clone)]
pub enum SmtpSendOutcome {
    /// 2xx — server accepted the message.
    Sent,
    /// 4xx — caller bumps `attempts` and reschedules.
    Transient { code: u16, message: String },
    /// 5xx — caller moves the job to DLQ.
    Permanent { code: u16, message: String },
}

impl SmtpClient {
    pub async fn build(
        endpoint: &SmtpEndpoint,
        account: &EmailAccount,
        google: Arc<GoogleCredentialStore>,
    ) -> Result<Self> {
        let mut builder = match endpoint.tls {
            TlsMode::ImplicitTls => AsyncSmtpTransport::<Tokio1Executor>::relay(&endpoint.host)
                .with_context(|| format!("email/smtp: relay({}) builder", endpoint.host))?,
            TlsMode::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(
                &endpoint.host,
            )
            .with_context(|| format!("email/smtp: starttls_relay({}) builder", endpoint.host))?,
            TlsMode::Plain => {
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&endpoint.host)
            }
        }
        .port(endpoint.port);

        let (creds, mech) = match &account.auth {
            EmailAuth::Password { username, password } => (
                Credentials::new(username.clone(), password.expose_secret().to_string()),
                Mechanism::Plain,
            ),
            EmailAuth::OAuth2Static { username, .. } | EmailAuth::OAuth2Google { username, .. } => {
                let token = account
                    .resolve_access_token(&google)
                    .await
                    .context("email/smtp: resolve XOAUTH2 token")?;
                (
                    Credentials::new(username.clone(), token.expose_secret().to_string()),
                    Mechanism::Xoauth2,
                )
            }
        };
        builder = builder.credentials(creds).authentication(vec![mech]);

        let transport = builder.build();
        Ok(Self {
            transport,
            instance: account.instance.clone(),
        })
    }

    /// Send raw RFC 5322 bytes. Caller passes the SMTP envelope
    /// (`MAIL FROM` + `RCPT TO` list including BCC) separately so BCC
    /// recipients don't leak into headers.
    pub async fn send_raw(
        &self,
        envelope: &SmtpEnvelope,
        raw_mime: &[u8],
    ) -> Result<SmtpSendOutcome> {
        let lettre_env = build_lettre_envelope(envelope)?;
        match self.transport.send_raw(&lettre_env, raw_mime).await {
            Ok(_resp) => Ok(SmtpSendOutcome::Sent),
            Err(e) => Ok(classify_error(&e)),
        }
    }

    /// Lettre `test_connection` pass-through — opens a TCP+TLS+AUTH
    /// session against the configured endpoint and tears it down
    /// immediately. Used by `nexo setup` to validate credentials
    /// before persisting `email.yaml` + the secret TOML.
    pub async fn test_connection(&self) -> Result<bool> {
        self.transport
            .test_connection()
            .await
            .context("email/smtp: test_connection")
    }
}

fn build_lettre_envelope(env: &SmtpEnvelope) -> Result<lettre::address::Envelope> {
    let from: Address = env
        .from
        .parse()
        .with_context(|| format!("email/smtp: invalid From address: {}", env.from))?;
    let mut tos: Vec<Address> = Vec::with_capacity(env.to.len() + env.cc.len() + env.bcc.len());
    for r in env.to.iter().chain(env.cc.iter()).chain(env.bcc.iter()) {
        let addr: Address = r
            .parse()
            .with_context(|| format!("email/smtp: invalid recipient: {r}"))?;
        tos.push(addr);
    }
    lettre::address::Envelope::new(Some(from), tos).context("email/smtp: build envelope")
}

/// Map a lettre SMTP error into our coarse outcome enum. Anything
/// that isn't an SMTP-coded transient/permanent is treated as
/// transient — TLS / network / DNS / pool-exhaustion errors all
/// deserve a retry rather than the DLQ.
fn classify_error(e: &lettre::transport::smtp::Error) -> SmtpSendOutcome {
    if e.is_permanent() {
        let code = e.status().map(code_to_u16).unwrap_or(500);
        return SmtpSendOutcome::Permanent {
            code,
            message: e.to_string(),
        };
    }
    if e.is_transient() {
        let code = e.status().map(code_to_u16).unwrap_or(450);
        return SmtpSendOutcome::Transient {
            code,
            message: e.to_string(),
        };
    }
    SmtpSendOutcome::Transient {
        code: 0,
        message: e.to_string(),
    }
}

fn code_to_u16(c: lettre::transport::smtp::response::Code) -> u16 {
    c.detail as u16 + 10 * c.category as u16 + 100 * c.severity as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_auth::email::{EmailAccount, EmailAuth};
    use secrecy::SecretString;

    fn pwd_account() -> EmailAccount {
        EmailAccount {
            instance: "ops".into(),
            address: "ops@example.com".into(),
            auth: EmailAuth::Password {
                username: "ops@example.com".into(),
                password: SecretString::new("hunter2".into()),
            },
            allow_agents: vec![],
        }
    }

    fn endpoint(tls: TlsMode, port: u16) -> SmtpEndpoint {
        SmtpEndpoint {
            host: "smtp.example.com".into(),
            port,
            tls,
        }
    }

    #[tokio::test]
    async fn build_implicit_tls_succeeds() {
        let g = Arc::new(GoogleCredentialStore::empty());
        let c = SmtpClient::build(&endpoint(TlsMode::ImplicitTls, 465), &pwd_account(), g)
            .await
            .unwrap();
        assert_eq!(c.instance, "ops");
    }

    #[tokio::test]
    async fn build_starttls_succeeds() {
        let g = Arc::new(GoogleCredentialStore::empty());
        let c = SmtpClient::build(&endpoint(TlsMode::Starttls, 587), &pwd_account(), g)
            .await
            .unwrap();
        assert_eq!(c.instance, "ops");
    }

    #[tokio::test]
    async fn build_plain_succeeds() {
        let g = Arc::new(GoogleCredentialStore::empty());
        let c = SmtpClient::build(&endpoint(TlsMode::Plain, 25), &pwd_account(), g)
            .await
            .unwrap();
        assert_eq!(c.instance, "ops");
    }

    #[test]
    fn build_lettre_envelope_handles_cc_and_bcc() {
        let env = SmtpEnvelope {
            from: "ops@x.com".into(),
            to: vec!["a@x.com".into()],
            cc: vec!["b@x.com".into()],
            bcc: vec!["secret@x.com".into()],
        };
        let le = build_lettre_envelope(&env).unwrap();
        // Lettre Envelope merges all recipients. We don't expose the
        // internal list publicly, so we just assert construction
        // succeeded and the From was preserved.
        assert_eq!(le.from().map(|a| a.to_string()), Some("ops@x.com".into()));
    }

    #[test]
    fn build_lettre_envelope_rejects_garbage_address() {
        let env = SmtpEnvelope {
            from: "not-an-email".into(),
            to: vec!["a@x.com".into()],
            cc: vec![],
            bcc: vec![],
        };
        assert!(build_lettre_envelope(&env).is_err());
    }
}
