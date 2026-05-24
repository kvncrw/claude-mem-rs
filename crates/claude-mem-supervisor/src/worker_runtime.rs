use crate::infrastructure::health_monitor::{
    http_shutdown, wait_for_port_free, wait_for_readiness,
};
use crate::infrastructure::process_manager::{read_pid_file, spawn_daemon};
use anyhow::{anyhow, Result};
use claude_mem_core::shared::worker_utils::worker_port_from_env;
use std::path::{Path, PathBuf};
use std::time::Duration;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub port: u16,
}

pub async fn ensure_worker_running() -> bool {
    let port = worker_port_from_env();
    if wait_for_readiness(port, Some(Duration::from_millis(800))).await {
        return true;
    }

    let Some(binary) = resolve_worker_binary() else {
        return false;
    };
    if spawn_daemon(binary, port, std::iter::empty::<(String, String)>()).is_none() {
        return false;
    }
    wait_for_readiness(port, Some(STARTUP_TIMEOUT)).await
}

pub async fn start_worker() -> Result<WorkerStatus> {
    let port = worker_port_from_env();
    if wait_for_readiness(port, Some(Duration::from_millis(800))).await {
        return Ok(status_from_pid(true, port));
    }
    let binary =
        resolve_worker_binary().ok_or_else(|| anyhow!("claude-mem-worker binary not found"))?;
    let pid = spawn_daemon(binary, port, std::iter::empty::<(String, String)>())
        .ok_or_else(|| anyhow!("failed to spawn claude-mem worker"))?;
    if !wait_for_readiness(port, Some(STARTUP_TIMEOUT)).await {
        return Err(anyhow!("worker did not become ready on port {port}"));
    }
    Ok(WorkerStatus {
        running: true,
        pid: Some(pid),
        port,
    })
}

pub async fn stop_worker() -> Result<WorkerStatus> {
    let port = worker_port_from_env();
    let _ = http_shutdown(port).await;
    let stopped = wait_for_port_free(port, Some(Duration::from_secs(15))).await;
    if !stopped {
        return Err(anyhow!("worker port {port} did not stop within timeout"));
    }
    Ok(WorkerStatus {
        running: false,
        pid: None,
        port,
    })
}

pub async fn restart_worker() -> Result<WorkerStatus> {
    let _ = stop_worker().await;
    start_worker().await
}

pub async fn worker_status() -> WorkerStatus {
    let port = worker_port_from_env();
    let running = wait_for_readiness(port, Some(Duration::from_millis(800))).await;
    status_from_pid(running, port)
}

pub fn resolve_worker_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CLAUDE_MEM_WORKER_BIN").map(PathBuf::from) {
        return Some(path);
    }

    let current = std::env::current_exe().ok()?;
    let file_name = current
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if matches!(file_name, "claude-mem" | "claude-mem-rs") {
        return Some(current);
    }

    let sibling = sibling_binary(&current, "claude-mem-worker");
    if sibling.exists() {
        return Some(sibling);
    }

    Some(current)
}

pub fn worker_daemon_args_for(binary: &Path) -> Vec<String> {
    let file_name = binary
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if matches!(file_name, "claude-mem" | "claude-mem-rs") {
        vec!["worker".to_owned(), "--daemon".to_owned()]
    } else {
        vec!["--daemon".to_owned()]
    }
}

fn sibling_binary(current: &Path, binary_name: &str) -> PathBuf {
    current
        .parent()
        .map(|parent| parent.join(binary_name))
        .unwrap_or_else(|| PathBuf::from(binary_name))
}

fn status_from_pid(running: bool, port: u16) -> WorkerStatus {
    let pid = read_pid_file().and_then(|info| (info.port == port).then_some(info.pid));
    WorkerStatus { running, pid, port }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unified_binary_uses_worker_subcommand() {
        assert_eq!(
            worker_daemon_args_for(Path::new("/tmp/claude-mem")),
            vec!["worker", "--daemon"]
        );
    }

    #[test]
    fn worker_binary_uses_direct_daemon_arg() {
        assert_eq!(
            worker_daemon_args_for(Path::new("/tmp/claude-mem-worker")),
            vec!["--daemon"]
        );
    }
}
