/// Shared helper: pull `host` out of a URL without dragging in the
/// `url` crate. Returns `None` for relative or malformed URLs.
pub(crate) fn brave_host(url: &str) -> Option<String> {
    let after = url.split("://").nth(1).unwrap_or(url);
    let host = after.split('/').next().unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

#[cfg(feature = "brave")]
pub mod brave;
#[cfg(feature = "duckduckgo")]
pub mod duckduckgo;
#[cfg(feature = "perplexity")]
pub mod perplexity;
#[cfg(feature = "tavily")]
pub mod tavily;
