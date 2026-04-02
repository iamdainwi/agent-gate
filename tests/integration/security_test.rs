use agentgate_core::config::{CircuitBreakerConfig, RateLimitConfig, ServerEntry, TransportKind};
use agentgate_core::policy::engine::PolicyEngine;
use agentgate_core::policy::rules::{PolicyFile, PolicyMetadata, PolicyRule, RuleAction};
use agentgate_core::proxy::http::HttpProxy;
use agentgate_core::ratelimit::{CircuitBreaker, RateLimiter};
use agentgate_core::storage::StorageWriter;
use axum::{routing::any, Router};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::net::TcpListener;

// ── helpers ──────────────────────────────────────────────────────────────────

async fn free_listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").await.unwrap()
}

fn make_components() -> (Arc<RateLimiter>, Arc<CircuitBreaker>, StorageWriter) {
    let rl = Arc::new(RateLimiter::new(RateLimitConfig {
        global_max_calls_per_minute: 100_000,
        per_tool_max_calls_per_minute: 100_000,
        per_agent_max_calls_per_minute: 100_000,
    }));
    let cb = Arc::new(CircuitBreaker::new(CircuitBreakerConfig {
        error_threshold: 1000,
        window_seconds: 30,
        cooldown_seconds: 60,
    }));
    let storage = StorageWriter::spawn(
        std::env::temp_dir().join(format!("agentgate-sec-{}.db", uuid::Uuid::new_v4())),
    )
    .unwrap();
    (rl, cb, storage)
}

fn make_entry(upstream: std::net::SocketAddr, bind_port: u16) -> ServerEntry {
    ServerEntry {
        name: "sec-test".to_string(),
        transport: TransportKind::Http,
        command: None,
        args: vec![],
        url: Some(format!("http://{upstream}")),
        headers: Default::default(),
        bind_port: Some(bind_port),
    }
}

fn make_policy(rules: Vec<PolicyRule>) -> Arc<PolicyEngine> {
    let pf = PolicyFile {
        metadata: PolicyMetadata {
            name: "test".to_string(),
            version: "1".to_string(),
        },
        rules,
    };
    let path = std::env::temp_dir().join(format!("agentgate-sec-{}.toml", uuid::Uuid::new_v4()));
    std::fs::write(&path, toml::to_string(&pf).unwrap()).unwrap();
    let engine = PolicyEngine::load(&path).unwrap();
    let _ = std::fs::remove_file(path);
    engine
}

async fn echo_handler(body: axum::body::Bytes) -> impl axum::response::IntoResponse {
    (
        axum::http::StatusCode::OK,
        [("content-type", "application/json")],
        body,
    )
}

fn spawn_upstream(listener: TcpListener, router: Router) {
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
}

async fn wait_for_ready(addr: std::net::SocketAddr) {
    for attempt in 0u32..20 {
        if reqwest::get(format!("http://{addr}/health"))
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return;
        }
        let delay = std::time::Duration::from_millis(5 * (1u64 << attempt.min(6)));
        tokio::time::sleep(delay).await;
    }
    panic!("proxy at {addr} never became ready");
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// JSON parsing normalizes unicode escapes before policy evaluation. A command encoded
/// as `\u0072\u006d` ("rm") must not evade a deny rule that matches the literal "rm".
#[tokio::test]
async fn deny_rule_catches_unicode_escaped_argument() {
    let up = free_listener().await;
    let up_addr = up.local_addr().unwrap();
    spawn_upstream(up, Router::new().fallback(any(echo_handler)));

    let policy = make_policy(vec![PolicyRule {
        id: "no-rm".to_string(),
        tool: "shell".to_string(),
        condition: Some("arguments.cmd matches '(rm)'".to_string()),
        action: RuleAction::Deny,
        message: Some("rm is not allowed".to_string()),
        fields: None,
        pattern: None,
        replacement: None,
        max_calls: None,
        window_seconds: None,
    }]);

    let pl = free_listener().await;
    let pl_addr = pl.local_addr().unwrap();
    let (rl, cb, storage) = make_components();
    let proxy = HttpProxy::new(
        &make_entry(up_addr, pl_addr.port()),
        Some(policy),
        rl,
        cb,
        storage,
    )
    .unwrap();
    tokio::spawn(proxy.run_with_listener(pl));
    wait_for_ready(pl_addr).await;

    // \u0072\u006d is "rm" — JSON parsing normalises this before policy evaluation.
    let raw_body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"shell","arguments":{"cmd":"\u0072\u006d -rf /"}}}"#;
    let resp = reqwest::Client::new()
        .post(format!("http://{pl_addr}"))
        .header("content-type", "application/json")
        .body(raw_body)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["error"].is_object(),
        "Unicode-escaped 'rm' must be caught by deny rule, got: {body}"
    );
    assert_eq!(body["error"]["message"], "rm is not allowed");
}

