//! Phase 31.6 — `nexo plugin new <id> --lang <lang>` scaffolder.
//!
//! Compiles in the four `extensions/template-plugin-{rust,python,
//! typescript,php}/` directories via `include_dir!`, then on
//! invocation copies one of them to a destination path while
//! substituting placeholder strings with the operator's chosen
//! id / owner / description. Output is a ready-to-build plugin
//! repo that needs only an LLM key + git push to ship.
//!
//! Replaces the manual `sed` pipeline previously documented in
//! each template README.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use include_dir::{include_dir, Dir, DirEntry};
use regex::Regex;
use serde::Serialize;

const PLUGIN_ID_REGEX: &str = r"^[a-z][a-z0-9_]{0,31}$";

const TEXT_EXTENSIONS: &[&str] = &[
    "toml", "md", "rs", "py", "ts", "mjs", "js", "php", "json",
    "sh", "yml", "yaml", "lock", "txt",
];

static TEMPLATE_RUST: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/extensions/template-plugin-rust");
static TEMPLATE_PYTHON: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/extensions/template-plugin-python");
static TEMPLATE_TYPESCRIPT: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/extensions/template-plugin-typescript");
static TEMPLATE_PHP: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/extensions/template-plugin-php");

// ── Public report types ────────────────────────────────────────────

/// Successful scaffold report. Serialized to JSON when `--json`.
#[derive(Debug, Clone, Serialize)]
pub struct PluginNewReport {
    pub ok: bool,
    pub id: String,
    pub lang: String,
    pub dest: PathBuf,
    pub files_created: u32,
    pub git_initialized: bool,
    pub next_steps: Vec<String>,
}

/// Error report shape. Serialized to JSON when `--json`.
#[derive(Debug, Clone, Serialize)]
pub struct PluginNewErrorReport {
    pub ok: bool,
    pub kind: &'static str,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dest: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum PluginNewError {
    #[error("invalid plugin id `{got}`: must match {regex}")]
    InvalidId { got: String, regex: &'static str },

    #[error(
        "invalid lang `{got}`: must be one of rust|python|typescript|php"
    )]
    InvalidLang { got: String },

    #[error(
        "destination `{}` already exists; pass --force to overwrite",
        dest.display()
    )]
    DestExists { dest: PathBuf },

    #[error("template read failed: {0}")]
    TemplateRead(String),

    #[error("io error: {0}")]
    Io(String),

    #[error("git init failed: {0}")]
    GitInit(String),
}

pub fn plugin_new_error_kind(e: &PluginNewError) -> &'static str {
    match e {
        PluginNewError::InvalidId { .. } => "InvalidId",
        PluginNewError::InvalidLang { .. } => "InvalidLang",
        PluginNewError::DestExists { .. } => "DestExists",
        PluginNewError::TemplateRead(_) => "TemplateRead",
        PluginNewError::Io(_) => "Io",
        PluginNewError::GitInit(_) => "GitInit",
    }
}

// ── Helpers ────────────────────────────────────────────────────────

fn id_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(PLUGIN_ID_REGEX).expect("hard-coded regex"))
}

fn validate_id(id: &str) -> Result<(), PluginNewError> {
    if !id_regex().is_match(id) {
        return Err(PluginNewError::InvalidId {
            got: id.to_string(),
            regex: PLUGIN_ID_REGEX,
        });
    }
    Ok(())
}

fn template_for(lang: &str) -> Result<&'static Dir<'static>, PluginNewError> {
    match lang {
        "rust" => Ok(&TEMPLATE_RUST),
        "python" => Ok(&TEMPLATE_PYTHON),
        "typescript" => Ok(&TEMPLATE_TYPESCRIPT),
        "php" => Ok(&TEMPLATE_PHP),
        other => Err(PluginNewError::InvalidLang {
            got: other.to_string(),
        }),
    }
}

fn lang_label(lang: &str) -> &'static str {
    match lang {
        "rust" => "Rust",
        "python" => "Python",
        "typescript" => "TypeScript",
        "php" => "PHP",
        _ => "",
    }
}

