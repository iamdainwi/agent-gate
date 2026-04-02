use agentgate_core::policy::rules::{PolicyFile, PolicyMetadata, PolicyRule, RuleAction};
use agentgate_core::policy::{PolicyDecision, PolicyEngine};
use proptest::prelude::*;
use serde_json::{json, Value};
use std::sync::Arc;

// ── helpers ──────────────────────────────────────────────────────────────────

fn build_engine(rules: Vec<PolicyRule>) -> Arc<PolicyEngine> {
    let pf = PolicyFile {
        metadata: PolicyMetadata {
            name: "proptest".to_string(),
            version: "1".to_string(),
        },
        rules,
    };
    let path =
        std::env::temp_dir().join(format!("agentgate-proptest-{}.toml", uuid::Uuid::new_v4()));
    std::fs::write(&path, toml::to_string(&pf).unwrap()).unwrap();
    let engine = PolicyEngine::load(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    engine
}

fn deny_rule(id: &str, tool: &str) -> PolicyRule {
    PolicyRule {
        id: id.to_string(),
        tool: tool.to_string(),
        condition: None,
        action: RuleAction::Deny,
        message: Some("blocked".to_string()),
        fields: None,
        pattern: None,
        replacement: None,
        max_calls: None,
        window_seconds: None,
    }
}

fn redact_rule(id: &str, pattern: &str, replacement: &str) -> PolicyRule {
    PolicyRule {
        id: id.to_string(),
        tool: "*".to_string(),
        condition: None,
        action: RuleAction::Redact,
        message: None,
        fields: None,
        pattern: Some(pattern.to_string()),
        replacement: Some(replacement.to_string()),
        max_calls: None,
        window_seconds: None,
    }
}

// ── property: deny rule always blocks matching tool ───────────────────────────

proptest! {
    /// For any arguments whatsoever, a deny rule for "bash" must always produce Deny.
    #[test]
    fn deny_rule_is_unconditional(
        // Arbitrary JSON-compatible key-value pairs as arguments
        keys in prop::collection::vec("[a-z]{1,8}", 0..5),
        vals in prop::collection::vec("[a-z0-9]{0,16}", 0..5),
    ) {
        let engine = build_engine(vec![deny_rule("block-bash", "bash")]);

        let args: Value = keys.into_iter()
            .zip(vals)
            .map(|(k, v)| (k, Value::String(v)))
            .collect::<serde_json::Map<_, _>>()
            .into();

        let decision = engine.evaluate("bash", Some(&args));
        prop_assert!(
            matches!(decision, PolicyDecision::Deny { .. }),
            "expected Deny, got: {decision:?}"
        );
    }

    /// A deny rule for "bash" must never affect a different tool name.
    #[test]
    fn deny_rule_does_not_affect_other_tools(
        // Tool names that are NOT "bash"
        tool in "[a-z]{1,4}(?:_[a-z]{1,4})?",
    ) {
        prop_assume!(tool != "bash");
        let engine = build_engine(vec![deny_rule("block-bash", "bash")]);
        let decision = engine.evaluate(&tool, Some(&json!({})));
        prop_assert!(
            !matches!(decision, PolicyDecision::Deny { .. }),
            "deny rule for 'bash' must not deny tool '{tool}'"
        );
    }
}

// ── property: redaction removes all matching secrets ─────────────────────────

proptest! {
    /// For any string value in a JSON object, if it contains a synthetic API key matching
    /// `sk-[A-Za-z0-9]{20}`, redact_output must not produce output containing that key.
    #[test]
    fn redact_output_removes_matching_secret(
        prefix in "[a-z]{0,16}",
        suffix in "[a-z]{0,16}",
        field_name in "[a-z]{1,8}",
    ) {
        let engine = build_engine(vec![redact_rule(
            "strip-api-key",
            r"sk-[A-Za-z0-9]{20}",
            "[REDACTED]",
        )]);

        // Embed a canonical 24-character key (sk- + 20 chars)
        let secret = format!("sk-{}", "A".repeat(20));
        let input = json!({ field_name: format!("{}{}{}", prefix, secret, suffix) });

        let output = engine.redact_output(&input);
        let output_str = output.to_string();

        prop_assert!(
            !output_str.contains(&secret),
            "secret must be redacted from output, got: {output_str}"
        );
    }

    /// Redaction must be idempotent: applying it twice yields the same result as once.
    #[test]
    fn redact_output_is_idempotent(
        field_name in "[a-z]{1,8}",
        value in "[a-zA-Z0-9 _-]{0,64}",
    ) {
        let engine = build_engine(vec![redact_rule(
            "strip-api-key",
            r"sk-[A-Za-z0-9]{20}",
            "[REDACTED]",
        )]);

        let input = json!({ field_name: value });
        let once = engine.redact_output(&input);
        let twice = engine.redact_output(&once);

        prop_assert_eq!(&once, &twice, "redact_output must be idempotent");
    }

    /// Redaction must not panic or corrupt non-matching values.
    #[test]
    fn redact_output_is_safe_on_arbitrary_strings(
        field_name in "[a-z]{1,8}",
        value in ".*",
    ) {
        let engine = build_engine(vec![redact_rule(
            "strip-api-key",
            r"sk-[A-Za-z0-9]{20}",
            "[REDACTED]",
        )]);

        let input = json!({ field_name: value });
        // Must not panic regardless of input content
        let _output = engine.redact_output(&input);
    }
}

// ── property: rule ordering (first match wins) ────────────────────────────────

proptest! {
    /// When deny precedes allow for the same tool, deny always wins regardless of arguments.
    #[test]
    fn first_deny_beats_later_allow(
        keys in prop::collection::vec("[a-z]{1,8}", 0..4),
        vals in prop::collection::vec("[a-z0-9]{0,16}", 0..4),
    ) {
        let engine = build_engine(vec![
            deny_rule("deny-first", "bash"),
            PolicyRule {
                id: "allow-second".to_string(),
                tool: "bash".to_string(),
                condition: None,
                action: RuleAction::Allow,
                message: None,
                fields: None,
                pattern: None,
                replacement: None,
                max_calls: None,
                window_seconds: None,
            },
        ]);

        let args: Value = keys.into_iter()
            .zip(vals)
            .map(|(k, v)| (k, Value::String(v)))
            .collect::<serde_json::Map<_, _>>()
            .into();

        let decision = engine.evaluate("bash", Some(&args));
        prop_assert!(
            matches!(decision, PolicyDecision::Deny { .. }),
            "deny before allow must always deny, got: {decision:?}"
        );
    }
}
