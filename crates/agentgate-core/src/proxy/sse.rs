use crate::config::{expand_env_vars, ServerEntry};
use crate::metrics;
use crate::policy::PolicyEngine;
use crate::protocol::jsonrpc::{extract_tool_params, JsonRpcMessage};
use crate::protocol::mcp;
use crate::proxy::evaluation::{evaluate_tool_call, make_record, EvalOutcome};
use crate::ratelimit::{CircuitBreaker, RateLimiter};
use crate::storage::{InvocationRecord, InvocationStatus, StorageWriter};
use anyhow::{bail, Context, Result};
use axum::{
    extract::State,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Router,
};
use chrono::Utc;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

#[derive(Clone)]
struct SseState {
    upstream_sse_url: String,
    /// Upstream message endpoint, discovered from the SSE `endpoint` event.
    message_endpoint: Arc<RwLock<String>>,
    policy: Option<Arc<PolicyEngine>>,
    rate_limiter: Arc<RateLimiter>,
    circuit_breaker: Arc<CircuitBreaker>,
    storage: StorageWriter,
    server_name: String,
    http_client: reqwest::Client,
    extra_headers: HashMap<String, String>,
}

pub struct SseProxy {
    state: SseState,
    bind_addr: SocketAddr,
}

impl SseProxy {
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
            .context("SSE server entry requires a `url`")?
            .to_string();

        let default_message = derive_message_endpoint(&url);

        let expanded_headers = entry
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), expand_env_vars(v)))
            .collect();

        let http_client = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .context("Failed to build HTTP client")?;

        let bind_port = entry.bind_port.unwrap_or(7071);
        let bind_addr: SocketAddr = format!("127.0.0.1:{bind_port}").parse()?;

        Ok(Self {
            state: SseState {
                upstream_sse_url: url,
                message_endpoint: Arc::new(RwLock::new(default_message)),
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
        tracing::info!(addr = %addr, "SSE proxy listening");

        let router = Router::new()
            .route("/sse", get(sse_handler))
            .route("/message", post(message_handler))
            .route("/health", get(health_handler))
            .route("/metrics", get(metrics::metrics_handler))
            .with_state(self.state);

        axum::serve(listener, router)
            .await
            .context("SSE proxy server error")
    }

    pub async fn run(self) -> Result<()> {
        let listener = TcpListener::bind(self.bind_addr)
            .await
            .with_context(|| format!("Failed to bind SSE proxy on {}", self.bind_addr))?;
        self.run_with_listener(listener).await
    }
}

async fn sse_handler(State(state): State<SseState>) -> impl IntoResponse {
    let (tx, rx) = mpsc::channel::<Result<Event, Infallible>>(64);
    let stream = ReceiverStream::new(rx);

    tokio::spawn(pump_upstream_sse(state, tx));

    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn pump_upstream_sse(state: SseState, tx: mpsc::Sender<Result<Event, Infallible>>) {
    if let Err(e) = try_pump_upstream_sse(state, tx).await {
        tracing::error!("Upstream SSE pump failed: {e}");
    }
}

async fn try_pump_upstream_sse(
    state: SseState,
    tx: mpsc::Sender<Result<Event, Infallible>>,
) -> Result<()> {
    let mut req = state
        .http_client
        .get(&state.upstream_sse_url)
        .header("Accept", "text/event-stream");

    for (k, v) in &state.extra_headers {
        req = req.header(k, v);
    }

    let resp = req.send().await.context("Upstream SSE connection failed")?;

    if !resp.status().is_success() {
        bail!("Upstream SSE returned {}", resp.status());
    }

    let mut byte_stream = resp.bytes_stream();
    let mut buf = String::new();
    let mut current = ParsedSseEvent::default();

    while let Some(chunk) = byte_stream.next().await {
        let chunk = chunk.context("Upstream SSE stream error")?;
        buf.push_str(std::str::from_utf8(&chunk).context("Non-UTF8 SSE data")?);

        while let Some(nl) = buf.find('\n') {
            let line = buf[..nl].trim_end_matches('\r').to_string();
            buf = buf[nl + 1..].to_string();

            if line.is_empty() {
                if current.data.is_some() || current.event_type.is_some() {
                    // Handle the `endpoint` event — update the message URL.
                    if current.event_type.as_deref() == Some("endpoint") {
                        if let Some(ref path) = current.data {
                            let base = base_url(&state.upstream_sse_url);
                            *state.message_endpoint.write().await = format!("{base}{path}");
                            tracing::info!(
                                endpoint = %format!("{base}{path}"),
                                "Upstream message endpoint discovered"
                            );
                        }
                    }

                    let event = current.into_axum_event();
                    if tx.send(Ok(event)).await.is_err() {
                        return Ok(());
                    }
                    current = ParsedSseEvent::default();
                }
            } else if let Some(data) = line.strip_prefix("data: ") {
                current.data = Some(data.to_string());
            } else if let Some(etype) = line.strip_prefix("event: ") {
                current.event_type = Some(etype.to_string());
            } else if let Some(id) = line.strip_prefix("id: ") {
                current.id = Some(id.to_string());
            }
        }
    }

    Ok(())
}

async fn message_handler(State(state): State<SseState>, body: axum::body::Bytes) -> Response {
    match try_message_handler(state, body).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!("Message handler error: {e}");
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

async fn try_message_handler(state: SseState, body: axum::body::Bytes) -> Result<Response> {
    let raw = std::str::from_utf8(&body).context("Non-UTF8 request body")?;

    let msg = JsonRpcMessage::parse(raw).ok();

    if let Some(JsonRpcMessage::Request(ref req)) = msg {
        if req.method == mcp::TOOLS_CALL {
            let (tool_name, arguments) = extract_tool_params(req);
            let started_at = Instant::now();

            match evaluate_tool_call(
                &req.id,
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
                        axum::http::StatusCode::OK,
                        [("Content-Type", "application/json")],
                        json,
                    )
                        .into_response());
                }
                EvalOutcome::Allow {
                    arguments: allowed_args,
                } => {
                    let endpoint = state.message_endpoint.read().await.clone();
                    let mut req_builder = state
                        .http_client
                        .post(&endpoint)
                        .header("Content-Type", "application/json")
                        .body(body);
                    for (k, v) in &state.extra_headers {
                        req_builder = req_builder.header(k, v);
                    }

                    metrics::global().active_sessions.inc();
                    let upstream_resp = req_builder
                        .send()
                        .await
                        .context("Failed to forward to upstream message endpoint")?;
                    let elapsed = started_at.elapsed();
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

                    state.storage.record(make_record(
                        &tool_name,
                        allowed_args,
                        &state.server_name,
                        if is_error {
                            InvocationStatus::Error
                        } else {
                            InvocationStatus::Allowed
                        },
                        None,
                    ));

                    let http_status =
                        axum::http::StatusCode::from_u16(upstream_resp.status().as_u16())
                            .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
                    let resp_body = upstream_resp
                        .bytes()
                        .await
                        .context("Failed to read upstream response")?;
                    return Ok((http_status, resp_body).into_response());
                }
            }
        }
    }

    let endpoint = state.message_endpoint.read().await.clone();
    let mut req_builder = state
        .http_client
        .post(&endpoint)
        .header("Content-Type", "application/json")
        .body(body);

    for (k, v) in &state.extra_headers {
        req_builder = req_builder.header(k, v);
    }

    let upstream_resp = req_builder
        .send()
        .await
        .context("Failed to forward to upstream message endpoint")?;

    let status = axum::http::StatusCode::from_u16(upstream_resp.status().as_u16())
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    let resp_body = upstream_resp
        .bytes()
        .await
        .context("Failed to read upstream response")?;

    Ok((status, resp_body).into_response())
}