fn echo_kind_suffix(lang: &str) -> &'static str {
    match lang {
        "rust" => "_rust",
        "python" => "_py",
        "typescript" => "_ts",
        "php" => "_php",
        _ => "",
    }
}

fn title_case_from_id(id: &str, lang: &str) -> String {
    let title: String = id
        .split('_')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut chars = s.chars();
            match chars.next() {
                Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("{} ({})", title, lang_label(lang))
}

/// Build the placeholder list (longest pattern first) for a given
/// scaffold. Order matters: longer keys are replaced before
/// shorter ones to avoid partial-match collisions.
fn placeholders_for(
    lang: &str,
    id: &str,
    owner: Option<&str>,
    description: Option<&str>,
) -> Vec<(String, String)> {
    let label = lang_label(lang);
    let echo_suffix = echo_kind_suffix(lang);
    let title = title_case_from_id(id, lang);

    let owner_line = owner
        .map(|o| {
            format!(
                "{} <{}@users.noreply.github.com>",
                o, o
            )
        })
        .unwrap_or_else(|| {
            "Cristian Garcia <informacion@cristiangarcia.co>".to_string()
        });

    let default_desc = format!(
        "{} plugin (scaffolded by `nexo plugin new`).",
        id
    );
    let desc = description
        .map(str::to_string)
        .unwrap_or(default_desc.clone());

    vec![
        // Order: longest first.
        (
            format!("template_plugin_{}", lang),
            id.to_string(),
        ),
        (
            format!("template-plugin-{}", lang),
            id.to_string(),
        ),
        (
            format!("template_echo{}", echo_suffix),
            format!("{}_echo", id),
        ),
        (
            format!("Template Plugin ({})", label),
            title,
        ),
        (
            format!(
                "Skeleton out-of-tree subprocess plugin in {}.",
                label
            ),
            desc.clone(),
        ),
        (
            "Skeleton out-of-tree subprocess plugin demonstrating PluginAdapter.".to_string(),
            desc,
        ),
        (
            "Cristian Garcia <informacion@cristiangarcia.co>".to_string(),
            owner_line,
        ),
    ]
}

fn is_text_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| TEXT_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn apply_substitutions(content: &str, replacements: &[(String, String)]) -> String {
    let mut out = content.to_string();
    for (needle, replacement) in replacements {
        if !needle.is_empty() {
            out = out.replace(needle.as_str(), replacement.as_str());
        }
    }
    out
}

fn write_dir_with_substitutions(
    template: &Dir<'_>,
    template_root: &Path,
    dest: &Path,
    replacements: &[(String, String)],
) -> Result<u32, PluginNewError> {
    let mut count: u32 = 0;
    for entry in template.entries() {
        match entry {
            DirEntry::Dir(d) => {
                let rel = d
                    .path()
                    .strip_prefix(template_root)
                    .unwrap_or_else(|_| d.path());
                let target = dest.join(rel);
                std::fs::create_dir_all(&target)
                    .map_err(|e| PluginNewError::Io(format!("mkdir {}: {e}", target.display())))?;
                count += write_dir_with_substitutions(d, template_root, dest, replacements)?;
            }
            DirEntry::File(f) => {
                let rel = f
                    .path()
                    .strip_prefix(template_root)
                    .unwrap_or_else(|_| f.path());
                let target = dest.join(rel);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        PluginNewError::Io(format!("mkdir {}: {e}", parent.display()))
                    })?;
                }
                if is_text_extension(f.path()) {
                    let raw = f
                        .contents_utf8()
                        .ok_or_else(|| {
                            PluginNewError::TemplateRead(format!(
                                "non-utf8 text file in template: {}",
                                f.path().display()
                            ))
                        })?;
                    let substituted = apply_substitutions(raw, replacements);
                    std::fs::write(&target, substituted).map_err(|e| {
                        PluginNewError::Io(format!("write {}: {e}", target.display()))
                    })?;
                } else {
                    std::fs::write(&target, f.contents()).map_err(|e| {
                        PluginNewError::Io(format!("write {}: {e}", target.display()))
                    })?;
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if rel.starts_with("scripts")
                        && rel
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e == "sh")
                            .unwrap_or(false)
                    {
                        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
                    }
                }
                count += 1;
            }
        }
    }
    Ok(count)
}

