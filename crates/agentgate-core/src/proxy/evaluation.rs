use crate::metrics;
use crate::policy::{PolicyDecision, PolicyEngine};
use crate::protocol::jsonrpc::{JsonRpcError, JsonRpcResponse};
use crate::ratelimit::{CircuitBreaker, CircuitDecision, RateLimitDecision, RateLimiter};
use crate::storage::{InvocationRecord, InvocationStatus, StorageWriter};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub enum EvalOutcome {
    /// Call is allowed. `arguments` may differ from the original if a redact rule matched.
    Allow { arguments: Option<Value> },
    /// Call is blocked. The response must be returned directly to the caller.
    Block { response: JsonRpcResponse },
}

/// Evaluate a `tools/call` request against the policy engine, rate limiter, and circuit
/// breaker in sequence. Denials are recorded to storage before returning `Block`.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_tool_call(
    req_id: &Option<Value>,
    tool_name: &str,
    arguments: Option<Value>,
    policy: Option<&Arc<PolicyEngine>>,
    rate_limiter: &RateLimiter,
    circuit_breaker: &CircuitBreaker,
    storage: &StorageWriter,
    server_name: &str,
) -> EvalOutcome {
    if let Some(engine) = policy {
        match engine.evaluate(tool_name, arguments.as_ref()) {
            PolicyDecision::Deny { rule_id, message } => {
                metrics::global()
                    .policy_denials_total
                    .with_label_values(&[&rule_id])
                    .inc();
                metrics::global()
                    .tool_calls_total
                    .with_label_values(&[tool_name, "denied"])
                    .inc();
                storage.record(make_record(
                    tool_name,
                    arguments,
                    server_name,
                    InvocationStatus::Denied,
                    Some(&rule_id),
                ));
                return EvalOutcome::Block {
                    response: error_resp(req_id, -32603, &message, None),
                };
            }
            PolicyDecision::RateLimited { rule_id } => {
                let msg = format!(
                    "Tool call blocked by rate-limit policy rule '{rule_id}'. \
                     STOP retrying immediately — do NOT attempt this tool or any workarounds \
                     until the rate-limit window resets."
                );
                metrics::global()
                    .rate_limit_hits_total
                    .with_label_values(&["policy"])
                    .inc();
                metrics::global()
                    .tool_calls_total
                    .with_label_values(&[tool_name, "rate_limited"])
                    .inc();
                storage.record(make_record(
                    tool_name,
                    arguments,
                    server_name,
                    InvocationStatus::RateLimited,
                    Some(&rule_id),
                ));
                return EvalOutcome::Block {
                    response: error_resp(req_id, -32029, &msg, None),
                };
            }
            PolicyDecision::Redact {
                rule_id,
                arguments: redacted,
            } => {
                tracing::info!(rule_id = %rule_id, tool = %tool_name, "Arguments redacted");
                return EvalOutcome::Allow {
                    arguments: Some(redacted),
                };
            }
            PolicyDecision::Allow => {}
        }
    }

    match rate_limiter.check(tool_name) {
        RateLimitDecision::GlobalLimitExceeded { retry_after_secs } => {
            let msg = format!(
                "GLOBAL RATE LIMIT EXCEEDED. You MUST WAIT {retry_after_secs} SECONDS before \
                 making any tool call. Do NOT retry immediately or attempt alternative tools."
            );
            metrics::global()
                .rate_limit_hits_total
                .with_label_values(&["global"])
                .inc();
            metrics::global()
                .tool_calls_total
                .with_label_values(&[tool_name, "rate_limited"])
                .inc();
            storage.record(make_record(
                tool_name,
                arguments,
                server_name,
                InvocationStatus::RateLimited,
                Some("global"),
            ));
            return EvalOutcome::Block {
                response: error_resp(
                    req_id,
                    -32029,
                    &msg,
                    Some(json!({ "retry_after_secs": retry_after_secs })),
                ),
            };
        }
        RateLimitDecision::ToolLimitExceeded {
            tool,
            retry_after_secs,
        } => {
            let msg = format!(
                "Rate limit exceeded for tool '{tool}'. \
                 WAIT {retry_after_secs} SECONDS before calling this tool again. \
                 Do NOT call this tool or attempt equivalent workarounds in the meantime."
            );
            metrics::global()
                .rate_limit_hits_total
                .with_label_values(&["per-tool"])
                .inc();
            metrics::global()
                .tool_calls_total
                .with_label_values(&[tool_name, "rate_limited"])
                .inc();
            storage.record(make_record(
                tool_name,
                arguments,
                server_name,
                InvocationStatus::RateLimited,
                Some("per-tool"),
            ));
            return EvalOutcome::Block {
                response: error_resp(
                    req_id,
                    -32029,
                    &msg,
                    Some(json!({ "retry_after_secs": retry_after_secs, "tool": tool })),
                ),
            };
        }
        RateLimitDecision::Allow => {}
    }

    match circuit_breaker.check(tool_name) {
        CircuitDecision::Open { retry_after_secs } => {
            let msg = format!(
                "Circuit breaker OPEN for '{tool_name}' due to repeated failures. \
                 WAIT {retry_after_secs} SECONDS before retrying. \
                 Do NOT call this tool repeatedly — it will not succeed until the cooldown expires."
            );
            metrics::global()
                .circuit_breaker_state
                .with_label_values(&[tool_name])
                .set(1.0);
            metrics::global()
                .tool_calls_total
                .with_label_values(&[tool_name, "circuit_open"])
                .inc();
            storage.record(make_record(
                tool_name,
                arguments,
                server_name,
                InvocationStatus::Error,
                Some("circuit-breaker"),
            ));
            return EvalOutcome::Block {
                response: error_resp(
                    req_id,
                    -32030,
                    &msg,
                    Some(json!({ "retry_after_secs": retry_after_secs, "state": "open" })),
                ),
            };
        }
        CircuitDecision::Allow { is_probe } => {
            if is_probe {
                tracing::info!(tool = %tool_name, "Circuit probe allowed");
                metrics::global()
                    .circuit_breaker_state
                    .with_label_values(&[tool_name])
                    .set(2.0); // half-open
            }
        }
    }

    EvalOutcome::Allow { arguments }
}

pub fn error_resp(
    id: &Option<Value>,
    code: i64,
    message: &str,
    data: Option<Value>,
) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: id.clone().unwrap_or(Value::Null),
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_string(),
            data,
        }),
    }
}

pub fn make_record(
    tool_name: &str,
    arguments: Option<Value>,
    server_name: &str,
    status: InvocationStatus,
    policy_hit: Option<&str>,
) -> InvocationRecord {
    InvocationRecord {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        agent_id: None,
        session_id: None,
        server_name: server_name.to_string(),
        tool_name: tool_name.to_string(),
        arguments,
        result: None,
        latency_ms: None,
        status,
        policy_hit: policy_hit.map(str::to_string),
    }
}
