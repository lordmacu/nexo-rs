use serde_json::{json, Value};

use crate::client::{self, GithubError, CLIENT_VERSION};

const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 100;

pub fn tool_schemas() -> Value {
    json!([
        {
            "name": "status",
            "description": "Returns GitHub extension status, default repo, and authenticated user (if token present)",
            "input_schema": { "type": "object", "additionalProperties": false }
        },
        {
            "name": "pr_list",
            "description": "Lists pull requests for a repository",
            "input_schema": {
                "type": "object",
                "properties": {
                    "repo": { "type": "string", "description": "owner/repo; falls back to GITHUB_DEFAULT_REPO env" },
                    "state": { "type": "string", "enum": ["open", "closed", "all"], "description": "default open" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "pr_view",
            "description": "Returns details of a pull request by number",
            "input_schema": {
                "type": "object",
                "properties": {
                    "repo": { "type": "string" },
                    "number": { "type": "integer", "minimum": 1 }
                },
                "required": ["number"],
                "additionalProperties": false
            }
        },
        {
            "name": "pr_checks",
            "description": "Returns CI check runs for the head commit of a pull request",
            "input_schema": {
                "type": "object",
                "properties": {
                    "repo": { "type": "string" },
                    "number": { "type": "integer", "minimum": 1 }
                },
                "required": ["number"],
                "additionalProperties": false
            }
        },
        {
            "name": "issue_list",
            "description": "Lists issues for a repository (excludes pull requests)",
            "input_schema": {
                "type": "object",
                "properties": {
                    "repo": { "type": "string" },
                    "state": { "type": "string", "enum": ["open", "closed", "all"] },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                },
                "additionalProperties": false
            }
        }
    ])
}

#[derive(Debug)]
pub struct ToolError {
    pub code: i32,
    pub message: String,
}

impl From<GithubError> for ToolError {
    fn from(e: GithubError) -> Self {
        Self {
            code: e.rpc_code(),
            message: e.message(),
        }
    }
}

pub fn dispatch(name: &str, args: &Value) -> Result<Value, ToolError> {
    match name {
        "status" => Ok(status()),
        "pr_list" => pr_list(args),
        "pr_view" => pr_view(args),
        "pr_checks" => pr_checks(args),
        "issue_list" => issue_list(args),
        other => Err(ToolError {
            code: -32601,
            message: format!("unknown tool `{other}`"),
        }),
    }
}

fn status() -> Value {
    let token_ok = client::token_present();
    let mut payload = json!({
        "ok": true,
        "provider": "github-rest",
        "endpoint": client::base_url(),
        "client_version": CLIENT_VERSION,
        "user_agent": client::user_agent(),
        "tools": ["status", "pr_list", "pr_view", "pr_checks", "issue_list"],
        "default_repo": std::env::var("GITHUB_DEFAULT_REPO").ok(),
        "token_present": token_ok,
    });
    if token_ok {
        match client::authenticated_user() {
            Ok(u) => {
                payload["auth"] = json!({
                    "login": u.login,
                    "id": u.id,
                    "name": u.name,
                });
            }
            Err(e) => {
                payload["ok"] = json!(false);
                payload["auth_error"] = json!({
                    "code": e.rpc_code(),
                    "message": e.message(),
                });
            }
        }
    }
    payload
}

fn pr_list(args: &Value) -> Result<Value, ToolError> {
    let (owner, repo) = repo_parts(args)?;
    let state = optional_string(args, "state")?.unwrap_or_else(|| "open".into());
    validate_state(&state)?;
    let limit = optional_u32(args, "limit")?.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(ToolError {
            code: -32602,
            message: format!("`limit` must be between 1 and {MAX_LIMIT}"),
        });
    }
    let pulls = client::list_pulls(&owner, &repo, &state, limit)?;
    let summary: Vec<Value> = pulls
        .into_iter()
        .map(|pr| {
            json!({
                "number": pr.get("number"),
                "title": pr.get("title"),
                "state": pr.get("state"),
                "draft": pr.get("draft"),
                "user": pr.get("user").and_then(|u| u.get("login")),
                "head_sha": pr.get("head").and_then(|h| h.get("sha")),
                "base_ref": pr.get("base").and_then(|b| b.get("ref")),
                "html_url": pr.get("html_url"),
                "created_at": pr.get("created_at"),
                "updated_at": pr.get("updated_at"),
            })
        })
        .collect();
    Ok(json!({
        "repo": format!("{}/{}", owner, repo),
        "state": state,
        "limit": limit,
        "count": summary.len(),
        "pulls": summary,
    }))
}

fn pr_view(args: &Value) -> Result<Value, ToolError> {
    let (owner, repo) = repo_parts(args)?;
    let number = required_u64(args, "number")?;
    let pr = client::get_pull(&owner, &repo, number)?;
    Ok(pr)
}

fn pr_checks(args: &Value) -> Result<Value, ToolError> {
    let (owner, repo) = repo_parts(args)?;
    let number = required_u64(args, "number")?;
    let pr = client::get_pull(&owner, &repo, number)?;
    let sha = pr
        .get("head")
        .and_then(|h| h.get("sha"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError {
            code: -32006,
            message: "pull request response missing head.sha".into(),
        })?
        .to_string();
    let checks = client::list_check_runs(&owner, &repo, &sha)?;
    Ok(json!({
        "repo": format!("{}/{}", owner, repo),
        "number": number,
        "head_sha": sha,
        "checks": checks,
    }))
}

fn issue_list(args: &Value) -> Result<Value, ToolError> {
    let (owner, repo) = repo_parts(args)?;
    let state = optional_string(args, "state")?.unwrap_or_else(|| "open".into());
    validate_state(&state)?;
    let limit = optional_u32(args, "limit")?.unwrap_or(DEFAULT_LIMIT);
    if limit == 0 || limit > MAX_LIMIT {
        return Err(ToolError {
            code: -32602,
            message: format!("`limit` must be between 1 and {MAX_LIMIT}"),
        });
    }
    let raw = client::list_issues(&owner, &repo, &state, limit)?;
    // GitHub returns PRs in /issues; filter them out for clarity.
    let issues: Vec<Value> = raw
        .into_iter()
        .filter(|i| i.get("pull_request").is_none())
        .map(|i| {
            json!({
                "number": i.get("number"),
                "title": i.get("title"),
                "state": i.get("state"),
                "user": i.get("user").and_then(|u| u.get("login")),
                "labels": i.get("labels"),
                "html_url": i.get("html_url"),
                "comments": i.get("comments"),
                "created_at": i.get("created_at"),
                "updated_at": i.get("updated_at"),
            })
        })
        .collect();
    Ok(json!({
        "repo": format!("{}/{}", owner, repo),
        "state": state,
        "limit": limit,
        "count": issues.len(),
        "issues": issues,
    }))
}

fn repo_parts(args: &Value) -> Result<(String, String), ToolError> {
    let raw = optional_string(args, "repo")?
        .or_else(|| std::env::var("GITHUB_DEFAULT_REPO").ok())
        .ok_or_else(|| ToolError {
            code: -32602,
            message: "missing `repo` (and GITHUB_DEFAULT_REPO not set)".into(),
        })?;
    let trimmed = raw.trim();
    let mut parts = trimmed.splitn(2, '/');
    let owner = parts.next().unwrap_or("").trim();
    let repo = parts.next().unwrap_or("").trim();
    if owner.is_empty() || repo.is_empty() {
        return Err(ToolError {
            code: -32602,
            message: format!("`repo` must be in 'owner/repo' form, got `{trimmed}`"),
        });
    }
    Ok((owner.to_string(), repo.to_string()))
}

fn validate_state(state: &str) -> Result<(), ToolError> {
    match state {
        "open" | "closed" | "all" => Ok(()),
        other => Err(ToolError {
            code: -32602,
            message: format!("`state` must be open|closed|all, got `{other}`"),
        }),
    }
}

fn optional_string(args: &Value, key: &str) -> Result<Option<String>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .map(|s| Some(s.trim().to_string()))
            .ok_or_else(|| ToolError {
                code: -32602,
                message: format!("`{key}` must be a string"),
            }),
    }
}

fn optional_u32(args: &Value, key: &str) -> Result<Option<u32>, ToolError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(|n| Some(n as u32)).ok_or_else(|| ToolError {
            code: -32602,
            message: format!("`{key}` must be a positive integer"),
        }),
    }
}

fn required_u64(args: &Value, key: &str) -> Result<u64, ToolError> {
    args.get(key).and_then(|v| v.as_u64()).ok_or_else(|| ToolError {
        code: -32602,
        message: format!("missing or invalid `{key}` (expected positive integer)"),
    })
}
