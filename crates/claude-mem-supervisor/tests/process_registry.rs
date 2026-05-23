//! Process registry tests (port of `tests/supervisor/process-registry.test.ts`,
//! 423 lines, 21 cases).

use claude_mem_supervisor::infrastructure::process_registry::{
    create_process_registry, is_pid_alive, ManagedProcessInfo, SessionId,
};
use serde_json::Value;
use std::fs::{read_to_string, write};
use std::path::Path;
use tempfile::TempDir;

fn registry_path(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("supervisor.json")
}

fn current_pid() -> i64 {
    i64::from(std::process::id())
}

fn process_info(
    process_type: &str,
    pid: i64,
    session_id: Option<SessionId>,
    started_at: &str,
) -> ManagedProcessInfo {
    ManagedProcessInfo {
        pid,
        process_type: process_type.to_owned(),
        session_id,
        started_at: started_at.to_owned(),
    }
}

fn disk_json(path: &Path) -> Value {
    serde_json::from_str(&read_to_string(path).unwrap()).unwrap()
}

#[test]
fn is_pid_alive_treats_current_process_as_alive() {
    assert!(is_pid_alive(current_pid()));
}

#[test]
fn is_pid_alive_treats_impossibly_high_pid_as_dead() {
    assert!(!is_pid_alive(2_147_483_647));
}

#[test]
fn is_pid_alive_treats_negative_pid_as_dead() {
    assert!(!is_pid_alive(-1));
}

#[test]
fn persists_entries_to_disk_and_reloads_them_on_initialize() {
    let dir = TempDir::new().unwrap();
    let path = registry_path(&dir);

    let mut registry1 = create_process_registry(&path);
    registry1
        .register(
            "worker:1",
            process_info("worker", current_pid(), None, "2026-03-15T00:00:00.000Z"),
        )
        .unwrap();

    assert!(path.exists());
    assert!(disk_json(&path)["processes"]["worker:1"].is_object());

    let mut registry2 = create_process_registry(&path);
    registry2.initialize().unwrap();
    let records = registry2.get_all().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, "worker:1");
    assert_eq!(records[0].pid, current_pid());
}

