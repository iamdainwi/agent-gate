use agentgate_core::config::{CircuitBreakerConfig, RateLimitConfig};
use agentgate_core::policy::rules::{PolicyFile, PolicyMetadata, PolicyRule, RuleAction};
use agentgate_core::policy::PolicyEngine;
use agentgate_core::protocol::jsonrpc::JsonRpcMessage;
use agentgate_core::proxy::evaluation::evaluate_tool_call;
use agentgate_core::ratelimit::{CircuitBreaker, RateLimiter};
use agentgate_core::storage::StorageWriter;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use serde_json::json;
use std::sync::Arc;

// ── fixtures ─────────────────────────────────────────────────────────────────

fn lenient_rate_limiter() -> Arc<RateLimiter> {
    Arc::new(RateLimiter::new(RateLimitConfig {
        global_max_calls_per_minute: 1_000_000,
        per_tool_max_calls_per_minute: 1_000_000,
        per_agent_max_calls_per_minute: 1_000_000,
    }))
}

fn lenient_circuit_breaker() -> Arc<CircuitBreaker> {
    Arc::new(CircuitBreaker::new(CircuitBreakerConfig {
        error_threshold: 1_000_000,
        window_seconds: 3600,
        cooldown_seconds: 1,
    }))
}

fn make_policy(n_deny_rules: usize) -> Arc<PolicyEngine> {
    let rules = (0..n_deny_rules)
        .map(|i| PolicyRule {
            id: format!("rule-{i}"),
            tool: format!("nonexistent_tool_{i}"), // none will match "read_file"
            condition: None,
            action: RuleAction::Deny,
            message: Some("blocked".to_string()),
            fields: None,
            pattern: None,
            replacement: None,
            max_calls: None,
            window_seconds: None,
        })
        .collect();

    let pf = PolicyFile {
        metadata: PolicyMetadata {
            name: "bench".to_string(),
            version: "1".to_string(),
        },
        rules,
    };
    let path = std::env::temp_dir().join(format!("agentgate-bench-{}.toml", uuid::Uuid::new_v4()));
    std::fs::write(&path, toml::to_string(&pf).unwrap()).unwrap();
    let engine = PolicyEngine::load(&path).unwrap();
    let _ = std::fs::remove_file(path);
    engine
}

// ── benches ──────────────────────────────────────────────────────────────────

fn bench_evaluate_tool_call(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = std::env::temp_dir().join(format!("agentgate-bench-{}.db", uuid::Uuid::new_v4()));
    let storage = rt.block_on(async { StorageWriter::spawn(db).unwrap() });
    let rl = lenient_rate_limiter();
    let cb = lenient_circuit_breaker();
    let req_id = Some(json!(1));
    let args = Some(json!({"path": "/tmp/bench"}));

    let mut group = c.benchmark_group("evaluate_tool_call");

    group.bench_function("no_policy", |b| {
        b.iter(|| {
            evaluate_tool_call(
                black_box(&req_id),
                black_box("read_file"),
                black_box(args.clone()),
                black_box(None),
                black_box(&rl),
                black_box(&cb),
                black_box(&storage),
                black_box("bench"),
            )
        });
    });

    for n in [1, 10, 50] {
        let policy = make_policy(n);
        group.bench_with_input(BenchmarkId::new("deny_rules_no_match", n), &n, |b, _| {
            b.iter(|| {
                evaluate_tool_call(
                    black_box(&req_id),
                    black_box("read_file"),
                    black_box(args.clone()),
                    black_box(Some(&policy)),
                    black_box(&rl),
                    black_box(&cb),
                    black_box(&storage),
                    black_box("bench"),
                )
            });
        });
    }

    group.finish();
}

fn bench_jsonrpc_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("jsonrpc_parse");

    let valid_call = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bash","arguments":{"command":"ls -la /tmp"}}}"#;
    let valid_notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
    let malformed = r#"{"jsonrpc":"2.0","id":1,"method":42,"params":}"#;
    let large_args = {
        let data = "X".repeat(1024);
        format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"read_file","arguments":{{"data":"{data}"}}}}}}"#
        )
    };

    group.bench_function("valid_call", |b| {
        b.iter(|| JsonRpcMessage::parse(black_box(valid_call)));
    });
    group.bench_function("valid_notification", |b| {
        b.iter(|| JsonRpcMessage::parse(black_box(valid_notif)));
    });
    group.bench_function("malformed_input", |b| {
        b.iter(|| JsonRpcMessage::parse(black_box(malformed)));
    });
    group.bench_function("1kb_arguments", |b| {
        b.iter(|| JsonRpcMessage::parse(black_box(&large_args)));
    });

    group.finish();
}

criterion_group!(benches, bench_evaluate_tool_call, bench_jsonrpc_parse);
criterion_main!(benches);
