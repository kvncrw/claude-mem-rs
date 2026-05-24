//! Validation helpers exposed by the supervisor entrypoint (port of
//! `src/supervisor/index.ts: validateWorkerPidFile`).

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidateWorkerPidStatus {
    /// No PID file present on disk.
    Missing,
    /// PID file exists but JSON is malformed or missing required fields.
    Invalid(String),
    /// PID file parses but the recorded PID does not belong to a process
    /// the current user can signal.
    Stale,
    /// PID file parses and the recorded PID is a reachable live process.
    Alive {
        pid: u32,
        port: u16,
        started_at: String,
    },
}

/// On-disk shape of the worker PID file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerPidFile {
    pub pid: u32,
    pub port: u16,
    #[serde(default)]
    pub started_at: Option<String>,
}

/// Validate a worker PID file at the given path.
///
/// - `Missing` when the file doesn't exist.
/// - `Invalid` when it exists but doesn't parse as the expected JSON
///   shape (or required fields are absent/wrong type).
/// - `Stale` when JSON parses but the PID isn't reachable via
///   `kill(pid, 0)` (process dead, wrong user, or reaped).
/// - `Alive` otherwise.
///
/// `log_alive` is the TS flag that prints a line on the alive path —
/// ignored in the Rust port; callers log via tracing if they want to.
pub fn validate_worker_pid_file<P: AsRef<Path>>(
    pid_file_path: P,
    _log_alive: bool,
) -> ValidateWorkerPidStatus {
    let path = pid_file_path.as_ref();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ValidateWorkerPidStatus::Missing
        }
        Err(e) => {
            return ValidateWorkerPidStatus::Invalid(format!("IO error reading PID file: {e}"))
        }
    };

    let parsed: WorkerPidFile = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(e) => return ValidateWorkerPidStatus::Invalid(e.to_string()),
    };

    if parsed.pid == 0 {
        return ValidateWorkerPidStatus::Invalid("pid field is 0".into());
    }

    // Liveness check delegates to the cross-platform helper:
    // POSIX uses `kill(pid, 0)`; Windows shells out to `tasklist`.
    if !crate::infrastructure::process_manager::is_process_alive(parsed.pid as i64) {
        return ValidateWorkerPidStatus::Stale;
    }

    ValidateWorkerPidStatus::Alive {
        pid: parsed.pid,
        port: parsed.port,
        started_at: parsed.started_at.unwrap_or_default(),
    }
}
