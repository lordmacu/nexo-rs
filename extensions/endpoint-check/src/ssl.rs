//! TLS certificate inspection. Opens a fresh TCP + TLS handshake, captures
//! the peer-presented certificate chain, and parses the leaf via `x509-parser`.
//!
//! Deliberately independent of the HTTP probe: we want to inspect the cert
//! even when the server returns a bad HTTP status or doesn't speak HTTP at all
//! (e.g. SMTP STARTTLS is out of scope today — only plain TLS).

use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme};
use x509_parser::parse_x509_certificate;

#[derive(Debug)]
pub enum SslError {
    Resolve(String),
    Connect(String),
    Tls(String),
    Parse(String),
    BadUrl(String),
}

impl SslError {
    pub fn rpc_code(&self) -> i32 {
        match self {
            SslError::BadUrl(_) => -32602,
            SslError::Resolve(_) => -32060,
            SslError::Connect(_) => -32061,
            SslError::Tls(_) => -32062,
            SslError::Parse(_) => -32063,
        }
    }
    pub fn message(&self) -> String {
        match self {
            SslError::BadUrl(m) => m.clone(),
            SslError::Resolve(m) => format!("dns resolve failed: {m}"),
            SslError::Connect(m) => format!("tcp connect failed: {m}"),
            SslError::Tls(m) => format!("tls handshake failed: {m}"),
            SslError::Parse(m) => format!("certificate parse failed: {m}"),
        }
    }
}

#[derive(Debug)]
pub struct CertInfo {
    pub subject: String,
    pub issuer: String,
    pub not_before_unix: i64,
    pub not_after_unix: i64,
    pub seconds_until_expiry: i64,
    pub sans: Vec<String>,
    pub serial_hex: String,
    pub signature_algorithm: String,
    pub chain_length: usize,
}

/// Accept-any verifier. We only care about the certificate bytes, not whether
/// the chain is trusted — operators routinely inspect expired/untrusted certs.
#[derive(Debug)]
struct AcceptAny;

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            SignatureScheme::ED25519,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PSS_SHA384,
            SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}

pub fn inspect(host: &str, port: u16, timeout_secs: u64) -> Result<CertInfo, SslError> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let root_store = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let _ = root_store; // ensure webpki_roots dep is referenced even though we use AcceptAny

    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny))
        .with_no_client_auth();

    let server_name = ServerName::try_from(host.to_string())
        .map_err(|e| SslError::BadUrl(format!("bad server name `{host}`: {e}")))?;
    let mut conn = ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| SslError::Tls(e.to_string()))?;

    let addr_iter = (host, port)
        .to_socket_addrs()
        .map_err(|e| SslError::Resolve(e.to_string()))?;
    let addr = addr_iter
        .into_iter()
        .next()
        .ok_or_else(|| SslError::Resolve(format!("no addresses for {host}")))?;

    let timeout = Duration::from_secs(timeout_secs);
    let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| SslError::Connect(e.to_string()))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| SslError::Connect(e.to_string()))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| SslError::Connect(e.to_string()))?;

    // Complete handshake; rustls gives us the peer cert chain after this.
    let mut tls = rustls::Stream::new(&mut conn, &mut stream);
    // Force at least one read/write cycle so the handshake completes.
    // Send a zero-byte write (which rustls handles by flushing handshake records).
    let _ = std::io::Write::flush(&mut tls);
    // In case rustls needs a read to receive ServerHello/Certificate, trigger it:
    let mut _buf = [0u8; 0];
    let _ = std::io::Read::read(&mut tls, &mut _buf);

    let chain = conn
        .peer_certificates()
        .ok_or_else(|| SslError::Tls("no peer certificate presented".into()))?
        .to_vec();
    if chain.is_empty() {
        return Err(SslError::Tls("empty peer certificate chain".into()));
    }
    let leaf = &chain[0];
    let (_, x509) =
        parse_x509_certificate(leaf.as_ref()).map_err(|e| SslError::Parse(e.to_string()))?;

    let subject = x509.tbs_certificate.subject.to_string();
    let issuer = x509.tbs_certificate.issuer.to_string();
    let not_before_unix = x509.tbs_certificate.validity.not_before.timestamp();
    let not_after_unix = x509.tbs_certificate.validity.not_after.timestamp();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let seconds_until_expiry = not_after_unix - now;

    let mut sans = Vec::new();
    for ext in x509.tbs_certificate.extensions() {
        if let x509_parser::extensions::ParsedExtension::SubjectAlternativeName(san) =
            ext.parsed_extension()
        {
            for gn in &san.general_names {
                sans.push(format!("{gn}"));
            }
        }
    }
    let serial_hex = format!("{:x}", x509.tbs_certificate.serial);
    let signature_algorithm = format!("{}", x509.signature_algorithm.algorithm);

    Ok(CertInfo {
        subject,
        issuer,
        not_before_unix,
        not_after_unix,
        seconds_until_expiry,
        sans,
        serial_hex,
        signature_algorithm,
        chain_length: chain.len(),
    })
}
