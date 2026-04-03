# AgentGate

**Nginx/Kong for AI agents — a transparent security & observability gateway for MCP tool calls.**

AgentGate sits between AI coding agents (Claude Code, Cursor, Codex, Devin) and the MCP servers they call. It intercepts every `tools/call` invocation, enforces declarative TOML policies, rate-limits, and logs everything to a local SQLite database — with zero behavior change to the agent or server, and sub-millisecond overhead on the proxy hot path.

```
┌──────────┐      ┌─────────────────────────────┐      ┌────────────┐
│ AI Agent │ ───▶ │          AgentGate           │ ───▶ │ MCP Server │
│          │ ◀─── │  policy · rate-limit · audit │ ◀─── │            │
└──────────┘      └─────────────────────────────┘      └────────────┘
                              │
                 ┌────────────┼────────────┐
                 ▼            ▼            ▼
            ┌────────┐  ┌─────────┐  ┌──────────┐
            │SQLite  │  │Metrics  │  │Dashboard │
            │logs.db │  │:9090    │  │:7070     │
            └────────┘  └─────────┘  └──────────┘
```

## Why

AI agents run tool calls autonomously — reading files, executing shell commands, making API requests. Without a gateway layer there is no way to answer:

- **"What did the agent actually do?"** — no audit trail across tool calls
- **"Can I restrict what it's allowed to do?"** — no policy layer between agents and tools
- **"Why did my API bill spike?"** — no rate limiting for agent-initiated calls
- **"Is a secret leaking through tool output?"** — no redaction before the agent sees results

AgentGate solves all four with a single `agentgate wrap --` prefix.

## Quick Start

### Install

```bash
cargo install agentgate
```

### Wrap any MCP server

```bash
agentgate wrap -- npx @modelcontextprotocol/server-filesystem /tmp
```

Every `tools/call` is now logged to `~/.agentgate/logs.db`. The agent and server see no change.

### Query logs

```bash
agentgate logs                        # last 50 invocations
agentgate logs --tool read_file       # filter by tool
agentgate logs --status denied        # filter by outcome
agentgate logs --limit 200 --jsonl    # JSONL export
agentgate logs --db /path/to/logs.db  # custom database
```

### Example output

```
+---------------------+--------+-----------+-------------+-------------+------------+
| Timestamp           | Server | Tool      | Status      | Latency (ms)| Policy Hit |
+---------------------+--------+-----------+-------------+-------------+------------+
| 2026-04-03 09:01:12 | fs     | read_file | allowed     | 9           | -          |
| 2026-04-03 09:01:14 | fs     | bash      | denied      | 1           | no-shell   |
| 2026-04-03 09:01:17 | fs     | read_file | rate_limited| 0           | -          |
+---------------------+--------+-----------+-------------+-------------+------------+
```

## Policy Engine

Create a TOML policy file and pass it with `--policy`:

```toml
# policies/default.toml

[[rules]]
id       = "no-shell"
action   = "deny"
tool     = "bash"
reason   = "Shell access is disabled for this agent."

[[rules]]
id     = "rate-limit-search"
action = "rate_limit"
tool   = "search"
rate_limit = { max_calls = 10, window_seconds = 60 }

[[rules]]
id      = "redact-keys"
action  = "redact"
pattern = "sk-[A-Za-z0-9]{20,}"    # strips Anthropic/OpenAI keys from tool results

[[rules]]
id          = "allow-reads"
action      = "allow"
tool_prefix = "read_"
```

```bash
agentgate wrap --policy policies/default.toml -- npx @modelcontextprotocol/server-filesystem /tmp
```

Rules are evaluated top-to-bottom; first match wins. The policy file is hot-reloaded on change — no restart needed.

### Redaction

`redact` rules apply regex substitution to tool **results before they reach the agent**. Secrets are scrubbed at the gateway boundary, not just in stored logs.

## Metrics

Expose a Prometheus `/metrics` endpoint alongside the proxy:

```bash
agentgate wrap --metrics-port 9090 -- <mcp-server>
```

Six metrics are exported:

| Metric                                 | Type      | Description                                   |
| -------------------------------------- | --------- | --------------------------------------------- |
| `agentgate_tool_calls_total`           | Counter   | Tool calls by tool name and status            |
| `agentgate_tool_call_duration_seconds` | Histogram | Latency distribution per tool                 |
| `agentgate_policy_denials_total`       | Counter   | Policy denials by rule ID                     |
| `agentgate_rate_limit_hits_total`      | Counter   | Rate limit hits by scope                      |
| `agentgate_circuit_breaker_state`      | Gauge     | Circuit state (0=closed, 1=open, 2=half-open) |
| `agentgate_active_sessions`            | Gauge     | In-flight tool calls                          |

A ready-made Grafana dashboard is at `dashboards/grafana.json`.

## Dashboard

A Next.js 15 real-time dashboard is served on port 7070 by default:

```bash
agentgate wrap --dashboard-port 7070 -- <mcp-server>
# or
agentgate serve --transport sse --upstream http://localhost:3001 --dashboard-port 7070
```

Build the static UI first:

```bash
cd dashboard && npm install && npm run build
```

Pages:

| Page       | Path          | Description                                         |
| ---------- | ------------- | --------------------------------------------------- |
| Overview   | `/`           | KPI cards, call rate sparkline, live WebSocket feed |
| Activity   | `/activity`   | Filterable, paginated invocations table             |
| Violations | `/violations` | Denied/rate-limited calls grouped by policy rule    |
| Analytics  | `/analytics`  | Per-tool call volume, error rate, latency chart     |
| Settings   | `/settings`   | In-browser TOML policy editor with live reload      |

