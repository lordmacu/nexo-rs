//! Extension discovery — scan configured search paths for `plugin.toml` files
//! and turn them into candidates. Synchronous (one-shot at boot). Never
//! panics: every failure becomes a diagnostic so agent startup continues.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::manifest::{ExtensionManifest, MANIFEST_FILENAME};

const SIDECAR_MCP_FILENAME: &str = ".mcp.json";

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SidecarMcpFile {
    #[serde(rename = "mcpServers", alias = "mcp_servers", default)]
    mcp_servers: BTreeMap<String, SidecarMcpServer>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SidecarMcpServer {
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    log_level: Option<String>,
    #[serde(default)]
    context_passthrough: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ExtensionDiscovery {
    search_paths: Vec<PathBuf>,
    ignore_dirs: Vec<String>,
    disabled: Vec<String>,
    allowlist: Vec<String>,
    max_depth: usize,
    /// When true, `walkdir` follows filesystem symlinks during
    /// discovery. Off by default (conservative: avoids loops + leaking
    /// paths outside `search_paths`). Flip via
    /// [`ExtensionDiscovery::with_follow_links`] when a monorepo
    /// symlinks shared plugins into the search tree.
    follow_links: bool,
}

#[derive(Debug, Clone)]
pub struct ExtensionCandidate {
    pub manifest: ExtensionManifest,
    pub root_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub origin: ExtensionOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionOrigin {
    Local,
    Installed { registry: String },
}

#[derive(Debug, Clone)]
pub struct DiscoveryReport {
    pub candidates: Vec<ExtensionCandidate>,
    pub diagnostics: Vec<DiscoveryDiagnostic>,
    pub scanned_dirs: usize,
    /// Number of candidates filtered out by `disabled`.
    pub disabled_count: usize,
    /// Number of error-level diagnostics observed during scan.
    pub invalid_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticLevel {
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct DiscoveryDiagnostic {
    pub level: DiagnosticLevel,
    pub path: PathBuf,
    pub message: String,
}

/// Phase 12.7 — extension-declared MCP servers ready to merge into
/// `McpRuntimeConfig::from_yaml_with_extensions`. Empty if the candidate
/// didn't declare anything under `[mcp_servers.*]`.
#[derive(Debug, Clone)]
pub struct ExtensionMcpDecl {
    pub ext_id: String,
    pub ext_version: String,
    pub ext_root: PathBuf,
    pub servers: std::collections::BTreeMap<String, agent_config::McpServerYaml>,
}

/// Pick up every candidate in the report that declares at least one MCP
/// server, skipping any whose id is in `disabled`.
pub fn collect_mcp_declarations(
    report: &DiscoveryReport,
    disabled: &[String],
) -> Vec<ExtensionMcpDecl> {
    let mut out: Vec<ExtensionMcpDecl> = Vec::new();
    for c in &report.candidates {
        if c.manifest.mcp_servers.is_empty() {
            continue;
        }
        let id = c.manifest.id().to_string();
        if disabled.iter().any(|d| d == &id) {
            continue;
        }
        out.push(ExtensionMcpDecl {
            ext_id: id,
            ext_version: c.manifest.version().to_string(),
            ext_root: c.root_dir.clone(),
            servers: c.manifest.mcp_servers.clone(),
        });
    }
    out
}

impl ExtensionDiscovery {
    pub fn new(
        search_paths: Vec<PathBuf>,
        ignore_dirs: Vec<String>,
        disabled: Vec<String>,
        allowlist: Vec<String>,
        max_depth: usize,
    ) -> Self {
        Self {
            search_paths,
            ignore_dirs,
            disabled,
            allowlist,
            max_depth: max_depth.max(1),
            follow_links: false,
        }
    }

    /// Enable following filesystem symlinks during scan. Default is
    /// `false` — flip for monorepos that symlink shared plugins in.
    pub fn with_follow_links(mut self, follow: bool) -> Self {
        self.follow_links = follow;
        self
    }

    /// Scan all configured search paths. Never panics. Result is sorted:
    /// candidates by `(root_index, id)`, diagnostics by `(path, message)`.
    pub fn discover(&self) -> DiscoveryReport {
        let mut report = DiscoveryReport {
            candidates: Vec::new(),
            diagnostics: Vec::new(),
            scanned_dirs: 0,
            disabled_count: 0,
            invalid_count: 0,
        };
        // Annotate each raw candidate with the root_index it came from so the
        // final sort is deterministic across runs.
        let mut annotated: Vec<(usize, ExtensionCandidate)> = Vec::new();

        for (root_index, root) in self.search_paths.iter().enumerate() {
            let canonical_root = match std::fs::canonicalize(root) {
                Ok(p) => p,
                Err(e) => {
                    report.diagnostics.push(DiscoveryDiagnostic {
                        level: DiagnosticLevel::Warn,
                        path: root.clone(),
                        message: format!("search path not found or unreadable: {e}"),
                    });
                    continue;
                }
            };
            let display_path =
                |path: &Path| normalize_path_for_display(path, root, &canonical_root);

            let ignore = &self.ignore_dirs;
            let walker = WalkDir::new(&canonical_root)
                .max_depth(self.max_depth)
                .follow_links(self.follow_links)
                .sort_by_file_name()
                .into_iter()
                .filter_entry(|entry| {
                    if entry.file_type().is_dir() {
                        if let Some(name) = entry.file_name().to_str() {
                            if ignore.iter().any(|ig| ig == name) {
                                return false;
                            }
                        }
                    }
                    true
                });

            for entry_result in walker {
                let entry = match entry_result {
                    Ok(e) => e,
                    Err(e) => {
                        report.diagnostics.push(DiscoveryDiagnostic {
                            level: DiagnosticLevel::Warn,
                            path: e
                                .path()
                                .map(|p| display_path(p))
                                .unwrap_or_else(|| root.clone()),
                            message: format!("walk error: {e}"),
                        });
                        continue;
                    }
                };
                if !entry.file_type().is_file() {
                    continue;
                }
                if entry.file_name() != MANIFEST_FILENAME {
                    continue;
                }

                report.scanned_dirs += 1;
                let manifest_path = entry.path().to_path_buf();
                let canonical_manifest = match std::fs::canonicalize(&manifest_path) {
                    Ok(p) => p,
                    Err(e) => {
                        report.diagnostics.push(DiscoveryDiagnostic {
                            level: DiagnosticLevel::Error,
                            path: display_path(&manifest_path),
                            message: format!("failed to canonicalize manifest: {e}"),
                        });
                        continue;
                    }
                };
                if !self.follow_links && !canonical_manifest.starts_with(&canonical_root) {
                    // With follow_links=false this should never trip (walkdir
                    // refused to cross the symlink), but we keep the belt-and-
                    // suspenders check for file-level ../ escapes. When the
                    // operator opts into follow_links, the check is relaxed —
                    // monorepo symlinks legitimately point outside the search
                    // root by design.
                    report.diagnostics.push(DiscoveryDiagnostic {
                        level: DiagnosticLevel::Error,
                        path: display_path(&manifest_path),
                        message: "manifest path escapes search root via symlink".into(),
                    });
                    continue;
                }

                match ExtensionManifest::from_path(&manifest_path) {
                    Ok(mut manifest) => {
                        let root_dir = canonical_manifest
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_else(|| canonical_root.clone());
                        if manifest.mcp_servers.is_empty() {
                            let sidecar_path = root_dir.join(SIDECAR_MCP_FILENAME);
                            if sidecar_path.exists() {
                                match load_sidecar_mcp_servers(&sidecar_path) {
                                    Ok(servers) if !servers.is_empty() => {
                                        manifest.mcp_servers = servers;
                                        if let Err(e) = manifest.validate() {
                                            report.diagnostics.push(DiscoveryDiagnostic {
                                                level: DiagnosticLevel::Warn,
                                                path: display_path(&sidecar_path),
                                                message: format!(
                                                    "ignoring invalid sidecar MCP declaration: {e}"
                                                ),
                                            });
                                            manifest.mcp_servers.clear();
                                        }
                                    }
                                    Ok(_) => {}
                                    Err(e) => {
                                        report.diagnostics.push(DiscoveryDiagnostic {
                                            level: DiagnosticLevel::Warn,
                                            path: display_path(&sidecar_path),
                                            message: format!(
                                                "failed to parse sidecar MCP file: {e}"
                                            ),
                                        });
                                    }
                                }
                            }
                        }
                        annotated.push((
                            root_index,
                            ExtensionCandidate {
                                manifest,
                                root_dir,
                                manifest_path: canonical_manifest,
                                origin: ExtensionOrigin::Local,
                            },
                        ));
                    }
                    Err(e) => {
                        report.diagnostics.push(DiscoveryDiagnostic {
                            level: DiagnosticLevel::Error,
                            path: display_path(&manifest_path),
                            message: e.to_string(),
                        });
                    }
                }
            }
        }

        // Post-scan pruning: nested plugin.toml — drop any candidate whose
        // root_dir is a strict descendant of another candidate's root_dir.
        prune_nested(&mut annotated);

        // Duplicate id: keep first (by root_index asc, then root_dir path asc).
        annotated.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.root_dir.cmp(&b.1.root_dir)));
        let mut seen: HashSet<String> = HashSet::new();
        let mut survivors: Vec<(usize, ExtensionCandidate)> = Vec::new();
        for (idx, cand) in annotated.into_iter() {
            let id = cand.manifest.id().to_string();
            if seen.insert(id.clone()) {
                survivors.push((idx, cand));
            } else {
                report.diagnostics.push(DiscoveryDiagnostic {
                    level: DiagnosticLevel::Warn,
                    path: cand.root_dir.clone(),
                    message: format!("duplicate extension id `{id}`; keeping first"),
                });
            }
        }

        // Disabled filter.
        let disabled: HashSet<&String> = self.disabled.iter().collect();
        let mut kept: Vec<(usize, ExtensionCandidate)> = Vec::new();
        for cand in survivors {
            if disabled.contains(&cand.1.manifest.id().to_string()) {
                report.disabled_count += 1;
            } else {
                kept.push(cand);
            }
        }
        survivors = kept;

        // Allowlist (only when non-empty).
        if !self.allowlist.is_empty() {
            let discovered_ids: HashSet<String> = survivors
                .iter()
                .map(|(_, c)| c.manifest.id().to_string())
                .collect();
            for id in &self.allowlist {
                if !discovered_ids.contains(id) {
                    report.diagnostics.push(DiscoveryDiagnostic {
                        level: DiagnosticLevel::Warn,
                        path: std::path::PathBuf::from(id),
                        message: format!(
                            "allowlist contains `{id}` but no extension with that id was discovered"
                        ),
                    });
                }
            }
            let allow: HashSet<&String> = self.allowlist.iter().collect();
            survivors.retain(|(_, c)| allow.contains(&c.manifest.id().to_string()));
        }

        // Final sort for stable output.
        survivors.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.manifest.id().cmp(b.1.manifest.id()))
        });
        report.candidates = survivors.into_iter().map(|(_, c)| c).collect();

        report.diagnostics.sort_by(|a, b| {
            a.path
                .to_string_lossy()
                .cmp(&b.path.to_string_lossy())
                .then_with(|| a.message.cmp(&b.message))
        });
        report.invalid_count = report
            .diagnostics
            .iter()
            .filter(|d| d.level == DiagnosticLevel::Error)
            .count();

        report
    }
}

