//! Graceful shutdown coordinator (port of
//! `src/services/infrastructure/graceful-shutdown.ts`).
//!
//! Shutdown order is fixed to prevent use-after-close on resources:
//!
//!   1. PID file removed (synchronous, first step, non-failing)
//!   2. HTTP server closes connections + stops accepting new ones
//!   3. Session manager drains in-flight sessions
//!   4. MCP client closes its IPC channel
//!   5. Chroma MCP manager stops (the TS side ran Chroma as a subprocess)
//!   6. SQLite DB manager closes last so writes from steps 2-5 can flush
//!
//! Every service is optional — a missing service is a no-op, not an error.
//! Trait objects use the `BoxFuture` pattern (`Pin<Box<dyn Future + '_>>`
//! return) since `impl Trait` in trait methods isn't dyn-compatible; this
//! lets the config carry real trait-object dispatch.

use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use thiserror::Error;

/// BoxFuture type alias — used by trait impls to make async dispatch
/// dyn-compatible (`impl Future` in trait methods is not).
///
/// Re-exported via the module so trait impls in tests can build their
/// return values with the same type.
pub type BoxFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// PID file on disk (JSON).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PidInfo {
    pub pid: u32,
    pub port: u16,
    pub started_at: String,
    pub started_at_epoch: i64,
}

#[derive(Debug, Error)]
pub enum PidError {
    #[error("IO error on PID file: {0}")]
    Io(#[from] std::io::Error),
    #[error("PID file JSON invalid: {0}")]
    Json(#[from] serde_json::Error),
}

fn default_pid_path() -> PathBuf {
    claude_mem_core::shared::platform_paths::worker_pid_path()
}

pub fn write_pid_file(info: &PidInfo) -> Result<PathBuf, PidError> {
    let path = default_pid_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(info)?)?;
    Ok(path)
}

pub fn read_pid_file() -> Result<Option<PidInfo>, PidError> {
    let path = default_pid_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn remove_pid_file() -> Result<(), PidError> {
    let path = default_pid_path();
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Trait-object service that can be shut down once (fire-and-forget).
///
/// The BoxFuture return makes this dyn-compatible — callers wrap an
/// `async {}` body via `Box::pin(async move { … })`.
pub trait ShutdownableService: Send + Sync {
    fn shutdown_all(&self) -> BoxFuture<'_>;
}

/// Trait-object async `close()` (e.g. http.Client, MCP client wrapper).
pub trait CloseableClient: Send + Sync {
    fn close(&self) -> BoxFuture<'_>;
}

/// Trait-object database-like "close" (e.g. an `rusqlite::Connection`
/// owner, a pool wrapper).
pub trait CloseableDatabase: Send + Sync {
    fn close(&self) -> BoxFuture<'_>;
}

/// Trait-object "stop" for sidecar subprocess-like services (TS:
/// `chromaMcpManager.stop()`).
pub trait StopableService: Send + Sync {
    fn stop(&self) -> BoxFuture<'_>;
}

/// Configuration for [`perform_graceful_shutdown`]. Every field is
/// `Option` — missing services are silently skipped.
#[derive(Default)]
pub struct GracefulShutdownConfig {
    pub server: Option<Box<dyn CloseableClient>>,
    pub session_manager: Option<Box<dyn ShutdownableService>>,
    pub mcp_client: Option<Box<dyn CloseableClient>>,
    pub chroma_mcp_manager: Option<Box<dyn StopableService>>,
    pub db_manager: Option<Box<dyn CloseableDatabase>>,
}

/// Run the fixed shutdown sequence. Errors inside any service are NOT
/// propagated — matches TS contract where `mock.fn(async () => {…})`
/// throws are awaited, and missing services don't throw.
pub async fn perform_graceful_shutdown(config: GracefulShutdownConfig) {
    // Step 1: remove the PID file synchronously. TS does this before
    // anything else so a stuck process doesn't keep its marker around.
    let _ = remove_pid_file();

    // Step 2: close server (drops in-flight HTTP requests).
    if let Some(server) = config.server {
        server.close().await;
    }

    // Step 3: drain sessions.
    if let Some(sm) = config.session_manager {
        sm.shutdown_all().await;
    }

    // Step 4: MCP client.
    if let Some(mc) = config.mcp_client {
        mc.close().await;
    }

    // Step 5: chroma sidecar (runs before DB close).
    if let Some(chroma) = config.chroma_mcp_manager {
        chroma.stop().await;
    }

    // Step 6: DB close last (writes from earlier steps may still be pending).
    if let Some(db) = config.db_manager {
        db.close().await;
    }
}