/// First-match-wins: a deny rule preceding an allow rule for the same tool must win.
#[tokio::test]
async fn deny_before_allow_is_denied() {
    let up = free_listener().await;
    let up_addr = up.local_addr().unwrap();
    spawn_upstream(up, Router::new().fallback(any(echo_handler)));

    let policy = make_policy(vec![
        PolicyRule {
            id: "deny-bash".to_string(),
            tool: "bash".to_string(),
            condition: None,
            action: RuleAction::Deny,
            message: Some("denied".to_string()),
            fields: None,
            pattern: None,
            replacement: None,
            max_calls: None,
            window_seconds: None,
        },
        PolicyRule {
            id: "allow-bash".to_string(),
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

    let pl = free_listener().await;
    let pl_addr = pl.local_addr().unwrap();
    let (rl, cb, storage) = make_components();
    let proxy = HttpProxy::new(
        &make_entry(up_addr, pl_addr.port()),
        Some(policy),
        rl,
        cb,
        storage,
    )
    .unwrap();
    tokio::spawn(proxy.run_with_listener(pl));
    wait_for_ready(pl_addr).await;

    let body: Value = reqwest::Client::new()
        .post(format!("http://{pl_addr}"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bash","arguments":{}}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(
        body["error"].is_object(),
        "deny before allow must be denied, got: {body}"
    );
}

/// First-match-wins: an allow rule preceding a deny rule for the same tool must win.
#[tokio::test]
async fn allow_before_deny_is_allowed() {
    let up = free_listener().await;
    let up_addr = up.local_addr().unwrap();
    spawn_upstream(up, Router::new().fallback(any(echo_handler)));

    let policy = make_policy(vec![
        PolicyRule {
            id: "allow-bash".to_string(),
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
        PolicyRule {
            id: "deny-bash".to_string(),
            tool: "bash".to_string(),
            condition: None,
            action: RuleAction::Deny,
            message: Some("should not reach this".to_string()),
            fields: None,
            pattern: None,
            replacement: None,
            max_calls: None,
            window_seconds: None,
        },
    ]);

    let pl = free_listener().await;
    let pl_addr = pl.local_addr().unwrap();
    let (rl, cb, storage) = make_components();
    let proxy = HttpProxy::new(
        &make_entry(up_addr, pl_addr.port()),
        Some(policy),
        rl,
        cb,
        storage,
    )
    .unwrap();
    tokio::spawn(proxy.run_with_listener(pl));
    wait_for_ready(pl_addr).await;

    let body: Value = reqwest::Client::new()
        .post(format!("http://{pl_addr}"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bash","arguments":{}}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // The request is forwarded to the echo upstream, so the response contains the original method.
    assert!(
        body.get("error").is_none(),
        "allow before deny must be forwarded, got: {body}"
    );
    assert_eq!(body["method"], "tools/call");
}

/// A 512 KB request body must be proxied without panic or connection reset.
/// The storage layer truncates payloads, but the proxy pipeline must remain intact.
#[tokio::test]
async fn large_payload_does_not_crash_proxy() {
    let up = free_listener().await;
    let up_addr = up.local_addr().unwrap();
    spawn_upstream(up, Router::new().fallback(any(echo_handler)));

    let pl = free_listener().await;
    let pl_addr = pl.local_addr().unwrap();
    let (rl, cb, storage) = make_components();
    let proxy =
        HttpProxy::new(&make_entry(up_addr, pl_addr.port()), None, rl, cb, storage).unwrap();
    tokio::spawn(proxy.run_with_listener(pl));
    wait_for_ready(pl_addr).await;

    let large_arg = "X".repeat(512 * 1024);
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": "read_file", "arguments": { "data": large_arg } }
    });

    let resp = reqwest::Client::new()
        .post(format!("http://{pl_addr}"))
        .json(&payload)
        .send()
        .await
        .expect("proxy must not drop the connection on a large payload");

    assert_eq!(resp.status(), 200);
}

/// 50 concurrent requests against the same proxy instance must all receive responses.
/// Detects data races in the shared state (rate limiter, circuit breaker, storage channel).
#[tokio::test]
async fn concurrent_requests_all_receive_responses() {
    let up = free_listener().await;
    let up_addr = up.local_addr().unwrap();
    spawn_upstream(up, Router::new().fallback(any(echo_handler)));

    let pl = free_listener().await;
    let pl_addr = pl.local_addr().unwrap();
    let (rl, cb, storage) = make_components();
    let proxy =
        HttpProxy::new(&make_entry(up_addr, pl_addr.port()), None, rl, cb, storage).unwrap();
    tokio::spawn(proxy.run_with_listener(pl));
    wait_for_ready(pl_addr).await;

    let client = reqwest::Client::new();
    let mut set = tokio::task::JoinSet::new();
    for i in 0u32..50 {
        let c = client.clone();
        let addr = pl_addr;
        set.spawn(async move {
            c.post(format!("http://{addr}"))
                .json(&json!({"jsonrpc":"2.0","id":i,"method":"tools/call","params":{"name":"read_file","arguments":{}}}))
                .send()
                .await
        });
    }

    let mut ok_count = 0usize;
    while let Some(result) = set.join_next().await {
        let resp = result.unwrap().expect("request must not fail");
        assert_eq!(resp.status(), 200);
        ok_count += 1;
    }
    assert_eq!(ok_count, 50, "all 50 concurrent requests must complete");
}
