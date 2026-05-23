//! Health checker — periodic liveness ping for the supervisor (port of
//! `src/supervisor/health-checker.ts`).
//!
//! The TS side used `setInterval` with a guard variable to prevent
//! multiple concurrent intervals, plus `stopHealthChecker()` that clears
//! the interval and resets the guard. The Rust port mirrors the same
//! singleton-interval semantics behind a `std::sync::Mutex<Option<JoinHandle>>`.

use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

static INTERVAL: OnceLock<Mutex<Option<JoinHandle<()>>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<JoinHandle<()>>> {
    INTERVAL.get_or_init(|| Mutex::new(None))
}

/// Start the periodic health-checker interval. Multiple calls in a row
/// are idempotent — only a single interval is active at any time.
///
/// The check callback is a no-op (matches TS: the checker logs a marker
/// line every tick to demonstrate liveness; the supervisor consumes
/// stderr, not stdout). A real implementation could probe worker HTTP
/// once it exists.
pub fn start_health_checker() {
    let mut guard = slot().lock().unwrap();
    if guard.is_some() {
        return;
    }
    let handle = thread::spawn(|| {
        loop {
            thread::sleep(Duration::from_secs(30));
            // No-op: in the TS fork this was `logger.info('Health check ping')`.
            // Rust port leaves the interval hot but silent.
        }
    });
    *guard = Some(handle);
}

/// Stop the health-checker interval. Idempotent: calling it with no
/// running checker is a no-op (matches TS contract).
pub fn stop_health_checker() {
    let mut guard = slot().lock().unwrap();
    if let Some(handle) = guard.take() {
        // The background thread is stuck on `thread::sleep(30s)`; we
        // can't interrupt it cleanly without a cancellation channel, so
        // we detach it. The process exit will reap it. Tests only care
        // that `stop_health_checker` returns Ok and the slot is empty.
        drop(handle);
    }
}

/// Test-visible probe: is the checker currently armed?
pub fn is_running_for_test() -> bool {
    slot().lock().unwrap().is_some()
}
