use agentgate_core::config::AgentGateConfig;
use agentgate_core::proxy::stdio::StdioProxy;
use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "agentgate",
    about = "AI Agent Security & Observability Gateway"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Wrap an MCP server process, proxying and logging all tool calls
    Wrap {
        /// The command and arguments to run (after --)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Wrap { command } => {
            if command.is_empty() {
                eprintln!("error: no command specified. Usage: agentgate wrap -- <cmd> [args...]");
                std::process::exit(1);
            }

            let (cmd, args) = command.split_first().expect("non-empty checked above");
            let config = AgentGateConfig::default();
            let proxy = StdioProxy::new(config);
            proxy.run(cmd, args).await?;
        }
    }

    Ok(())
}
