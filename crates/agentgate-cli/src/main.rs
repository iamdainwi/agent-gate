use agentgate_core::config::{agentgate_dir, AgentGateConfig, ServerEntry, TransportKind};
use agentgate_core::dashboard::{spawn_dashboard, DashboardState};
use agentgate_core::policy::PolicyEngine;
use agentgate_core::proxy::http::HttpProxy;
use agentgate_core::proxy::sse::SseProxy;
use agentgate_core::proxy::stdio::StdioProxy;
use agentgate_core::ratelimit::{CircuitBreaker, RateLimiter};
use agentgate_core::storage::{InvocationFilter, StorageReader, StorageWriter};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::sync::Arc;
use tabled::{Table, Tabled};

#[derive(Parser)]
#[command(
    name = "agentgate",
    about = "AI Agent Security & Observability Gateway"
)]
struct Cli {
    /// Path to a config TOML file [default: ~/.agentgate/config.toml]
    #[arg(long, global = true)]
    config: Option<std::path::PathBuf>,

    /// Override the database path
    #[arg(long, global = true)]
    db: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Wrap a stdio MCP server, proxying and logging all tool calls
    Wrap {
        /// Path to a TOML policy file
        #[arg(long)]
        policy: Option<std::path::PathBuf>,
        /// Expose Prometheus metrics on this port (e.g. 9090)
        #[arg(long)]
        metrics_port: Option<u16>,
        /// Port for the dashboard UI and REST API (default: 7070)
        #[arg(long)]
        dashboard_port: Option<u16>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Start an SSE or HTTP transport proxy
    Serve {
        /// Transport kind: sse or http
        #[arg(long)]
        transport: String,
        /// Upstream URL to proxy to
        #[arg(long)]
        upstream: String,
        /// Local port to bind
        #[arg(long, default_value = "7072")]
        port: u16,
        /// Extra request header in `Key: Value` format (repeatable)
        #[arg(long = "header", value_name = "KEY:VALUE")]
        headers: Vec<String>,
        /// Path to a TOML policy file
        #[arg(long)]
        policy: Option<std::path::PathBuf>,
        /// Port for the dashboard UI and REST API (default: 7070)
        #[arg(long)]
        dashboard_port: Option<u16>,
    },
    /// Query and display logged tool invocations
    Logs {
        #[arg(long)]
        tool: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value = "50")]
        limit: usize,
        #[arg(long)]
        jsonl: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    let mut config = load_config(cli.config.as_deref());
    if let Some(db) = cli.db {
        config.db_path = db;
    }

