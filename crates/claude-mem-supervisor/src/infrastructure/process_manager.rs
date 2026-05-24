//! PID file and process lifecycle helpers.
//!
//! Port of `src/services/infrastructure/ProcessManager.ts`. Behaviour is
//! shared across Unix and Windows hosts; platform-specific differences
//! (signals/setsid vs. taskkill/detached creation flags) are gated below.

use claude_mem_core::shared::platform_paths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

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
    // Accept the Windows shim variants used by `bun upgrade` and `where bun`.
    matches!(file_name.as_str(), "bun" | "bun.exe" | "bun.cmd")
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

    #[cfg(windows)]
    {
        let _ = build_taskkill_command(pid, TaskkillMode::Kill)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Send a graceful termination request to `pid`.
///
/// On Unix this delivers `SIGTERM`. On Windows this issues
/// `taskkill /PID <pid> /T` (no `/F`, so console processes receive
/// `WM_CLOSE`/`CTRL_BREAK` and may run shutdown handlers).
pub async fn terminate_process(pid: i64) {
    if pid <= 0 || pid > i64::from(i32::MAX) {
        return;
    }

    #[cfg(unix)]
    {
        let _ = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }

    #[cfg(windows)]
    {
        let _ = build_taskkill_command(pid, TaskkillMode::Term)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Try a graceful terminate, poll for exit, then force-kill if the process is
/// still alive after `grace`.
pub async fn graceful_kill(pid: i64, grace: Duration) {
    if pid <= 0 || pid > i64::from(i32::MAX) {
        return;
    }
    if !is_process_alive(pid) {
        return;
    }
    terminate_process(pid).await;

    let deadline = tokio::time::Instant::now() + grace;
    while tokio::time::Instant::now() < deadline {
        if !is_process_alive(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if is_process_alive(pid) {
        force_kill_process(pid).await;
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskkillMode {
    /// Graceful: `/T` (tree) without `/F` — sends `WM_CLOSE`/`CTRL_BREAK`.
    Term,
    /// Forced: `/F /T` — kill the process tree immediately.
    Kill,
}

#[cfg(windows)]
fn taskkill_args(pid: i64, mode: TaskkillMode) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    if mode == TaskkillMode::Kill {
        args.push("/F".to_owned());
    }
    args.push("/T".to_owned());
    args.push("/PID".to_owned());
    args.push(pid.to_string());
    args
}

#[cfg(windows)]
fn build_taskkill_command(pid: i64, mode: TaskkillMode) -> Command {
    let mut command = Command::new("taskkill");
    command.args(taskkill_args(pid, mode));
    command
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

    #[cfg(windows)]
    {
        is_process_alive_windows(pid)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

#[cfg(windows)]
fn is_process_alive_windows(pid: i64) -> bool {
    // `tasklist /FI "PID eq <pid>" /NH /FO CSV` prints the matching row when
    // the process is running and an INFO: line when it is not. Shelling out
    // avoids pulling a Windows FFI crate for this first compatibility pass.
    let Ok(output) = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .any(|line| line.trim_start().starts_with('"'))
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

    #[cfg(windows)]
    {
        // DETACHED_PROCESS (0x00000008) drops the console handle inheritance,
        // CREATE_NEW_PROCESS_GROUP (0x00000200) gives the child its own
        // process group so Ctrl-C from the supervisor terminal does not
        // signal it. Together they match the intent of `setsid` on Unix.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    command.spawn().ok().map(|child| child.id())
}

fn worker_daemon_args(executable_path: &Path) -> Vec<&'static str> {
    let file_name = executable_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    // Strip `.exe` on Windows so the multiplexed CLI name is recognised the
    // same way as on Unix.
    let normalised = file_name
        .strip_suffix(".exe")
        .or_else(|| file_name.strip_suffix(".EXE"))
        .unwrap_or(file_name);
    if matches!(normalised, "claude-mem" | "claude-mem-rs") {
        vec!["worker", "--daemon"]
    } else {
        vec!["--daemon"]
    }
}

fn default_pid_path() -> PathBuf {
    platform_paths::worker_pid_path()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_daemon_args_unix_multiplexed() {
        assert_eq!(
            worker_daemon_args(Path::new("/usr/local/bin/claude-mem")),
            vec!["worker", "--daemon"]
        );
        assert_eq!(
            worker_daemon_args(Path::new("/opt/claude-mem-rs")),
            vec!["worker", "--daemon"]
        );
    }

    #[test]
    fn worker_daemon_args_strips_exe_suffix() {
        // `Path::file_name` does not treat `\` as a separator on Unix, so use
        // forward-slash paths with `.exe`/`.EXE` suffixes to exercise the
        // suffix-stripping branch on Linux CI.
        assert_eq!(
            worker_daemon_args(Path::new("/opt/claude-mem.exe")),
            vec!["worker", "--daemon"]
        );
        assert_eq!(
            worker_daemon_args(Path::new("/opt/claude-mem-rs.EXE")),
            vec!["worker", "--daemon"]
        );
    }

    #[test]
    fn worker_daemon_args_dedicated_worker_binary() {
        // Existing behaviour: a binary named anything else just gets `--daemon`.
        assert_eq!(
            worker_daemon_args(Path::new("/usr/local/bin/claude-mem-worker")),
            vec!["--daemon"]
        );
        assert_eq!(
            worker_daemon_args(Path::new("/opt/claude-mem-worker.exe")),
            vec!["--daemon"]
        );
    }

    #[test]
    fn bun_path_matches_unix_and_windows_shims() {
        assert!(is_bun_executable_path("/usr/local/bin/bun"));
        assert!(is_bun_executable_path("/c/Users/me/.bun/bin/bun.exe"));
        assert!(is_bun_executable_path("/c/bin/BUN.CMD"));
        assert!(!is_bun_executable_path("/usr/local/bin/node"));
        assert!(!is_bun_executable_path("/usr/local/bin/bunny"));
    }

    #[test]
    fn dead_pid_zero_is_never_alive() {
        assert!(!is_process_alive(0));
        assert!(!is_process_alive(-1));
    }

    #[cfg(windows)]
    #[test]
    fn taskkill_term_uses_tree_without_force() {
        let args = super::taskkill_args(1234, super::TaskkillMode::Term);
        assert_eq!(args, vec!["/T", "/PID", "1234"]);
        assert!(!args.iter().any(|a| a == "/F"));
    }

    #[cfg(windows)]
    #[test]
    fn taskkill_kill_uses_force_and_tree() {
        let args = super::taskkill_args(1234, super::TaskkillMode::Kill);
        assert_eq!(args, vec!["/F", "/T", "/PID", "1234"]);
    }
}
