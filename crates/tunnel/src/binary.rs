//! Locate (or download + cache) the `cloudflared` binary for the
//! current platform.
//!
//! Strategy:
//!
//! 1. Honour `CLOUDFLARED_BINARY` env var if set (operators who want to
//!    pin a specific build).
//! 2. Try `$PATH` via a which-style lookup — if the user already has
//!    cloudflared installed (brew, apt, choco, winget), reuse it.
//! 3. Look for a cached copy under the app's platform data dir
//!    (`$XDG_DATA_HOME/agent/bin/cloudflared`, etc).
//! 4. As a last resort, download from the official GitHub release for
//!    the detected (os, arch), extract if needed (macOS ships `.tgz`),
//!    `chmod +x` on Unix, and cache.
//!
//! The download URL pattern is documented by Cloudflare and stable:
//! <https://github.com/cloudflare/cloudflared/releases/latest>.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use tokio::fs;
use tokio::io::AsyncWriteExt;

const RELEASE_BASE: &str = "https://github.com/cloudflare/cloudflared/releases/latest/download";

/// Resolve a working cloudflared binary, downloading if necessary.
pub async fn ensure_cloudflared() -> Result<PathBuf> {
    if let Ok(override_path) = std::env::var("CLOUDFLARED_BINARY") {
        let p = PathBuf::from(override_path);
        if p.exists() {
            tracing::debug!(path = %p.display(), "cloudflared via CLOUDFLARED_BINARY");
            return Ok(p);
        }
        bail!(
            "CLOUDFLARED_BINARY={} set but file does not exist",
            p.display()
        );
    }

    if let Some(p) = which_path("cloudflared").await {
        tracing::debug!(path = %p.display(), "cloudflared on PATH");
        return Ok(p);
    }

    let cache = cache_binary_path()?;
    if cache.exists() {
        tracing::debug!(path = %cache.display(), "cloudflared cached");
        return Ok(cache);
    }

    tracing::info!("cloudflared not found — downloading to {}", cache.display());
    download_into(&cache).await?;
    Ok(cache)
}

/// Minimal `which` — cross-platform, no extra dep.
async fn which_path(name: &str) -> Option<PathBuf> {
    let exe = exe_name(name);
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(&exe);
        if tokio::fs::metadata(&candidate).await.is_ok() {
            return Some(candidate);
        }
    }
    None
}

fn exe_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn cache_binary_path() -> Result<PathBuf> {
    let proj = directories::ProjectDirs::from("co", "agent", "agent-tunnel")
        .ok_or_else(|| anyhow!("cannot resolve platform data directory"))?;
    let dir = proj.data_local_dir().join("bin");
    Ok(dir.join(exe_name("cloudflared")))
}

fn release_asset_name() -> Result<&'static str> {
    // Matches the filenames Cloudflare publishes on every release.
    let name = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "cloudflared-linux-amd64",
        ("linux", "aarch64") => "cloudflared-linux-arm64",
        ("linux", "arm") => "cloudflared-linux-arm",
        ("macos", "x86_64") => "cloudflared-darwin-amd64.tgz",
        ("macos", "aarch64") => "cloudflared-darwin-arm64.tgz",
        ("windows", "x86_64") => "cloudflared-windows-amd64.exe",
        ("windows", "x86") => "cloudflared-windows-386.exe",
        (os, arch) => bail!("unsupported platform for cloudflared auto-install: {os}/{arch}"),
    };
    Ok(name)
}

async fn download_into(target: &Path) -> Result<()> {
    use indicatif::{ProgressBar, ProgressStyle};
    use std::time::Duration;

    let asset = release_asset_name()?;
    let url = format!("{RELEASE_BASE}/{asset}");
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("mkdir {}", parent.display()))?;
    }

    let resp = reqwest::get(&url)
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("HTTP status for {url}"))?;

    let total = resp.content_length();
    // Pick the right progress style: byte-accurate bar if the server
    // sent Content-Length, indeterminate spinner otherwise.
    let pb = if let Some(n) = total {
        let pb = ProgressBar::new(n);
        pb.set_style(
            ProgressStyle::with_template(
                "  {spinner:.cyan} cloudflared [{bar:30.cyan/blue}] {bytes}/{total_bytes} {bytes_per_sec} · {eta}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("█▉▊▋▌▍▎▏ "),
        );
        pb
    } else {
        let pb = ProgressBar::new_spinner();
        pb.enable_steady_tick(Duration::from_millis(100));
        pb.set_style(
            ProgressStyle::with_template("  {spinner:.cyan} cloudflared · {bytes} {bytes_per_sec}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        pb
    };
    pb.set_message(format!("descargando {asset}"));

    // Stream directly to disk. Previously we collected the whole
    // ~50 MB binary in a `Vec<u8>` before writing — on a small VPS
    // that's 10 % of memory wasted + extracting the tgz doubled the
    // cost. We stream into a temp file, then either rename it into
    // place (non-tgz) or re-open and extract (tgz).
    use futures::StreamExt;
    let tmp = target.with_extension("partial");
    if let Some(parent) = tmp.parent() {
        fs::create_dir_all(parent).await.ok();
    }
    let mut file = fs::File::create(&tmp)
        .await
        .with_context(|| format!("create tmp {}", tmp.display()))?;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("streaming {url}"))?;
        pb.inc(chunk.len() as u64);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);
    pb.finish_with_message("cloudflared descargado");

    if asset.ends_with(".tgz") {
        // Re-open and extract the single binary entry into target,
        // then drop the tmp.
        extract_tgz_single_binary_from_path(&tmp, target)?;
        let _ = fs::remove_file(&tmp).await;
    } else {
        // Atomic rename so an interrupted download never leaves a
        // half-written `cloudflared` at the final path.
        fs::rename(&tmp, target)
            .await
            .with_context(|| format!("rename {} → {}", tmp.display(), target.display()))?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(target)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(target, perms)?;
    }

    tracing::info!(path = %target.display(), "cloudflared installed");
    Ok(())
}

/// macOS releases are tgz bundles containing a single `cloudflared`
/// binary — extract that one file into the target path from a
/// previously-downloaded tarball on disk. Reading from disk avoids the
/// double-buffer cost of the old in-memory variant.
///
/// Defense in depth: the entry's `file_name()` must be literally
/// `"cloudflared"` — absolute paths or `..` traversal segments are
/// rejected, so a hypothetical malicious tarball can't use our extract
/// to write outside the target directory.
/// True when `p` is a safe relative path that we can join under a
/// chosen target directory without escaping it. Split out so it can be
/// unit-tested without constructing a malicious tarball (tar::Builder
/// refuses to write `..` paths in Rust, making the full integration
/// test awkward).
pub(crate) fn path_is_safe(p: &Path) -> bool {
    !p.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::RootDir
        )
    })
}

pub(crate) fn extract_tgz_single_binary_from_path(tarball: &Path, target: &Path) -> Result<()> {
    let f = std::fs::File::open(tarball).with_context(|| format!("open {}", tarball.display()))?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut tar = tar::Archive::new(gz);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        // Reject anything beyond a bare filename — a component like
        // `..` or an absolute path in the archive header would end up
        // writing outside `target`'s parent.
        if !path_is_safe(&path) {
            bail!(
                "tarball entry `{}` contains `..` or absolute path — refusing to extract",
                path.display()
            );
        }
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name == "cloudflared" {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(target)?;
            std::io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
    }
    bail!("cloudflared binary not found inside downloaded tarball")
}
