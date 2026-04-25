//! Resolve the gateway URL for `agent pair start`.
//!
//! Priority chain:
//! 1. `pairing.public_url` (operator-set; highest priority)
//! 2. `tunnel.url` (active Phase tunnel)
//! 3. `gateway.remote.url` (legacy)
//! 4. LAN bind address
//! 5. error: `gateway only on loopback` — fail-closed.
//!
//! Cleartext `ws://` is allowed only on hosts the operator can
//! reasonably trust to be private:
//! - `127.0.0.1` / `::1` (loopback)
//! - RFC1918 (10/8, 172.16/12, 192.168/16)
//! - link-local (169.254/16)
//! - `*.local` mDNS hostnames
//! - `10.0.2.2` (Android emulator)
//! - any extra host the operator listed in `ws_cleartext_allow`
//!
//! Everything else exigirá `wss://`.

use std::net::IpAddr;

#[derive(Debug, Clone)]
pub struct UrlInputs {
    pub public_url: Option<String>,
    pub tunnel_url: Option<String>,
    pub gateway_remote_url: Option<String>,
    pub lan_url: Option<String>,
    /// Extra hostnames where cleartext `ws://` is allowed.
    pub ws_cleartext_allow_extra: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedUrl {
    pub url: String,
    pub source: &'static str,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ResolveError {
    #[error("gateway only bound to loopback; set pairing.public_url, enable tunnel, or use gateway.bind=lan")]
    LoopbackOnly,
    #[error("resolved url '{url}' uses ws:// but host is not in the cleartext-allow list (loopback / RFC1918 / link-local / .local / 10.0.2.2 / extras)")]
    InsecureCleartext { url: String },
    #[error("invalid url: {0}")]
    Invalid(String),
}

pub fn resolve(inputs: &UrlInputs) -> Result<ResolvedUrl, ResolveError> {
    let candidate = pick_candidate(inputs)?;
    enforce_security(&candidate.url, &inputs.ws_cleartext_allow_extra)?;
    Ok(candidate)
}

fn pick_candidate(inputs: &UrlInputs) -> Result<ResolvedUrl, ResolveError> {
    if let Some(u) = inputs.public_url.as_ref().filter(|s| !s.trim().is_empty()) {
        return Ok(ResolvedUrl {
            url: u.trim().to_string(),
            source: "pairing.public_url",
        });
    }
    if let Some(u) = inputs.tunnel_url.as_ref().filter(|s| !s.trim().is_empty()) {
        return Ok(ResolvedUrl {
            url: u.trim().to_string(),
            source: "tunnel.url",
        });
    }
    if let Some(u) = inputs
        .gateway_remote_url
        .as_ref()
        .filter(|s| !s.trim().is_empty())
    {
        return Ok(ResolvedUrl {
            url: u.trim().to_string(),
            source: "gateway.remote.url",
        });
    }
    if let Some(u) = inputs.lan_url.as_ref().filter(|s| !s.trim().is_empty()) {
        return Ok(ResolvedUrl {
            url: u.trim().to_string(),
            source: "gateway.bind=lan",
        });
    }
    Err(ResolveError::LoopbackOnly)
}

fn enforce_security(url: &str, extras: &[String]) -> Result<(), ResolveError> {
    let scheme = url
        .split("://")
        .next()
        .ok_or_else(|| ResolveError::Invalid(url.into()))?
        .to_ascii_lowercase();
    if scheme == "wss" || scheme == "https" {
        return Ok(());
    }
    if scheme != "ws" && scheme != "http" {
        return Err(ResolveError::Invalid(format!(
            "unsupported scheme: {scheme}"
        )));
    }
    let after = url.split("://").nth(1).unwrap_or("");
    let host = after.split('/').next().unwrap_or("").split(':').next().unwrap_or("");
    if host.is_empty() {
        return Err(ResolveError::Invalid(url.into()));
    }
    if is_cleartext_allowed(host, extras) {
        return Ok(());
    }
    Err(ResolveError::InsecureCleartext { url: url.into() })
}

fn is_cleartext_allowed(host: &str, extras: &[String]) -> bool {
    if extras.iter().any(|h| h.eq_ignore_ascii_case(host)) {
        return true;
    }
    if host.eq_ignore_ascii_case("localhost") || host == "10.0.2.2" {
        return true;
    }
    if host.to_ascii_lowercase().ends_with(".local") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            if v4.is_loopback() || v4.is_link_local() || v4.is_private() {
                return true;
            }
            false
        }
        Ok(IpAddr::V6(v6)) => {
            // `Ipv6Addr::is_unicast_link_local` is unstable on MSRV 1.80;
            // hand-rolled check: fe80::/10
            let segs = v6.segments();
            v6.is_loopback() || (segs[0] & 0xffc0) == 0xfe80
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> UrlInputs {
        UrlInputs {
            public_url: None,
            tunnel_url: None,
            gateway_remote_url: None,
            lan_url: None,
            ws_cleartext_allow_extra: vec![],
        }
    }

    #[test]
    fn loopback_only_fails_closed() {
        let err = resolve(&empty()).unwrap_err();
        assert!(matches!(err, ResolveError::LoopbackOnly));
    }

    #[test]
    fn priority_public_over_tunnel() {
        let mut i = empty();
        i.public_url = Some("wss://op.example.com".into());
        i.tunnel_url = Some("wss://abc.ngrok.app".into());
        let r = resolve(&i).unwrap();
        assert_eq!(r.source, "pairing.public_url");
        assert_eq!(r.url, "wss://op.example.com");
    }

    #[test]
    fn priority_tunnel_over_remote() {
        let mut i = empty();
        i.tunnel_url = Some("wss://abc.ngrok.app".into());
        i.gateway_remote_url = Some("wss://legacy".into());
        let r = resolve(&i).unwrap();
        assert_eq!(r.source, "tunnel.url");
    }

    #[test]
    fn lan_ws_allowed_for_rfc1918() {
        let mut i = empty();
        i.lan_url = Some("ws://192.168.1.10:9090".into());
        let r = resolve(&i).unwrap();
        assert_eq!(r.source, "gateway.bind=lan");
    }

    #[test]
    fn ws_blocked_on_public_host() {
        let mut i = empty();
        i.public_url = Some("ws://api.example.com".into());
        let err = resolve(&i).unwrap_err();
        assert!(matches!(err, ResolveError::InsecureCleartext { .. }));
    }

    #[test]
    fn ws_allowed_on_localhost() {
        let mut i = empty();
        i.public_url = Some("ws://localhost:9090".into());
        resolve(&i).unwrap();
    }

    #[test]
    fn ws_allowed_on_dot_local_mdns() {
        let mut i = empty();
        i.public_url = Some("ws://kitchen-pi.local:9090".into());
        resolve(&i).unwrap();
    }

    #[test]
    fn ws_allowed_on_extras() {
        let mut i = empty();
        i.public_url = Some("ws://my.cool.host:9090".into());
        i.ws_cleartext_allow_extra = vec!["my.cool.host".into()];
        resolve(&i).unwrap();
    }

    #[test]
    fn ws_allowed_on_android_emu() {
        let mut i = empty();
        i.public_url = Some("ws://10.0.2.2:9090".into());
        resolve(&i).unwrap();
    }

    #[test]
    fn link_local_v4_allowed() {
        let mut i = empty();
        i.lan_url = Some("ws://169.254.1.5:9090".into());
        resolve(&i).unwrap();
    }
}
