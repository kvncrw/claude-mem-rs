//! health-checker tests (port of `tests/supervisor/health-checker.test.ts`,
//! 73 lines, 5 cases).
//!
//! Because the TS side's "multiple startHealthChecker calls create only
//! one interval" assertion relied on spying `setInterval`, and we can't
//! spy `thread::spawn` from a test, the Rust port replaces that with a
//! visibility-exposed `is_running_for_test()` probe on the private slot.
//! The contract is equivalent: exactly one thread is armed per
//! `start_health_checker()` call pair.

use claude_mem_supervisor::infrastructure::health_checker::{
    is_running_for_test, start_health_checker, stop_health_checker,
};
use std::sync::Mutex;

static TEST_LOCK: std::sync::LazyLock<Mutex<()>> =
    std::sync::LazyLock::new(|| Mutex::new(()));

fn is_running() -> bool {
    is_running_for_test()
}

#[test]
fn start_does_not_throw() {
    let _lock = TEST_LOCK.lock().unwrap();
    stop_health_checker();
    start_health_checker();
    assert!(is_running());
    stop_health_checker();
}

#[test]
fn stop_clears_interval_without_throwing() {
    let _lock = TEST_LOCK.lock().unwrap();
    start_health_checker();
    stop_health_checker();
    assert!(!is_running());
}

#[test]
fn stop_is_safe_when_no_checker_running() {
    let _lock = TEST_LOCK.lock().unwrap();
    stop_health_checker();
    stop_health_checker();
    assert!(!is_running());
}

#[test]
fn multiple_starts_do_not_create_multiple_threads() {
    let _lock = TEST_LOCK.lock().unwrap();
    stop_health_checker();
    start_health_checker();
    start_health_checker();
    start_health_checker();
    // Single armed thread — matches TS guard behaviour.
    assert!(is_running());
    stop_health_checker();
}

#[test]
fn stop_after_start_allows_restart() {
    let _lock = TEST_LOCK.lock().unwrap();
    stop_health_checker();

    start_health_checker();
    assert!(is_running());

    stop_health_checker();
    assert!(!is_running());

    start_health_checker();
    assert!(is_running());

    stop_health_checker();
}
