//! claude-mem-worker — axum HTTP API, search strategies (FTS5 + BM25;
//! embedding/Chroma is gated behind `feature = "chroma"` and off by default),
//! session/queue management.

use claude_mem_core::db;
use claude_mem_core::shared::worker_utils::worker_port_from_env;
use http::router::{build_router_with_state, default_db_path, AppState};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Notify;

pub mod agents;
pub mod http;
pub mod queue;
pub mod search;

pub async fn run_from_env() -> Result<(), Box<dyn std::error::Error>> {
    let host = std::env::var("CLAUDE_MEM_WORKER_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned());
    let port = worker_port_from_env();
    let db_path = default_db_path();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = db::open_or_create(db_path)?;

    let shutdown = Arc::new(Notify::new());
    let app = build_router_with_state(AppState::with_shutdown(conn, Arc::clone(&shutdown)));
    let listener = TcpListener::bind(format!("{host}:{port}")).await?;

    write_pid_file(port)?;
    tracing::info!(%host, port, "claude-mem worker listening");

    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = shutdown.notified() => {},
            }
        })
        .await;

    let _ = remove_pid_file();
    result?;
    Ok(())
}

fn pid_file_path() -> PathBuf {
    claude_mem_core::shared::platform_paths::worker_pid_path()
}

fn write_pid_file(port: u16) -> std::io::Result<()> {
    let path = pid_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let started_at = time::OffsetDateTime::from_unix_timestamp(now_ms / 1000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();

    let body = serde_json::to_string_pretty(&json!({
        "pid": std::process::id(),
        "port": port,
        "startedAt": started_at,
        "startedAtEpoch": now_ms
    }))
    .map_err(std::io::Error::other)?;

    std::fs::write(path, body)
}

fn remove_pid_file() -> std::io::Result<()> {
    match std::fs::remove_file(pid_file_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
