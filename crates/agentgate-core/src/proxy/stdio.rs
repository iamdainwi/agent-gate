use crate::config::AgentGateConfig;
use crate::logging::structured::{log_event, Direction, LogEvent};
use crate::protocol::jsonrpc::JsonRpcMessage;
use anyhow::{Context, Result};
use chrono::Utc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

pub struct StdioProxy {
    config: AgentGateConfig,
}

impl StdioProxy {
    pub fn new(config: AgentGateConfig) -> Self {
        Self { config }
    }

    /// Spawn `command` with `args`, then proxy stdin↔stdout between the parent process
    /// and the child, parsing and logging every JSON-RPC message.
    pub async fn run(&self, command: &str, args: &[String]) -> Result<()> {
        tracing::info!(
            log_level = %self.config.log_level,
            "Starting stdio proxy for command: {} {:?}",
            command,
            args
        );

        let mut child = Command::new(command)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to spawn command: {command}"))?;

        let child_stdin = child.stdin.take().expect("child stdin is piped");
        let child_stdout = child.stdout.take().expect("child stdout is piped");
        let child_stderr = child.stderr.take().expect("child stderr is piped");

        // Task A: our stdin → child stdin (inbound)
        let task_a = tokio::spawn(proxy_inbound(child_stdin));

        // Task B: child stdout → our stdout (response)
        let task_b = tokio::spawn(proxy_response(child_stdout));

        // Task C: pipe child stderr → our stderr
        let task_c = tokio::spawn(pipe_stderr(child_stderr));

        // Wait for the child to exit
        let status = child
            .wait()
            .await
            .context("Failed to wait for child process")?;

        // Give tasks a moment to flush remaining data, then abort
        // (they naturally finish when their streams hit EOF)
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task_a).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task_b).await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task_c).await;

        if !status.success() {
            let code = status.code().unwrap_or(1);
            std::process::exit(code);
        }

        Ok(())
    }
}

/// Read lines from our (process) stdin, parse JSON-RPC, log, and write to child stdin.
async fn proxy_inbound(mut child_stdin: tokio::process::ChildStdin) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    while let Some(line) = reader.next_line().await? {
        if line.is_empty() {
            continue;
        }

        match JsonRpcMessage::parse(&line) {
            Ok(msg) => {
                let event = LogEvent {
                    timestamp: Utc::now(),
                    direction: Direction::Inbound,
                    message: msg,
                    raw: line.clone(),
                };
                log_event(&event);
            }
            Err(e) => {
                tracing::warn!("Failed to parse inbound line as JSON-RPC: {e}. Forwarding raw.");
            }
        }

        // Forward the line (with newline) to the child regardless of parse result
        child_stdin.write_all(line.as_bytes()).await?;
        child_stdin.write_all(b"\n").await?;
        child_stdin.flush().await?;
    }

    Ok(())
}

/// Read lines from child stdout, parse JSON-RPC, log, and write to our stdout.
async fn proxy_response(child_stdout: tokio::process::ChildStdout) -> Result<()> {
    let mut reader = BufReader::new(child_stdout).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = reader.next_line().await? {
        if line.is_empty() {
            continue;
        }

        match JsonRpcMessage::parse(&line) {
            Ok(msg) => {
                let event = LogEvent {
                    timestamp: Utc::now(),
                    direction: Direction::Response,
                    message: msg,
                    raw: line.clone(),
                };
                log_event(&event);
            }
            Err(e) => {
                tracing::warn!("Failed to parse response line as JSON-RPC: {e}. Forwarding raw.");
            }
        }

        stdout.write_all(line.as_bytes()).await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }

    Ok(())
}

/// Pipe child stderr directly to our stderr.
async fn pipe_stderr(child_stderr: tokio::process::ChildStderr) -> Result<()> {
    let mut reader = BufReader::new(child_stderr).lines();
    let mut stderr = tokio::io::stderr();

    while let Some(line) = reader.next_line().await? {
        stderr.write_all(line.as_bytes()).await?;
        stderr.write_all(b"\n").await?;
        stderr.flush().await?;
    }

    Ok(())
}
