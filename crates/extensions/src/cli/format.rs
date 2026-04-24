//! Plaintext table and JSON rendering for CLI output.

use std::io::Write;

use serde::Serialize;

use crate::manifest::ExtensionManifest;

use super::status::CliStatus;

pub const CLI_JSON_SCHEMA_VERSION: u32 = 1;

/// One row in `ext list` — also the JSON element shape.
#[derive(Debug, Clone, Serialize)]
pub struct ListRow {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<usize>,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ListRow {
    pub fn from_manifest(manifest: &ExtensionManifest, status: &CliStatus, path: String) -> Self {
        let transport = match manifest.transport {
            crate::manifest::Transport::Stdio { .. } => "stdio",
            crate::manifest::Transport::Nats { .. } => "nats",
            crate::manifest::Transport::Http { .. } => "http",
        };
        let error = match status {
            CliStatus::ManifestError(m) => Some(m.clone()),
            _ => None,
        };
        Self {
            id: manifest.plugin.id.clone(),
            version: Some(manifest.plugin.version.clone()),
            status: status.as_str(),
            tools: Some(manifest.capabilities.tools.len()),
            hooks: Some(manifest.capabilities.hooks.len()),
            path,
            transport: Some(transport),
            error,
        }
    }

    pub fn error_row(id: String, path: String, message: String) -> Self {
        Self {
            id,
            version: None,
            status: "error",
            tools: None,
            hooks: None,
            path,
            transport: None,
            error: Some(message),
        }
    }
}

const COL_VERSION: usize = 10;
const COL_STATUS: usize = 10;
const COL_TOOLS: usize = 7;
const ID_MIN: usize = 16;
const ID_MAX: usize = 32;

fn should_color() -> bool {
    use std::io::IsTerminal;

    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if let Ok(v) = std::env::var("CLICOLOR") {
        if v == "0" {
            return false;
        }
    }
    if let Ok(v) = std::env::var("CLICOLOR_FORCE") {
        if v != "0" {
            return true;
        }
    }
    std::io::stdout().is_terminal()
}

fn colorize_status(status: &str, padded: String, color: bool) -> String {
    if !color {
        return padded;
    }
    let code = match status {
        "enabled" => "32",  // green
        "disabled" => "33", // yellow
        "error" => "31",    // red
        _ => return padded,
    };
    format!("\x1b[{code}m{padded}\x1b[0m")
}

/// Render a plaintext table with header row.
pub fn render_list_table(rows: &[ListRow], out: &mut dyn Write) -> std::io::Result<()> {
    if rows.is_empty() {
        writeln!(out, "No extensions discovered.")?;
        return Ok(());
    }
    let color = should_color();
    let id_width = rows
        .iter()
        .map(|r| r.id.chars().count())
        .max()
        .unwrap_or(ID_MIN)
        .max(ID_MIN)
        .min(ID_MAX);

    writeln!(
        out,
        "{id:<id_width$}  {ver:<COL_VERSION$}  {status:<COL_STATUS$}  {tools:<COL_TOOLS$}  PATH",
        id = "ID",
        ver = "VERSION",
        status = "STATUS",
        tools = "TOOLS",
    )?;
    writeln!(
        out,
        "{dash:<id_width$}  {dashv:<COL_VERSION$}  {dashs:<COL_STATUS$}  {dasht:<COL_TOOLS$}  ----",
        dash = "-".repeat(id_width.min(id_width)),
        dashv = "-".repeat(COL_VERSION),
        dashs = "-".repeat(COL_STATUS),
        dasht = "-".repeat(COL_TOOLS),
    )?;

    for r in rows {
        let id = truncate(&r.id, id_width);
        let ver = r.version.as_deref().unwrap_or("-");
        let tools = match r.tools {
            Some(n) => n.to_string(),
            None => "-".into(),
        };
        let status_plain = format!("{:<COL_STATUS$}", r.status);
        let status = colorize_status(r.status, status_plain, color);
        writeln!(
            out,
            "{id:<id_width$}  {ver:<COL_VERSION$}  {status}  {tools:<COL_TOOLS$}  {path}",
            path = r.path,
        )?;
    }
    Ok(())
}

pub fn render_list_json(rows: &[ListRow], out: &mut dyn Write) -> std::io::Result<()> {
    #[derive(Debug, Serialize)]
    struct ListOut<'a> {
        schema_version: u32,
        rows: &'a [ListRow],
    }
    let body = ListOut {
        schema_version: CLI_JSON_SCHEMA_VERSION,
        rows,
    };
    let s = serde_json::to_string_pretty(&body).map_err(std::io::Error::other)?;
    writeln!(out, "{s}")
}

