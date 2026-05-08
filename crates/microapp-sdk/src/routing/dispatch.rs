use thiserror::Error;

use nexo_tool_meta::marketing::{
    AssignTarget, DomainKind, RoutingRule, RulePredicate, RuleSet, TenantIdRef, SellerId,
};

/// Per-call evaluation context. Caller fills the fields they
/// have; missing fields fail predicates that need them (a
/// `CompanyIndustry` predicate against a `MatchContext` with
/// `industry: None` always evaluates false). Keeps the
/// dispatcher synchronous + testable.
#[derive(Debug, Clone, Default)]
pub struct MatchContext<'a> {
    pub sender_email: Option<&'a str>,
    pub sender_domain_kind: Option<DomainKind>,
    pub company_industry: Option<&'a str>,
    pub person_tags: &'a [&'a str],
    pub score: Option<u8>,
    pub body: Option<&'a str>,
    pub subject: Option<&'a str>,
}

/// Result of dispatching a single inbound through a rule set.
/// The `matched_rule_id` is `None` when the default target
/// fired (no rule matched). Operator audit log uses both
/// fields to render "matched rule X → assigned to seller Y".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingDecision {
    pub target: AssignTarget,
    pub matched_rule_id: Option<String>,
    pub why: Vec<String>,
}

#[derive(Debug, Error)]
pub enum RoutingError {
    /// Caller passed a `tenant_id` that doesn't match the rule
    /// set's tenant scope. Defense-in-depth — rule sets are
    /// already loaded per tenant; this catches a bug where a
    /// caller hands tenant A's set + tenant B's match context.
    #[error(
        "tenant mismatch: rule set scoped to {expected:?}, dispatcher called with {got:?}"
    )]
    TenantMismatch {
        expected: TenantIdRef,
        got: TenantIdRef,
    },
}

/// Dispatcher evaluates rules in declaration order and returns
/// the first match. No rule matches → returns the rule set's
/// `default_target`.
pub struct Dispatcher<'a> {
    rule_set: &'a RuleSet,
}

impl<'a> Dispatcher<'a> {
    pub fn new(rule_set: &'a RuleSet) -> Self {
        Self { rule_set }
    }

    /// Evaluate every rule against `ctx`. The first active rule
    /// whose conditions all match wins. `tenant_id` must match
    /// the rule set's scope or the call returns
    /// `RoutingError::TenantMismatch` (defense-in-depth).
    pub fn dispatch(
        &self,
        tenant_id: &TenantIdRef,
        ctx: &MatchContext<'_>,
    ) -> Result<RoutingDecision, RoutingError> {
        if tenant_id != &self.rule_set.tenant_id {
            return Err(RoutingError::TenantMismatch {
                expected: self.rule_set.tenant_id.clone(),
                got: tenant_id.clone(),
            });
        }
        for rule in &self.rule_set.rules {
            if !rule.active {
                continue;
            }
            if rule_matches(rule, ctx) {
                return Ok(RoutingDecision {
                    target: rule.assigns_to.clone(),
                    matched_rule_id: Some(rule.id.clone()),
                    why: rule.conditions.iter().map(predicate_label).collect(),
                });
            }
        }
        Ok(RoutingDecision {
            target: self.rule_set.default_target.clone(),
            matched_rule_id: None,
            why: vec!["default target (no rule matched)".to_string()],
        })
    }

    /// Round-robin pick from a `RoundRobin` target. Caller owns
    /// the cursor (e.g. sqlite counter per tenant); we just
    /// hand back the indexed element so the dispatcher stays
    /// pure.
    pub fn round_robin_pick(pool: &[SellerId], cursor: usize) -> Option<&SellerId> {
        if pool.is_empty() {
            return None;
        }
        Some(&pool[cursor % pool.len()])
    }
}

// ── Predicate evaluator ─────────────────────────────────────────

fn rule_matches(rule: &RoutingRule, ctx: &MatchContext<'_>) -> bool {
    rule.conditions.iter().all(|p| predicate_matches(p, ctx))
}

