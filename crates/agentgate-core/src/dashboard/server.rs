use super::api;
use super::state::DashboardState;
use super::ws::ws_live_handler;
use crate::metrics;
use anyhow::Result;
use axum::{http::StatusCode, routing::get, Router};
use tower_http::cors::{AllowHeaders, AllowMethods, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

/// Build the axum router for the dashboard API and static file serving.
/// CORS is restricted to same-machine origins only — the dashboard must never
/// be reachable from arbitrary websites.
fn build_router(state: DashboardState, port: u16) -> Router {
    // Only allow requests originating from the machine itself.  External origins
    // (including other machines on the same LAN) are rejected by the browser.
    let cors = CorsLayer::new()
        .allow_origin([
            format!("http://localhost:{port}")
                .parse::<axum::http::HeaderValue>()
                .expect("static origin"),
            format!("http://127.0.0.1:{port}")
                .parse::<axum::http::HeaderValue>()
                .expect("static origin"),
        ])
        .allow_methods(AllowMethods::any())
        .allow_headers(AllowHeaders::any());

    let api = Router::new()
        .route("/api/invocations", get(api::get_invocations))
        .route("/api/invocations/:id", get(api::get_invocation_by_id))
        .route("/api/stats/overview", get(api::get_stats_overview))
        .route("/api/stats/tools", get(api::get_stats_tools))
        .route("/api/stats/agents", get(api::get_stats_agents))
        .route(
            "/api/policies",
            get(api::get_policies).put(api::put_policies),
        )
        .route("/api/ws/live", get(ws_live_handler))
        .route("/health", get(|| async { "ok" }))
        .route("/metrics", get(metrics::metrics_handler));

    // Serve the pre-built Next.js static export when the `dashboard/out` directory
    // exists. Otherwise return a helpful 503 for unknown paths.
    let static_path = std::path::Path::new("dashboard/out");
    let router = if static_path.exists() {
        let fallback_file = static_path.join("index.html");
        api.fallback_service(
            ServeDir::new(static_path)
                .append_index_html_on_directories(true)
                .fallback(ServeFile::new(fallback_file)),
        )
    } else {
        api.fallback(|| async {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "Dashboard not built. Run `npm run build` inside the `dashboard/` directory.",
            )
        })
    };

    router.layer(cors).with_state(state)
}

/// Spawn the dashboard API + UI server on `port` as a background tokio task.
/// Binds to 127.0.0.1 only — never exposed to the network.
pub fn spawn_dashboard(state: DashboardState, port: u16) -> Result<()> {
    // 127.0.0.1, not 0.0.0.0 — the dashboard contains sensitive audit data and
    // policy configuration that must never be reachable from other network hosts.
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let router = build_router(state, port);

    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!(addr = %addr, "Dashboard listening");
                if let Err(e) = axum::serve(listener, router).await {
                    tracing::error!("Dashboard server error: {e}");
                }
            }
            Err(e) => tracing::error!("Failed to bind dashboard on {addr}: {e}"),
        }
    });

    Ok(())
}
