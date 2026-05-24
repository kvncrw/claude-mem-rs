//! `validateWorkerPidFile` tests (port of `tests/supervisor/index.test.ts`,
//! the `validateWorkerPidFile` section — 4 cases).
//!
//! The remaining sections in that TS file (`Supervisor assertCanSpawn
//! behavior`, `Supervisor start idempotency`) tie into a `Supervisor`
//! singleton whose registry lifecycle isn't ported yet; those will land
//! in a follow-up commit once `supervisor::Supervisor` is implemented.

use claude_mem_supervisor::supervisor::{validate_worker_pid_file, ValidateWorkerPidStatus};
use std::fs::write as fs_write;
use std::sync::Mutex;
use tempfile::TempDir;

static TEST_LOCK: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

fn new_temp_dir() -> TempDir {
    tempfile::TempDir::new().unwrap()
}

#[test]
fn missing_returns_missing() {
    let _lock = TEST_LOCK.lock().unwrap();
    let dir = new_temp_dir();
    let pid_file = dir.path().join("worker.pid");
    let status = validate_worker_pid_file(&pid_file, false);
    assert_eq!(status, ValidateWorkerPidStatus::Missing);
}

#[test]
fn invalid_returns_invalid_for_bad_json() {
    let _lock = TEST_LOCK.lock().unwrap();
    let dir = new_temp_dir();
    let pid_file = dir.path().join("worker.pid");
    fs_write(&pid_file, "not-json!!!").unwrap();

    let status = validate_worker_pid_file(&pid_file, false);
    assert!(
        matches!(status, ValidateWorkerPidStatus::Invalid(_)),
        "expected Invalid, got {status:?}"
    );
}

#[test]
fn stale_returns_stale_for_dead_pid() {
    let _lock = TEST_LOCK.lock().unwrap();
    let dir = new_temp_dir();
    let pid_file = dir.path().join("worker.pid");
    // 2_147_483_647 is the maximum 32-bit signed int — almost certainly
    // not a live PID on any modern system.
    let body = r#"{"pid":2147483647,"port":37777,"startedAt":"2026-05-23T15:00:00Z"}"#;
    fs_write(&pid_file, body).unwrap();

    let status = validate_worker_pid_file(&pid_file, false);
    assert_eq!(status, ValidateWorkerPidStatus::Stale);
}

#[test]
fn alive_returns_alive_for_current_process() {
    let _lock = TEST_LOCK.lock().unwrap();
    let dir = new_temp_dir();
    let pid_file = dir.path().join("worker.pid");
    let body = format!(
        r#"{{"pid":{},"port":37777,"startedAt":"2026-05-23T15:00:00Z"}}"#,
        std::process::id()
    );
    fs_write(&pid_file, body).unwrap();

    let status = validate_worker_pid_file(&pid_file, false);
    match status {
        ValidateWorkerPidStatus::Alive { pid, port, .. } => {
            assert_eq!(pid, std::process::id());
            assert_eq!(port, 37777);
        }
        other => panic!("expected Alive, got {other:?}"),
    }
}
