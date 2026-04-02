use super::api;
use super::state::DashboardState;
use super::ws::ws_live_handler;
use crate::metrics;
use anyhow::Result;
use axum::{http::StatusCode, routing::get, Router};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

/// Build the axum router for the dashboard API and static file serving.
fn build_router(state: DashboardState) -> Router {
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

    router.layer(CorsLayer::permissive()).with_state(state)
}

/// Spawn the dashboard API + UI server on `port` as a background tokio task.
/// Returns immediately — the server runs until the process exits.
pub fn spawn_dashboard(state: DashboardState, port: u16) -> Result<()> {
    let addr: std::net::SocketAddr = format!("0.0.0.0:{port}").parse()?;
    let router = build_router(state);

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
