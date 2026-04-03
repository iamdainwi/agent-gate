use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentGateConfig {
    pub log_level: String,
    pub log_format: LogFormat,
    pub db_path: PathBuf,
    pub server_name: String,
    pub policy_path: Option<PathBuf>,
    /// When set, a Prometheus `/metrics` endpoint is exposed on this port.
    /// Required for the stdio transport, which has no built-in HTTP server.
    /// SSE and HTTP transports always expose `/metrics` on their own bind port.
    #[serde(default)]
    pub metrics_port: Option<u16>,
    /// Port for the REST/WebSocket API and dashboard UI. Defaults to 7070.
    #[serde(default)]
    pub dashboard_port: Option<u16>,
    #[serde(default)]
    pub rate_limits: RateLimitConfig,
    #[serde(default)]
    pub circuit_breaker: CircuitBreakerConfig,
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
    /// Log retention settings — prevents unbounded disk growth.
    #[serde(default)]
    pub log_retention: LogRetentionConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Pretty,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub global_max_calls_per_minute: u64,
    pub per_tool_max_calls_per_minute: u64,
    pub per_agent_max_calls_per_minute: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            global_max_calls_per_minute: 500,
            per_tool_max_calls_per_minute: 100,
            per_agent_max_calls_per_minute: 200,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    pub error_threshold: usize,
    pub window_seconds: u64,
    pub cooldown_seconds: u64,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            error_threshold: 5,
            window_seconds: 30,
            cooldown_seconds: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TransportKind {
    Stdio,
    Sse,
    Http,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    pub name: String,
    pub transport: TransportKind,
    /// Stdio transport: binary to spawn.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// SSE / HTTP transport: upstream base URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Extra request headers sent to the upstream (supports `${VAR}` expansion).
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Local port to bind for SSE / HTTP transports.
    #[serde(default)]
    pub bind_port: Option<u16>,
}

/// Controls automatic pruning of the audit log database to prevent unbounded disk growth.
/// A background task runs hourly and enforces both limits; the stricter one wins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRetentionConfig {
    /// Delete records older than this many days. 0 = disabled. Default: 30.
    pub retention_days: u64,
    /// Cap the total number of rows. Oldest rows are deleted first. 0 = disabled. Default: 500_000.
    pub max_rows: u64,
}

impl Default for LogRetentionConfig {
    fn default() -> Self {
        Self {
            retention_days: 30,
            max_rows: 500_000,
        }
    }
}

impl Default for AgentGateConfig {
    fn default() -> Self {
        Self {
            log_level: "info".to_string(),
            log_format: LogFormat::Pretty,
            db_path: agentgate_dir().join("logs.db"),
            server_name: "unknown".to_string(),
            policy_path: None,
            metrics_port: None,
            dashboard_port: None,
            rate_limits: RateLimitConfig::default(),
            circuit_breaker: CircuitBreakerConfig::default(),
            servers: Vec::new(),
            log_retention: LogRetentionConfig::default(),
        }
    }
}

impl AgentGateConfig {
    pub fn load_toml(path: &std::path::Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config: {}", path.display()))?;
        toml::from_str(&content).with_context(|| format!("Invalid config TOML: {}", path.display()))
    }
}

/// Expand `${VAR}` placeholders with values from the environment.
/// Un-resolvable variables are left as-is.
pub fn expand_env_vars(s: &str) -> String {
    let re = regex::Regex::new(r"\$\{([^}]+)\}").expect("static regex");
    re.replace_all(s, |caps: &regex::Captures| {
        std::env::var(&caps[1]).unwrap_or_else(|_| caps[0].to_string())
    })
    .into_owned()
}

pub fn agentgate_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".agentgate")
}
