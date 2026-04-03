use crate::config::{expand_env_vars, ServerEntry};
use crate::metrics;
use crate::policy::PolicyEngine;
use crate::protocol::jsonrpc::{extract_tool_params, JsonRpcMessage};
use crate::protocol::mcp;
use crate::proxy::evaluation::{error_resp, evaluate_tool_call, EvalOutcome};
use crate::ratelimit::{CircuitBreaker, RateLimiter};
use crate::storage::{InvocationRecord, InvocationStatus, StorageWriter};
use anyhow::{Context, Result};
use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use chrono::Utc;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;
use uuid::Uuid;

#[derive(Clone)]
struct HttpState {
    upstream_url: String,
    policy: Option<Arc<PolicyEngine>>,
    rate_limiter: Arc<RateLimiter>,
    circuit_breaker: Arc<CircuitBreaker>,
    storage: StorageWriter,
    server_name: String,
    http_client: reqwest::Client,
    extra_headers: HashMap<String, String>,
}

pub struct HttpProxy {
    state: HttpState,
    bind_addr: SocketAddr,
}

impl HttpProxy {
    pub fn new(
        entry: &ServerEntry,
        policy: Option<Arc<PolicyEngine>>,
        rate_limiter: Arc<RateLimiter>,
        circuit_breaker: Arc<CircuitBreaker>,
        storage: StorageWriter,
    ) -> Result<Self> {
        let url = entry
            .url
            .as_deref()
            .context("HTTP server entry requires a `url`")?
            .to_string();

        let expanded_headers = entry
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), expand_env_vars(v)))
            .collect();

        let http_client = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .context("Failed to build HTTP client")?;

        let bind_port = entry.bind_port.unwrap_or(7072);
        let bind_addr: SocketAddr = format!("127.0.0.1:{bind_port}").parse()?;

        Ok(Self {
            state: HttpState {
                upstream_url: url,
                policy,
                rate_limiter,
                circuit_breaker,
                storage,
                server_name: entry.name.clone(),
                http_client,
                extra_headers: expanded_headers,
            },
            bind_addr,
        })
    }

    pub async fn run_with_listener(self, listener: TcpListener) -> Result<()> {
        let addr = listener.local_addr()?;
        tracing::info!(addr = %addr, "HTTP proxy listening");

        let router = Router::new()
            .route("/health", axum::routing::get(health_handler))
            .route("/metrics", axum::routing::get(metrics::metrics_handler))
            .fallback(any(proxy_handler))
            .with_state(self.state);

        axum::serve(listener, router)
            .await
            .context("HTTP proxy server error")
    }

    pub async fn run(self) -> Result<()> {
        let listener = TcpListener::bind(self.bind_addr)
            .await
            .with_context(|| format!("Failed to bind HTTP proxy on {}", self.bind_addr))?;
        self.run_with_listener(listener).await
    }
}

async fn proxy_handler(State(state): State<HttpState>, req: Request) -> Response {
    match try_proxy(state, req).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("HTTP proxy error: {e}");
            (StatusCode::BAD_GATEWAY, e.to_string()).into_response()
        }
    }
}

