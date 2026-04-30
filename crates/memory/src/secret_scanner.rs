//! Secret scanner — detects API keys, tokens, and private keys before
//! they are committed to persistent memory storage.
//!
//! Rule set ported from `claude-code-leak/src/services/teamMemorySync/secretScanner.ts`
//! (high-specificity gitleaks rules with distinctive prefixes, near-zero
//! false-positive rate). No entropy detection — postponed to 77.7.b.
//!
//! The Anthropic API key prefix is assembled at runtime to avoid the
//! literal byte sequence in the binary.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::{Regex, RegexBuilder};
use sha2::{Digest, Sha256};

// ── Public types ────────────────────────────────────────────────────

/// A secret detected in content. Never contains the secret value itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMatch {
    /// Rule identifier, e.g. "anthropic-api-key", "github-pat".
    pub rule_id: &'static str,
    /// Human-readable label, e.g. "Anthropic API Key".
    pub label: &'static str,
    /// Byte offset where the match starts.
    pub offset: usize,
}

/// Pre-commit policy for handling detected secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnSecret {
    /// Refuse the write entirely. Returns SecretBlockedError.
    Block,
    /// Replace matched secrets with [REDACTED:rule_id] and write.
    Redact,
    /// Write intact but emit warning event + log.
    Warn,
}

/// Error returned when a secret is detected and policy is Block.
#[derive(Debug, Clone)]
pub struct SecretBlockedError {
    /// Human-readable labels of detected secrets (comma-joined).
    pub labels: String,
    /// Rule IDs that matched (for telemetry).
    pub rule_ids: Vec<&'static str>,
    /// SHA-256 of the content that was blocked (for audit correlation).
    pub content_hash: String,
}

impl std::fmt::Display for SecretBlockedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "content contains potential secrets ({labels}) and cannot be written to memory",
            labels = self.labels,
        )
    }
}

impl std::error::Error for SecretBlockedError {}

// ── Compiled rule ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct CompiledRule {
    rule_id: &'static str,
    label: &'static str,
    regex: Regex,
}

// ── Anthropic prefix (assembled at runtime) ─────────────────────────

fn anthro_prefix() -> String {
    ["sk", "ant", "api"].join("-")
}

// ── Rule definitions ────────────────────────────────────────────────

