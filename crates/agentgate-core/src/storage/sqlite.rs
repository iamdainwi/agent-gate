use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc::{self, UnboundedSender};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tool_invocations (
    id          TEXT PRIMARY KEY,
    timestamp   TEXT NOT NULL,
    agent_id    TEXT,
    session_id  TEXT,
    server_name TEXT NOT NULL,
    tool_name   TEXT NOT NULL,
    arguments   TEXT,
    result      TEXT,
    latency_ms  INTEGER,
    status      TEXT NOT NULL,
    policy_hit  TEXT
);
CREATE INDEX IF NOT EXISTS idx_invocations_ts     ON tool_invocations(timestamp);
CREATE INDEX IF NOT EXISTS idx_invocations_tool   ON tool_invocations(tool_name);
CREATE INDEX IF NOT EXISTS idx_invocations_status ON tool_invocations(status);
";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InvocationStatus {
    Allowed,
    Denied,
    Error,
    RateLimited,
}

impl InvocationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            InvocationStatus::Allowed => "allowed",
            InvocationStatus::Denied => "denied",
            InvocationStatus::Error => "error",
            InvocationStatus::RateLimited => "rate_limited",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "allowed" => Some(InvocationStatus::Allowed),
            "denied" => Some(InvocationStatus::Denied),
            "error" => Some(InvocationStatus::Error),
            "rate_limited" => Some(InvocationStatus::RateLimited),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationRecord {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub server_name: String,
    pub tool_name: String,
    pub arguments: Option<Value>,
    pub result: Option<Value>,
    pub latency_ms: Option<i64>,
    pub status: InvocationStatus,
    pub policy_hit: Option<String>,
}

/// Non-blocking writer that queues records to a background SQLite writer task.
#[derive(Clone)]
pub struct StorageWriter {
    tx: UnboundedSender<InvocationRecord>,
}

impl StorageWriter {
    /// Spawns a background tokio task that drains the channel and writes to SQLite.
    pub fn spawn(db_path: PathBuf) -> Result<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel::<InvocationRecord>();

        tokio::task::spawn_blocking(move || {
            let conn = open_and_migrate(&db_path)?;

            // Drain the channel synchronously — runs on the blocking thread pool.
            // Using a runtime handle to block on the async receiver.
            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                while let Some(record) = rx.recv().await {
                    if let Err(e) = insert_record(&conn, &record) {
                        tracing::error!("SQLite insert failed: {e}");
                    }
                }
            });

            Ok::<_, anyhow::Error>(())
        });

        Ok(Self { tx })
    }

    /// Enqueue a record for async persistence. Never blocks the caller.
    pub fn record(&self, record: InvocationRecord) {
        if self.tx.send(record).is_err() {
            tracing::warn!("Storage writer channel closed; record dropped");
        }
    }
}

/// Synchronous read interface used by `agentgate logs`.
pub struct StorageReader {
    conn: Connection,
}

#[derive(Debug, Default)]
pub struct InvocationFilter {
    pub tool: Option<String>,
    pub status: Option<String>,
    pub limit: usize,
}

impl StorageReader {
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = open_and_migrate(db_path)?;
        Ok(Self { conn })
    }

    pub fn query(&self, filter: &InvocationFilter) -> Result<Vec<InvocationRecord>> {
        let limit = if filter.limit == 0 { 50 } else { filter.limit };

        let mut conditions: Vec<String> = Vec::new();
        let mut positional: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(tool) = &filter.tool {
            conditions.push("tool_name = ?".to_string());
            positional.push(Box::new(tool.clone()));
        }
        if let Some(status) = &filter.status {
            conditions.push("status = ?".to_string());
            positional.push(Box::new(status.clone()));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql = format!(
            "SELECT id, timestamp, agent_id, session_id, server_name, tool_name,
                    arguments, result, latency_ms, status, policy_hit
             FROM tool_invocations
             {where_clause}
             ORDER BY timestamp DESC
             LIMIT {limit}"
        );

        let params_refs: Vec<&dyn rusqlite::ToSql> =
            positional.iter().map(|b| b.as_ref()).collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_refs.as_slice(), row_to_record)?;

        rows.collect::<Result<Vec<_>, _>>()
            .context("Failed to query invocations")
    }

    /// Export all records matching the filter as JSONL to the given writer.
    pub fn export_jsonl<W: std::io::Write>(
        &self,
        filter: &InvocationFilter,
        writer: &mut W,
    ) -> Result<()> {
        let records = self.query(filter)?;
        for record in &records {
            let line = serde_json::to_string(record)?;
            writeln!(writer, "{line}")?;
        }
        Ok(())
    }
}

fn open_and_migrate(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create db directory: {}", parent.display()))?;
    }

    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open SQLite db at {}", db_path.display()))?;

    conn.execute_batch(SCHEMA)
        .context("Failed to apply schema migration")?;

    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .context("Failed to configure SQLite pragmas")?;

    Ok(conn)
}

fn insert_record(conn: &Connection, r: &InvocationRecord) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO tool_invocations
         (id, timestamp, agent_id, session_id, server_name, tool_name,
          arguments, result, latency_ms, status, policy_hit)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            r.id,
            r.timestamp.to_rfc3339(),
            r.agent_id,
            r.session_id,
            r.server_name,
            r.tool_name,
            r.arguments.as_ref().map(|v| v.to_string()),
            r.result.as_ref().map(|v| v.to_string()),
            r.latency_ms,
            r.status.as_str(),
            r.policy_hit,
        ],
    )
    .context("INSERT into tool_invocations failed")?;
    Ok(())
}

fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<InvocationRecord> {
    let ts_str: String = row.get(1)?;
    let timestamp = DateTime::parse_from_rfc3339(&ts_str)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());

    let args_str: Option<String> = row.get(6)?;
    let result_str: Option<String> = row.get(7)?;
    let status_str: String = row.get(9)?;

    Ok(InvocationRecord {
        id: row.get(0)?,
        timestamp,
        agent_id: row.get(2)?,
        session_id: row.get(3)?,
        server_name: row.get(4)?,
        tool_name: row.get(5)?,
        arguments: args_str.and_then(|s| serde_json::from_str(&s).ok()),
        result: result_str.and_then(|s| serde_json::from_str(&s).ok()),
        latency_ms: row.get(8)?,
        status: InvocationStatus::parse(&status_str).unwrap_or(InvocationStatus::Allowed),
        policy_hit: row.get(10)?,
    })
}
