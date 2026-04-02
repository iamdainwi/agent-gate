use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::sync::broadcast;
use tokio::sync::mpsc::{self, Sender};

/// Payloads larger than this are truncated before storage to prevent database bloat.
const MAX_PAYLOAD_BYTES: usize = 2048;

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

/// Capacity of the storage write channel. Records are dropped (with a warning) when full
/// rather than allowing unbounded memory growth under I/O backpressure.
const STORAGE_CHANNEL_CAPACITY: usize = 10_000;

/// Broadcast capacity for the live event stream. Slow WebSocket receivers that fall
/// behind by more than this many records will receive `RecvError::Lagged`.
const LIVE_BROADCAST_CAPACITY: usize = 512;

/// Non-blocking writer that queues records to a background SQLite writer task and
/// broadcasts each persisted record to live WebSocket subscribers.
#[derive(Clone)]
pub struct StorageWriter {
    tx: Sender<InvocationRecord>,
    live_tx: broadcast::Sender<InvocationRecord>,
}

impl StorageWriter {
    /// Spawns a background task that drains the channel and writes to SQLite.
    pub fn spawn(db_path: PathBuf) -> Result<Self> {
        let (tx, mut rx) = mpsc::channel::<InvocationRecord>(STORAGE_CHANNEL_CAPACITY);
        let (live_tx, _) = broadcast::channel::<InvocationRecord>(LIVE_BROADCAST_CAPACITY);
        let live_tx_bg = live_tx.clone();

        tokio::task::spawn_blocking(move || {
            let conn = open_and_migrate(&db_path)?;

            let rt = tokio::runtime::Handle::current();
            rt.block_on(async move {
                while let Some(record) = rx.recv().await {
                    if let Err(e) = insert_record(&conn, &record) {
                        tracing::error!("SQLite insert failed: {e}");
                    }
                    // Silently drop if no WebSocket subscribers are connected.
                    let _ = live_tx_bg.send(record);
                }
            });

            Ok::<_, anyhow::Error>(())
        });

        Ok(Self { tx, live_tx })
    }

    /// Enqueue a record for persistence. Drops the record (with a warning) if the
    /// channel is full — never blocks the proxy hot path.
    pub fn record(&self, record: InvocationRecord) {
        match self.tx.try_send(record) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                tracing::warn!("Storage channel full; invocation record dropped");
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                tracing::warn!("Storage writer channel closed; record dropped");
            }
        }
    }

    /// Subscribe to the live record stream. Each new persisted record is broadcast
    /// to all active subscribers.
    pub fn subscribe(&self) -> broadcast::Receiver<InvocationRecord> {
        self.live_tx.subscribe()
    }

    /// Return a clone of the broadcast sender for passing to dashboard state.
    pub fn live_sender(&self) -> broadcast::Sender<InvocationRecord> {
        self.live_tx.clone()
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

/// Open a WAL-mode SQLite connection at `db_path`, applying the schema if needed.
/// Exposed to the dashboard API module so it can open its own read connections.
pub fn open_connection(db_path: &Path) -> Result<Connection> {
    open_and_migrate(db_path)
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

fn truncate_payload(s: String) -> String {
    if s.len() <= MAX_PAYLOAD_BYTES {
        return s;
    }
    // Find the last valid UTF-8 char boundary at or before the limit.
    let mut boundary = MAX_PAYLOAD_BYTES;
    while !s.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}...[truncated]", &s[..boundary])
}

fn insert_record(conn: &Connection, r: &InvocationRecord) -> Result<()> {
    let arguments = r
        .arguments
        .as_ref()
        .map(|v| truncate_payload(v.to_string()));
    let result = r.result.as_ref().map(|v| truncate_payload(v.to_string()));

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
            arguments,
            result,
            r.latency_ms,
            r.status.as_str(),
            r.policy_hit,
        ],
    )
    .context("INSERT into tool_invocations failed")?;
    Ok(())
}

pub fn row_to_record(row: &rusqlite::Row) -> rusqlite::Result<InvocationRecord> {
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