#[derive(Debug, Serialize)]
pub struct InfoOut<'a> {
    pub schema_version: u32,
    pub id: &'a str,
    pub version: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_agent_version: Option<&'a str>,
    pub status: &'static str,
    pub transport: &'static str,
    pub capabilities: CapabilitiesOut<'a>,
    pub path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<&'a str>,
    /// Phase 12.7 — inline MCP server declarations carried by this manifest.
    /// `None` when the section is absent or empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct CapabilitiesOut<'a> {
    pub tools: &'a [String],
    pub hooks: &'a [String],
    pub channels: &'a [String],
    pub providers: &'a [String],
}

pub fn render_info_json(info: &InfoOut<'_>, out: &mut dyn Write) -> std::io::Result<()> {
    let s = serde_json::to_string_pretty(info).map_err(std::io::Error::other)?;
    writeln!(out, "{s}")
}

pub fn render_info_plain(info: &InfoOut<'_>, out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(out, "id:          {}", info.id)?;
    writeln!(out, "version:     {}", info.version)?;
    if let Some(n) = info.name {
        writeln!(out, "name:        {n}")?;
    }
    if let Some(d) = info.description {
        writeln!(out, "description: {d}")?;
    }
    if let Some(v) = info.min_agent_version {
        writeln!(out, "min agent:   {v}")?;
    }
    writeln!(out, "status:      {}", info.status)?;
    writeln!(out, "transport:   {}", info.transport)?;
    writeln!(out, "path:        {}", info.path)?;
    writeln!(out, "tools:       {}", info.capabilities.tools.join(", "))?;
    writeln!(out, "hooks:       {}", info.capabilities.hooks.join(", "))?;
    if !info.capabilities.channels.is_empty() {
        writeln!(
            out,
            "channels:    {}",
            info.capabilities.channels.join(", ")
        )?;
    }
    if !info.capabilities.providers.is_empty() {
        writeln!(
            out,
            "providers:   {}",
            info.capabilities.providers.join(", ")
        )?;
    }
    if let Some(a) = info.author {
        writeln!(out, "author:      {a}")?;
    }
    if let Some(l) = info.license {
        writeln!(out, "license:     {l}")?;
    }
    if let Some(servers) = info.mcp_servers.as_ref() {
        writeln!(out, "mcp_servers:")?;
        if let Some(obj) = servers.as_object() {
            for (name, body) in obj {
                let transport = body
                    .get("transport")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                writeln!(out, "  {name} ({transport})")?;
                if let Some(url) = body.get("url").and_then(|v| v.as_str()) {
                    writeln!(out, "    url: {url}")?;
                }
                if let Some(cmd) = body.get("command").and_then(|v| v.as_str()) {
                    writeln!(out, "    command: {cmd}")?;
                }
            }
        }
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, version: Option<&str>, status: &'static str, tools: Option<usize>) -> ListRow {
        ListRow {
            id: id.into(),
            version: version.map(|s| s.to_string()),
            status,
            tools,
            hooks: None,
            path: "./p".into(),
            transport: Some("stdio"),
            error: None,
        }
    }

    #[test]
    fn truncate_long_id() {
        let s = truncate("abcdefghijklmnopqrstuvwxyz", 10);
        assert_eq!(s.chars().count(), 10);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn table_header_and_separator() {
        let rows = vec![row("weather", Some("0.3.1"), "enabled", Some(3))];
        let mut buf = Vec::new();
        render_list_table(&rows, &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("ID") && s.contains("VERSION") && s.contains("PATH"));
        assert!(s.contains("weather"));
        assert!(s.contains("0.3.1"));
        assert!(s.contains("enabled"));
    }

    #[test]
    fn table_empty_message() {
        let mut buf = Vec::new();
        render_list_table(&[], &mut buf).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("No extensions"));
    }

    #[test]
    fn json_schema_stable() {
        let rows = vec![row("weather", Some("0.3.1"), "enabled", Some(3))];
        let mut buf = Vec::new();
        render_list_json(&rows, &mut buf).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(v["schema_version"], CLI_JSON_SCHEMA_VERSION);
        assert_eq!(v["rows"][0]["id"], "weather");
        assert_eq!(v["rows"][0]["status"], "enabled");
        assert_eq!(v["rows"][0]["tools"], 3);
        assert_eq!(v["rows"][0]["transport"], "stdio");
    }
}
