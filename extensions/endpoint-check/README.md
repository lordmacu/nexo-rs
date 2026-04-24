# Endpoint Check Extension (Rust)

HTTP probe + TLS certificate inspection. Rust + rustls + reqwest + x509-parser.
Reinterpretation of OpenClaw's `healthcheck` skill (which is actually host
hardening for the OpenClaw process itself, not generic endpoint monitoring).

## Tools

- `status` — limits + info
- `http_probe` — GET/HEAD with latency, final_url, body preview, expected_status match
- `ssl_cert` — issuer, subject, SANs, expiry, serial, signature alg

## Security posture

- SSL cert inspection uses an **accept-any** verifier because the point is
  to report on certs the operator *wants to see*, including expired /
  self-signed ones. Trust validation is not this extension's job.
- No HTTP SSRF guard here (unlike fetch-url): this tool is for the operator,
  not for LLM-exposed remote URL fetches. If the operator hands this tool to
  an LLM, they should do so on a trusted-network deployment.

## Error codes

- -32005 request timeout
- -32003 transport error
- -32602 bad input
- -32060 DNS resolve failed (ssl_cert)
- -32061 TCP connect failed (ssl_cert)
- -32062 TLS handshake failed (ssl_cert)
- -32063 certificate parse failed (ssl_cert)

## Tests

10 integration tests using wiremock for HTTP and a nonexistent host for
SSL resolve errors. Live SSL against public endpoints is out of scope
(would depend on network).