fn predicate_matches(p: &RulePredicate, ctx: &MatchContext<'_>) -> bool {
    match p {
        RulePredicate::SenderDomainKind { value } => ctx.sender_domain_kind == Some(*value),
        RulePredicate::SenderEmailMatches { pattern } => {
            ctx.sender_email.map_or(false, |e| glob_match(pattern, e))
        }
        RulePredicate::CompanyIndustry { value } => {
            ctx.company_industry.map_or(false, |i| i.eq_ignore_ascii_case(value))
        }
        RulePredicate::PersonHasTag { tag } => ctx.person_tags.iter().any(|t| t == tag),
        RulePredicate::ScoreGte { score } => ctx.score.map_or(false, |s| s >= *score),
        RulePredicate::BodyContains { needle } => ctx
            .body
            .map_or(false, |b| b.to_lowercase().contains(&needle.to_lowercase())),
        RulePredicate::SubjectContains { needle } => ctx
            .subject
            .map_or(false, |s| s.to_lowercase().contains(&needle.to_lowercase())),
    }
}

fn predicate_label(p: &RulePredicate) -> String {
    match p {
        RulePredicate::SenderDomainKind { value } => {
            format!("sender.domain_kind == {value:?}")
        }
        RulePredicate::SenderEmailMatches { pattern } => {
            format!("sender.email matches {pattern:?}")
        }
        RulePredicate::CompanyIndustry { value } => format!("company.industry == {value:?}"),
        RulePredicate::PersonHasTag { tag } => format!("person.tags contains {tag:?}"),
        RulePredicate::ScoreGte { score } => format!("score >= {score}"),
        RulePredicate::BodyContains { needle } => format!("body contains {needle:?}"),
        RulePredicate::SubjectContains { needle } => format!("subject contains {needle:?}"),
    }
}

