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
    about = "AI Agent Security & Observability Gateway",
    version = env!("CARGO_PKG_VERSION"),
    long_about = None,
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
    /// Scaffold the default config and policy files in ~/.agentgate/
    Init,
    /// Check the AgentGate installation for common problems
    Doctor,
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

        Commands::Init => {
            run_init()?;
        }

        Commands::Doctor => {
            run_doctor(&config);
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

// ── Init ─────────────────────────────────────────────────────────────────────

const DEFAULT_CONFIG_TOML: &str = r#"# AgentGate configuration
log_level  = "info"
log_format = "pretty"

# Path where the audit SQLite database is stored.
# db_path = "~/.agentgate/logs.db"   # (default)

# Uncomment to load a policy file on startup.
# policy_path = "~/.agentgate/policies/default.toml"

# Port for the dashboard UI and REST API (default 7070).
# dashboard_port = 7070

[rate_limits]
global_max_calls_per_minute   = 500
per_tool_max_calls_per_minute = 100
per_agent_max_calls_per_minute = 200

[circuit_breaker]
error_threshold  = 5
window_seconds   = 30
cooldown_seconds = 60
"#;

const DEFAULT_POLICY_TOML: &str = r#"# AgentGate policy file
#
# Rules are evaluated top-to-bottom; the first match wins.
# Supported actions: "allow", "deny", "redact"
#
# Example — block the shell-execution tool entirely:
# [[rules]]
# tool    = "bash"
# action  = "deny"
# reason  = "Shell execution is not permitted"
#
# Example — redact AWS secrets from any tool response:
# [[rules]]
# action  = "redact"
# redact_patterns = ["AKIA[0-9A-Z]{16}", "(?i)aws_secret[^\\s]*"]
"#;

fn run_init() -> Result<()> {
    let dir = agentgate_dir();

    // Create the base directory.
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Cannot create directory: {}", dir.display()))?;

    // Write config.toml if it does not already exist.
    let config_path = dir.join("config.toml");
    if config_path.exists() {
        println!("  exists  {}", config_path.display());
    } else {
        std::fs::write(&config_path, DEFAULT_CONFIG_TOML)
            .with_context(|| format!("Cannot write {}", config_path.display()))?;
        println!("  created {}", config_path.display());
    }

    // Create the policies sub-directory.
    let policies_dir = dir.join("policies");
    std::fs::create_dir_all(&policies_dir)
        .with_context(|| format!("Cannot create directory: {}", policies_dir.display()))?;

    // Write the default policy file if it does not already exist.
    let policy_path = policies_dir.join("default.toml");
    if policy_path.exists() {
        println!("  exists  {}", policy_path.display());
    } else {
        std::fs::write(&policy_path, DEFAULT_POLICY_TOML)
            .with_context(|| format!("Cannot write {}", policy_path.display()))?;
        println!("  created {}", policy_path.display());
    }

    println!("\nAgentGate initialised. Edit the files above, then run `agentgate wrap -- <server>`.");
    Ok(())
}

// ── Doctor ───────────────────────────────────────────────────────────────────

fn run_doctor(config: &AgentGateConfig) {
    let mut all_ok = true;

    // 1. Config directory exists and is writable.
    let dir = agentgate_dir();
    let dir_ok = dir.exists() && is_writable(&dir);
    print_check(dir_ok, &format!("Config directory: {}", dir.display()));
    all_ok &= dir_ok;

    // 2. Config file parses cleanly (if it exists).
    let config_path = dir.join("config.toml");
    if config_path.exists() {
        let parse_ok = AgentGateConfig::load_toml(&config_path).is_ok();
        print_check(parse_ok, &format!("config.toml is valid TOML: {}", config_path.display()));
        all_ok &= parse_ok;
    } else {
        print_check(false, &format!("config.toml not found (run `agentgate init`): {}", config_path.display()));
        all_ok = false;
    }

    // 3. Database path is writable (try creating the parent dir and touching a temp file).
    let db_ok = check_db_writable(&config.db_path);
    print_check(db_ok, &format!("DB path is writable: {}", config.db_path.display()));
    all_ok &= db_ok;

    // 4. Dashboard port (default 7070) is available.
    let dash_port = config.dashboard_port.unwrap_or(7070);
    let dash_ok = port_available(dash_port);
    print_check(
        dash_ok,
        &format!("Dashboard port {dash_port} is available (or already in use by AgentGate)"),
    );
    // Port in use is a warning, not a hard failure — AgentGate itself may already be running.

    // 5. Policy file parses cleanly (if configured).
    if let Some(ref policy_path) = config.policy_path {
        if policy_path.exists() {
            let ok = agentgate_core::policy::PolicyEngine::load(policy_path).is_ok();
            print_check(ok, &format!("Policy file is valid: {}", policy_path.display()));
            all_ok &= ok;
        } else {
            print_check(
                false,
                &format!("Policy file not found: {}", policy_path.display()),
            );
            all_ok = false;
        }
    }

    println!();
    if all_ok {
        println!("All checks passed.");
    } else {
        println!("One or more checks failed. Fix the issues above and re-run `agentgate doctor`.");
        std::process::exit(1);
    }
}

fn print_check(ok: bool, msg: &str) {
    let mark = if ok { "[ok]" } else { "[!!]" };
    println!("  {mark}  {msg}");
}

fn is_writable(path: &std::path::Path) -> bool {
    // Attempt to create a temp file inside the directory.
    let probe = path.join(".agentgate_write_probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn check_db_writable(db_path: &std::path::Path) -> bool {
    if let Some(parent) = db_path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
        is_writable(parent)
    } else {
        false
    }
}

fn port_available(port: u16) -> bool {
    std::net::TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], port))).is_ok()
}
