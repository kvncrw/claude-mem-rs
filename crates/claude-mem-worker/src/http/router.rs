//! Axum router.

use axum::routing::{get, post};
use axum::Router;
use claude_mem_core::db;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::routes::{
    context_inject, health, memory_save, observations_batch, readiness, search, search_by_file,
    semantic_context, sessions_complete, sessions_init, sessions_observations, version,
};

#[derive(Clone)]
pub struct AppState {
    pub conn: Arc<Mutex<Connection>>,
    pub initialized: bool,
    pub mcp_ready: bool,
}

impl AppState {
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            initialized: true,
            mcp_ready: true,
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
    let conn = db::open_or_create(default_db_path()).expect("failed to open claude-mem database");
    build_router_with_state(AppState::new(conn))
}

pub fn build_router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/readiness", get(readiness))
        .route("/api/version", get(version))
        .route("/api/sessions/init", post(sessions_init))
        .route("/api/sessions/observations", post(sessions_observations))
        .route("/api/sessions/complete", post(sessions_complete))
        .route("/api/memory/save", post(memory_save))
        .route("/api/context/inject", get(context_inject))
        .route("/api/context/semantic", post(semantic_context))
        .route("/api/search", get(search))
        .route("/api/search/by-file", get(search_by_file))
        .route("/api/observations/batch", post(observations_batch))
        .with_state(state)
}
