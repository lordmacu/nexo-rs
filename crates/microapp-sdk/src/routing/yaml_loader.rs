use serde::{Deserialize, Serialize};

use nexo_tool_meta::marketing::{
    AssignTarget, RoutingRule, RuleSet, TenantIdRef,
};

/// Operator-facing YAML schema. Round-trips into `RuleSet` so
/// the YAML stays editable as the canonical source while the
/// dispatcher works with the strongly-typed wire shape.
///
/// Example:
///
/// ```yaml
/// version: 1
/// default_target:
///   kind: round_robin
///   pool:
///     - { id: pedro }
///     - { id: ana }
/// rules:
///   - id: vip-personal
///     name: VIP personal
///     active: true
///     followup_profile: vip
///     conditions:
///       - { kind: person_has_tag, tag: vip }
///     assigns_to:
///       kind: vendedor
///       id:
///         id: ana
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSetYaml {
    pub version: u32,
    pub rules: Vec<RoutingRule>,
    pub default_target: AssignTarget,
}

/// Parse a YAML document into a typed `RuleSet`. Caller passes
/// the active `tenant_id` because the YAML doesn't repeat it
/// (the file path encodes it: `<state>/marketing/<tenant>/rules.yaml`).
pub fn load_rule_set_from_str(
    tenant_id: TenantIdRef,
    yaml: &str,
) -> Result<RuleSet, serde_yaml::Error> {
    let parsed: RuleSetYaml = serde_yaml::from_str(yaml)?;
    Ok(RuleSet {
        tenant_id,
        version: parsed.version,
        rules: parsed.rules,
        default_target: parsed.default_target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_tool_meta::marketing::{
        AssignTarget, RulePredicate, TenantIdRef, VendedorId,
    };

    const SAMPLE: &str = r#"
version: 1
default_target:
  kind: round_robin
  pool:
    - "pedro"
    - "ana"
rules:
  - id: vip-personal
    name: VIP personal
    active: true
    followup_profile: vip
    conditions:
      - kind: person_has_tag
        tag: vip
    assigns_to:
      kind: vendedor
      id: "ana"
"#;

    #[test]
    fn parses_sample_yaml() {
        let rs = load_rule_set_from_str(TenantIdRef("acme".into()), SAMPLE).unwrap();
        assert_eq!(rs.tenant_id, TenantIdRef("acme".into()));
        assert_eq!(rs.version, 1);
        assert_eq!(rs.rules.len(), 1);
        assert_eq!(rs.rules[0].id, "vip-personal");
        assert!(matches!(
            rs.rules[0].conditions[0],
            RulePredicate::PersonHasTag { .. }
        ));
        assert!(matches!(
            rs.default_target,
            AssignTarget::RoundRobin { ref pool } if pool == &vec![VendedorId("pedro".into()), VendedorId("ana".into())]
        ));
    }

    #[test]
    fn empty_rules_ok() {
        let yaml = r#"
version: 1
default_target:
  kind: drop
rules: []
"#;
        let rs = load_rule_set_from_str(TenantIdRef("acme".into()), yaml).unwrap();
        assert!(rs.rules.is_empty());
    }

    #[test]
    fn invalid_predicate_kind_fails() {
        let yaml = r#"
version: 1
default_target:
  kind: drop
rules:
  - id: bad
    name: bad
    active: true
    followup_profile: x
    conditions:
      - kind: nonexistent_predicate
    assigns_to:
      kind: drop
"#;
        let r = load_rule_set_from_str(TenantIdRef("acme".into()), yaml);
        assert!(r.is_err());
    }

    #[test]
    fn round_trip_yaml() {
        let rs1 = load_rule_set_from_str(TenantIdRef("acme".into()), SAMPLE).unwrap();
        let yaml_out = serde_yaml::to_string(&RuleSetYaml {
            version: rs1.version,
            rules: rs1.rules.clone(),
            default_target: rs1.default_target.clone(),
        })
        .unwrap();
        let rs2 = load_rule_set_from_str(TenantIdRef("acme".into()), &yaml_out).unwrap();
        assert_eq!(rs1.rules, rs2.rules);
        assert_eq!(rs1.default_target, rs2.default_target);
    }
}