async fn health_handler() -> &'static str {
    "ok"
}

#[derive(Default)]
struct ParsedSseEvent {
    event_type: Option<String>,
    data: Option<String>,
    id: Option<String>,
}

impl ParsedSseEvent {
    fn into_axum_event(self) -> Event {
        let mut e = Event::default();
        if let Some(et) = self.event_type {
            e = e.event(et);
        }
        if let Some(data) = self.data {
            e = e.data(data);
        }
        if let Some(id) = self.id {
            e = e.id(id);
        }
        e
    }
}

fn derive_message_endpoint(sse_url: &str) -> String {
    if let Some(base) = sse_url.strip_suffix("/sse") {
        format!("{base}/message")
    } else {
        format!("{sse_url}/message")
    }
}

fn base_url(url: &str) -> &str {
    url.rfind('/').map(|i| &url[..i]).unwrap_or(url)
}

#[allow(dead_code)]
fn make_invocation_record(
    tool_name: &str,
    server_name: &str,
    started_at: Instant,
    result: Option<serde_json::Value>,
    is_error: bool,
) -> InvocationRecord {
    InvocationRecord {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        agent_id: None,
        session_id: None,
        server_name: server_name.to_string(),
        tool_name: tool_name.to_string(),
        arguments: None,
        result,
        latency_ms: Some(started_at.elapsed().as_millis() as i64),
        status: if is_error {
            InvocationStatus::Error
        } else {
            InvocationStatus::Allowed
        },
        policy_hit: None,
    }
}
