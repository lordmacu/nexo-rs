//! Phase 82.2 — pure-fn client-IP resolution honouring trusted
//! reverse-proxy lists.
//!
//! Every helper here is decoupled from any HTTP framework — caller
//! passes the socket peer (an `IpAddr`) plus a typed
//! `(forwarded_for, real_ip)` header view. Tests cover the gates
//! without spinning a TCP listener.
//!
//! Mirror of OpenClaw `research/extensions/webhooks/src/http.ts:705-715`
//! `resolveRequestClientIp(req, gateway.trustedProxies,
//! allowRealIpFallback)`. Differs in being pure-fn (no `req`
//! coupling), CIDR-based (matches against `IpNetwork` not single
//! IP literals), and returning a typed `IpAddr` rather than
//! `string | undefined`.

use std::net::IpAddr;

use ipnetwork::IpNetwork;

/// Header view the caller passes in. Stays Copy-cheap; HTTP
/// frameworks (axum, hyper) populate from `HeaderMap` via simple
/// `.get(...).and_then(|v| v.to_str().ok())` calls.
#[derive(Debug, Clone, Copy)]
pub struct ProxyHeaders<'a> {
    pub forwarded_for: Option<&'a str>,
    pub real_ip: Option<&'a str>,
}

/// Decide which `IpAddr` represents the *originating* client of an
/// inbound request.
///
/// Algorithm (defensive, mirrors OpenClaw):
/// 1. If `trusted_proxies` is empty, ignore both headers — return
///    `socket_peer`.
/// 2. If `socket_peer` is NOT inside any trusted CIDR, ignore both
///    headers — return `socket_peer` (anonymous client claiming to
///    be a proxy is rejected).
/// 3. Walk `X-Forwarded-For` right-to-left; return the first IP
///    that is NOT itself inside a trusted CIDR (the originating
///    client). This is the canonical `XFF` chain semantics.
/// 4. If the entire chain is composed of trusted proxies (rare,
///    happens behind multiple LB hops), AND `allow_realip_fallback`
///    is `true`, honour `X-Real-IP`.
/// 5. Final fallback: `socket_peer`.
///
/// Never trusts a header from an untrusted peer. Constant gate
/// order; no provider-specific branching.
pub fn resolve_request_client_ip(
    socket_peer: IpAddr,
    headers: ProxyHeaders<'_>,
    trusted_proxies: &[IpNetwork],
    allow_realip_fallback: bool,
) -> IpAddr {
    if trusted_proxies.is_empty() {
        return socket_peer;
    }
    if !is_trusted(socket_peer, trusted_proxies) {
        return socket_peer;
    }

    // Walk the XFF chain right-to-left looking for the first
    // non-trusted hop — that's the real client.
    if let Some(xff) = headers.forwarded_for {
        let chain = extract_x_forwarded_for_chain(xff);
        for ip in chain.iter().rev() {
            if !is_trusted(*ip, trusted_proxies) {
                return *ip;
            }
        }
    }

    if allow_realip_fallback {
        if let Some(real) = headers.real_ip {
            if let Ok(ip) = real.trim().parse::<IpAddr>() {
                if !is_trusted(ip, trusted_proxies) {
                    return ip;
                }
            }
        }
    }

    socket_peer
}

/// Parse a comma-separated `X-Forwarded-For` header value into the
/// hop chain (left-most = original client; right-most = nearest
/// proxy). Malformed entries are silently dropped — XFF tolerates
/// dirty input by spec.
pub fn extract_x_forwarded_for_chain(header: &str) -> Vec<IpAddr> {
    header
        .split(',')
        .filter_map(|raw| raw.trim().parse::<IpAddr>().ok())
        .collect()
}