The dashboard WebSocket endpoint (`/api/ws/live`) streams every persisted invocation in real time.

## Transport Support

| Mode      | Command                                             | Use case                       |
| --------- | --------------------------------------------------- | ------------------------------ |
| **stdio** | `agentgate wrap -- <cmd>`                           | Any stdio MCP server           |
| **SSE**   | `agentgate serve --transport sse --upstream <url>`  | Server-Sent Events MCP servers |
| **HTTP**  | `agentgate serve --transport http --upstream <url>` | HTTP MCP servers               |

For SSE/HTTP transports, the proxy binds on port 7072 by default:

```bash
agentgate serve \
  --transport sse \
  --upstream http://localhost:3001 \
  --port 7072 \
  --policy policies/default.toml \
  --metrics-port 9090
```

## Configuration

All options can be set via a TOML config file (`~/.agentgate/config.toml`):

```toml
db_path       = "~/.agentgate/logs.db"
metrics_port  = 9090
dashboard_port = 7070

[rate_limits]
max_calls_per_minute = 60

[circuit_breaker]
failure_threshold = 5
recovery_seconds  = 30
```

CLI flags always override the config file.

## How It Works

AgentGate spawns the target MCP server as a child process and intercepts the JSON-RPC 2.0 stdio stream:

1. **Inbound** — Each message from the agent is parsed. `tools/call` requests are evaluated against the policy engine and rate limiter. Blocked calls get an immediate JSON-RPC error response; allowed calls are forwarded to the MCP server and tracked in a pending-call map.
2. **Response** — Responses from the MCP server are correlated with their pending call (for latency), circuit-breaker state is updated, redaction is applied, and the (possibly scrubbed) response is forwarded to the agent.
3. **Persistence** — Records are enqueued on a bounded channel and written to SQLite by a background task. The proxy hot path is never blocked by I/O.
4. **Live stream** — Every persisted record is broadcast to WebSocket subscribers via a `tokio::broadcast` channel, powering the dashboard's live feed.

JSON-RPC notifications (id-less messages) are forwarded immediately without tracking — they never expect a response, so they are never inserted into the pending-call map.

## Project Structure

```
agentgate/
├── crates/
│   ├── agentgate-core/
│   │   └── src/
│   │       ├── config.rs
│   │       ├── dashboard/          # REST + WebSocket API server (axum)
│   │       │   ├── api.rs
│   │       │   ├── server.rs
│   │       │   ├── state.rs
│   │       │   └── ws.rs
│   │       ├── logging/
│   │       │   └── structured.rs
│   │       ├── metrics.rs          # Prometheus metrics
│   │       ├── policy/             # TOML policy engine with hot-reload
│   │       │   ├── engine.rs
│   │       │   └── rules.rs
│   │       ├── protocol/
│   │       │   ├── jsonrpc.rs
│   │       │   └── mcp.rs
│   │       ├── proxy/
│   │       │   ├── evaluation.rs   # Policy + rate-limit evaluation
│   │       │   ├── http.rs
│   │       │   ├── sse.rs
│   │       │   └── stdio.rs
│   │       ├── ratelimit/          # Token bucket + circuit breaker
│   │       └── storage/            # SQLite persistence
│   └── agentgate-cli/
│       └── src/
│           └── main.rs
├── dashboard/                      # Next.js 15 static export
│   └── src/
│       ├── app/                    # App Router pages
│       └── components/
├── dashboards/
│   └── grafana.json                # Grafana dashboard
└── policies/
    └── default.toml
```

## Roadmap

- [x] **Phase 0** — MCP stdio proxy with structured logging
- [x] **Phase 1** — SQLite persistence, CLI log queries, JSONL export
- [x] **Phase 2** — Declarative TOML policy engine (deny/allow/redact rules)
- [x] **Phase 3** — Rate limiting (token bucket) & circuit breaker
- [x] **Phase 4** — SSE & HTTP transport support
- [x] **Phase 5** — Prometheus metrics & Grafana dashboard
- [x] **Phase 6** — Real-time dashboard (Next.js 15)
- [x] **Phase 7** — Distribution (Docker, Homebrew, installer)

## Tech Stack

| Component  | Technology                           |
| ---------- | ------------------------------------ |
| Core       | Rust, Tokio, Serde                   |
| Protocol   | JSON-RPC 2.0, MCP                    |
| Storage    | SQLite (rusqlite, WAL mode)          |
| API server | axum 0.7, tower-http                 |
| Metrics    | prometheus 0.13                      |
| Dashboard  | Next.js 15, Tailwind CSS 3, Recharts |
| CLI        | Clap, Tabled                         |

## Building from Source

```bash
git clone https://github.com/iamdainwi/AgentGate.git
cd AgentGate
cargo build --release

# Optional: build the dashboard UI
cd dashboard && npm install && npm run build
```

The binary is at `target/release/agentgate`. The dashboard static files are served from `dashboard/out/` by the embedded axum server.

## License

MIT

## Author

**Dainwi Choudhary** — [@iamdainwi](https://github.com/iamdainwi)

- [LinkedIn](https://www.linkedin.com/in/dainwi-choudhary/)
- [Portfolio](https://dainwi.vercel.app)
- [LICENSE](LICENSE)
