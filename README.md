# AgentGate

**A transparent security & observability gateway for AI agents.**

AgentGate sits between AI coding agents (Claude Code, Cursor, Codex, Devin) and the MCP servers they call — logging every tool invocation to a local SQLite database with zero behavior change and sub-millisecond overhead.

```
┌──────────┐      ┌──────────────┐      ┌────────────┐
│ AI Agent │ ───▶ │  AgentGate   │ ───▶ │ MCP Server │
│          │ ◀─── │  (proxy)     │ ◀─── │            │
└──────────┘      └──────────────┘      └────────────┘
                    │
                    ▼
               ┌──────────┐
               │  SQLite  │
               │ logs.db  │
               └──────────┘
```

## Why

AI agents run tool calls autonomously — reading files, executing shell commands, making API requests. Today there's no unified way to answer:

- **"What did the agent actually do?"** — No audit trail across tool calls.
- **"Can I restrict what the agent is allowed to do?"** — No policy layer between agents and tools. _(coming soon)_
- **"Why did my API bill spike?"** — No rate limiting for agent-initiated calls. _(coming soon)_

AgentGate gives you full visibility into agent behavior with a single command.

## Quick Start

### Install

```bash
cargo install agentgate
```

### Usage

Wrap any MCP server — AgentGate proxies stdin/stdout transparently:

```bash
agentgate wrap -- npx @modelcontextprotocol/server-filesystem /tmp
```

That's it. Every `tools/call` invocation is now logged to `~/.agentgate/logs.db`.

### Query Logs

```bash
# Show the last 50 tool invocations
agentgate logs

# Filter by tool name
agentgate logs --tool read_file

# Filter by status
agentgate logs --status error

# Export as newline-delimited JSON
agentgate logs --jsonl

# Custom database path
agentgate logs --db /path/to/logs.db --limit 100
```

### Example Output

```
+---------------------+------------+-----------+---------+-------------+------------+
| Timestamp           | Server     | Tool      | Status  | Latency (ms)| Policy Hit |
+---------------------+------------+-----------+---------+-------------+------------+
| 2026-04-02 14:32:01 | npx        | read_file | allowed | 12          | -          |
| 2026-04-02 14:32:03 | npx        | bash      | allowed | 847         | -          |
| 2026-04-02 14:32:05 | npx        | write_file| allowed | 8           | -          |
+---------------------+------------+-----------+---------+-------------+------------+
```

## How It Works

AgentGate spawns the target MCP server as a child process and intercepts the stdin/stdout JSON-RPC 2.0 message stream:

1. **Inbound** — Messages from the AI agent are parsed, logged, and forwarded to the MCP server unchanged.
2. **Response** — Messages from the MCP server are parsed, correlated with their originating request (for latency tracking), logged, and forwarded to the agent unchanged.
3. **Persistence** — Every `tools/call` invocation is written to SQLite asynchronously on a background thread, adding zero latency to the proxy hot path.

### What Gets Logged

For each `tools/call` request-response pair:

| Field         | Description                                          |
| ------------- | ---------------------------------------------------- |
| `id`          | Unique invocation UUID                               |
| `timestamp`   | When the response was received                       |
| `server_name` | Name of the MCP server binary                        |
| `tool_name`   | The tool that was called (e.g., `read_file`, `bash`) |
| `arguments`   | JSON arguments passed to the tool                    |
| `result`      | JSON result returned by the tool                     |
| `latency_ms`  | Round-trip time in milliseconds                      |
| `status`      | `allowed`, `denied`, `error`, or `rate_limited`      |
| `policy_hit`  | Which policy rule matched (if any)                   |

## Project Structure

```
agentgate/
├── crates/
│   ├── agentgate-core/          # Library: proxy, protocol, storage, logging
│   │   └── src/
│   │       ├── config.rs        # Configuration types
│   │       ├── protocol/
│   │       │   ├── jsonrpc.rs   # JSON-RPC 2.0 parser
│   │       │   └── mcp.rs      # MCP message types
│   │       ├── proxy/
│   │       │   └── stdio.rs    # Stdio transport proxy
│   │       ├── logging/
│   │       │   └── structured.rs # Structured stderr logger
│   │       └── storage/         # SQLite persistence
│   └── agentgate-cli/           # Binary: CLI interface
│       └── src/
│           └── main.rs          # wrap + logs commands
├── policies/
│   └── default.toml             # Default policy (Phase 2)
└── .github/
    └── workflows/
        └── ci.yml               # Clippy, rustfmt, tests
```

## Roadmap

- [x] **Phase 0** — MCP stdio proxy with structured logging
- [x] **Phase 1** — SQLite persistence, CLI log queries, JSONL export
- [ ] **Phase 2** — Declarative TOML policy engine (deny/allow/redact rules)
- [ ] **Phase 3** — Rate limiting & circuit breaker
- [ ] **Phase 4** — SSE & HTTP transport support
- [ ] **Phase 5** — OpenTelemetry metrics export
- [ ] **Phase 6** — Real-time dashboard (Next.js)
- [ ] **Phase 7** — Distribution (Docker, Homebrew, installer)

## Tech Stack

| Component | Technology                 |
| --------- | -------------------------- |
| Core      | Rust, Tokio, Serde         |
| Protocol  | JSON-RPC 2.0, MCP          |
| Storage   | SQLite (rusqlite, bundled) |
| CLI       | Clap, Tabled               |

## Building from Source

```bash
git clone https://github.com/iamdainwi/AgentGate.git
cd AgentGate
cargo build --release
```

The binary is at `target/release/agentgate`.

## Configuration

AgentGate uses sensible defaults and requires zero configuration. Optional overrides:

| Option        | Default                | Description              |
| ------------- | ---------------------- | ------------------------ |
| `--db <path>` | `~/.agentgate/logs.db` | SQLite database location |

Environment variables:

| Variable   | Description                                            |
| ---------- | ------------------------------------------------------ |
| `RUST_LOG` | Log level for tracing output (`info`, `debug`, `warn`) |

## License

MIT

## Author

**Dainwi Choudhary** — [@iamdainwi](https://github.com/iamdainwi)

- [LinkedIn](https://www.linkedin.com/in/dainwi-choudhary/)
- [Portfolio](https://dainwi.vercel.app)
