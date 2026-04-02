use crate::config::AgentGateConfig;
use crate::logging::structured::{log_event, Direction, LogEvent};
use crate::metrics;
use crate::policy::PolicyEngine;
use crate::protocol::jsonrpc::{JsonRpcMessage, JsonRpcRequest};
use crate::protocol::mcp;
use crate::proxy::evaluation::{evaluate_tool_call, EvalOutcome};
use crate::ratelimit::{CircuitBreaker, RateLimiter};
use crate::storage::{InvocationRecord, InvocationStatus, StorageWriter};
use anyhow::{Context, Result};
use axum::Router;
use chrono::Utc;
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

struct PendingCall {
    tool_name: String,
    arguments: Option<Value>,
    started_at: Instant,
}

type PendingMap = Arc<Mutex<HashMap<String, PendingCall>>>;

pub struct StdioProxy {
    config: AgentGateConfig,
}

impl StdioProxy {
    pub fn new(config: AgentGateConfig) -> Self {
        Self { config }
    }

    pub async fn run(&self, command: &str, args: &[String]) -> Result<()> {
        tracing::info!("Starting stdio proxy for: {} {:?}", command, args);

        let storage = StorageWriter::spawn(self.config.db_path.clone())?;

        let policy = self
            .config
            .policy_path
            .as_deref()
            .map(|p| {
                let e = PolicyEngine::load(p)?;
                PolicyEngine::spawn_watcher(Arc::clone(&e), p.to_path_buf());
                Ok::<_, anyhow::Error>(e)
            })
            .transpose()?;

        let rate_limiter = Arc::new(RateLimiter::new(self.config.rate_limits.clone()));
        let circuit_breaker = Arc::new(CircuitBreaker::new(self.config.circuit_breaker.clone()));
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        if let Some(port) = self.config.metrics_port {
            let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
            let router =
                Router::new().route("/metrics", axum::routing::get(metrics::metrics_handler));
            match TcpListener::bind(addr).await {
                Ok(listener) => {
                    tracing::info!(addr = %addr, "Metrics server listening");
                    tokio::spawn(async move {
                        let _ = axum::serve(listener, router).await;
                    });
                }
                Err(e) => tracing::warn!("Failed to bind metrics server on {addr}: {e}"),
            }
        }

        // FIX: Bounded channel (10_000 messages) to prevent memory leaks under high backpressure
        let (stdout_tx, mut stdout_rx) = tokio::sync::mpsc::channel::<String>(10_000);

        let stdout_writer = tokio::spawn(async move {
            let mut out = tokio::io::stdout();
            while let Some(line) = stdout_rx.recv().await {
                out.write_all(line.as_bytes()).await?;
                out.write_all(b"\n").await?;
                out.flush().await?;
            }
            Ok::<_, anyhow::Error>(())
        });

        let mut child = Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to spawn: {command}"))?;

        let child_stdin = child.stdin.take().expect("stdin piped");
        let child_stdout = child.stdout.take().expect("stdout piped");
        let child_stderr = child.stderr.take().expect("stderr piped");

        let task_a = tokio::spawn(proxy_inbound(
            child_stdin,
            Arc::clone(&pending),
            policy.clone(),
            Arc::clone(&rate_limiter),
            Arc::clone(&circuit_breaker),
            storage.clone(),
            self.config.server_name.clone(),
            stdout_tx.clone(),
        ));

        let task_b = tokio::spawn(proxy_response(
            child_stdout,
            Arc::clone(&pending),
            policy.clone(),
            Arc::clone(&circuit_breaker),
            storage,
            self.config.server_name.clone(),
            stdout_tx,
        ));

        let task_c = tokio::spawn(pipe_stderr(child_stderr));

        let status = tokio::select! {
            res = child.wait() => {
                res.context("Failed to wait for child process")?
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received Ctrl+C, terminating child");
                let _ = child.kill().await;
                std::process::exit(0);
            }
            _ = sigterm_signal() => {
                tracing::info!("Received SIGTERM, terminating child");
                let _ = child.kill().await;
                std::process::exit(0);
            }
        };

        let flush = std::time::Duration::from_secs(2);
        let _ = tokio::time::timeout(flush, task_a).await;
        let _ = tokio::time::timeout(flush, task_b).await;
        let _ = tokio::time::timeout(flush, task_c).await;
        let _ = tokio::time::timeout(flush, stdout_writer).await;

        if !status.success() {
            std::process::exit(status.code().unwrap_or(1));
        }

        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn proxy_inbound(
    mut child_stdin: tokio::process::ChildStdin,
    pending: PendingMap,
    policy: Option<Arc<PolicyEngine>>,
    rate_limiter: Arc<RateLimiter>,
    circuit_breaker: Arc<CircuitBreaker>,
    storage: StorageWriter,
    server_name: String,
    stdout_tx: Sender<String>,
) -> Result<()> {
    let mut reader = BufReader::new(tokio::io::stdin()).lines();

    while let Some(line) = reader.next_line().await? {
        if line.is_empty() {
            continue;
        }

        let msg = match JsonRpcMessage::parse(&line) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Inbound parse error: {e}");
                forward_raw(&mut child_stdin, &line).await?;
                continue;
            }
        };

        log_event(&LogEvent {
            timestamp: Utc::now(),
            direction: Direction::Inbound,
            message: msg.clone(),
            raw: line.clone(),
        });

        if let JsonRpcMessage::Request(ref req) = msg {
            if req.method == mcp::TOOLS_CALL {
                let (tool_name, arguments) = extract_params(req);
                let original_args = arguments.clone();

                match evaluate_tool_call(
                    &req.id,
                    &tool_name,
                    arguments,
                    policy.as_ref(),
                    &rate_limiter,
                    &circuit_breaker,
                    &storage,
                    &server_name,
                ) {
                    EvalOutcome::Block { response } => {
                        let res_str = serde_json::to_string(&response)?;
                        // FIX: Await bounded channel sending
                        stdout_tx
                            .send(res_str)
                            .await
                            .map_err(|e| anyhow::anyhow!("Channel error: {e}"))?;
                        continue;
                    }
                    EvalOutcome::Allow {
                        arguments: allowed_args,
                    } => {
                        let forward_line = if allowed_args != original_args {
                            serde_json::to_string(&rebuild_call(req, allowed_args.clone()))?
                        } else {
                            line.clone()
                        };
                        metrics::global().active_sessions.inc();
                        pending.lock().unwrap().insert(
                            id_key(req),
                            PendingCall {
                                tool_name,
                                arguments: allowed_args,
                                started_at: Instant::now(),
                            },
                        );
                        forward_raw(&mut child_stdin, &forward_line).await?;
                        continue;
                    }
                }
            }
        }

        forward_raw(&mut child_stdin, &line).await?;
    }

