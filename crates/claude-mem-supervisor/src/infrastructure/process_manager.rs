//! PID file and process lifecycle helpers.
//!
//! Port of the Linux/Unix-relevant parts of
//! `src/services/infrastructure/ProcessManager.ts`. This fork targets
//! POSIX-like hosts only (Linux/macOS); Windows-specific WMIC and PowerShell
//! behavior is intentionally not present.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const CHROMA_MIGRATION_MARKER_FILENAME: &str = ".chroma-cleaned-v10.3";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PidInfo {
    pub pid: u32,
    pub port: u16,
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanStalePidFileStatus {
    Missing,
    RemovedStale(PidInfo),
    Alive(PidInfo),
    Invalid,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeResolverOptions {
    pub platform: Option<String>,
    pub exec_path: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub home_directory: Option<PathBuf>,
}

pub fn write_pid_file(info: &PidInfo) -> std::io::Result<PathBuf> {
    write_pid_file_at(default_pid_path(), info)
}

pub fn write_pid_file_at(path: impl AsRef<Path>, info: &PidInfo) -> std::io::Result<PathBuf> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(info)?)?;
    Ok(path.to_path_buf())
}

pub fn read_pid_file() -> Option<PidInfo> {
    read_pid_file_at(default_pid_path())
}

pub fn read_pid_file_at(path: impl AsRef<Path>) -> Option<PidInfo> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

pub fn remove_pid_file() -> std::io::Result<()> {
    remove_pid_file_at(default_pid_path())
}

pub fn remove_pid_file_at(path: impl AsRef<Path>) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn parse_elapsed_time(etime: &str) -> i64 {
    let cleaned = etime.trim();
    if cleaned.is_empty() {
        return -1;
    }

    if let Some((days, rest)) = cleaned.split_once('-') {
        let Some((hours, minutes, _seconds)) = parse_colon_triplet(rest) else {
            return -1;
        };
        let Ok(days) = days.parse::<i64>() else {
            return -1;
        };
        return days * 24 * 60 + hours * 60 + minutes;
    }

    let parts: Vec<_> = cleaned.split(':').collect();
    match parts.as_slice() {
        [minutes, _seconds] => minutes.parse::<i64>().unwrap_or(-1),
        [hours, minutes, seconds] => {
            let (Ok(hours), Ok(minutes), Ok(_seconds)) = (
                hours.parse::<i64>(),
                minutes.parse::<i64>(),
                seconds.parse::<i64>(),
            ) else {
                return -1;
            };
            hours * 60 + minutes
        }
        _ => -1,
    }
}

pub fn get_platform_timeout(base: Duration) -> Duration {
    base
}

pub fn is_bun_executable_path(path: impl AsRef<str>) -> bool {
    let trimmed = path.as_ref().trim();
    let file_name = trimmed
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    file_name == "bun"
}

pub fn resolve_worker_runtime_path(options: RuntimeResolverOptions) -> Option<PathBuf> {
    Some(options.exec_path.unwrap_or_else(|| {
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("claude-mem-worker"))
    }))
}

pub async fn get_child_processes(parent_pid: i64) -> Vec<u32> {
    let _ = parent_pid;
    Vec::new()
}

pub async fn force_kill_process(pid: i64) {
    if pid <= 0 || pid > i64::from(i32::MAX) {
        return;
    }

    #[cfg(unix)]
    {
        let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
}

pub async fn wait_for_processes_exit(pids: &[i64], timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if pids.iter().all(|pid| !is_process_alive(*pid)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub fn is_process_alive(pid: i64) -> bool {
    if pid <= 0 || pid > i64::from(i32::MAX) {
        return false;
    }

    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as i32, 0) };
        if rc == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(not(unix))]
    compile_error!("claude-mem-rs supervisor requires a POSIX-like target");
}

pub fn clean_stale_pid_file() -> CleanStalePidFileStatus {
    clean_stale_pid_file_at(default_pid_path())
}

pub fn clean_stale_pid_file_at(path: impl AsRef<Path>) -> CleanStalePidFileStatus {
    let path = path.as_ref();
    let Some(info) = read_pid_file_at(path) else {
        return if path.exists() {
            let _ = remove_pid_file_at(path);
            CleanStalePidFileStatus::Invalid
        } else {
            CleanStalePidFileStatus::Missing
        };
    };

    if is_process_alive(i64::from(info.pid)) {
        CleanStalePidFileStatus::Alive(info)
    } else {
        let _ = remove_pid_file_at(path);
        CleanStalePidFileStatus::RemovedStale(info)
    }
}

pub fn is_pid_file_recent(threshold: Duration) -> bool {
    is_pid_file_recent_at(default_pid_path(), threshold)
}

pub fn is_pid_file_recent_at(path: impl AsRef<Path>, threshold: Duration) -> bool {
    if threshold.is_zero() {
        return false;
    }

    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age < threshold)
}

pub fn touch_pid_file() -> std::io::Result<()> {
    touch_pid_file_at(default_pid_path())
}

pub fn touch_pid_file_at(path: impl AsRef<Path>) -> std::io::Result<()> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(());
    }
    let bytes = std::fs::read(path)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

pub fn run_one_time_chroma_migration(data_directory: impl AsRef<Path>) -> std::io::Result<()> {
    let data_directory = data_directory.as_ref();
    let marker_path = data_directory.join(CHROMA_MIGRATION_MARKER_FILENAME);
    if marker_path.exists() {
        return Ok(());
    }

    let chroma_dir = data_directory.join("chroma");
    if chroma_dir.exists() {
        std::fs::remove_dir_all(chroma_dir)?;
    }

    std::fs::create_dir_all(data_directory)?;
    std::fs::write(marker_path, "completed")?;
    Ok(())
}

pub fn spawn_daemon(
    executable_path: impl AsRef<Path>,
    port: u16,
    extra_env: impl IntoIterator<Item = (String, String)>,
) -> Option<u32> {
    let executable_path = executable_path.as_ref();
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.insert("CLAUDE_MEM_WORKER_PORT".to_owned(), port.to_string());
    env.extend(extra_env);

    let mut command = Command::new(executable_path);
    command
        .args(worker_daemon_args(executable_path))
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    command.spawn().ok().map(|child| child.id())
}

fn worker_daemon_args(executable_path: &Path) -> Vec<&'static str> {
    let file_name = executable_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if matches!(file_name, "claude-mem" | "claude-mem-rs") {
        vec!["worker", "--daemon"]
    } else {
        vec!["--daemon"]
    }
}

fn default_pid_path() -> PathBuf {
    if let Ok(p) = std::env::var("CLAUDE_MEM_HOME") {
        return PathBuf::from(p).join("worker.pid");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".claude-mem").join("worker.pid")
}

fn parse_colon_triplet(value: &str) -> Option<(i64, i64, i64)> {
    let parts: Vec<_> = value.split(':').collect();
    match parts.as_slice() {
        [hours, minutes, seconds] => Some((
            hours.parse().ok()?,
            minutes.parse().ok()?,
            seconds.parse().ok()?,
        )),
        _ => None,
    }
}
