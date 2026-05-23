//! Graceful shutdown tests (port of
//! `tests/infrastructure/graceful-shutdown.test.ts`, 256 lines, 8 cases).
//!
//! Exercises the fixed shutdown order + PID file lifecycle against
//! trait-object services.
//!
//! Because the PID file path is derived from `CLAUDE_MEM_HOME` (env var,
//! process-global) all tests here run serialised under `TEST_LOCK` and
//! each gets its own `tempfile::TempDir` set as `CLAUDE_MEM_HOME` so they
//! don't collide on `$HOME/.claude-mem/worker.pid`.

use claude_mem_supervisor::infrastructure::graceful_shutdown::{
    perform_graceful_shutdown, read_pid_file, remove_pid_file, write_pid_file,
    BoxFuture, CloseableClient, CloseableDatabase, GracefulShutdownConfig, PidInfo,
    ShutdownableService, StopableService,
};
use std::sync::{Arc, Mutex, MutexGuard};
use tempfile::TempDir;

static TEST_LOCK: std::sync::LazyLock<Mutex<()>> =
    std::sync::LazyLock::new(|| Mutex::new(()));

/// Acquire TEST_LOCK, set CLAUDE_MEM_HOME to a fresh tempdir, and return
/// both the lock guard (keeps other tests out) and the tempdir (must be
/// held for the test's duration).
struct TestHome {
    _lock: MutexGuard<'static, ()>,
    _dir: TempDir,
}

impl TestHome {
    fn new() -> Self {
        let lock = TEST_LOCK.lock().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        std::env::set_var("CLAUDE_MEM_HOME", dir.path());
        // Ensure no leftover PID file from a previous panicked test.
        let _ = remove_pid_file();
        Self { _lock: lock, _dir: dir }
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        // Best-effort cleanup of env var. The next TestHome::new() will
        // reset it anyway.
        std::env::remove_var("CLAUDE_MEM_HOME");
    }
}

fn fresh_pid_info() -> PidInfo {
    PidInfo {
        pid: std::process::id(),
        port: 37777,
        started_at: "2026-05-23T15:00:00Z".into(),
        started_at_epoch: 1_748_012_400_000,
    }
}

/// Record of a shutdown step, in insertion order.
type CallOrder = Arc<Mutex<Vec<&'static str>>>;

struct TrackingServer(CallOrder);
impl CloseableClient for TrackingServer {
    fn close(&self) -> BoxFuture<'_> {
        let o = Arc::clone(&self.0);
        Box::pin(async move {
            o.lock().unwrap().push("serverClose");
        })
    }
}

struct TrackingSessionManager(CallOrder);
impl ShutdownableService for TrackingSessionManager {
    fn shutdown_all(&self) -> BoxFuture<'_> {
        let o = Arc::clone(&self.0);
        Box::pin(async move {
            o.lock().unwrap().push("sessionManager");
        })
    }
}

struct TrackingMcpClient(CallOrder);
impl CloseableClient for TrackingMcpClient {
    fn close(&self) -> BoxFuture<'_> {
        let o = Arc::clone(&self.0);
        Box::pin(async move {
            o.lock().unwrap().push("mcpClient");
        })
    }
}

struct TrackingChroma(CallOrder);
impl StopableService for TrackingChroma {
    fn stop(&self) -> BoxFuture<'_> {
        let o = Arc::clone(&self.0);
        Box::pin(async move {
            o.lock().unwrap().push("chromaMcpManager");
        })
    }
}

struct TrackingDb(CallOrder);
impl CloseableDatabase for TrackingDb {
    fn close(&self) -> BoxFuture<'_> {
        let o = Arc::clone(&self.0);
        Box::pin(async move {
            o.lock().unwrap().push("dbManager");
        })
    }
}

fn order() -> CallOrder {
    Arc::new(Mutex::new(Vec::new()))
}

#[test]
fn pid_file_lifecycle_round_trips() {
    let _home = TestHome::new();
    let info = fresh_pid_info();
    write_pid_file(&info).unwrap();
    let back = read_pid_file().unwrap().expect("read_pid_file should return Some");
    assert_eq!(back, info);
    remove_pid_file().unwrap();
    assert!(read_pid_file().unwrap().is_none());
}

#[test]
fn remove_pid_file_is_idempotent_when_missing() {
    let _home = TestHome::new();
    remove_pid_file().unwrap();
}

#[tokio::test]
async fn shutdown_calls_services_in_fixed_order() {
    let _home = TestHome::new();
    write_pid_file(&fresh_pid_info()).unwrap();

    let o = order();
    let config = GracefulShutdownConfig {
        server: Some(Box::new(TrackingServer(Arc::clone(&o)))),
        session_manager: Some(Box::new(TrackingSessionManager(Arc::clone(&o)))),
        mcp_client: Some(Box::new(TrackingMcpClient(Arc::clone(&o)))),
        chroma_mcp_manager: Some(Box::new(TrackingChroma(Arc::clone(&o)))),
        db_manager: Some(Box::new(TrackingDb(Arc::clone(&o)))),
    };
    perform_graceful_shutdown(config).await;

    assert!(read_pid_file().unwrap().is_none());

    let recorded = o.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec!["serverClose", "sessionManager", "mcpClient", "chromaMcpManager", "dbManager"],
        "order must match TS: server→session→mcp→chroma→db"
    );
}

#[tokio::test]
async fn shutdown_works_when_all_optional_services_missing() {
    let _home = TestHome::new();
    perform_graceful_shutdown(GracefulShutdownConfig::default()).await;
}

#[tokio::test]
async fn shutdown_works_when_only_session_manager_present() {
    let _home = TestHome::new();
    let o = order();
    let config = GracefulShutdownConfig {
        session_manager: Some(Box::new(TrackingSessionManager(Arc::clone(&o)))),
        ..Default::default()
    };
    perform_graceful_shutdown(config).await;
    let recorded = o.lock().unwrap().clone();
    assert_eq!(recorded, vec!["sessionManager"]);
}

#[tokio::test]
async fn shutdown_order_preserved_even_with_gaps() {
    let _home = TestHome::new();
    let o = order();
    let config = GracefulShutdownConfig {
        server: Some(Box::new(TrackingServer(Arc::clone(&o)))),
        db_manager: Some(Box::new(TrackingDb(Arc::clone(&o)))),
        ..Default::default()
    };
    perform_graceful_shutdown(config).await;
    let recorded = o.lock().unwrap().clone();
    assert_eq!(recorded, vec!["serverClose", "dbManager"]);
}

#[tokio::test]
async fn chroma_stops_before_db_close_when_both_present() {
    let _home = TestHome::new();
    let o = order();
    let config = GracefulShutdownConfig {
        chroma_mcp_manager: Some(Box::new(TrackingChroma(Arc::clone(&o)))),
        db_manager: Some(Box::new(TrackingDb(Arc::clone(&o)))),
        ..Default::default()
    };
    perform_graceful_shutdown(config).await;
    let recorded = o.lock().unwrap().clone();
    assert_eq!(recorded, vec!["chromaMcpManager", "dbManager"]);
}

#[tokio::test]
async fn pid_file_removed_even_when_no_services_configured() {
    let _home = TestHome::new();
    write_pid_file(&fresh_pid_info()).unwrap();

    perform_graceful_shutdown(GracefulShutdownConfig::default()).await;
    assert!(read_pid_file().unwrap().is_none());
}