    Ok(())
}

async fn proxy_response(
    child_stdout: tokio::process::ChildStdout,
    pending: PendingMap,
    policy: Option<Arc<PolicyEngine>>,
    circuit_breaker: Arc<CircuitBreaker>,
    storage: StorageWriter,
    server_name: String,
    stdout_tx: Sender<String>,
) -> Result<()> {
    let mut reader = BufReader::new(child_stdout).lines();

    while let Some(line) = reader.next_line().await? {
        if line.is_empty() {
            continue;
        }

        match JsonRpcMessage::parse(&line) {
            Ok(msg) => {
                log_event(&LogEvent {
                    timestamp: Utc::now(),
                    direction: Direction::Response,
                    message: msg.clone(),
                    raw: line.clone(),
                });
                flush_pending(
                    &msg,
                    &pending,
                    policy.as_ref(),
                    &circuit_breaker,
                    &storage,
                    &server_name,
                );
            }
            Err(e) => tracing::warn!("Response parse error: {e}"),
        }

        stdout_tx
            .send(line)
            .await
            .map_err(|e| anyhow::anyhow!("Channel error: {e}"))?;
    }

    Ok(())
}

async fn pipe_stderr(child_stderr: tokio::process::ChildStderr) -> Result<()> {
    let mut reader = BufReader::new(child_stderr).lines();
    let mut stderr = tokio::io::stderr();
    while let Some(line) = reader.next_line().await? {
        stderr.write_all(line.as_bytes()).await?;
        stderr.write_all(b"\n").await?;
        stderr.flush().await?;
    }
    Ok(())
}