async fn try_proxy(state: HttpState, req: Request) -> Result<Response> {
    let method = req.method().clone();
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.to_string())
        .unwrap_or_else(|| "/".to_string());
    let in_headers = req.headers().clone();
    let body_bytes = axum::body::to_bytes(req.into_body(), usize::MAX)
        .await
        .context("Failed to read request body")?;

    // For POST requests, attempt JSON-RPC inspection and policy evaluation.
    if method == Method::POST {
        if let Ok(raw) = std::str::from_utf8(&body_bytes) {
            if let Ok(JsonRpcMessage::Request(ref rpc_req)) = JsonRpcMessage::parse(raw) {
                if rpc_req.method == mcp::TOOLS_CALL {
                    let (tool_name, arguments) = extract_tool_params(rpc_req);
                    let started_at = Instant::now();

                    match evaluate_tool_call(
                        &rpc_req.id,
                        &tool_name,
                        arguments,
                        state.policy.as_ref(),
                        &state.rate_limiter,
                        &state.circuit_breaker,
                        &state.storage,
                        &state.server_name,
                    ) {
                        EvalOutcome::Block { response } => {
                            let json = serde_json::to_string(&response)?;
                            return Ok((
                                StatusCode::OK,
                                [("Content-Type", "application/json")],
                                json,
                            )
                                .into_response());
                        }
                        EvalOutcome::Allow {
                            arguments: allowed_args,
                        } => {
                            metrics::global().active_sessions.inc();
                            let upstream_resp =
                                forward(&state, &method, &path, &in_headers, body_bytes.clone())
                                    .await?;
                            let elapsed = started_at.elapsed();
                            let latency_ms = elapsed.as_millis() as i64;
                            let is_error = !upstream_resp.status().is_success();
                            let status_label = if is_error { "error" } else { "success" };

                            let m = metrics::global();
                            m.tool_calls_total
                                .with_label_values(&[&tool_name, status_label])
                                .inc();
                            m.tool_call_duration_seconds
                                .with_label_values(&[&tool_name])
                                .observe(elapsed.as_secs_f64());
                            m.circuit_breaker_state
                                .with_label_values(&[&tool_name])
                                .set(metrics::circuit_state_to_f64(
                                    state.circuit_breaker.state_kind(&tool_name),
                                ));
                            m.active_sessions.dec();

                            state.storage.record(InvocationRecord {
                                id: Uuid::new_v4().to_string(),
                                timestamp: Utc::now(),
                                agent_id: None,
                                session_id: None,
                                server_name: state.server_name.clone(),
                                tool_name,
                                arguments: allowed_args,
                                result: None,
                                latency_ms: Some(latency_ms),
                                status: if is_error {
                                    InvocationStatus::Error
                                } else {
                                    InvocationStatus::Allowed
                                },
                                policy_hit: None,
                            });

                            return build_axum_response(upstream_resp).await;
                        }
                    }
                }
            }
        }
    }

    // Generic reverse proxy — no JSON-RPC inspection.
    let upstream_resp = forward(&state, &method, &path, &in_headers, body_bytes).await?;
    build_axum_response(upstream_resp).await
}

async fn forward(
    state: &HttpState,
    method: &Method,
    path: &String,
    in_headers: &HeaderMap,
    body: axum::body::Bytes,
) -> Result<reqwest::Response> {
    let upstream = format!("{}{}", state.upstream_url.trim_end_matches('/'), path);

    let reqwest_method =
        reqwest::Method::from_bytes(method.as_str().as_bytes()).context("Invalid HTTP method")?;

    let mut req = state
        .http_client
        .request(reqwest_method, &upstream)
        .body(body);

    // Forward a safe subset of inbound headers.
    for (name, value) in in_headers {
        let n = name.as_str().to_lowercase();
        if matches!(
            n.as_str(),
            "content-type" | "accept" | "authorization" | "x-request-id"
        ) {
            if let Ok(v) = value.to_str() {
                req = req.header(name.as_str(), v);
            }
        }
    }

    // Inject configured extra headers (may override forwarded ones).
    for (k, v) in &state.extra_headers {
        req = req.header(k, v);
    }

    req.send().await.context("Upstream HTTP request failed")
}

async fn build_axum_response(upstream: reqwest::Response) -> Result<Response> {
    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut header_map = HeaderMap::new();
    for (name, value) in upstream.headers() {
        let n = name.as_str().to_lowercase();
        if matches!(
            n.as_str(),
            "content-type" | "content-length" | "transfer-encoding" | "cache-control"
        ) {
            if let (Ok(hn), Ok(hv)) = (
                HeaderName::from_bytes(name.as_str().as_bytes()),
                HeaderValue::from_bytes(value.as_bytes()),
            ) {
                header_map.insert(hn, hv);
            }
        }
    }

    let body = upstream
        .bytes()
        .await
        .context("Failed to read upstream body")?;
    Ok((status, header_map, body).into_response())
}

async fn health_handler() -> &'static str {
    "ok"
}

pub fn error_response_body(id: Option<&serde_json::Value>, code: i64, message: &str) -> String {
    let id_owned = id.cloned();
    serde_json::to_string(&error_resp(&id_owned, code, message, None)).unwrap_or_else(|_| {
        r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"internal error"}}"#.to_string()
    })
}
