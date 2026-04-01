use crate::protocol::jsonrpc::JsonRpcMessage;
use chrono::{DateTime, Utc};

/// Direction of a proxied message
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Agent → proxy (incoming from MCP client)
    Inbound,
    /// Backend (MCP server) → proxy → agent
    Response,
}

impl Direction {
    pub fn label(&self) -> &'static str {
        match self {
            Direction::Inbound => "INBOUND ",
            Direction::Response => "RESPONSE",
        }
    }
}

/// A single proxied log event
pub struct LogEvent {
    pub timestamp: DateTime<Utc>,
    pub direction: Direction,
    pub message: JsonRpcMessage,
    pub raw: String,
}

/// Emit a structured log line to stderr.
pub fn log_event(event: &LogEvent) {
    let ts = event.timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ");
    let dir = event.direction.label();
    let id = event.message.id_label();
    let method_or_response = match &event.message {
        JsonRpcMessage::Request(r) => format!("method={}", r.method),
        JsonRpcMessage::Response(_) => "response".to_string(),
    };
    eprintln!("[agentgate] {ts} [{dir}] id={id} {method_or_response}");
}