fn next_steps_for(lang: &str, id: &str, owner: Option<&str>) -> Vec<String> {
    let mut steps = vec![format!("cd {}", id)];
    match lang {
        "rust" => {
            steps.push("cargo build --release".into());
        }
        "python" => {
            steps.push("python3 -m venv .venv && source .venv/bin/activate".into());
            steps.push("pip install -r requirements.txt".into());
        }
        "typescript" => {
            steps.push("npm install".into());
            steps.push("npm run build".into());
        }
        "php" => {
            steps.push("composer install".into());
        }
        _ => {}
    }
    if let Some(o) = owner {
        steps.push(format!("git remote add origin git@github.com:{}/{}.git", o, id));
    } else {
        steps.push("git remote add origin git@github.com:<your-handle>/<repo>.git".into());
    }
    steps.push("git push -u origin main".into());
    steps.push("git tag v0.1.0 && git push --tags".into());
    steps
}

async fn try_git_init(dest: &Path, lang: &str) -> Result<bool, PluginNewError> {
    use tokio::process::Command;
    if Command::new("git").arg("--version").output().await.is_err() {
        eprintln!(
            "! git binary not found on PATH; skipping --git step. Run `git init` manually."
        );
        return Ok(false);
    }
    let init_status = Command::new("git")
        .arg("init")
        .arg("--initial-branch=main")
        .current_dir(dest)
        .status()
        .await
        .map_err(|e| PluginNewError::GitInit(format!("git init: {e}")))?;
    if !init_status.success() {
        return Err(PluginNewError::GitInit(format!(
            "git init exited {init_status}"
        )));
    }
    Command::new("git")
        .arg("add")
        .arg(".")
        .current_dir(dest)
        .status()
        .await
        .map_err(|e| PluginNewError::GitInit(format!("git add: {e}")))?;
    let msg = format!("chore: scaffold from `nexo plugin new --lang {}`", lang);
    Command::new("git")
        .arg("commit")
        .arg("-m")
        .arg(msg)
        .current_dir(dest)
        .status()
        .await
        .map_err(|e| PluginNewError::GitInit(format!("git commit: {e}")))?;
    Ok(true)
}

// ── Public orchestration ───────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn run_plugin_new(
    id: String,
    lang: String,
    dest_override: Option<PathBuf>,
    owner: Option<String>,
    description: Option<String>,
    git_init: bool,
    force: bool,
    json: bool,
) -> Result<i32> {
    if let Err(e) = validate_id(&id) {
        return Ok(emit_error(&e, json, None));
    }
    let template = match template_for(&lang) {
        Ok(t) => t,
        Err(e) => return Ok(emit_error(&e, json, None)),
    };

    let dest = dest_override.unwrap_or_else(|| PathBuf::from(&id));
    if dest.exists() {
        if force {
            if let Err(e) = std::fs::remove_dir_all(&dest) {
                return Ok(emit_error(
                    &PluginNewError::Io(format!(
                        "remove existing {}: {e}",
                        dest.display()
                    )),
                    json,
                    Some(dest.clone()),
                ));
            }
        } else {
            return Ok(emit_error(
                &PluginNewError::DestExists { dest: dest.clone() },
                json,
                Some(dest),
            ));
        }
    }
    if let Err(e) = std::fs::create_dir_all(&dest) {
        return Ok(emit_error(
            &PluginNewError::Io(format!("mkdir {}: {e}", dest.display())),
            json,
            Some(dest),
        ));
    }

    if !json {
        eprintln!("→ Scaffolding {} plugin `{}` at {}", lang_label(&lang), id, dest.display());
    }

    let placeholders = placeholders_for(
        &lang,
        &id,
        owner.as_deref(),
        description.as_deref(),
    );
    let files_created = match write_dir_with_substitutions(template, template.path(), &dest, &placeholders) {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dest);
            return Ok(emit_error(&e, json, Some(dest)));
        }
    };

    if !json {
        eprintln!("✓ Wrote {} files", files_created);
    }

    let git_initialized = if git_init {
        match try_git_init(&dest, &lang).await {
            Ok(b) => b,
            Err(e) => {
                if !json {
                    eprintln!("! git init failed: {e}");
                }
                false
            }
        }
    } else {
        false
    };
    if git_initialized && !json {
        eprintln!("✓ Initialized git repository with one commit");
    }

    let report = PluginNewReport {
        ok: true,
        id: id.clone(),
        lang: lang.clone(),
        dest: dest.clone(),
        files_created,
        git_initialized,
        next_steps: next_steps_for(&lang, &id, owner.as_deref()),
    };

    if json {
        println!(
            "{}",
            serde_json::to_string(&report).unwrap_or_default()
        );
    } else {
        eprintln!();
        eprintln!("✓ Plugin `{}` scaffolded at {}", id, dest.display());
        eprintln!();
        eprintln!("Next steps:");
        for step in &report.next_steps {
            eprintln!("  $ {step}");
        }
        eprintln!();
        eprintln!(
            "Then push to GitHub + tag v0.1.0; the bundled .github/workflows/release.yml"
        );
        eprintln!("does the rest (vendor, pack, optional cosign sign, gh release upload).");
    }

    Ok(0)
}

