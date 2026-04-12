use super::api;
use super::state::DashboardState;
use super::ws::ws_live_handler;
use crate::metrics;
use anyhow::Result;
use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rust_embed::{Embed, RustEmbed};
use serde_json::json;
use axum::http::{header, Method};
use tower_http::cors::CorsLayer;

/// Embed the pre-built Next.js static export into the binary at compile time.
///
/// `dashboard/out/` must exist at compile time (it's created by `npm run build`
/// in the `dashboard/` directory). An empty directory produces a binary with no
/// embedded UI assets; the fallback 503 handler is returned for UI routes.
#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../../dashboard/out"]
struct DashboardAssets;

// ── Auth middleware ───────────────────────────────────────────────────────────

/// Require `Authorization: Bearer <token>` on all routes except `/health` and `/metrics`.
/// Those two are exempt so monitoring systems can scrape without credentials.
async fn auth_layer(
    State(state): State<DashboardState>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path();

    // Public endpoints — no auth required.
    if path == "/health" || path == "/metrics" {
        return next.run(req).await;
    }

    // The WebSocket endpoint cannot receive custom headers in a browser
    // (`new WebSocket(url)` has no headers option). Authentication for that
    // route is handled inside `ws_live_handler` via the `?token=` query param.
    if path == "/api/ws/live" {
        return next.run(req).await;
    }

    let expected = format!("Bearer {}", state.auth_token);
    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // String equality is fine here: the token is a CSPRNG-generated hex string
    // and the server is bound to 127.0.0.1 only — no timing oracle exists.
    if provided != expected {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Invalid or missing Authorization: Bearer token" })),
        )
            .into_response();
    }

    next.run(req).await
}

// ── Router ────────────────────────────────────────────────────────────────────

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
        .allow_methods([Method::GET, Method::PUT, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

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

    // Serve the embedded Next.js static export. Falls back to a 503 when no
    // assets were embedded (e.g. a dev build without a prior `npm run build`).
    let router = api.fallback(embedded_asset_handler);

    router
        .layer(middleware::from_fn_with_state(state.clone(), auth_layer))
        .layer(cors)
        .with_state(state)
}

/// Serve embedded static assets or fall back to `/index.html` for SPA routes.
async fn embedded_asset_handler(req: Request) -> Response {
    let path = req.uri().path();

    // Try the exact path, then with `/index.html` appended (directories),
    // then fall back to the root `/index.html` for SPA client-side routing.
    let candidates = [
        path.to_string(),
        format!("{}/index.html", path.trim_end_matches('/')),
        "/index.html".to_string(),
    ];

    for candidate in &candidates {
        let key = candidate.trim_start_matches('/');
        if let Some(asset) = <DashboardAssets as Embed>::get(key) {
            let mime = mime_guess::from_path(key)
                .first_or_octet_stream()
                .to_string();
            return (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, mime)],
                asset.data.into_owned(),
            )
                .into_response();
        }
    }

    (
        StatusCode::SERVICE_UNAVAILABLE,
        "Dashboard not built. Run `npm run build` inside the `dashboard/` directory.",
    )
        .into_response()
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