    match cli.command {
        Commands::Wrap {
            policy,
            metrics_port,
            dashboard_port,
            command,
        } => {
            if command.is_empty() {
                eprintln!("error: no command specified. Usage: agentgate wrap -- <cmd> [args...]");
                std::process::exit(1);
            }
            let (cmd, args) = command.split_first().expect("non-empty");
            config.server_name = std::path::Path::new(cmd)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(cmd)
                .to_string();
            if policy.is_some() {
                config.policy_path = policy;
            }
            if metrics_port.is_some() {
                config.metrics_port = metrics_port;
            }
            if dashboard_port.is_some() {
                config.dashboard_port = dashboard_port;
            }
            StdioProxy::new(config).run(cmd, args).await?;
        }

        Commands::Serve {
            transport,
            upstream,
            port,
            headers,
            policy,
            dashboard_port,
        } => {
            let kind = match transport.as_str() {
                "sse" => TransportKind::Sse,
                "http" => TransportKind::Http,
                other => bail!("Unknown transport '{other}'. Valid values: sse, http"),
            };

            let mut header_map = std::collections::HashMap::new();
            for h in &headers {
                let (k, v) = h
                    .split_once(':')
                    .with_context(|| format!("Invalid header '{h}'. Use Key: Value format"))?;
                header_map.insert(k.trim().to_string(), v.trim().to_string());
            }

            let entry = ServerEntry {
                name: "cli".to_string(),
                transport: kind.clone(),
                command: None,
                args: vec![],
                url: Some(upstream),
                headers: header_map,
                bind_port: Some(port),
            };

            if policy.is_some() {
                config.policy_path = policy;
            }
            if dashboard_port.is_some() {
                config.dashboard_port = dashboard_port;
            }

            let (policy_engine, rate_limiter, circuit_breaker, storage) =
                build_shared_components(&config)?;

            let dash_state = DashboardState {
                db_path: config.db_path.clone(),
                policy_path: config.policy_path.clone(),
                policy_engine: policy_engine.clone(),
                live_tx: storage.live_sender(),
            };
            spawn_dashboard(dash_state, config.dashboard_port.unwrap_or(7070))?;

            match kind {
                TransportKind::Sse => {
                    SseProxy::new(
                        &entry,
                        policy_engine,
                        rate_limiter,
                        circuit_breaker,
                        storage,
                    )?
                    .run()
                    .await?;
                }
                TransportKind::Http => {
                    HttpProxy::new(
                        &entry,
                        policy_engine,
                        rate_limiter,
                        circuit_breaker,
                        storage,
                    )?
                    .run()
                    .await?;
                }
                TransportKind::Stdio => bail!("Use `agentgate wrap` for stdio transport"),
            }
        }

        Commands::Logs {
            tool,
            status,
            limit,
            jsonl,
        } => {
            let reader = StorageReader::open(&config.db_path)?;
            let filter = InvocationFilter {
                tool,
                status,
                limit,
            };
            if jsonl {
                reader.export_jsonl(&filter, &mut std::io::stdout())?;
            } else {
                let records = reader.query(&filter)?;
                if records.is_empty() {
                    println!("No invocations found.");
                    return Ok(());
                }
                print_table(&records);
            }
        }
    }

    Ok(())
}

type SharedComponents = (
    Option<Arc<PolicyEngine>>,
    Arc<RateLimiter>,
    Arc<CircuitBreaker>,
    StorageWriter,
);

fn build_shared_components(config: &AgentGateConfig) -> Result<SharedComponents> {
    let policy = config
        .policy_path
        .as_deref()
        .map(|p| {
            let e = PolicyEngine::load(p)?;
            PolicyEngine::spawn_watcher(Arc::clone(&e), p.to_path_buf());
            Ok::<_, anyhow::Error>(e)
        })
        .transpose()?;

    let rate_limiter = Arc::new(RateLimiter::new(config.rate_limits.clone()));
    let circuit_breaker = Arc::new(CircuitBreaker::new(config.circuit_breaker.clone()));
    let storage = StorageWriter::spawn(config.db_path.clone())?;

    Ok((policy, rate_limiter, circuit_breaker, storage))
}

fn load_config(explicit: Option<&std::path::Path>) -> AgentGateConfig {
    let path = explicit
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| agentgate_dir().join("config.toml"));

    if path.exists() {
        match AgentGateConfig::load_toml(&path) {
            Ok(c) => return c,
            Err(e) => tracing::warn!("Failed to load config {}: {e}", path.display()),
        }
    }

    AgentGateConfig::default()
}

#[derive(Tabled)]
struct InvocationRow {
    #[tabled(rename = "Timestamp")]
    timestamp: String,
    #[tabled(rename = "Server")]
    server_name: String,
    #[tabled(rename = "Tool")]
    tool_name: String,
    #[tabled(rename = "Status")]
    status: String,
    #[tabled(rename = "Latency (ms)")]
    latency_ms: String,
    #[tabled(rename = "Policy Hit")]
    policy_hit: String,
}

fn print_table(records: &[agentgate_core::storage::InvocationRecord]) {
    let rows: Vec<InvocationRow> = records
        .iter()
        .map(|r| InvocationRow {
            timestamp: r.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
            server_name: r.server_name.clone(),
            tool_name: r.tool_name.clone(),
            status: r.status.as_str().to_string(),
            latency_ms: r
                .latency_ms
                .map(|l| l.to_string())
                .unwrap_or_else(|| "-".to_string()),
            policy_hit: r.policy_hit.clone().unwrap_or_else(|| "-".to_string()),
        })
        .collect();
    println!("{}", Table::new(rows));
}
