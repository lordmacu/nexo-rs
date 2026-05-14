//! Smoke test for `nexo-tunnel` v0.2 (pure-Rust quick-tunnel
//! backbone). Brings up a `nexo_tunnel::TunnelManager` against a
//! local HTTP echo server, polls the public URL until the edge
//! routes a request through, then tears down. Validates the whole
//! tunnel path end-to-end without touching the daemon binary or
//! its 15k-line main.rs.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example quick_tunnel_smoke
//! ```
//!
//! Pin a CF anycast IP with `CFQT_RESOLVE_IP=104.16.230.132` to
//! bypass local stub-resolver NXDOMAIN caches on dev boxes.

use std::time::Duration;

use nexo_tunnel::TunnelManager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,cloudflare_quick_tunnel=debug".into()),
        )
        .init();

    // ── Local echo server ───────────────────────────────────────────────────
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    eprintln!("local echo on 127.0.0.1:{port}");
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let mut req = Vec::new();
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    req.extend_from_slice(&buf[..n]);
                    if req.len() > 16 * 1024 {
                        return;
                    }
                }
                let path = std::str::from_utf8(&req)
                    .ok()
                    .and_then(|s| s.lines().next())
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/unknown")
                    .to_string();
                let body = format!("nexo-tunnel smoke OK ({path})");
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });

    // ── Tunnel up via nexo-tunnel ───────────────────────────────────────────
    let handle = TunnelManager::new(port).start().await?;
    eprintln!();
    eprintln!("  Public URL: {}", handle.url);
    eprintln!();

    let resolve_pin = std::env::var("CFQT_RESOLVE_IP").ok();
    let host = handle.url.trim_start_matches("https://").to_string();
    let mut client_builder = reqwest::Client::builder().timeout(Duration::from_secs(10));
    if let Some(ip_str) = resolve_pin.as_ref() {
        let ip: std::net::IpAddr = ip_str.parse()?;
        client_builder = client_builder.resolve(&host, std::net::SocketAddr::new(ip, 443));
        eprintln!("  DNS pin:    {ip_str}:443");
    }
    let client = client_builder.build()?;
    let probe_url = format!("{}/probe", handle.url);

    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let mut attempt = 0u32;
    let mut body = String::new();
    while std::time::Instant::now() < deadline {
        attempt += 1;
        match client.get(&probe_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                body = resp.text().await.unwrap_or_default();
                eprintln!("attempt {attempt}: 200 OK body={body:?}");
                break;
            }
            Ok(resp) => {
                eprintln!("attempt {attempt}: status {} (warming up)", resp.status());
            }
            Err(e) => {
                eprintln!("attempt {attempt}: transport error {e}");
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    if !body.contains("nexo-tunnel smoke OK") {
        anyhow::bail!("smoke FAILED — never saw the echoed body; got {body:?}");
    }

    handle.shutdown().await;
    eprintln!("smoke OK ✓ — tunnel closed cleanly");
    Ok(())
}