/// Prefer a path rooted at the operator-provided `search_root` for logs.
/// When the scanner walks a canonicalized path (e.g. `/private/...` on macOS),
/// this remaps diagnostics to the configured root when possible.
fn normalize_path_for_display(path: &Path, search_root: &Path, canonical_root: &Path) -> PathBuf {
    match path.strip_prefix(canonical_root) {
        Ok(rel) => search_root.join(rel),
        Err(_) => path.to_path_buf(),
    }
}

fn load_sidecar_mcp_servers(
    path: &Path,
) -> anyhow::Result<BTreeMap<String, agent_config::McpServerYaml>> {
    let raw = std::fs::read_to_string(path)?;
    let sidecar: SidecarMcpFile = serde_json::from_str(&raw)?;
    let mut out: BTreeMap<String, agent_config::McpServerYaml> = BTreeMap::new();
    for (name, server) in sidecar.mcp_servers {
        let transport = server
            .transport
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let decl = match transport {
            Some("stdio") => {
                let command = server
                    .command
                    .ok_or_else(|| anyhow::anyhow!("server `{name}` is missing `command`"))?;
                agent_config::McpServerYaml::Stdio {
                    command,
                    args: server.args,
                    env: server.env,
                    cwd: server.cwd,
                    log_level: server.log_level,
                    context_passthrough: server.context_passthrough,
                }
            }
            Some("streamable_http") => {
                let url = server
                    .url
                    .ok_or_else(|| anyhow::anyhow!("server `{name}` is missing `url`"))?;
                agent_config::McpServerYaml::StreamableHttp {
                    url,
                    headers: server.headers,
                    log_level: server.log_level,
                    context_passthrough: server.context_passthrough,
                }
            }
            Some("streamable-http") | Some("streamablehttp") => {
                let url = server
                    .url
                    .ok_or_else(|| anyhow::anyhow!("server `{name}` is missing `url`"))?;
                agent_config::McpServerYaml::StreamableHttp {
                    url,
                    headers: server.headers,
                    log_level: server.log_level,
                    context_passthrough: server.context_passthrough,
                }
            }
            Some("sse") => {
                let url = server
                    .url
                    .ok_or_else(|| anyhow::anyhow!("server `{name}` is missing `url`"))?;
                agent_config::McpServerYaml::Sse {
                    url,
                    headers: server.headers,
                    log_level: server.log_level,
                    context_passthrough: server.context_passthrough,
                }
            }
            Some("auto") | Some("http") => {
                let url = server
                    .url
                    .ok_or_else(|| anyhow::anyhow!("server `{name}` is missing `url`"))?;
                agent_config::McpServerYaml::Auto {
                    url,
                    headers: server.headers,
                    log_level: server.log_level,
                    context_passthrough: server.context_passthrough,
                }
            }
            Some(other) => {
                return Err(anyhow::anyhow!(
                    "server `{name}` uses unsupported transport `{other}`"
                ));
            }
            None => {
                if let Some(command) = server.command {
                    agent_config::McpServerYaml::Stdio {
                        command,
                        args: server.args,
                        env: server.env,
                        cwd: server.cwd,
                        log_level: server.log_level,
                        context_passthrough: server.context_passthrough,
                    }
                } else if let Some(url) = server.url {
                    agent_config::McpServerYaml::Auto {
                        url,
                        headers: server.headers,
                        log_level: server.log_level,
                        context_passthrough: server.context_passthrough,
                    }
                } else {
                    return Err(anyhow::anyhow!(
                        "server `{name}` must define either `command` or `url`"
                    ));
                }
            }
        };
        out.insert(name, decl);
    }
    Ok(out)
}

