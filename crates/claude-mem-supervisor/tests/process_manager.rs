use claude_mem_supervisor::infrastructure::process_manager::{
    clean_stale_pid_file_at, get_child_processes, get_platform_timeout, is_bun_executable_path,
    is_pid_file_recent_at, is_process_alive, parse_elapsed_time, read_pid_file_at,
    remove_pid_file_at, resolve_worker_runtime_path, run_one_time_chroma_migration,
    touch_pid_file_at, write_pid_file_at, CleanStalePidFileStatus, PidInfo, RuntimeResolverOptions,
};
use std::path::PathBuf;
use std::time::Duration;
use tempfile::tempdir;

fn pid_info(pid: u32) -> PidInfo {
    PidInfo {
        pid,
        port: 37777,
        started_at: "2026-05-23T00:00:00.000Z".to_owned(),
    }
}

#[test]
fn writes_reads_overwrites_and_removes_pid_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("worker.pid");

    write_pid_file_at(&path, &pid_info(12345)).unwrap();
    assert_eq!(read_pid_file_at(&path).unwrap().pid, 12345);

    write_pid_file_at(&path, &pid_info(22222)).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("\"pid\": 22222"));
    assert_eq!(read_pid_file_at(&path).unwrap().port, 37777);

    remove_pid_file_at(&path).unwrap();
    assert!(read_pid_file_at(&path).is_none());
    remove_pid_file_at(&path).unwrap();
}

#[test]
fn corrupted_pid_file_reads_as_none() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("worker.pid");
    std::fs::write(&path, "not json").unwrap();

    assert!(read_pid_file_at(&path).is_none());
}

#[test]
fn parses_posix_elapsed_time_formats() {
    assert_eq!(parse_elapsed_time("05:30"), 5);
    assert_eq!(parse_elapsed_time("00:45"), 0);
    assert_eq!(parse_elapsed_time("01:30:00"), 90);
    assert_eq!(parse_elapsed_time("02:15:30"), 135);
    assert_eq!(parse_elapsed_time("1-00:00:00"), 1440);
    assert_eq!(parse_elapsed_time("2-12:30:00"), 3630);
    assert_eq!(parse_elapsed_time(""), -1);
    assert_eq!(parse_elapsed_time("invalid"), -1);
    assert_eq!(parse_elapsed_time("01:nope:00"), -1);
}

#[test]
fn platform_timeout_is_unmodified_on_posix() {
    assert_eq!(
        get_platform_timeout(Duration::from_millis(333)),
        Duration::from_millis(333)
    );
}

#[test]
fn resolves_current_runtime_path_for_posix_worker_spawn() {
    let resolved = resolve_worker_runtime_path(RuntimeResolverOptions {
        exec_path: Some(PathBuf::from("/usr/local/bin/claude-mem-worker")),
        ..RuntimeResolverOptions::default()
    });

    assert_eq!(
        resolved,
        Some(PathBuf::from("/usr/local/bin/claude-mem-worker"))
    );
}

#[test]
fn bun_path_detection_is_basename_based() {
    assert!(is_bun_executable_path("/home/alice/.bun/bin/bun"));
    assert!(!is_bun_executable_path("/usr/bin/node"));
}

#[test]
fn process_liveness_uses_posix_signal_zero() {
    assert!(is_process_alive(i64::from(std::process::id())));
    assert!(!is_process_alive(0));
    assert!(!is_process_alive(-1));
    assert!(!is_process_alive(i64::from(i32::MAX)));
}

#[test]
fn clean_stale_pid_file_removes_dead_process_but_keeps_live_one() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("worker.pid");

    write_pid_file_at(&path, &pid_info(i32::MAX as u32)).unwrap();
    assert!(matches!(
        clean_stale_pid_file_at(&path),
        CleanStalePidFileStatus::RemovedStale(_)
    ));
    assert!(!path.exists());

    write_pid_file_at(&path, &pid_info(std::process::id())).unwrap();
    assert!(matches!(
        clean_stale_pid_file_at(&path),
        CleanStalePidFileStatus::Alive(_)
    ));
    assert!(path.exists());
}

#[test]
fn pid_file_recency_and_touch_are_best_effort() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("worker.pid");

    assert!(!is_pid_file_recent_at(&path, Duration::from_secs(15)));
    write_pid_file_at(&path, &pid_info(std::process::id())).unwrap();
    assert!(is_pid_file_recent_at(&path, Duration::from_secs(15)));
    assert!(!is_pid_file_recent_at(&path, Duration::ZERO));
    touch_pid_file_at(&path).unwrap();
    touch_pid_file_at(dir.path().join("missing.pid")).unwrap();
}

#[test]
fn chroma_migration_removes_chroma_once_and_writes_marker() {
    let dir = tempdir().unwrap();
    let chroma = dir.path().join("chroma");
    std::fs::create_dir_all(&chroma).unwrap();
    std::fs::write(chroma.join("data.bin"), "rebuildable").unwrap();

    run_one_time_chroma_migration(dir.path()).unwrap();
    assert!(!chroma.exists());
    assert!(dir.path().join(".chroma-cleaned-v10.3").exists());

    std::fs::create_dir_all(&chroma).unwrap();
    std::fs::write(chroma.join("data.bin"), "kept").unwrap();
    run_one_time_chroma_migration(dir.path()).unwrap();
    assert!(chroma.join("data.bin").exists());
}

#[tokio::test]
async fn child_process_enumeration_is_empty_on_posix_path() {
    assert!(get_child_processes(i64::from(std::process::id()))
        .await
        .is_empty());
    assert!(get_child_processes(-1).await.is_empty());
}