async fn forward_raw(sink: &mut tokio::process::ChildStdin, line: &str) -> Result<()> {
    sink.write_all(line.as_bytes()).await?;
    sink.write_all(b"\n").await?;
    sink.flush().await?;
    Ok(())
}

fn flush_pending(
    msg: &JsonRpcMessage,
    pending: &PendingMap,
    policy: Option<&Arc<PolicyEngine>>,
    circuit_breaker: &CircuitBreaker,
    storage: &StorageWriter,
    server_name: &str,
) {
    let JsonRpcMessage::Response(resp) = msg else {
        return;
    };

    let key = resp.id.to_string();
    let Some(call) = pending.lock().unwrap().remove(&key) else {
        return;
    };

    let elapsed = call.started_at.elapsed();
    let latency_ms = elapsed.as_millis() as i64;

    if resp.error.is_some() {
        circuit_breaker.on_error(&call.tool_name);
    } else {
        circuit_breaker.on_success(&call.tool_name);
    }

    let status = if resp.error.is_some() {
        InvocationStatus::Error
    } else {
        InvocationStatus::Allowed
    };

    let status_label = match status {
        InvocationStatus::Error => "error",
        InvocationStatus::Allowed => "success",
        _ => "unknown",
    };
    let m = metrics::global();
    m.tool_calls_total
        .with_label_values(&[&call.tool_name, status_label])
        .inc();
    m.tool_call_duration_seconds
        .with_label_values(&[&call.tool_name])
        .observe(elapsed.as_secs_f64());
    m.circuit_breaker_state
        .with_label_values(&[&call.tool_name])
        .set(metrics::circuit_state_to_f64(
            circuit_breaker.state_kind(&call.tool_name),
        ));
    m.active_sessions.dec();

    // Scan result through policy redact rules before storing — catches secrets in tool output.
    let result_to_store = resp.result.as_ref().map(|res| match policy {
        Some(engine) => engine.redact_output(res),
        None => res.clone(),
    });

    storage.record(InvocationRecord {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        agent_id: None,
        session_id: None,
        server_name: server_name.to_string(),
        tool_name: call.tool_name,
        arguments: call.arguments,
        result: result_to_store,
        latency_ms: Some(latency_ms),
        status,
        policy_hit: None,
    });
}

fn rebuild_call(original: &JsonRpcRequest, new_arguments: Option<Value>) -> JsonRpcRequest {
    let mut params = original
        .params
        .clone()
        .unwrap_or(Value::Object(Default::default()));
    if let (Value::Object(ref mut map), Some(args)) = (&mut params, new_arguments) {
        map.insert("arguments".to_string(), args);
    }
    JsonRpcRequest {
        jsonrpc: original.jsonrpc.clone(),
        id: original.id.clone(),
        method: original.method.clone(),
        params: Some(params),
    }
}

fn id_key(req: &JsonRpcRequest) -> String {
    req.id
        .as_ref()
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn extract_params(req: &JsonRpcRequest) -> (String, Option<Value>) {
    let Some(params) = &req.params else {
        return ("unknown".to_string(), None);
    };
    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let arguments = params.get("arguments").cloned();
    (tool_name, arguments)
}

/// Resolves when SIGTERM is received on Unix; never resolves on other platforms.
async fn sigterm_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
            return;
        }
    }
    std::future::pending::<()>().await
}
