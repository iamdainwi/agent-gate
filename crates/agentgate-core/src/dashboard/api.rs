use super::state::DashboardState;
use crate::storage::sqlite::{open_connection, row_to_record};
use anyhow::Context;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// ── query param structs ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct InvocationQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub tool: Option<String>,
    pub status: Option<String>,
}

// ── response types ───────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct OverviewStats {
    pub total_calls: i64,
    pub total_denials: i64,
    pub avg_latency_ms: Option<f64>,
    pub calls_per_minute_now: f64,
    pub sparkline: Vec<SparklinePoint>,
}

#[derive(Serialize)]
pub struct SparklinePoint {
    pub bucket: String,
    pub count: i64,
}

#[derive(Serialize)]
pub struct ToolStat {
    pub tool_name: String,
    pub total_calls: i64,
    pub error_count: i64,
    pub denial_count: i64,
    pub avg_latency_ms: Option<f64>,
    pub last_seen: String,
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn db_err(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    tracing::error!("DB error: {e}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "Internal server error" })),
    )
}

// ── handlers ─────────────────────────────────────────────────────────────────

pub async fn get_invocations(
    State(state): State<DashboardState>,
    Query(q): Query<InvocationQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(50).min(500);
    let offset = q.offset.unwrap_or(0);
    let filter_tool = q.tool.clone();
    let filter_status = q.status.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = open_connection(&state.db_path)?;
        let mut conditions: Vec<&'static str> = Vec::new();

        if filter_tool.is_some() {
            conditions.push("tool_name = ?");
        }
        if filter_status.is_some() {
            conditions.push("status = ?");
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            "SELECT id, timestamp, agent_id, session_id, server_name, tool_name,
                    arguments, result, latency_ms, status, policy_hit
             FROM tool_invocations {where_clause}
             ORDER BY timestamp DESC LIMIT ? OFFSET ?"
        );

        let mut stmt = conn.prepare(&sql)?;
        // Parameterize LIMIT/OFFSET (appended after filter params) to prevent injection.
        let limit_i64 = limit as i64;
        let offset_i64 = offset as i64;
        let rows = match (&filter_tool, &filter_status) {
            (Some(t), Some(s)) => stmt.query_map(rusqlite::params![t, s, limit_i64, offset_i64], row_to_record)?,
            (Some(t), None) => stmt.query_map(rusqlite::params![t, limit_i64, offset_i64], row_to_record)?,
            (None, Some(s)) => stmt.query_map(rusqlite::params![s, limit_i64, offset_i64], row_to_record)?,
            (None, None) => stmt.query_map(rusqlite::params![limit_i64, offset_i64], row_to_record)?,
        };
        rows.collect::<Result<Vec<_>, _>>().context("query")
    })
    .await;

    match result {
        Ok(Ok(records)) => Json(json!(records)).into_response(),
        Ok(Err(e)) => db_err(e).into_response(),
        Err(e) => db_err(e).into_response(),
    }
}

pub async fn get_invocation_by_id(
    State(state): State<DashboardState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let result = tokio::task::spawn_blocking(move || {
        let conn = open_connection(&state.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT id, timestamp, agent_id, session_id, server_name, tool_name,
                    arguments, result, latency_ms, status, policy_hit
             FROM tool_invocations WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], row_to_record)?;
        Ok::<_, anyhow::Error>(rows.next().transpose()?)
    })
    .await;

    match result {
        Ok(Ok(Some(record))) => Json(record).into_response(),
        Ok(Ok(None)) => {
            (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response()
        }
        Ok(Err(e)) => db_err(e).into_response(),
        Err(e) => db_err(e).into_response(),
    }
}

pub async fn get_stats_overview(State(state): State<DashboardState>) -> impl IntoResponse {
    let result = tokio::task::spawn_blocking(move || {
        let conn = open_connection(&state.db_path)?;

        let (total_calls, total_denials, avg_latency_ms): (i64, i64, Option<f64>) = conn
            .query_row(
                "SELECT
                    COUNT(*),
                    SUM(CASE WHEN status IN ('denied','rate_limited') THEN 1 ELSE 0 END),
                    AVG(CAST(latency_ms AS REAL))
                 FROM tool_invocations",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;

        let mut sparkline: Vec<SparklinePoint> = Vec::new();
        let mut stmt = conn.prepare(
            "SELECT strftime('%Y-%m-%dT%H:%M:00Z', timestamp) AS bucket, COUNT(*)
             FROM tool_invocations
             WHERE timestamp >= datetime('now', '-60 minutes')
             GROUP BY bucket ORDER BY bucket ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (bucket, count) = row?;
            sparkline.push(SparklinePoint { bucket, count });
        }

        let calls_per_minute_now = sparkline.last().map(|p| p.count as f64).unwrap_or(0.0);

        Ok::<_, anyhow::Error>(OverviewStats {
            total_calls,
            total_denials,
            avg_latency_ms,
            calls_per_minute_now,
            sparkline,
        })
    })
    .await;

    match result {
        Ok(Ok(stats)) => Json(stats).into_response(),
        Ok(Err(e)) => db_err(e).into_response(),
        Err(e) => db_err(e).into_response(),
    }
}

pub async fn get_stats_tools(State(state): State<DashboardState>) -> impl IntoResponse {
    let result = tokio::task::spawn_blocking(move || {
        let conn = open_connection(&state.db_path)?;
        let mut stmt = conn.prepare(
            "SELECT
                tool_name,
                COUNT(*)                                                 AS total_calls,
                SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END)       AS error_count,
                SUM(CASE WHEN status IN ('denied','rate_limited') THEN 1 ELSE 0 END) AS denial_count,
                AVG(CAST(latency_ms AS REAL))                           AS avg_latency_ms,
                MAX(timestamp)                                           AS last_seen
             FROM tool_invocations
             GROUP BY tool_name
             ORDER BY total_calls DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ToolStat {
                tool_name: row.get(0)?,
                total_calls: row.get(1)?,
                error_count: row.get(2)?,
                denial_count: row.get(3)?,
                avg_latency_ms: row.get(4)?,
                last_seen: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().context("query")
    })
    .await;

    match result {
        Ok(Ok(stats)) => Json(stats).into_response(),
        Ok(Err(e)) => db_err(e).into_response(),
        Err(e) => db_err(e).into_response(),
    }
}

/// Agent-level stats are not yet tracked (no agent_id population). Returns an empty array.
pub async fn get_stats_agents() -> impl IntoResponse {
    Json(json!([])).into_response()
}

pub async fn get_policies(State(state): State<DashboardState>) -> impl IntoResponse {
    let Some(path) = &state.policy_path else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "No policy file configured" })),
        )
            .into_response();
    };

    match std::fs::read_to_string(path) {
        Ok(content) => (StatusCode::OK, content).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn put_policies(State(state): State<DashboardState>, body: String) -> impl IntoResponse {
    let Some(path) = &state.policy_path else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "No policy file configured — start AgentGate with --policy" })),
        )
            .into_response();
    };

    // Validate before writing.
    if let Err(e) = toml::from_str::<crate::policy::rules::PolicyFile>(&body) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Invalid policy TOML: {e}") })),
        )
            .into_response();
    }

    if let Err(e) = std::fs::write(path, &body) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response();
    }

    if let Some(engine) = &state.policy_engine {
        if let Err(e) = engine.reload(path) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Policy reload failed: {e}") })),
            )
                .into_response();
        }
    }

    Json(json!({ "ok": true })).into_response()
}