/// All 36 compiled rules, lazily initialized once.
static RULES: LazyLock<Vec<CompiledRule>> = LazyLock::new(|| {
    let ant_pfx = anthro_prefix();

    // Anthropic API key source is built at runtime — the literal
    // "sk-ant-api" does NOT appear in the binary.
    let anthropic_source = format!(
        r"\b({}-03-[a-zA-Z0-9_\-]{{93}}AA)(?:[\x60'\x22\s;]|\\[nr]|$)",
        ant_pfx
    );

    // Each entry: (rule_id, label, regex_source)
    // Uses Vec<String> for sources so the anthropic format! result
    // is owned rather than a dangling reference.
    let rule_defs: Vec<(&str, &str, String)> = vec![
        // ── Cloud providers ──
        (
            "aws-access-token",
            "AWS Access Token",
            r"\b((?:A3T[A-Z0-9]|AKIA|ASIA|ABIA|ACCA)[A-Z2-7]{16})\b".to_string(),
        ),
        (
            "gcp-api-key",
            "GCP API Key",
            r"\b(AIza[\w-]{35})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        (
            "azure-ad-client-secret",
            "Azure AD Client Secret",
            r"(?:^|[\\'\x22\x60\s>=:(,])([a-zA-Z0-9_~.]{3}\dQ~[a-zA-Z0-9_~.-]{31,34})(?:$|[\\'\x22\x60\s<),])".to_string(),
        ),
        (
            "digitalocean-pat",
            "DigitalOcean PAT",
            r"\b(dop_v1_[a-f0-9]{64})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        (
            "digitalocean-access-token",
            "DigitalOcean Access Token",
            r"\b(doo_v1_[a-f0-9]{64})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        // ── AI APIs ──
        (
            "anthropic-api-key",
            "Anthropic API Key",
            anthropic_source,
        ),
        (
            "anthropic-admin-api-key",
            "Anthropic Admin API Key",
            r"\b(sk-ant-admin01-[a-zA-Z0-9_\-]{93}AA)(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        (
            "openai-api-key",
            "OpenAI API Key",
            r"\b(sk-(?:proj|svcacct|admin)-(?:[A-Za-z0-9_-]{74}|[A-Za-z0-9_-]{58})T3BlbkFJ(?:[A-Za-z0-9_-]{74}|[A-Za-z0-9_-]{58})\b|sk-[a-zA-Z0-9]{20}T3BlbkFJ[a-zA-Z0-9]{20})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        (
            "huggingface-access-token",
            "HuggingFace Access Token",
            r"\b(hf_[a-zA-Z]{34})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        // ── Version control ──
        (
            "github-pat",
            "GitHub PAT",
            r"ghp_[0-9a-zA-Z]{36}".to_string(),
        ),
        (
            "github-fine-grained-pat",
            "GitHub Fine-Grained PAT",
            r"github_pat_\w{82}".to_string(),
        ),
        (
            "github-app-token",
            "GitHub App Token",
            r"(?:ghu|ghs)_[0-9a-zA-Z]{36}".to_string(),
        ),
        (
            "github-oauth",
            "GitHub OAuth Token",
            r"gho_[0-9a-zA-Z]{36}".to_string(),
        ),
        (
            "github-refresh-token",
            "GitHub Refresh Token",
            r"ghr_[0-9a-zA-Z]{36}".to_string(),
        ),
        (
            "gitlab-pat",
            "GitLab PAT",
            r"glpat-[\w-]{20}".to_string(),
        ),
        (
            "gitlab-deploy-token",
            "GitLab Deploy Token",
            r"gldt-[0-9a-zA-Z_\-]{20}".to_string(),
        ),
        // ── Communication ──
        (
            "slack-bot-token",
            "Slack Bot Token",
            r"xoxb-[0-9]{10,13}-[0-9]{10,13}[a-zA-Z0-9-]*".to_string(),
        ),
        (
            "slack-user-token",
            "Slack User Token",
            r"xox[pe](?:-[0-9]{10,13}){3}-[a-zA-Z0-9-]{28,34}".to_string(),
        ),
        (
            "slack-app-token",
            "Slack App Token",
            r"(?i)xapp-\d-[A-Z0-9]+-\d+-[a-z0-9]+".to_string(),
        ),
        (
            "twilio-api-key",
            "Twilio API Key",
            r"SK[0-9a-fA-F]{32}".to_string(),
        ),
        (
            "sendgrid-api-token",
            "SendGrid API Token",
            r"\b(SG\.[a-zA-Z0-9=_\-.]{66})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        // ── Dev tooling ──
        (
            "npm-access-token",
            "NPM Access Token",
            r"\b(npm_[a-zA-Z0-9]{36})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        (
            "pypi-upload-token",
            "PyPI Upload Token",
            r"pypi-AgEIcHlwaS5vcmc[\w-]{50,1000}".to_string(),
        ),
        (
            "databricks-api-token",
            "Databricks API Token",
            r"\b(dapi[a-f0-9]{32}(?:-\d)?)(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        (
            "hashicorp-tf-api-token",
            "HashiCorp TF API Token",
            r"[a-zA-Z0-9]{14}\.atlasv1\.[a-zA-Z0-9\-_=]{60,70}".to_string(),
        ),
        (
            "pulumi-api-token",
            "Pulumi API Token",
            r"\b(pul-[a-f0-9]{40})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        (
            "postman-api-token",
            "Postman API Token",
            r"\b(PMAK-[a-fA-F0-9]{24}-[a-fA-F0-9]{34})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        // ── Observability ──
        (
            "grafana-api-key",
            "Grafana API Key",
            r"\b(eyJrIjoi[A-Za-z0-9+/]{70,400}={0,3})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        (
            "grafana-cloud-api-token",
            "Grafana Cloud API Token",
            r"\b(glc_[A-Za-z0-9+/]{32,400}={0,3})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        (
            "grafana-service-account-token",
            "Grafana Service Account Token",
            r"\b(glsa_[A-Za-z0-9]{32}_[A-Fa-f0-9]{8})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        (
            "sentry-user-token",
            "Sentry User Token",
            r"\b(sntryu_[a-f0-9]{64})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        (
            "sentry-org-token",
            "Sentry Org Token",
            r"\bsntrys_eyJpYXQiO[a-zA-Z0-9+/]{10,200}(?:LCJyZWdpb25fdXJs|InJlZ2lvbl91cmwi|cmVnaW9uX3VybCI6)[a-zA-Z0-9+/]{10,200}={0,2}_[a-zA-Z0-9+/]{43}".to_string(),
        ),
        // ── Payments ──
        (
            "stripe-access-token",
            "Stripe Access Token",
            r"\b((?:sk|rk)_(?:test|live|prod)_[a-zA-Z0-9]{10,99})(?:[\x60'\x22\s;]|\\[nr]|$)".to_string(),
        ),
        (
            "shopify-access-token",
            "Shopify Access Token",
            r"shpat_[a-fA-F0-9]{32}".to_string(),
        ),
        (
            "shopify-shared-secret",
            "Shopify Shared Secret",
            r"shpss_[a-fA-F0-9]{32}".to_string(),
        ),
        // ── Crypto ──
        (
            "private-key",
            "Private Key",
            r"(?i)-----BEGIN[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----[\s\S]{64,}?-----END[ A-Z0-9_-]{0,100}PRIVATE KEY(?: BLOCK)?-----".to_string(),
        ),
    ];

    rule_defs
        .into_iter()
        .map(|(id, label, source)| {
            let regex = RegexBuilder::new(&source)
                .size_limit(50 * 1024 * 1024) // 50 MB limit for large patterns
                .build()
                .unwrap_or_else(|e| panic!("invalid secret scanner regex for {id}: {e}"));
            CompiledRule {
                rule_id: id,
                label,
                regex,
            }
        })
        .collect()
});

// ── SecretScanner ────────────────────────────────────────────────────

/// The scanner. Holds compiled regexes. Created once, reused.
#[derive(Debug, Clone)]
pub struct SecretScanner {
    rules: Vec<&'static CompiledRule>,
}

impl Default for SecretScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretScanner {
    /// Create with all default rules (36 rules).
    pub fn new() -> Self {
        Self {
            rules: RULES.iter().collect(),
        }
    }

    /// Create with only the named rules. Unknown names are silently ignored.
    pub fn with_rules(rule_ids: &[&str]) -> Self {
        let id_set: HashSet<&str> = rule_ids.iter().copied().collect();
        Self {
            rules: RULES
                .iter()
                .filter(|r| id_set.contains(r.rule_id))
                .collect(),
        }
    }

    /// Exclude named rules from the default set.
    pub fn without_rules(exclude_ids: &[&str]) -> Self {
        let exclude: HashSet<&str> = exclude_ids.iter().copied().collect();
        Self {
            rules: RULES
                .iter()
                .filter(|r| !exclude.contains(r.rule_id))
                .collect(),
        }
    }

    /// Number of active rules in this scanner.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Scan content and return all matches, deduplicated by rule_id,
    /// ordered by offset.
    pub fn scan(&self, content: &str) -> Vec<SecretMatch> {
        let mut matches: Vec<SecretMatch> = Vec::new();
        let mut seen: HashSet<&'static str> = HashSet::new();

        for rule in &self.rules {
            if seen.contains(rule.rule_id) {
                continue;
            }
            if let Some(m) = rule.regex.find(content) {
                seen.insert(rule.rule_id);
                matches.push(SecretMatch {
                    rule_id: rule.rule_id,
                    label: rule.label,
                    offset: m.start(),
                });
            }
        }

        // Sort by offset
        matches.sort_by_key(|m| m.offset);
        matches
    }

    /// Fast check — returns true on first match without collecting all.
    pub fn has_secrets(&self, content: &str) -> bool {
        for rule in &self.rules {
            if rule.regex.is_match(content) {
                return true;
            }
        }
        false
    }
}

// ── SecretGuard ──────────────────────────────────────────────────────

/// The guard. Wraps a scanner with a policy decision.
#[derive(Debug, Clone)]
pub struct SecretGuard {
    scanner: SecretScanner,
    on_secret: OnSecret,
    enabled: bool,
}

impl SecretGuard {
    /// Create a new guard with the given scanner and policy.
    pub fn new(scanner: SecretScanner, on_secret: OnSecret) -> Self {
        Self {
            scanner,
            on_secret,
            enabled: true,
        }
    }

    /// Create a disabled guard that always passes content through.
    pub fn disabled() -> Self {
        Self {
            scanner: SecretScanner::new(),
            on_secret: OnSecret::Block,
            enabled: false,
        }
    }

    /// Set whether the guard is enabled.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Check content. Returns:
    /// - Ok(content) if no secrets or policy is Warn (content unchanged)
    /// - Ok(redacted) if policy is Redact and secrets were found
    /// - Err(SecretBlockedError) if policy is Block and secrets were found
    pub fn check(&self, content: &str) -> Result<String, SecretBlockedError> {
        if !self.enabled {
            return Ok(content.to_string());
        }

        let matches = self.scanner.scan(content);
        if matches.is_empty() {
            return Ok(content.to_string());
        }

        let labels: Vec<&str> = matches.iter().map(|m| m.label).collect();
        let rule_ids: Vec<&'static str> = matches.iter().map(|m| m.rule_id).collect();
        let content_hash = hex::encode(Sha256::digest(content.as_bytes()));

        match self.on_secret {
            OnSecret::Block => Err(SecretBlockedError {
                labels: labels.join(", "),
                rule_ids,
                content_hash,
            }),
            OnSecret::Redact => {
                let mut redacted = content.to_string();
                // Replace from end to start so offsets stay valid.
                for m in matches.iter().rev() {
                    let rule = self
                        .scanner
                        .rules
                        .iter()
                        .find(|r| r.rule_id == m.rule_id)
                        .expect("matched rule must exist in scanner");
                    if let Some(hit) = rule.regex.find(&redacted) {
                        let replacement = format!("[REDACTED:{}]", m.rule_id);
                        redacted.replace_range(hit.range(), &replacement);
                    }
                }
                Ok(redacted)
            }
            OnSecret::Warn => Ok(content.to_string()),
        }
    }

    /// Return the current policy.
    pub fn on_secret(&self) -> OnSecret {
        self.on_secret
    }

    /// Return whether the guard is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Short-circuit check — returns true if content contains any secret.
    pub fn has_secrets(&self, content: &str) -> bool {
        if !self.enabled {
            return false;
        }
        self.scanner.has_secrets(content)
    }

    /// Scan content and return matches for logging/display.
    /// Returns empty vec when disabled.
    pub fn scan_for_display(&self, content: &str) -> Vec<SecretMatch> {
        if !self.enabled {
            return Vec::new();
        }
        self.scanner.scan(content)
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a test token at runtime to avoid GitHub push protection
    /// false positives on fake keys. Each part is joined without separator.
    fn tk(parts: &[&str]) -> String {
        parts.concat()
    }

    fn has_rule(matches: &[SecretMatch], rule_id: &str) -> bool {
        matches.iter().any(|m| m.rule_id == rule_id)
    }

    // ── Scanner: positive tests (one per family) ──

    #[test]
    fn detects_aws_access_key() {
        let scanner = SecretScanner::new();
        let m = scanner.scan("AKIAIOSFODNN7EXAMPLE");
        assert!(has_rule(&m, "aws-access-token"), "got: {m:?}");
    }

    #[test]
    fn detects_gcp_api_key() {
        let scanner = SecretScanner::new();
        let m = scanner.scan("AIzaSyDJBh5B4HGfBj8dG4hbCq1kh3GJ7nG8lMg");
        assert!(has_rule(&m, "gcp-api-key"), "got: {m:?}");
    }

    #[test]
    fn detects_github_pat() {
        let scanner = SecretScanner::new();
        let m = scanner.scan(&tk(&["ghp", "_abcdefghijklmnopqrstuvwxyz1234567890"]));
        assert!(has_rule(&m, "github-pat"), "got: {m:?}");
    }

    #[test]
    fn detects_github_fine_grained_pat() {
        let scanner = SecretScanner::new();
        // github_pat_ + exactly 82 word chars
        let suffix: String = std::iter::repeat('X').take(82).collect();
        let key = format!("github_pat_{suffix}");
        let m = scanner.scan(&key);
        assert!(
            has_rule(&m, "github-fine-grained-pat"),
            "got: {m:?}"
        );
    }

    #[test]
    fn detects_openai_api_key() {
        let scanner = SecretScanner::new();
        // sk-proj-<74 chars>T3BlbkFJ<74 chars>
        let part: String = std::iter::repeat('A').take(74).collect();
        let key = format!("sk-proj-{part}T3BlbkFJ{part}");
        let m = scanner.scan(&key);
        assert!(has_rule(&m, "openai-api-key"), "got: {m:?}");
    }

    #[test]
    fn detects_anthropic_api_key() {
        let scanner = SecretScanner::new();
        let ant = ["sk", "ant", "api"].join("-");
        let key = format!(
            "{}-03-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaAA",
            ant
        );
        let m = scanner.scan(&key);
        assert!(has_rule(&m, "anthropic-api-key"), "got: {m:?}");
    }

    #[test]
    fn detects_anthropic_admin_api_key() {
        let scanner = SecretScanner::new();
        let key = "sk-ant-admin01-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaAA";
        let m = scanner.scan(key);
        assert!(
            has_rule(&m, "anthropic-admin-api-key"),
            "got: {m:?}"
        );
    }

    #[test]
    fn detects_stripe_live_key() {
        let scanner = SecretScanner::new();
        let m = scanner.scan(&tk(&["sk_live", "_abcdefghijklmnopqrstuvwxyz123456"]));
        assert!(has_rule(&m, "stripe-access-token"), "got: {m:?}");
    }

    #[test]
    fn detects_slack_bot_token() {
        let scanner = SecretScanner::new();
        let m = scanner.scan(&tk(&["xoxb", "-1234567890-1234567890-aAbBcCdDeEfFgGhH"]));
        assert!(has_rule(&m, "slack-bot-token"), "got: {m:?}");
    }

    #[test]
    fn detects_npm_access_token() {
        let scanner = SecretScanner::new();
        let m = scanner.scan(&tk(&["npm", "_abcdefghijklmnopqrstuvwxyz1234567890"]));
        assert!(has_rule(&m, "npm-access-token"), "got: {m:?}");
    }

    #[test]
    fn detects_gitlab_pat() {
        let scanner = SecretScanner::new();
        let m = scanner.scan(&tk(&["glpat", "-abcdefghijklmnopqrst"]));
        assert!(has_rule(&m, "gitlab-pat"), "got: {m:?}");
    }

    #[test]
    fn detects_grafana_api_key() {
        let scanner = SecretScanner::new();
        let key = tk(&["eyJ", "rIjoiQUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWZnaGlqa2xtbm9wcXJzdHV2d3h5ekFCQ0RFRkdISUpLTE1OT1BRUlNUVVZXWFla"]);
        let m = scanner.scan(&key);
        assert!(has_rule(&m, "grafana-api-key"), "got: {m:?}");
    }

    #[test]
    fn detects_private_key_pem() {
        let scanner = SecretScanner::new();
        let key = tk(&["-----BEGIN ", "PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7VJTUt9Us8cKj\n-----END PRIVATE KEY-----"]);
        let m = scanner.scan(&key);
        assert!(has_rule(&m, "private-key"), "got: {m:?}");
    }

    #[test]
    fn detects_twilio_api_key() {
        let scanner = SecretScanner::new();
        let m = scanner.scan(&tk(&["SK", "1234567890abcdef1234567890abcdef"]));
        assert!(has_rule(&m, "twilio-api-key"), "got: {m:?}");
    }

    #[test]
    fn detects_huggingface_token() {
        let scanner = SecretScanner::new();
        let m = scanner.scan(&tk(&["hf", "_abcdefghijklmnopqrstuvwxyzABCDEFGH"]));
        assert!(has_rule(&m, "huggingface-access-token"), "got: {m:?}");
    }

    #[test]
    fn detects_digitalocean_pat() {
        let scanner = SecretScanner::new();
        let key = tk(&["dop_v1", "_abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"]);
        let m = scanner.scan(&key);
        assert!(has_rule(&m, "digitalocean-pat"), "got: {m:?}");
    }

    #[test]
    fn detects_pypi_upload_token() {
        let scanner = SecretScanner::new();
        let key = "pypi-AgEIcHlwaS5vcmcCJGNhcmVlcjIKAjQyNzA3Mjg3LWZhN2ItNGZjZC04ZGI0LWNhOTIwNjNmNDA3NAACJFsicmVwb3NpdG9yeS1hZG1pbiJdAAIxbCJ7ImRlbGl2ZXJ5IjoiZ3JhbnQiLCJyZXBvc2l0b3J5IjoiZ3JhbnQifQA=";
        let m = scanner.scan(key);
        assert!(has_rule(&m, "pypi-upload-token"), "got: {m:?}");
    }

    #[test]
    fn detects_sentry_user_token() {
        let scanner = SecretScanner::new();
        let key = tk(&["sntryu", "_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"]);
        let m = scanner.scan(&key);
        assert!(has_rule(&m, "sentry-user-token"), "got: {m:?}");
    }

    // ── Scanner: has_secrets fast path ──

    #[test]
    fn has_secrets_detects() {
        let scanner = SecretScanner::new();
        assert!(scanner.has_secrets(&tk(&["ghp", "_abcdefghijklmnopqrstuvwxyz1234567890"])));
    }

    #[test]
    fn has_secrets_clean_content() {
        let scanner = SecretScanner::new();
        assert!(!scanner.has_secrets("The quick brown fox jumps over the lazy dog."));
    }

    // ── Scanner: filtering ──

    #[test]
    fn with_rules_only_includes_named() {
        let scanner = SecretScanner::with_rules(&["github-pat", "aws-access-token"]);
        assert_eq!(scanner.rule_count(), 2);
        let m = scanner.scan(&tk(&["ghp", "_abcdefghijklmnopqrstuvwxyz1234567890"]));
        assert!(has_rule(&m, "github-pat"));
    }

    #[test]
    fn without_rules_excludes_named() {
        let scanner = SecretScanner::without_rules(&["github-pat"]);
        let m = scanner.scan(&tk(&["ghp", "_abcdefghijklmnopqrstuvwxyz1234567890"]));
        assert!(!has_rule(&m, "github-pat"));
        // But still detects other families
        let m2 = scanner.scan("AKIAIOSFODNN7EXAMPLE");
        assert!(has_rule(&m2, "aws-access-token"));
    }

    // ── Scanner: dedup + ordering ──

    #[test]
    fn dedup_by_rule_id() {
        let scanner = SecretScanner::new();
        let content = &(tk(&["ghp", "_abcdefghijklmnopqrstuvwxyz1234567890"]) + " ghp_999999999999999999999999999999999999");
        let m = scanner.scan(content);
        let github_count = m.iter().filter(|h| h.rule_id == "github-pat").count();
        assert_eq!(
            github_count, 1,
            "expected 1 github-pat, got {github_count}: {m:?}"
        );
    }

    #[test]
    fn multiple_rule_families() {
        let scanner = SecretScanner::new();
        let content = "ghp_abcdefghijklmnopqrstuvwxyz1234567890 AKIAIOSFODNN7EXAMPLE";
        let m = scanner.scan(content);
        assert!(has_rule(&m, "github-pat"));
        assert!(has_rule(&m, "aws-access-token"));
    }

    #[test]
    fn matches_ordered_by_offset() {
        let scanner = SecretScanner::new();
        // AWS at offset ~0, GitHub at offset ~45
        let content = "AKIAIOSFODNN7EXAMPLE some text ghp_abcdefghijklmnopqrstuvwxyz1234567890";
        let m = scanner.scan(content);
        assert!(m[0].offset <= m[1].offset, "expected ordered by offset, got: {m:?}");
    }

    // ── Scanner: false positive tests ──

    #[test]
    fn no_false_positive_english_text() {
        let scanner = SecretScanner::new();
        let m = scanner.scan(
            "The quick brown fox jumps over the lazy dog. This is a normal sentence.",
        );
        assert!(m.is_empty(), "expected no matches, got: {m:?}");
    }

    #[test]
    fn no_false_positive_uuid() {
        let scanner = SecretScanner::new();
        let m = scanner.scan("550e8400-e29b-41d4-a716-446655440000");
        assert!(m.is_empty(), "expected no matches, got: {m:?}");
    }

    #[test]
    fn no_false_positive_sha256_hex() {
        let scanner = SecretScanner::new();
        let m = scanner.scan("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert!(m.is_empty(), "expected no matches, got: {m:?}");
    }

    #[test]
    fn empty_content() {
        let scanner = SecretScanner::new();
        let m = scanner.scan("");
        assert!(m.is_empty());
    }

    // ── Guard: Block ──

    #[test]
    fn guard_block_rejects_secret() {
        let guard = SecretGuard::new(SecretScanner::new(), OnSecret::Block);
        let result = guard.check(&tk(&["ghp", "_abcdefghijklmnopqrstuvwxyz1234567890"]));
        assert!(result.is_err(), "expected block, got: {result:?}");
        let err = result.unwrap_err();
        assert!(err.labels.contains("GitHub PAT"), "got labels: {}", err.labels);
        assert!(
            err.rule_ids.contains(&"github-pat"),
            "got rule_ids: {:?}",
            err.rule_ids
        );
        assert_eq!(err.content_hash.len(), 64); // SHA-256 hex = 64 chars
    }

    #[test]
    fn guard_block_passes_clean_content() {
        let guard = SecretGuard::new(SecretScanner::new(), OnSecret::Block);
        let result = guard.check("Hello, world!");
        assert!(result.is_ok(), "expected pass, got: {result:?}");
        assert_eq!(result.unwrap(), "Hello, world!");
    }

    // ── Guard: Redact ──

    #[test]
    fn guard_redact_replaces_secret() {
        let guard = SecretGuard::new(SecretScanner::new(), OnSecret::Redact);
        let result = guard.check("My token is ghp_abcdefghijklmnopqrstuvwxyz1234567890 in code");
        assert!(result.is_ok(), "expected ok, got: {result:?}");
        let redacted = result.unwrap();
        assert!(
            redacted.contains("[REDACTED:github-pat]"),
            "expected redacted marker, got: {redacted}"
        );
        assert!(
            !redacted.contains(&tk(&["ghp", "_abcdefghijklmnopqrstuvwxyz1234567890"])),
            "secret should be gone, got: {redacted}"
        );
        assert!(
            redacted.contains("My token is"),
            "surrounding text should survive"
        );
        assert!(
            redacted.contains("in code"),
            "surrounding text should survive"
        );
    }

    #[test]
    fn guard_redact_clean_content_unchanged() {
        let guard = SecretGuard::new(SecretScanner::new(), OnSecret::Redact);
        let result = guard.check("Hello, world!");
        assert_eq!(result.unwrap(), "Hello, world!");
    }

    // ── Guard: Warn ──

    #[test]
    fn guard_warn_passes_content_unchanged() {
        let guard = SecretGuard::new(SecretScanner::new(), OnSecret::Warn);
        let content = tk(&["ghp", "_abcdefghijklmnopqrstuvwxyz1234567890"]);
        let result = guard.check(&content);
        assert!(result.is_ok(), "expected pass, got: {result:?}");
        assert_eq!(result.unwrap(), content); // unchanged
    }

    // ── Guard: Disabled ──

    #[test]
    fn guard_disabled_passes_all() {
        let guard = SecretGuard::disabled();
        let result = guard.check(&tk(&["ghp", "_abcdefghijklmnopqrstuvwxyz1234567890"]));
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            tk(&["ghp", "_abcdefghijklmnopqrstuvwxyz1234567890"])
        );
    }

    #[test]
    fn guard_with_enabled_false_passes_all() {
        let guard =
            SecretGuard::new(SecretScanner::new(), OnSecret::Block).with_enabled(false);
        let result = guard.check(&tk(&["ghp", "_abcdefghijklmnopqrstuvwxyz1234567890"]));
        assert!(result.is_ok());
    }

    // ── Guard: Multi-secret error ──

    #[test]
    fn guard_block_multi_secret_lists_all_labels() {
        let guard = SecretGuard::new(SecretScanner::new(), OnSecret::Block);
        let content = "ghp_abcdefghijklmnopqrstuvwxyz1234567890 AKIAIOSFODNN7EXAMPLE";
        let err = guard.check(content).unwrap_err();
        assert!(err.labels.contains("GitHub PAT"));
        assert!(err.labels.contains("AWS Access Token"));
    }

    // ── SecretBlockedError Display ──

    #[test]
    fn secret_blocked_error_display() {
        let err = SecretBlockedError {
            labels: "GitHub PAT, AWS Access Token".to_string(),
            rule_ids: vec!["github-pat", "aws-access-token"],
            content_hash: "a".repeat(64),
        };
        let msg = err.to_string();
        assert!(msg.contains("GitHub PAT, AWS Access Token"));
        assert!(msg.contains("cannot be written to memory"));
    }

    // ── Content hash stability ──

    #[test]
    fn content_hash_is_stable() {
        let guard = SecretGuard::new(SecretScanner::new(), OnSecret::Block);
        let content = &tk(&["ghp", "_abcdefghijklmnopqrstuvwxyz1234567890"]);
        let err1 = guard.check(content).unwrap_err();
        let err2 = guard.check(content).unwrap_err();
        assert_eq!(err1.content_hash, err2.content_hash);
    }

    // ── Edge cases ──

    #[test]
    fn scan_empty_string() {
        let scanner = SecretScanner::new();
        assert!(scanner.scan("").is_empty());
    }

    #[test]
    fn scan_unicode_without_false_positive() {
        let scanner = SecretScanner::new();
        let m = scanner.scan("café résumé naïve 日本語");
        assert!(m.is_empty(), "expected no matches, got: {m:?}");
    }
}