fn is_trusted(ip: IpAddr, trusted: &[IpNetwork]) -> bool {
    trusted.iter().any(|net| net.contains(ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cidr(s: &str) -> IpNetwork {
        s.parse().unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn empty_trusted_list_returns_socket_peer() {
        let peer = ip("203.0.113.5");
        let got = resolve_request_client_ip(
            peer,
            ProxyHeaders {
                forwarded_for: Some("1.2.3.4"),
                real_ip: Some("5.6.7.8"),
            },
            &[],
            true,
        );
        assert_eq!(got, peer);
    }

    #[test]
    fn untrusted_peer_with_xff_ignores_header() {
        let peer = ip("203.0.113.5");
        let trusted = [cidr("10.0.0.0/8")];
        let got = resolve_request_client_ip(
            peer,
            ProxyHeaders {
                forwarded_for: Some("1.2.3.4"),
                real_ip: None,
            },
            &trusted,
            false,
        );
        assert_eq!(got, peer);
    }

    #[test]
    fn trusted_peer_returns_first_xff_hop() {
        let peer = ip("10.0.0.5");
        let trusted = [cidr("10.0.0.0/8")];
        let got = resolve_request_client_ip(
            peer,
            ProxyHeaders {
                forwarded_for: Some("1.2.3.4"),
                real_ip: None,
            },
            &trusted,
            false,
        );
        assert_eq!(got, ip("1.2.3.4"));
    }

    #[test]
    fn multi_hop_chain_skips_trusted_internal_proxies() {
        // Chain order in XFF: client, proxy_1, proxy_2.
        // socket_peer is the last hop (proxy_2 = 10.0.0.5).
        let peer = ip("10.0.0.5");
        let trusted = [cidr("10.0.0.0/8")];
        let got = resolve_request_client_ip(
            peer,
            ProxyHeaders {
                forwarded_for: Some("1.2.3.4, 10.0.0.4, 10.0.0.5"),
                real_ip: None,
            },
            &trusted,
            false,
        );
        assert_eq!(got, ip("1.2.3.4"));
    }

    #[test]
    fn ipv6_chain_resolves() {
        let peer: IpAddr = "fd00::5".parse().unwrap();
        let trusted = [cidr("fd00::/8")];
        let got = resolve_request_client_ip(
            peer,
            ProxyHeaders {
                forwarded_for: Some("2001:db8::1"),
                real_ip: None,
            },
            &trusted,
            false,
        );
        assert_eq!(got, ip("2001:db8::1"));
    }

    #[test]
    fn malformed_xff_entries_skipped() {
        let peer = ip("10.0.0.5");
        let trusted = [cidr("10.0.0.0/8")];
        let got = resolve_request_client_ip(
            peer,
            ProxyHeaders {
                forwarded_for: Some("not-an-ip, 1.2.3.4"),
                real_ip: None,
            },
            &trusted,
            false,
        );
        assert_eq!(got, ip("1.2.3.4"));
    }

    #[test]
    fn empty_xff_with_realip_fallback_honoured_when_enabled() {
        let peer = ip("10.0.0.5");
        let trusted = [cidr("10.0.0.0/8")];
        let got = resolve_request_client_ip(
            peer,
            ProxyHeaders {
                forwarded_for: None,
                real_ip: Some("5.6.7.8"),
            },
            &trusted,
            true,
        );
        assert_eq!(got, ip("5.6.7.8"));
    }

    #[test]
    fn empty_xff_with_realip_fallback_disabled_returns_peer() {
        let peer = ip("10.0.0.5");
        let trusted = [cidr("10.0.0.0/8")];
        let got = resolve_request_client_ip(
            peer,
            ProxyHeaders {
                forwarded_for: None,
                real_ip: Some("5.6.7.8"),
            },
            &trusted,
            false,
        );
        assert_eq!(got, peer);
    }

    #[test]
    fn cidr_membership_full_octet() {
        // 10.0.0.0/24 — only 10.0.0.* trusted; 10.0.1.5 NOT trusted.
        let peer = ip("10.0.1.5");
        let trusted = [cidr("10.0.0.0/24")];
        let got = resolve_request_client_ip(
            peer,
            ProxyHeaders {
                forwarded_for: Some("1.2.3.4"),
                real_ip: None,
            },
            &trusted,
            false,
        );
        // Untrusted peer → header ignored.
        assert_eq!(got, peer);
    }

    #[test]
    fn extract_chain_handles_whitespace_and_empty() {
        let chain = extract_x_forwarded_for_chain("  1.2.3.4  ,  5.6.7.8  ");
        assert_eq!(chain, vec![ip("1.2.3.4"), ip("5.6.7.8")]);
        assert!(extract_x_forwarded_for_chain("").is_empty());
        assert!(extract_x_forwarded_for_chain(",,,").is_empty());
    }
}