/// In-place removal of candidates whose `root_dir` is a strict descendant of
/// another candidate's `root_dir`. Complexity is O(N * depth): candidates are
/// sorted once, then each path checks only its ancestor chain against a
/// `BTreeSet` of already-kept roots.
fn prune_nested(items: &mut Vec<(usize, ExtensionCandidate)>) {
    if items.len() < 2 {
        return;
    }

    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|a, b| items[*a].1.root_dir.cmp(&items[*b].1.root_dir));

    let mut kept_roots: BTreeSet<PathBuf> = BTreeSet::new();
    let mut keep = vec![true; items.len()];
    for idx in order {
        let path = &items[idx].1.root_dir;
        let mut parent = path.parent();
        let mut is_nested = false;
        while let Some(p) = parent {
            if kept_roots.contains(p) {
                is_nested = true;
                break;
            }
            parent = p.parent();
        }
        if is_nested {
            keep[idx] = false;
        } else {
            kept_roots.insert(path.clone());
        }
    }

    let mut idx = 0;
    items.retain(|_| {
        let k = keep[idx];
        idx += 1;
        k
    });
}

// Kept around for test readability — noop in prod, but helps avoid warnings on
// minor helper imports if tests shrink.
#[allow(dead_code)]
fn dedup_ids_to_map(cands: &[ExtensionCandidate]) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for (i, c) in cands.iter().enumerate() {
        map.entry(c.manifest.id().to_string()).or_insert(i);
    }
    map
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_manifest(dir: &Path, id: &str, tool: &str) {
        fs::create_dir_all(dir).unwrap();
        let src = format!(
            r#"
[plugin]
id = "{id}"
version = "0.1.0"

[capabilities]
tools = ["{tool}"]

[transport]
kind = "stdio"
command = "./bin"
"#
        );
        fs::write(dir.join(MANIFEST_FILENAME), src).unwrap();
    }

    fn write_sidecar(dir: &Path, src: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(SIDECAR_MCP_FILENAME), src).unwrap();
    }

    fn discovery_for(root: &Path) -> ExtensionDiscovery {
        ExtensionDiscovery::new(
            vec![root.to_path_buf()],
            vec!["node_modules".into(), "target".into(), ".git".into()],
            vec![],
            vec![],
            3,
        )
    }

    #[test]
    fn scan_empty_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let d = discovery_for(tmp.path());
        let r = d.discover();
        assert!(r.candidates.is_empty());
        assert!(r.diagnostics.is_empty());
        assert_eq!(r.disabled_count, 0);
        assert_eq!(r.invalid_count, 0);
    }

    #[test]
    fn scan_finds_valid_manifest() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp.path().join("weather"), "weather", "get_weather");
        let r = discovery_for(tmp.path()).discover();
        assert_eq!(r.candidates.len(), 1);
        assert_eq!(r.candidates[0].manifest.id(), "weather");
        assert!(r.diagnostics.is_empty());
    }

    #[test]
    fn scan_invalid_manifest_becomes_error_diagnostic() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("broken");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(MANIFEST_FILENAME), "not = valid [toml").unwrap();
        let r = discovery_for(tmp.path()).discover();
        assert!(r.candidates.is_empty());
        assert_eq!(r.diagnostics.len(), 1);
        assert_eq!(r.diagnostics[0].level, DiagnosticLevel::Error);
        assert_eq!(r.invalid_count, 1);
    }

    #[test]
    fn scan_loads_sidecar_mcp_when_manifest_has_none() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("weather");
        write_manifest(&root, "weather", "get_weather");
        write_sidecar(
            &root,
            r#"{
  "mcpServers": {
    "local": {
      "command": "/bin/true",
      "args": ["--ok"]
    }
  }
}"#,
        );
        let r = discovery_for(tmp.path()).discover();
        assert_eq!(r.candidates.len(), 1);
        let cand = &r.candidates[0];
        assert!(cand.manifest.mcp_servers.contains_key("local"));
    }

    #[test]
    fn scan_ignores_sidecar_when_manifest_declares_mcp_servers() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("weather");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join(MANIFEST_FILENAME),
            r#"
