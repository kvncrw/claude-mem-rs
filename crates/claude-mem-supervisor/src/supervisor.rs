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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerPidFile {
    pub pid: u32,
    pub port: u16,
    #[serde(default)]
    pub started_at: Option<String>,
}

impl Default for WorkerPidFile {
    fn default() -> Self {
        Self {
            pid: 0,
            port: 0,
            started_at: None,
        }
    }
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

    // Liveness check: `kill(pid, 0)` returns 0 if process exists and
    // the caller has permission; -1 with ESRCH means no such process.
    #[cfg(unix)]
    {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        // SAFETY: `kill(pid, 0)` is the standard POSIX liveness probe;
        // never delivers a signal, just checks reachability.
        let ret = unsafe { kill(parsed.pid as i32, 0) };
        if ret != 0 {
            return ValidateWorkerPidStatus::Stale;
        }
    }
    #[cfg(not(unix))]
    {
        // On non-Unix (Windows target), fall back to "assume alive".
        // The TS side does the same with `spawnSync('tasklist')`.
    }

    ValidateWorkerPidStatus::Alive {
        pid: parsed.pid,
        port: parsed.port,
        started_at: parsed.started_at.unwrap_or_default(),
    }
}
