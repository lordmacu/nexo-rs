//! Serve a built SPA `dist/` from the same loopback port as the
//! API.
//!
//! Mount as the outer router's `fallback_service` so the API
//! router claims `/api/*` first and the SPA shell loads on every
//! other path:
//!
//! - SPA shell + login page load without a bearer (auth gate
//!   is on JSON-RPC, not on file fetch).
//! - Unknown SPA routes (`/chat/:key`, `/login`) fall back to
//!   `index.html` for client-side routing.
//! - `/api/*` is matched by the API router first → 404s on
//!   unknown API methods instead of leaking the SPA shell.
//!
//! ## Compression
//!
//! Per-request negotiation via `tower-http::CompressionLayer`:
//! brotli wins when the client advertises `br` (every modern
//! browser does), gzip is the fallback. Brotli compresses ~15%
//! smaller than gzip on JS/CSS bundles; the cost is on-the-fly
//! encoding, mitigated for hashed assets by the immutable
//! `Cache-Control` (browser caches the compressed body).
//!
//! ## Caching
//!
//! Two-tier policy on each served file:
//!
//! | Path | `Cache-Control` |
//! |---|---|
//! | `<dist>/assets/*-<8+ hex>.<ext>` | `public, max-age=31536000, immutable` |
//! | Everything else (`index.html`, `favicon.ico`, top-level) | `no-cache` |
//!
//! The hash detector recognises Vite's content-addressed naming
//! convention (`[name]-[hash:8].[ext]` with hex hash, ≥ 8 chars
//! for forward-compat). Non-hashed files revalidate on every
//! visit so a redeploy reaches operators on the next page load.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tower_http::compression::CompressionLayer;

/// Configuration for the static-assets sub-router.
#[derive(Debug, Clone)]
pub struct StaticAssetsConfig {
    /// Absolute path to the built SPA `dist/`. The `index.html`
    /// at the root is the fallback for any path not matching a
    /// real file under this directory.
    pub dist: PathBuf,
}

impl StaticAssetsConfig {
    /// Direct constructor when the caller already has the path.
    /// No filesystem checks — caller validates.
    pub fn from_dir(dist: PathBuf) -> Self {
        Self { dist }
    }

    /// Resolve config from a microapp-supplied env var. Returns
    /// `None` for any of:
    /// - env var unset (dev mode, Vite serves the UI on a
    ///   separate port)
    /// - path doesn't exist (operator hasn't built the SPA yet)
    /// - path isn't a directory
    ///
    /// All error states log a warn but do NOT crash — caller
    /// degrades to API-only.
    pub fn from_env(env_var: &str) -> Option<Self> {
        let raw = std::env::var(env_var).ok()?;
        let dist = PathBuf::from(raw);
        if !dist.exists() {
            tracing::warn!(
                env = %env_var,
                dist = %dist.display(),
                "static assets path does not exist; serving disabled"
            );
            return None;
        }
        if !dist.is_dir() {
            tracing::warn!(
                env = %env_var,
                dist = %dist.display(),
                "static assets path is not a directory; serving disabled"
            );
            return None;
        }
        Some(Self { dist })
    }
}

/// Build the static-asset sub-router. The router serves `/` and
/// every fallback path through `spa_handler`, which:
///
/// - Refuses `/api/*` paths (returns 404) so the SPA shell never
///   shadows the API router's own 404.
/// - Resolves the request path to `<dist>/<path>` with traversal
///   guard.
/// - Serves the file when it exists (with two-tier
///   `Cache-Control` + content-type guess).
/// - Falls through to `<dist>/index.html` for SPA client routes.
///
/// Compression layer wraps the response stream — brotli + gzip
/// negotiated per request.
pub fn router(cfg: &StaticAssetsConfig) -> Router {
    let cfg_arc = Arc::new(cfg.clone());
    Router::new()
        .route("/", get(spa_handler))
        .fallback(get(spa_handler))
        .with_state(cfg_arc)
        .layer(CompressionLayer::new().gzip(true).br(true))
}

/// True when `path` is a content-addressed Vite asset under
/// `<dist>/assets/`. Vite naming template:
/// `[name]-[hash:8].[ext]` with hex hash; we accept ≥ 8 hex
/// chars to stay forward-compat with longer hashes.
pub fn is_hashed_asset(path: &Path, dist: &Path) -> bool {
    let assets = dist.join("assets");
    if path.strip_prefix(&assets).is_err() {
        return false;
    }
    if path.extension().is_none() {
        return false;
    }
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    let Some(dash_idx) = stem.rfind('-') else {
        return false;
    };
    let candidate = &stem[dash_idx + 1..];
    candidate.len() >= 8 && candidate.chars().all(|c| c.is_ascii_hexdigit())
}

/// Pick the `Cache-Control` value for `path`. Two-tier:
/// hashed Vite asset → 1y immutable; everything else →
/// revalidate on every visit so a redeploy reaches operators
/// on next load.
fn cache_control_for(path: &Path, dist: &Path) -> &'static str {
    if is_hashed_asset(path, dist) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Resolve the request path under `dist/`. If a regular file
/// exists at the resolved location, serve it with a content-
/// type guess. Otherwise return `dist/index.html` so React
/// Router can handle client-side routing.
async fn spa_handler(State(cfg): State<Arc<StaticAssetsConfig>>, uri: Uri) -> Response {
    let raw = uri.path();
    // Critical correctness: refuse to serve the SPA shell for
    // `/api/*` paths. The API router owns that prefix; if a
    // method isn't registered it must surface as 404 to the
    // caller, not silently render the SPA HTML.
    if raw.starts_with("/api/") || raw == "/api" {
        return (StatusCode::NOT_FOUND, "API method not found").into_response();
    }
    let rel = raw.trim_start_matches('/');
    if let Some(target) = resolve_under_dist(&cfg.dist, rel) {
        if target.is_file() {
            return serve_file(&target, &cfg.dist).await;
        }
    }
    let index = cfg.dist.join("index.html");
    serve_file(&index, &cfg.dist).await
}

