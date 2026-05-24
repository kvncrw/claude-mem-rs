//! Axum router.

use axum::Router;
use axum::routing::{get, post};
use claude_mem_core::db;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

use super::routes::{
    admin_doctor, admin_shutdown, branch_status, branch_switch, branch_update, context_inject,
    export_data, health, import_data, logs_clear, logs_get, memory_save, observations_batch,
    observations_get, processing_status, projects, readiness, root_viewer, search,
    search_by_concept, search_by_file, search_by_type, semantic_context, sessions_complete,
    sessions_init, sessions_observations, sessions_status, sessions_summarize, settings_get,
    settings_post, stats, stream, summaries_get, timeline, version,
};
#[cfg(feature = "qdrant")]
use super::routes::{qdrant_health, qdrant_reindex};

#[derive(Clone)]
pub struct AppState {
    pub conn: Arc<Mutex<Connection>>,
    pub initialized: bool,
    pub mcp_ready: bool,
    pub shutdown: Option<Arc<Notify>>,
}

impl AppState {
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            initialized: true,
            mcp_ready: true,
            shutdown: None,
        }
    }

    pub fn with_shutdown(conn: Connection, shutdown: Arc<Notify>) -> Self {
        Self {
            shutdown: Some(shutdown),
            ..Self::new(conn)
        }
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        Ok(Self::new(db::open_in_memory()?))
    }
}

pub fn default_db_path() -> PathBuf {
    let home = std::env::var_os("CLAUDE_MEM_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude-mem")))
        .unwrap_or_else(|| PathBuf::from(".claude-mem"));
    home.join("claude-mem.db")
}

pub fn build_router() -> Router {
    let db_path = default_db_path();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create claude-mem data directory");
    }
    let conn = db::open_or_create(db_path).expect("failed to open claude-mem database");
    build_router_with_state(AppState::new(conn))
}

pub fn build_router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/", get(root_viewer))
        .route("/health", get(health))
        .route("/stream", get(stream))
        .route("/api/health", get(health))
        .route("/api/readiness", get(readiness))
        .route("/api/version", get(version))
        .route("/api/admin/doctor", get(admin_doctor))
        .route("/api/admin/shutdown", post(admin_shutdown))
        .route("/api/sessions/init", post(sessions_init))
        .route("/api/sessions/observations", post(sessions_observations))
        .route("/api/sessions/complete", post(sessions_complete))
        .route("/api/sessions/summarize", post(sessions_summarize))
        .route("/api/sessions/status", get(sessions_status))
        .route("/api/memory/save", post(memory_save))
        .route("/api/context/inject", get(context_inject))
        .route("/api/context/semantic", post(semantic_context))
        .route("/api/search", get(search))
        .route("/api/timeline", get(timeline))
        .route("/api/search/by-file", get(search_by_file))
        .route("/api/search/by-concept", get(search_by_concept))
        .route("/api/search/by-type", get(search_by_type))
        .route("/api/observations", get(observations_get))
        .route("/api/summaries", get(summaries_get))
        .route("/api/observations/batch", post(observations_batch))
        .route("/api/stats", get(stats))
        .route("/api/projects", get(projects))
        .route("/api/processing-status", get(processing_status))
        .route("/api/export", get(export_data))
        .route("/api/import", post(import_data))
        .route("/api/settings", get(settings_get).post(settings_post))
        .route("/api/logs", get(logs_get))
        .route("/api/logs/clear", post(logs_clear))
        .route("/api/branch/status", get(branch_status))
        .route("/api/branch/switch", post(branch_switch))
        .route("/api/branch/update", post(branch_update))
        .merge(qdrant_routes())
        .with_state(state)
}

#[cfg(feature = "qdrant")]
fn qdrant_routes() -> Router<AppState> {
    Router::new()
        .route("/api/vector/qdrant/health", get(qdrant_health))
        .route("/api/vector/qdrant/reindex", post(qdrant_reindex))
}

#[cfg(not(feature = "qdrant"))]
fn qdrant_routes() -> Router<AppState> {
    Router::new()
}
