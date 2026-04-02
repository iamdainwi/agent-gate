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
}
