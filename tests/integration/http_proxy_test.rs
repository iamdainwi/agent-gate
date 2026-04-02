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

async fn free_listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").await.unwrap()
}

fn make_components() -> (Arc<RateLimiter>, Arc<CircuitBreaker>, StorageWriter) {
    let rl = Arc::new(RateLimiter::new(RateLimitConfig {
        global_max_calls_per_minute: 10_000,
        per_tool_max_calls_per_minute: 10_000,
        per_agent_max_calls_per_minute: 10_000,
    }));
    let cb = Arc::new(CircuitBreaker::new(CircuitBreakerConfig {
        error_threshold: 100,
        window_seconds: 30,
        cooldown_seconds: 60,
    }));
    let storage = StorageWriter::spawn(
        std::env::temp_dir().join(format!("agentgate-test-{}.db", uuid::Uuid::new_v4())),
    )
    .unwrap();
    (rl, cb, storage)
}

fn entry(upstream: std::net::SocketAddr, bind_port: u16) -> ServerEntry {
    ServerEntry {
        name: "test".to_string(),
        transport: TransportKind::Http,
        command: None,
        args: vec![],
        url: Some(format!("http://{upstream}")),
        headers: Default::default(),
        bind_port: Some(bind_port),
    }
}

async fn echo_handler(body: axum::body::Bytes) -> impl axum::response::IntoResponse {
    (
        axum::http::StatusCode::OK,
        [("content-type", "application/json")],
        body,
    )
}

async fn tools_list_handler() -> axum::Json<Value> {
    axum::Json(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": { "tools": [{ "name": "bash", "description": "Run shell" }] }
    }))
}

fn spawn_upstream(listener: TcpListener, router: Router) {
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
}

/// Poll /health with exponential back-off until the proxy is accepting connections.
/// Avoids the flakiness of a fixed-duration sleep on slow CI machines.
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
    panic!("proxy at {addr} never became ready after 20 attempts");
}

#[tokio::test]
async fn http_proxy_forwards_generic_request() {
    let up = free_listener().await;
    let up_addr = up.local_addr().unwrap();
    spawn_upstream(up, Router::new().fallback(any(echo_handler)));

    let pl = free_listener().await;
    let pl_addr = pl.local_addr().unwrap();
    let (rl, cb, storage) = make_components();

    let proxy = HttpProxy::new(&entry(up_addr, pl_addr.port()), None, rl, cb, storage).unwrap();
    tokio::spawn(proxy.run_with_listener(pl));
    wait_for_ready(pl_addr).await;

    let payload = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}});
    let resp = reqwest::Client::new()
        .post(format!("http://{pl_addr}"))
        .json(&payload)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["method"], "tools/list");
}

#[tokio::test]
async fn http_proxy_returns_upstream_response() {
    let up = free_listener().await;
    let up_addr = up.local_addr().unwrap();
    spawn_upstream(up, Router::new().fallback(any(tools_list_handler)));

    let pl = free_listener().await;
    let pl_addr = pl.local_addr().unwrap();
    let (rl, cb, storage) = make_components();

    let proxy = HttpProxy::new(&entry(up_addr, pl_addr.port()), None, rl, cb, storage).unwrap();
    tokio::spawn(proxy.run_with_listener(pl));
    wait_for_ready(pl_addr).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{pl_addr}"))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["result"]["tools"].is_array());
}

#[tokio::test]
async fn http_proxy_blocks_denied_tool_call() {
    let up = free_listener().await;
    let up_addr = up.local_addr().unwrap();
    spawn_upstream(up, Router::new().fallback(any(echo_handler)));

    let policy_file = PolicyFile {
        metadata: PolicyMetadata {
            name: "test".to_string(),
            version: "1.0".to_string(),
        },
        rules: vec![PolicyRule {
            id: "block-bash".to_string(),
            tool: "bash".to_string(),
            condition: None,
            action: RuleAction::Deny,
            message: Some("bash is blocked".to_string()),
            fields: None,
            pattern: None,
            replacement: None,
            max_calls: None,
            window_seconds: None,
        }],
    };

    let tmp = std::env::temp_dir().join(format!("policy-{}.toml", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, toml::to_string(&policy_file).unwrap()).unwrap();
    let engine = PolicyEngine::load(&tmp).unwrap();

    let pl = free_listener().await;
    let pl_addr = pl.local_addr().unwrap();
    let (rl, cb, storage) = make_components();

    let proxy = HttpProxy::new(
        &entry(up_addr, pl_addr.port()),
        Some(engine),
        rl,
        cb,
        storage,
    )
    .unwrap();
    tokio::spawn(proxy.run_with_listener(pl));
    wait_for_ready(pl_addr).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{pl_addr}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "bash", "arguments": { "command": "ls" } }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["error"].is_object(),
        "Expected JSON-RPC error, got: {body}"
    );
    assert_eq!(body["error"]["message"], "bash is blocked");

    std::fs::remove_file(tmp).ok();
}

#[tokio::test]
async fn http_proxy_health_endpoint() {
    let up = free_listener().await;
    let up_addr = up.local_addr().unwrap();
    spawn_upstream(up, Router::new().fallback(any(echo_handler)));

    let pl = free_listener().await;
    let pl_addr = pl.local_addr().unwrap();
    let (rl, cb, storage) = make_components();

    let proxy = HttpProxy::new(&entry(up_addr, pl_addr.port()), None, rl, cb, storage).unwrap();
    tokio::spawn(proxy.run_with_listener(pl));
    wait_for_ready(pl_addr).await;

    let resp = reqwest::get(format!("http://{pl_addr}/health"))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.text().await.unwrap(), "ok");
}