fn emit_error(err: &PluginNewError, json: bool, dest: Option<PathBuf>) -> i32 {
    let kind = plugin_new_error_kind(err);
    if json {
        let report = PluginNewErrorReport {
            ok: false,
            kind,
            error: err.to_string(),
            dest,
        };
        println!(
            "{}",
            serde_json::to_string(&report).unwrap_or_default()
        );
    } else {
        eprintln!("✗ Scaffold failed: {}", err);
        match err {
            PluginNewError::InvalidId { regex, .. } => {
                eprintln!("  Hint: plugin id must match {}", regex);
            }
            PluginNewError::DestExists { .. } => {
                eprintln!("  Hint: pass --force to overwrite, or pick a different --dest.");
            }
            PluginNewError::InvalidLang { .. } => {
                eprintln!("  Hint: --lang must be one of rust, python, typescript, php.");
            }
            _ => {}
        }
    }
    1
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn id_validation_rejects_invalid_chars() {
        let bad = ["My-Plugin", "my plugin", "123_plugin", "", &"a".repeat(33), "ñ"];
        for id in bad {
            assert!(
                validate_id(id).is_err(),
                "expected `{id}` to be rejected"
            );
        }
        let good = ["a", "ab", "my_plugin", "abc_def_ghi", &"a".repeat(32)];
        for id in good {
            assert!(
                validate_id(id).is_ok(),
                "expected `{id}` to be accepted"
            );
        }
    }

    #[test]
    fn lang_validation_rejects_unknown() {
        for lang in ["go", "ruby", ""] {
            assert!(matches!(
                template_for(lang),
                Err(PluginNewError::InvalidLang { .. })
            ));
        }
        for lang in ["rust", "python", "typescript", "php"] {
            assert!(template_for(lang).is_ok());
        }
    }

    #[test]
    fn title_case_appends_lang_label() {
        assert_eq!(
            title_case_from_id("my_plugin", "typescript"),
            "My Plugin (TypeScript)"
        );
        assert_eq!(title_case_from_id("slack", "python"), "Slack (Python)");
        assert_eq!(
            title_case_from_id("a_b_c", "rust"),
            "A B C (Rust)"
        );
    }

    #[test]
    fn placeholder_list_is_longest_first() {
        let p = placeholders_for("typescript", "my_plugin", None, None);
        // First entry is the snake-case template_plugin_<lang> match —
        // longer than any of its substrings.
        assert_eq!(p[0].0, "template_plugin_typescript");
        // Title case mapping derives "My Plugin (TypeScript)".
        let title_pair = p
            .iter()
            .find(|(k, _)| k == "Template Plugin (TypeScript)")
            .expect("title pair present");
        assert_eq!(title_pair.1, "My Plugin (TypeScript)");
    }

    #[tokio::test]
    async fn scaffold_rust_creates_expected_files() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("my_plugin");
        let code = run_plugin_new(
            "my_plugin".into(),
            "rust".into(),
            Some(dest.clone()),
            None,
            None,
            false,
            false,
            true, // json
        )
        .await
        .unwrap();
        assert_eq!(code, 0);
        assert!(dest.join("Cargo.toml").is_file());
        assert!(dest.join("nexo-plugin.toml").is_file());
        assert!(dest.join("src/main.rs").is_file());
        let manifest = std::fs::read_to_string(dest.join("nexo-plugin.toml")).unwrap();
        assert!(
            manifest.contains("id = \"my_plugin\""),
            "manifest id not substituted: {manifest}"
        );
    }

    #[tokio::test]
    async fn scaffold_python_creates_expected_files() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("my_plugin_py");
        run_plugin_new(
            "my_plugin_py".into(),
            "python".into(),
            Some(dest.clone()),
            None,
            None,
            false,
            false,
            true,
        )
        .await
        .unwrap();
        assert!(dest.join("nexo-plugin.toml").is_file());
        assert!(dest.join("src/main.py").is_file());
        assert!(dest.join("requirements.txt").is_file());
        let manifest = std::fs::read_to_string(dest.join("nexo-plugin.toml")).unwrap();
        assert!(manifest.contains("id = \"my_plugin_py\""));
    }

    #[tokio::test]
    async fn scaffold_typescript_creates_expected_files() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("my_plugin_ts");
        run_plugin_new(
            "my_plugin_ts".into(),
            "typescript".into(),
            Some(dest.clone()),
            None,
            None,
            false,
            false,
            true,
        )
        .await
        .unwrap();
        assert!(dest.join("nexo-plugin.toml").is_file());
        assert!(dest.join("package.json").is_file());
        assert!(dest.join("tsconfig.json").is_file());
        assert!(dest.join("src/main.ts").is_file());
        let pkg = std::fs::read_to_string(dest.join("package.json")).unwrap();
        assert!(
            pkg.contains("\"name\": \"my_plugin_ts\""),
            "package.json name not substituted: {pkg}"
        );
    }

    #[tokio::test]
    async fn scaffold_php_creates_expected_files() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("my_plugin_php");
        run_plugin_new(
            "my_plugin_php".into(),
            "php".into(),
            Some(dest.clone()),
            None,
            None,
            false,
            false,
            true,
        )
        .await
        .unwrap();
        assert!(dest.join("nexo-plugin.toml").is_file());
        assert!(dest.join("composer.json").is_file());
        assert!(dest.join("src/main.php").is_file());
    }

    #[tokio::test]
    async fn dest_already_exists_without_force_fails() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("collision");
        std::fs::create_dir(&dest).unwrap();
        let code = run_plugin_new(
            "my_plugin".into(),
            "rust".into(),
            Some(dest.clone()),
            None,
            None,
            false,
            false, // no force
            true,
        )
        .await
        .unwrap();
        assert_eq!(code, 1);
        // Dest should still exist (we didn't touch it).
        assert!(dest.is_dir());
        // And it should still be empty (no scaffold leaked through).
        assert!(std::fs::read_dir(&dest).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn force_flag_overwrites_existing_dest() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("overwrite");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("junk.txt"), b"old").unwrap();
        let code = run_plugin_new(
            "my_plugin".into(),
            "rust".into(),
            Some(dest.clone()),
            None,
            None,
            false,
            true, // force
            true,
        )
        .await
        .unwrap();
        assert_eq!(code, 0);
        // Junk file gone.
        assert!(!dest.join("junk.txt").exists());
        // Template files present.
        assert!(dest.join("Cargo.toml").is_file());
    }

    #[tokio::test]
    async fn owner_substitution_lands_in_manifest_files() {
        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("owned_plugin");
        run_plugin_new(
            "owned_plugin".into(),
            "rust".into(),
            Some(dest.clone()),
            Some("alice".into()),
            None,
            false,
            false,
            true,
        )
        .await
        .unwrap();
        let cargo = std::fs::read_to_string(dest.join("Cargo.toml")).unwrap();
        assert!(
            cargo.contains("alice"),
            "owner not substituted in Cargo.toml: {cargo}"
        );
    }
}