[plugin]
id = "weather"
version = "0.1.0"

[capabilities]
tools = ["get_weather"]

[transport]
kind = "stdio"
command = "./bin"

[mcp_servers.inline]
transport = "stdio"
command = "/bin/true"
"#,
        )
        .unwrap();
        write_sidecar(
            &root,
            r#"{
  "mcpServers": {
    "sidecar": {
      "command": "/bin/echo"
    }
  }
}"#,
        );

        let r = discovery_for(tmp.path()).discover();
        assert_eq!(r.candidates.len(), 1);
        let cand = &r.candidates[0];
        assert!(cand.manifest.mcp_servers.contains_key("inline"));
        assert!(!cand.manifest.mcp_servers.contains_key("sidecar"));
    }

    #[test]
    fn scan_invalid_sidecar_emits_warn_and_keeps_candidate() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("weather");
        write_manifest(&root, "weather", "get_weather");
        write_sidecar(&root, "{ not valid json");

        let r = discovery_for(tmp.path()).discover();
        assert_eq!(r.candidates.len(), 1);
        assert!(r.candidates[0].manifest.mcp_servers.is_empty());
        assert!(r
            .diagnostics
            .iter()
            .any(|d| d.level == DiagnosticLevel::Warn && d.path.ends_with(SIDECAR_MCP_FILENAME)));
    }

    #[test]
    fn scan_respects_max_depth() {
        let tmp = TempDir::new().unwrap();
        // depth: tmp/a/b/c/d/plugin.toml  → 5 levels, max_depth=3
        let deep = tmp.path().join("a").join("b").join("c").join("d");
        write_manifest(&deep, "deep", "t");
        let r = ExtensionDiscovery::new(vec![tmp.path().to_path_buf()], vec![], vec![], vec![], 3)
            .discover();
        assert!(
            r.candidates.is_empty(),
            "manifest beyond max_depth should not be found"
        );
    }

    #[test]
    fn scan_skips_ignore_dirs() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp.path().join("node_modules").join("foo"), "foo", "t");
        let r = discovery_for(tmp.path()).discover();
        assert!(r.candidates.is_empty());
    }

    #[test]
    fn scan_handles_multiple_roots() {
        let root_a = TempDir::new().unwrap();
        let root_b = TempDir::new().unwrap();
        write_manifest(&root_a.path().join("a1"), "alpha", "t");
        write_manifest(&root_b.path().join("b1"), "bravo", "t");
        let d = ExtensionDiscovery::new(
            vec![root_a.path().to_path_buf(), root_b.path().to_path_buf()],
            vec![],
            vec![],
            vec![],
            3,
        );
        let r = d.discover();
        let ids: Vec<&str> = r.candidates.iter().map(|c| c.manifest.id()).collect();
        assert_eq!(ids, vec!["alpha", "bravo"]);
    }

    #[test]
    fn scan_dedups_by_id_keeps_first() {
        let root_a = TempDir::new().unwrap();
        let root_b = TempDir::new().unwrap();
        write_manifest(&root_a.path().join("x"), "same", "t");
        write_manifest(&root_b.path().join("x"), "same", "t");
        let d = ExtensionDiscovery::new(
            vec![root_a.path().to_path_buf(), root_b.path().to_path_buf()],
            vec![],
            vec![],
            vec![],
            3,
        );
        let r = d.discover();
        assert_eq!(r.candidates.len(), 1);
        assert!(r
            .diagnostics
            .iter()
            .any(|d| d.level == DiagnosticLevel::Warn && d.message.contains("duplicate")));
    }

    #[test]
    fn scan_applies_disabled() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp.path().join("a"), "ext_a", "t");
        write_manifest(&tmp.path().join("b"), "ext_b", "t");
        let d = ExtensionDiscovery::new(
            vec![tmp.path().to_path_buf()],
            vec![],
            vec!["ext_a".into()],
            vec![],
            3,
        );
        let r = d.discover();
        let ids: Vec<&str> = r.candidates.iter().map(|c| c.manifest.id()).collect();
        assert_eq!(ids, vec!["ext_b"]);
        assert_eq!(r.disabled_count, 1);
    }

    #[test]
    fn scan_applies_allowlist() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp.path().join("a"), "ext_a", "t");
        write_manifest(&tmp.path().join("b"), "ext_b", "t");
        let d = ExtensionDiscovery::new(
            vec![tmp.path().to_path_buf()],
            vec![],
            vec![],
            vec!["ext_a".into()],
            3,
        );
        let r = d.discover();
        let ids: Vec<&str> = r.candidates.iter().map(|c| c.manifest.id()).collect();
        assert_eq!(ids, vec!["ext_a"]);
    }

    #[cfg(unix)]
    #[test]
    fn follow_links_flag_discovers_symlinked_plugin() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        // Real plugin lives outside the search path.
        let real = tmp.path().join("real");
        write_manifest(&real, "monorepo_plugin", "t");
        // Search path contains a symlink pointing at the real plugin dir.
        let search = tmp.path().join("search");
        fs::create_dir_all(&search).unwrap();
        symlink(&real, search.join("linked")).unwrap();

        // Default behaviour — symlinks ignored.
        let d = ExtensionDiscovery::new(vec![search.clone()], vec![], vec![], vec![], 3);
        let r = d.discover();
        assert!(
            r.candidates.is_empty(),
            "default follow_links=false must skip symlinks"
        );

        // Opt-in — symlinked plugin is discovered.
        let d = ExtensionDiscovery::new(vec![search], vec![], vec![], vec![], 3)
            .with_follow_links(true);
        let r = d.discover();
        let ids: Vec<&str> = r.candidates.iter().map(|c| c.manifest.id()).collect();
        assert_eq!(ids, vec!["monorepo_plugin"]);
    }

    #[cfg(unix)]
    #[test]
    fn diagnostics_use_configured_search_path_prefix_when_root_is_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir_all(real.join("broken")).unwrap();
        fs::write(real.join("broken").join(MANIFEST_FILENAME), "not = valid [toml").unwrap();

        let search = tmp.path().join("search");
        symlink(&real, &search).unwrap();

        let d = ExtensionDiscovery::new(vec![search.clone()], vec![], vec![], vec![], 3);
        let r = d.discover();
        assert_eq!(r.diagnostics.len(), 1);
        assert!(
            r.diagnostics[0].path.starts_with(&search),
            "diagnostic path should be under configured search path, got: {}",
            r.diagnostics[0].path.display()
        );
    }

    #[test]
    fn allowlist_with_unknown_id_emits_warn_diagnostic() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp.path().join("a"), "ext_a", "t");
        let d = ExtensionDiscovery::new(
            vec![tmp.path().to_path_buf()],
            vec![],
            vec![],
            vec!["ext_a".into(), "ext_ghost".into()],
            3,
        );
        let r = d.discover();
        let ids: Vec<&str> = r.candidates.iter().map(|c| c.manifest.id()).collect();
        assert_eq!(ids, vec!["ext_a"]);
        let hit = r
            .diagnostics
            .iter()
            .find(|d| d.message.contains("ext_ghost"));
        assert!(hit.is_some(), "expected warn for ext_ghost");
        assert_eq!(hit.unwrap().level, DiagnosticLevel::Warn);
    }

    #[test]
    fn missing_search_path_emits_warn_diagnostic() {
        let d = ExtensionDiscovery::new(
            vec![PathBuf::from("/definitely/not/here-xyz123")],
            vec![],
            vec![],
            vec![],
            3,
        );
        let r = d.discover();
        assert!(r.candidates.is_empty());
        assert_eq!(r.diagnostics.len(), 1);
        assert_eq!(r.diagnostics[0].level, DiagnosticLevel::Warn);
    }

    #[test]
    fn scan_prunes_nested_plugin_toml() {
        let tmp = TempDir::new().unwrap();
        let outer = tmp.path().join("outer");
        let inner = outer.join("inner");
        write_manifest(&outer, "outer", "t");
        write_manifest(&inner, "inner", "t");
        let r = discovery_for(tmp.path()).discover();
        let ids: Vec<&str> = r.candidates.iter().map(|c| c.manifest.id()).collect();
        assert_eq!(ids, vec!["outer"]);
    }

    #[test]
    fn discovery_is_deterministic() {
        let tmp = TempDir::new().unwrap();
        write_manifest(&tmp.path().join("beta"), "beta", "t");
        write_manifest(&tmp.path().join("alpha"), "alpha", "t");
        write_manifest(&tmp.path().join("gamma"), "gamma", "t");
        let d = discovery_for(tmp.path());
        let r1 = d.discover();
        let r2 = d.discover();
        let ids1: Vec<&str> = r1.candidates.iter().map(|c| c.manifest.id()).collect();
        let ids2: Vec<&str> = r2.candidates.iter().map(|c| c.manifest.id()).collect();
        assert_eq!(ids1, ids2);
        assert_eq!(ids1, vec!["alpha", "beta", "gamma"]);
    }
}
