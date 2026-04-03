use crate::policy::PolicyEngine;
use crate::storage::InvocationRecord;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Shared state injected into every dashboard API handler via axum's `State` extractor.
#[derive(Clone)]
pub struct DashboardState {
    pub db_path: PathBuf,
    /// Path of the active policy TOML file, if one was loaded.
    pub policy_path: Option<PathBuf>,
    /// Live policy engine — used to trigger hot-reload after a PUT /api/policies.
    pub policy_engine: Option<Arc<PolicyEngine>>,
    /// Sender half of the live invocation broadcast — handlers subscribe per WebSocket connection.
    pub live_tx: broadcast::Sender<InvocationRecord>,
    /// Bearer token required for all authenticated routes.
    /// Generated randomly at startup and printed to stderr so only the local user can access the
    /// dashboard — even on a multi-user machine or cloud VM with an open firewall.
    pub auth_token: String,
}

/// Generate a cryptographically-random dashboard token and print it to stderr.
/// Uses UUID v4 (122 bits of randomness) — sufficient for a single-host bearer token.
pub fn generate_and_print_token() -> String {
    let token = uuid::Uuid::new_v4().to_string();
    eprintln!(
        "[agentgate] Dashboard token: {token}\n\
         [agentgate] Access the dashboard at http://127.0.0.1:7070 with:\n\
         [agentgate]   Authorization: Bearer {token}"
    );
    token
}