/// Tiny glob matcher supporting `*` only (no `?`, no charset
/// brackets). Sufficient for sender-email patterns like
/// `*@acme.com`, `juan@*`, exact `juan@acme.com`. Case-insensitive
/// because email local-part comparisons are case-insensitive in
/// every real-world system.
fn glob_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let value = value.to_ascii_lowercase();
    let mut p_chunks = pattern.split('*').peekable();
    let mut v = value.as_str();
    let first = p_chunks.next().unwrap_or("");
    if !v.starts_with(first) {
        return false;
    }
    v = &v[first.len()..];
    while let Some(chunk) = p_chunks.next() {
        if p_chunks.peek().is_none() {
            // Last chunk — must be a suffix.
            return v.ends_with(chunk);
        }
        if chunk.is_empty() {
            continue;
        }
        match v.find(chunk) {
            Some(idx) => {
                v = &v[idx + chunk.len()..];
            }
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::marketing::{
        AssignTarget, RoutingRule, RulePredicate, RuleSet, TenantIdRef, SellerId,
    };

    fn rs(tenant: &str, rules: Vec<RoutingRule>, default: AssignTarget) -> RuleSet {
        RuleSet {
            tenant_id: TenantIdRef(tenant.into()),
            version: 1,
            rules,
            default_target: default,
        }
    }

    fn seller(id: &str) -> SellerId {
        SellerId(id.into())
    }

    fn rule(id: &str, conds: Vec<RulePredicate>, target: AssignTarget) -> RoutingRule {
        RoutingRule {
            id: id.into(),
            name: id.into(),
            conditions: conds,
            assigns_to: target,
            followup_profile: "default".into(),
            active: true,
        }
    }

    #[test]
    fn first_matching_rule_wins() {
        let r = rs(
            "acme",
            vec![
                rule(
                    "vip",
                    vec![RulePredicate::PersonHasTag { tag: "vip".into() }],
                    AssignTarget::Seller { id: seller("ana") },
                ),
                rule(
                    "default-rule",
                    vec![RulePredicate::ScoreGte { score: 50 }],
                    AssignTarget::Seller { id: seller("pedro") },
                ),
            ],
            AssignTarget::Drop,
        );
        let ctx = MatchContext {
            person_tags: &["vip"],
            score: Some(80),
            ..Default::default()
        };
        let d = Dispatcher::new(&r)
            .dispatch(&TenantIdRef("acme".into()), &ctx)
            .unwrap();
        assert_eq!(d.matched_rule_id.as_deref(), Some("vip"));
        assert!(matches!(d.target, AssignTarget::Seller { id } if id == seller("ana")));
    }

    #[test]
    fn no_match_falls_through_to_default() {
        let r = rs(
            "acme",
            vec![rule(
                "vip",
                vec![RulePredicate::PersonHasTag { tag: "vip".into() }],
                AssignTarget::Seller { id: seller("ana") },
            )],
            AssignTarget::Seller { id: seller("luis") },
        );
        let ctx = MatchContext::default();
        let d = Dispatcher::new(&r)
            .dispatch(&TenantIdRef("acme".into()), &ctx)
            .unwrap();
        assert_eq!(d.matched_rule_id, None);
        assert!(matches!(d.target, AssignTarget::Seller { id } if id == seller("luis")));
    }

    #[test]
    fn inactive_rules_skipped() {
        let mut paused = rule(
            "vip",
            vec![RulePredicate::PersonHasTag { tag: "vip".into() }],
            AssignTarget::Seller { id: seller("ana") },
        );
        paused.active = false;
        let r = rs("acme", vec![paused], AssignTarget::Drop);
        let ctx = MatchContext {
            person_tags: &["vip"],
            ..Default::default()
        };
        let d = Dispatcher::new(&r)
            .dispatch(&TenantIdRef("acme".into()), &ctx)
            .unwrap();
        // Falls through to default since the only rule is paused.
        assert_eq!(d.matched_rule_id, None);
        assert!(matches!(d.target, AssignTarget::Drop));
    }

    #[test]
    fn cross_tenant_dispatch_rejected() {
        let r = rs("acme", vec![], AssignTarget::Drop);
        let ctx = MatchContext::default();
        let err = Dispatcher::new(&r)
            .dispatch(&TenantIdRef("globex".into()), &ctx)
            .unwrap_err();
        assert!(matches!(err, RoutingError::TenantMismatch { .. }));
    }

    #[test]
    fn glob_match_full_wildcard_suffix() {
        assert!(glob_match("*@acme.com", "juan@acme.com"));
        assert!(glob_match("*@acme.com", "MARIA@ACME.COM")); // case-insensitive
        assert!(!glob_match("*@acme.com", "juan@globex.io"));
    }

    #[test]
    fn glob_match_full_wildcard_prefix() {
        assert!(glob_match("juan@*", "juan@gmail.com"));
        assert!(!glob_match("juan@*", "ana@gmail.com"));
    }

    #[test]
    fn glob_match_exact_no_star() {
        assert!(glob_match("juan@acme.com", "juan@acme.com"));
        assert!(!glob_match("juan@acme.com", "juan@acme.io"));
    }

    #[test]
    fn predicate_score_gte_evaluates() {
        let r = rs(
            "acme",
            vec![rule(
                "warm",
                vec![RulePredicate::ScoreGte { score: 70 }],
                AssignTarget::Seller { id: seller("pedro") },
            )],
            AssignTarget::Drop,
        );
        let d = Dispatcher::new(&r);

        let hit = d
            .dispatch(
                &TenantIdRef("acme".into()),
                &MatchContext {
                    score: Some(75),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(hit.matched_rule_id.as_deref(), Some("warm"));

        let miss = d
            .dispatch(
                &TenantIdRef("acme".into()),
                &MatchContext {
                    score: Some(60),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(miss.matched_rule_id, None);

        let absent = d
            .dispatch(
                &TenantIdRef("acme".into()),
                &MatchContext {
                    score: None,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(absent.matched_rule_id, None);
    }

    #[test]
    fn round_robin_pick_wraps() {
        let pool = vec![seller("a"), seller("b"), seller("c")];
        assert_eq!(Dispatcher::round_robin_pick(&pool, 0).unwrap().0, "a");
        assert_eq!(Dispatcher::round_robin_pick(&pool, 4).unwrap().0, "b");
        assert_eq!(
            Dispatcher::round_robin_pick(&[] as &[SellerId], 0),
            None
        );
    }

    #[test]
    fn body_subject_predicates_case_insensitive() {
        let r = rs(
            "acme",
            vec![rule(
                "support",
                vec![
                    RulePredicate::SubjectContains {
                        needle: "API".into(),
                    },
                    RulePredicate::BodyContains {
                        needle: "rate limit".into(),
                    },
                ],
                AssignTarget::Seller { id: seller("luis") },
            )],
            AssignTarget::Drop,
        );
        let d = Dispatcher::new(&r);
        let hit = d
            .dispatch(
                &TenantIdRef("acme".into()),
                &MatchContext {
                    subject: Some("issue with api"),
                    body: Some("hitting RATE LIMIT errors"),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(hit.matched_rule_id.as_deref(), Some("support"));
    }
}
