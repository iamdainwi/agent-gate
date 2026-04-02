use axum::response::IntoResponse;
use prometheus::{
    CounterVec, Encoder, GaugeVec, HistogramOpts, HistogramVec, IntGauge, Opts, Registry,
    TextEncoder,
};
use std::sync::OnceLock;

pub struct Metrics {
    registry: Registry,
    /// Total tool invocations, partitioned by tool name and outcome status.
    pub tool_calls_total: CounterVec,
    /// Tool call round-trip latency in seconds (upstream included).
    pub tool_call_duration_seconds: HistogramVec,
    /// Tool calls blocked by a named policy deny rule.
    pub policy_denials_total: CounterVec,
    /// Rate-limit rejections, labelled by scope: `global`, `per-tool`, or `policy`.
    pub rate_limit_hits_total: CounterVec,
    /// Current circuit breaker state per tool: 0=closed, 1=open, 2=half-open.
    pub circuit_breaker_state: GaugeVec,
    /// In-flight tool calls currently awaiting an upstream response.
    pub active_sessions: IntGauge,
}

static GLOBAL: OnceLock<Metrics> = OnceLock::new();

impl Metrics {
    fn new() -> Self {
        let registry = Registry::new();

        let tool_calls_total = CounterVec::new(
            Opts::new(
                "agentgate_tool_calls_total",
                "Total tool invocations by outcome",
            ),
            &["tool", "status"],
        )
        .expect("static metric definition is valid");

        let tool_call_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "agentgate_tool_call_duration_seconds",
                "Tool call round-trip latency in seconds",
            )
            .buckets(vec![0.005, 0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]),
            &["tool"],
        )
        .expect("static metric definition is valid");

        let policy_denials_total = CounterVec::new(
            Opts::new(
                "agentgate_policy_denials_total",
                "Tool calls blocked by policy deny rules",
            ),
            &["rule_id"],
        )
        .expect("static metric definition is valid");

        let rate_limit_hits_total = CounterVec::new(
            Opts::new(
                "agentgate_rate_limit_hits_total",
                "Rate-limit rejections by scope (global, per-tool, policy)",
            ),
            &["scope"],
        )
        .expect("static metric definition is valid");

        let circuit_breaker_state = GaugeVec::new(
            Opts::new(
                "agentgate_circuit_breaker_state",
                "Circuit breaker state per tool: 0=closed 1=open 2=half-open",
            ),
            &["tool"],
        )
        .expect("static metric definition is valid");

        let active_sessions = IntGauge::new(
            "agentgate_active_sessions",
            "In-flight tool calls awaiting an upstream response",
        )
        .expect("static metric definition is valid");

        registry
            .register(Box::new(tool_calls_total.clone()))
            .expect("registration");
        registry
            .register(Box::new(tool_call_duration_seconds.clone()))
            .expect("registration");
        registry
            .register(Box::new(policy_denials_total.clone()))
            .expect("registration");
        registry
            .register(Box::new(rate_limit_hits_total.clone()))
            .expect("registration");
        registry
            .register(Box::new(circuit_breaker_state.clone()))
            .expect("registration");
        registry
            .register(Box::new(active_sessions.clone()))
            .expect("registration");

        Self {
            registry,
            tool_calls_total,
            tool_call_duration_seconds,
            policy_denials_total,
            rate_limit_hits_total,
            circuit_breaker_state,
            active_sessions,
        }
    }

    pub fn render(&self) -> String {
        let encoder = TextEncoder::new();
        let families = self.registry.gather();
        let mut buf = Vec::new();
        if let Err(e) = encoder.encode(&families, &mut buf) {
            tracing::error!("Metrics encoding failed: {e}");
        }
        String::from_utf8(buf).unwrap_or_default()
    }
}

/// Returns the process-global `Metrics` instance, initialising it on first call.
pub fn global() -> &'static Metrics {
    GLOBAL.get_or_init(Metrics::new)
}

/// Axum handler: responds with Prometheus text-format metrics.
pub async fn metrics_handler() -> impl IntoResponse {
    let body = global().render();
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

/// Convert a `CircuitStateKind` to the numeric gauge value defined in the metric help text.
pub fn circuit_state_to_f64(state: crate::ratelimit::CircuitStateKind) -> f64 {
    match state {
        crate::ratelimit::CircuitStateKind::Closed => 0.0,
        crate::ratelimit::CircuitStateKind::Open => 1.0,
        crate::ratelimit::CircuitStateKind::HalfOpen => 2.0,
    }
}
