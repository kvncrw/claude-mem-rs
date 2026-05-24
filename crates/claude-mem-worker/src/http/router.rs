//! Axum router.

use axum::routing::{get, post};
use axum::Router;
use claude_mem_core::db;
use rusqlite::Connection;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, Notify};

use super::routes::{
    admin_doctor, admin_restart, admin_shutdown, branch_status, branch_switch, branch_update,
    changes, context_inject, context_preview, context_recent, context_timeline, decisions,
    export_data, health, how_it_works, import_data, instructions, logs_clear, logs_get, mcp_status,
    mcp_toggle, memory_save, observation_get, observations_batch, observations_by_file,
    observations_get, pending_queue_all_clear, pending_queue_failed_clear, pending_queue_get,
    pending_queue_process, processing_set, processing_status, projects, prompt_get, prompts_get,
    readiness, root_viewer, sdk_sessions_batch, search, search_by_concept, search_by_file,
    search_by_type, search_help, search_observations_route, search_prompts_route,
    search_sessions_route, semantic_context, session_get, session_legacy_complete,
    session_legacy_delete, session_legacy_init, session_legacy_observations, session_legacy_status,
    session_legacy_summarize, sessions_complete, sessions_init, sessions_observations,
    sessions_status, sessions_summarize, settings_get, settings_post, stats, stream, summaries_get,
    timeline, timeline_by_query, version,
};
#[cfg(feature = "qdrant")]
use super::routes::{qdrant_health, qdrant_reindex};

#[derive(Clone)]
pub struct AppState {
    pub conn: Arc<Mutex<Connection>>,
    pub initialized: bool,
    pub mcp_ready: bool,
    pub shutdown: Option<Arc<Notify>>,
    pub events: broadcast::Sender<WorkerEvent>,
}

#[derive(Debug, Clone)]
pub struct WorkerEvent {
    pub event: String,
    pub data: Value,
}

impl AppState {
    pub fn new(conn: Connection) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            conn: Arc::new(Mutex::new(conn)),
            initialized: true,
            mcp_ready: true,
            shutdown: None,
            events,
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

    pub fn publish(&self, event: impl Into<String>, data: Value) {
        let _ = self.events.send(WorkerEvent {
            event: event.into(),
            data,
        });
    }
}

pub fn default_db_path() -> PathBuf {
    claude_mem_core::shared::platform_paths::default_db_path()
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
        .route("/api/instructions", get(instructions))
        .route("/api/admin/doctor", get(admin_doctor))
        .route("/api/admin/restart", post(admin_restart))
        .route("/api/admin/shutdown", post(admin_shutdown))
        .route("/sessions/:sessionDbId/init", post(session_legacy_init))
        .route(
            "/sessions/:sessionDbId/observations",
            post(session_legacy_observations),
        )
        .route(
            "/sessions/:sessionDbId/summarize",
            post(session_legacy_summarize),
        )
        .route("/sessions/:sessionDbId/status", get(session_legacy_status))
        .route(
            "/sessions/:sessionDbId",
            axum::routing::delete(session_legacy_delete),
        )
        .route(
            "/sessions/:sessionDbId/complete",
            post(session_legacy_complete),
        )
        .route("/api/sessions/init", post(sessions_init))
        .route("/api/sessions/observations", post(sessions_observations))
        .route("/api/sessions/complete", post(sessions_complete))
        .route("/api/sessions/summarize", post(sessions_summarize))
        .route("/api/sessions/status", get(sessions_status))
        .route("/api/memory/save", post(memory_save))
        .route("/api/context/inject", get(context_inject))
        .route("/api/context/semantic", post(semantic_context))
        .route("/api/context/recent", get(context_recent))
        .route("/api/context/timeline", get(context_timeline))
        .route("/api/context/preview", get(context_preview))
        .route("/api/search", get(search))
        .route("/api/timeline", get(timeline))
        .route("/api/timeline/by-query", get(timeline_by_query))
        .route("/api/search/help", get(search_help))
        .route("/api/decisions", get(decisions))
        .route("/api/changes", get(changes))
        .route("/api/how-it-works", get(how_it_works))
        .route("/api/search/observations", get(search_observations_route))
        .route("/api/search/sessions", get(search_sessions_route))
        .route("/api/search/prompts", get(search_prompts_route))
        .route("/api/search/by-file", get(search_by_file))
        .route("/api/search/by-concept", get(search_by_concept))
        .route("/api/search/by-type", get(search_by_type))
        .route("/api/observations", get(observations_get))
        .route("/api/observation/:id", get(observation_get))
        .route("/api/observations/by-file", get(observations_by_file))
        .route("/api/summaries", get(summaries_get))
        .route("/api/prompts", get(prompts_get))
        .route("/api/prompt/:id", get(prompt_get))
        .route("/api/session/:id", get(session_get))
        .route("/api/sdk-sessions/batch", post(sdk_sessions_batch))
        .route("/api/observations/batch", post(observations_batch))
        .route("/api/stats", get(stats))
        .route("/api/projects", get(projects))
        .route("/api/processing-status", get(processing_status))
        .route("/api/processing", post(processing_set))
        .route("/api/pending-queue", get(pending_queue_get))
        .route("/api/pending-queue/process", post(pending_queue_process))
        .route(
            "/api/pending-queue/failed",
            axum::routing::delete(pending_queue_failed_clear),
        )
        .route(
            "/api/pending-queue/all",
            axum::routing::delete(pending_queue_all_clear),
        )
        .route("/api/export", get(export_data))
        .route("/api/import", post(import_data))
        .route("/api/settings", get(settings_get).post(settings_post))
        .route("/api/mcp/status", get(mcp_status))
        .route("/api/mcp/toggle", post(mcp_toggle))
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