/// Resolve `<dist>/<rel>` while rejecting any `..` traversal.
/// Returns `None` when the resolved path escapes `dist`.
fn resolve_under_dist(dist: &Path, rel: &str) -> Option<PathBuf> {
    if rel.is_empty() {
        return None;
    }
    let mut out = dist.to_path_buf();
    for segment in rel.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return None;
        }
        out.push(segment);
    }
    Some(out)
}

/// Read `path` + return it with the right content-type and the
/// two-tier `Cache-Control` policy. Falls through to a 404 on
/// read failure (no cache header — caller decides whether to
/// retry against `index.html`).
async fn serve_file(path: &Path, dist: &Path) -> Response {
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    let ct = content_type_for(path);
    let cache = cache_control_for(path, dist);
    (
        [
            (header::CONTENT_TYPE, ct),
            (header::CACHE_CONTROL, cache),
        ],
        bytes,
    )
        .into_response()
}

/// Hardcoded extension → MIME map covering the file types Vite
/// ships in `dist/`. Avoids pulling `mime_guess` as a direct
/// dependency.
fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("map") => "application/json; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::io::Write;
    use std::sync::Mutex;
    use tower::ServiceExt;

    /// `std::env::set_var` is process-global; tests that mutate
    /// env must serialise.
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    const TEST_ENV: &str = "NEXO_MICROAPP_HTTP_TEST_DIST";

    fn clear_env() {
        std::env::remove_var(TEST_ENV);
    }

    #[test]
    fn from_env_returns_none_when_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        assert!(StaticAssetsConfig::from_env(TEST_ENV).is_none());
    }

    #[test]
    fn from_env_returns_some_for_valid_dir() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let dir = tempfile::TempDir::new().unwrap();
        std::env::set_var(TEST_ENV, dir.path());
        let cfg = StaticAssetsConfig::from_env(TEST_ENV).expect("Some(cfg)");
        assert_eq!(cfg.dist, dir.path());
        clear_env();
    }

    #[test]
    fn from_env_returns_none_for_missing_path() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        std::env::set_var(TEST_ENV, "/tmp/nexo-microapp-http-does-not-exist-xyzzy");
        assert!(StaticAssetsConfig::from_env(TEST_ENV).is_none());
        clear_env();
    }

    #[test]
    fn from_env_returns_none_for_file_path() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let f = tempfile::NamedTempFile::new().unwrap();
        std::env::set_var(TEST_ENV, f.path());
        assert!(StaticAssetsConfig::from_env(TEST_ENV).is_none());
        clear_env();
    }

    #[test]
    fn is_hashed_asset_recognises_hex_suffix() {
        let dist = Path::new("/var/www/dist");
        assert!(is_hashed_asset(
            Path::new("/var/www/dist/assets/index-deadbeef.js"),
            dist
        ));
        assert!(is_hashed_asset(
            Path::new("/var/www/dist/assets/main-0123456789ab.css"),
            dist
        ));
    }

    #[test]
    fn is_hashed_asset_rejects_non_hex_or_short_or_outside_assets() {
        let dist = Path::new("/var/www/dist");
        assert!(!is_hashed_asset(
            Path::new("/var/www/dist/assets/index-XXXXXXXX.js"),
            dist
        ));
        assert!(!is_hashed_asset(
            Path::new("/var/www/dist/assets/logo-deadbee.png"),
            dist
        ));
        assert!(!is_hashed_asset(
            Path::new("/var/www/dist/assets/logo.png"),
            dist
        ));
        assert!(!is_hashed_asset(
            Path::new("/var/www/dist/index-deadbeef.html"),
            dist
        ));
    }

    fn cache_header(res: &Response) -> &str {
        res.headers()
            .get("cache-control")
            .map(|v| v.to_str().unwrap_or(""))
            .unwrap_or("")
    }

    #[tokio::test]
    async fn router_serves_index_for_spa_route() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("index.html"), "<html>SPA</html>").unwrap();
        let cfg = StaticAssetsConfig {
            dist: dir.path().to_path_buf(),
        };
        let app = router(&cfg);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(cache_header(&res), "no-cache");
    }

    #[tokio::test]
    async fn hashed_asset_gets_long_cache() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("index.html"), "<html>SPA</html>").unwrap();
        let mut f =
            std::fs::File::create(dir.path().join("assets/index-deadbeef.js")).unwrap();
        write!(f, "console.log(1);").unwrap();
        let cfg = StaticAssetsConfig {
            dist: dir.path().to_path_buf(),
        };
        let app = router(&cfg);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/assets/index-deadbeef.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            cache_header(&res),
            "public, max-age=31536000, immutable"
        );
    }

    #[tokio::test]
    async fn api_paths_404_instead_of_leaking_spa() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("index.html"), "<html>SPA</html>").unwrap();
        let cfg = StaticAssetsConfig {
            dist: dir.path().to_path_buf(),
        };
        let app = router(&cfg);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/unknown")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn resolve_under_dist_rejects_traversal() {
        let dist = Path::new("/var/www/dist");
        assert!(resolve_under_dist(dist, "../etc/passwd").is_none());
        assert!(resolve_under_dist(dist, "a/../../etc").is_none());
    }
}