#[test]
fn prunes_dead_processes_on_initialize() {
    let dir = TempDir::new().unwrap();
    let path = registry_path(&dir);
    write(
        &path,
        serde_json::json!({
            "processes": {
                "alive": {
                    "pid": current_pid(),
                    "type": "worker",
                    "startedAt": "2026-03-15T00:00:00.000Z"
                },
                "dead": {
                    "pid": 2_147_483_647i64,
                    "type": "mcp",
                    "startedAt": "2026-03-15T00:00:01.000Z"
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let mut registry = create_process_registry(&path);
    registry.initialize().unwrap();

    let records = registry.get_all().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, "alive");
    assert!(path.exists());
}

#[test]
fn handles_corrupted_registry_file_gracefully() {
    let dir = TempDir::new().unwrap();
    let path = registry_path(&dir);
    write(&path, "{ not valid json!!!").unwrap();

    let mut registry = create_process_registry(&path);
    registry.initialize().unwrap();

    assert!(registry.get_all().unwrap().is_empty());
}

#[test]
fn register_adds_an_entry_retrievable_by_get_all() {
    let dir = TempDir::new().unwrap();
    let mut registry = create_process_registry(registry_path(&dir));

    assert!(registry.get_all().unwrap().is_empty());

    registry
        .register(
            "sdk:1",
            process_info("sdk", current_pid(), None, "2026-03-15T00:00:00.000Z"),
        )
        .unwrap();

    let records = registry.get_all().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, "sdk:1");
    assert_eq!(records[0].process_type, "sdk");
}

#[test]
fn unregister_removes_an_entry() {
    let dir = TempDir::new().unwrap();
    let mut registry = create_process_registry(registry_path(&dir));

    registry
        .register(
            "sdk:1",
            process_info("sdk", current_pid(), None, "2026-03-15T00:00:00.000Z"),
        )
        .unwrap();
    assert_eq!(registry.get_all().unwrap().len(), 1);

    registry.unregister("sdk:1").unwrap();
    assert!(registry.get_all().unwrap().is_empty());
}

#[test]
fn unregister_is_a_noop_for_unknown_ids() {
    let dir = TempDir::new().unwrap();
    let mut registry = create_process_registry(registry_path(&dir));

    registry
        .register(
            "sdk:1",
            process_info("sdk", current_pid(), None, "2026-03-15T00:00:00.000Z"),
        )
        .unwrap();

    registry.unregister("nonexistent").unwrap();
    assert_eq!(registry.get_all().unwrap().len(), 1);
}

#[test]
fn get_all_returns_records_sorted_by_started_at_ascending() {
    let dir = TempDir::new().unwrap();
    let mut registry = create_process_registry(registry_path(&dir));

    registry
        .register(
            "newest",
            process_info("sdk", current_pid(), None, "2026-03-15T00:00:02.000Z"),
        )
        .unwrap();
    registry
        .register(
            "oldest",
            process_info("worker", current_pid(), None, "2026-03-15T00:00:00.000Z"),
        )
        .unwrap();
    registry
        .register(
            "middle",
            process_info("mcp", current_pid(), None, "2026-03-15T00:00:01.000Z"),
        )
        .unwrap();

    let ids: Vec<_> = registry
        .get_all()
        .unwrap()
        .into_iter()
        .map(|record| record.id)
        .collect();
    assert_eq!(ids, vec!["oldest", "middle", "newest"]);
}

#[test]
fn get_all_returns_empty_array_when_no_entries_exist() {
    let dir = TempDir::new().unwrap();
    let mut registry = create_process_registry(registry_path(&dir));

    assert!(registry.get_all().unwrap().is_empty());
}

#[test]
fn get_by_session_filters_records_by_session_id() {
    let dir = TempDir::new().unwrap();
    let mut registry = create_process_registry(registry_path(&dir));

    registry
        .register(
            "sdk:1",
            process_info(
                "sdk",
                current_pid(),
                Some(42.into()),
                "2026-03-15T00:00:00.000Z",
            ),
        )
        .unwrap();
    registry
        .register(
            "sdk:2",
            process_info(
                "sdk",
                current_pid(),
                Some("other".into()),
                "2026-03-15T00:00:01.000Z",
            ),
        )
        .unwrap();

    let records = registry.get_by_session(42).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, "sdk:1");
}

#[test]
fn get_by_session_returns_empty_array_when_no_processes_match_the_session() {
    let dir = TempDir::new().unwrap();
    let mut registry = create_process_registry(registry_path(&dir));

    registry
        .register(
            "sdk:1",
            process_info(
                "sdk",
                current_pid(),
                Some(42.into()),
                "2026-03-15T00:00:00.000Z",
            ),
        )
        .unwrap();

    assert!(registry.get_by_session(999).unwrap().is_empty());
}

#[test]
fn get_by_session_matches_string_and_numeric_session_ids_by_string_comparison() {
    let dir = TempDir::new().unwrap();
    let mut registry = create_process_registry(registry_path(&dir));

    registry
        .register(
            "sdk:1",
            process_info(
                "sdk",
                current_pid(),
                Some("42".into()),
                "2026-03-15T00:00:00.000Z",
            ),
        )
        .unwrap();

    assert_eq!(registry.get_by_session(42).unwrap().len(), 1);
}

#[test]
fn prune_dead_entries_removes_dead_pids_and_preserves_live_ones() {
    let dir = TempDir::new().unwrap();
    let mut registry = create_process_registry(registry_path(&dir));

    registry
        .register(
            "alive",
            process_info("worker", current_pid(), None, "2026-03-15T00:00:00.000Z"),
        )
        .unwrap();
    registry
        .register(
            "dead",
            process_info("mcp", 2_147_483_647, None, "2026-03-15T00:00:01.000Z"),
        )
        .unwrap();

    let removed = registry.prune_dead_entries().unwrap();
    let records = registry.get_all().unwrap();
    assert_eq!(removed, 1);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, "alive");
}

#[test]
fn prune_dead_entries_returns_zero_when_all_entries_are_alive() {
    let dir = TempDir::new().unwrap();
    let mut registry = create_process_registry(registry_path(&dir));

    registry
        .register(
            "alive",
            process_info("worker", current_pid(), None, "2026-03-15T00:00:00.000Z"),
        )
        .unwrap();

    let removed = registry.prune_dead_entries().unwrap();
    assert_eq!(removed, 0);
    assert_eq!(registry.get_all().unwrap().len(), 1);
}

#[test]
fn prune_dead_entries_persists_changes_to_disk_after_pruning() {
    let dir = TempDir::new().unwrap();
    let path = registry_path(&dir);
    let mut registry = create_process_registry(&path);

    registry
        .register(
            "dead",
            process_info("mcp", 2_147_483_647, None, "2026-03-15T00:00:01.000Z"),
        )
        .unwrap();

    registry.prune_dead_entries().unwrap();

    let disk_data = disk_json(&path);
    assert_eq!(disk_data["processes"].as_object().unwrap().len(), 0);
}

#[test]
fn clear_removes_all_entries_and_persists_to_disk() {
    let dir = TempDir::new().unwrap();
    let path = registry_path(&dir);
    let mut registry = create_process_registry(&path);

    registry
        .register(
            "sdk:1",
            process_info("sdk", current_pid(), None, "2026-03-15T00:00:00.000Z"),
        )
        .unwrap();
    registry
        .register(
            "sdk:2",
            process_info("sdk", current_pid(), None, "2026-03-15T00:00:01.000Z"),
        )
        .unwrap();

    assert_eq!(registry.get_all().unwrap().len(), 2);

    registry.clear().unwrap();
    assert!(registry.get_all().unwrap().is_empty());

    let disk_data = disk_json(&path);
    assert_eq!(disk_data["processes"].as_object().unwrap().len(), 0);
}

#[test]
fn create_process_registry_creates_an_isolated_instance_with_custom_path() {
    let dir1 = TempDir::new().unwrap();
    let dir2 = TempDir::new().unwrap();
    let mut registry1 = create_process_registry(registry_path(&dir1));
    let mut registry2 = create_process_registry(registry_path(&dir2));

    registry1
        .register(
            "sdk:1",
            process_info("sdk", current_pid(), None, "2026-03-15T00:00:00.000Z"),
        )
        .unwrap();

    assert_eq!(registry1.get_all().unwrap().len(), 1);
    assert!(registry2.get_all().unwrap().is_empty());
}

#[tokio::test]
async fn reap_session_unregisters_dead_processes_for_the_given_session() {
    let dir = TempDir::new().unwrap();
    let mut registry = create_process_registry(registry_path(&dir));

    registry
        .register(
            "sdk:99:50001",
            process_info(
                "sdk",
                2_147_483_640,
                Some(99.into()),
                "2026-03-15T00:00:00.000Z",
            ),
        )
        .unwrap();
    registry
        .register(
            "mcp:99:50002",
            process_info(
                "mcp",
                2_147_483_641,
                Some(99.into()),
                "2026-03-15T00:00:01.000Z",
            ),
        )
        .unwrap();
    registry
        .register(
            "sdk:100:50003",
            process_info(
                "sdk",
                current_pid(),
                Some(100.into()),
                "2026-03-15T00:00:02.000Z",
            ),
        )
        .unwrap();

    let reaped = registry.reap_session(99).await.unwrap();
    assert_eq!(reaped, 2);
    assert!(registry.get_by_session(99).unwrap().is_empty());
    assert_eq!(registry.get_by_session(100).unwrap().len(), 1);
}

#[tokio::test]
async fn reap_session_returns_zero_when_no_processes_match_the_session() {
    let dir = TempDir::new().unwrap();
    let mut registry = create_process_registry(registry_path(&dir));

    registry
        .register(
            "sdk:1",
            process_info(
                "sdk",
                current_pid(),
                Some(42.into()),
                "2026-03-15T00:00:00.000Z",
            ),
        )
        .unwrap();

    let reaped = registry.reap_session(999).await.unwrap();
    assert_eq!(reaped, 0);
    assert_eq!(registry.get_all().unwrap().len(), 1);
}
