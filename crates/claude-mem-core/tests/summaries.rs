//! Summaries module tests (port of
//! `tests/sqlite/summaries.test.ts`).

use claude_mem_core::db::open_in_memory;
use claude_mem_core::db::sessions;
use claude_mem_core::db::summaries::{get_summary_for_session, store_summary, SummaryInput};
use claude_mem_core::types::session::CreateSessionInput;

fn base_input() -> SummaryInput {
    SummaryInput {
        memory_session_id: String::new(),
        project: String::new(),
        request: Some("User requested feature X".into()),
        investigated: Some("Explored the codebase".into()),
        learned: Some("Discovered pattern Y".into()),
        completed: Some("Implemented feature X".into()),
        next_steps: Some("Add tests and documentation".into()),
        notes: Some("Consider edge case Z".into()),
        files_read: None,
        files_edited: None,
        prompt_number: None,
        discovery_tokens: None,
        created_at: "2026-05-23T15:00:00Z".into(),
        created_at_epoch: 1748012400,
        merged_into_project: None,
    }
}

fn create_session_with_memory_id(
    conn: &rusqlite::Connection,
    content_session_id: &str,
    memory_session_id: &str,
    project: &str,
) -> String {
    let input = CreateSessionInput {
        content_session_id: content_session_id.into(),
        project: project.into(),
        user_prompt: Some("initial prompt".into()),
        started_at: "2026-05-23T15:00:00Z".into(),
        started_at_epoch: 1748012400,
    };
    sessions::create_session(conn, &input).unwrap();
    sessions::update_memory_session_id(conn, content_session_id, memory_session_id).unwrap();
    memory_session_id.to_string()
}

/// Fill the FK-scoped fields for a summary input.
fn fill(input: SummaryInput, memory_session_id: &str, project: &str) -> SummaryInput {
    SummaryInput {
        memory_session_id: memory_session_id.into(),
        project: project.into(),
        ..input
    }
}

#[test]
fn store_returns_positive_id_and_epoch() {
    let conn = open_in_memory().unwrap();
    let mem =
        create_session_with_memory_id(&conn, "content-sum-123", "mem-sum-123", "test-project");
    let id = store_summary(&conn, &fill(base_input(), &mem, "test-project")).unwrap();
    assert!(id > 0);
}

#[test]
fn store_persists_all_fields() {
    let conn = open_in_memory().unwrap();
    let mem =
        create_session_with_memory_id(&conn, "content-sum-456", "mem-sum-456", "test-project");
    let mut input = base_input();
    input.request = Some("Refactor the database layer".into());
    input.investigated = Some("Analyzed current schema".into());
    input.learned = Some("Found N+1 query issues".into());
    input.completed = Some("Optimized queries".into());
    input.next_steps = Some("Monitor performance".into());
    input.notes = Some("May need caching".into());
    input.prompt_number = Some(1);
    input.discovery_tokens = Some(500);

    store_summary(&conn, &fill(input, &mem, "test-project")).unwrap();

    let rows = get_summary_for_session(&conn, &mem).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].request.as_deref(),
        Some("Refactor the database layer")
    );
    assert_eq!(
        rows[0].investigated.as_deref(),
        Some("Analyzed current schema")
    );
    assert_eq!(rows[0].learned.as_deref(), Some("Found N+1 query issues"));
    assert_eq!(rows[0].completed.as_deref(), Some("Optimized queries"));
    assert_eq!(rows[0].next_steps.as_deref(), Some("Monitor performance"));
    assert_eq!(rows[0].notes.as_deref(), Some("May need caching"));
    assert_eq!(rows[0].prompt_number, Some(1));
}

#[test]
fn override_timestamp_epoch_honours_caller() {
    let conn = open_in_memory().unwrap();
    let mem =
        create_session_with_memory_id(&conn, "content-sum-789", "mem-sum-789", "test-project");
    let past = 1_650_000_000_000i64;
    let mut input = base_input();
    input.created_at_epoch = past;
    store_summary(&conn, &fill(input, &mem, "test-project")).unwrap();

    let rows = get_summary_for_session(&conn, &mem).unwrap();
    assert_eq!(rows[0].created_at_epoch, past);
}

#[test]
fn null_notes_round_trip() {
    let conn = open_in_memory().unwrap();
    let mem = create_session_with_memory_id(&conn, "content-sum-null", "mem-sum-null", "p");
    let mut input = base_input();
    input.notes = None;
    store_summary(&conn, &fill(input, &mem, "p")).unwrap();

    let rows = get_summary_for_session(&conn, &mem).unwrap();
    assert!(rows[0].notes.is_none());
}

#[test]
fn by_memory_session_returns_none_for_missing() {
    let conn = open_in_memory().unwrap();
    let rows = get_summary_for_session(&conn, "nonexistent-session").unwrap();
    assert!(rows.is_empty());
}

#[test]
fn most_recent_when_multiple() {
    let conn = open_in_memory().unwrap();
    let mem = create_session_with_memory_id(&conn, "content-multi", "multi-sum", "project");

    let mut older = base_input();
    older.request = Some("First request".into());
    older.prompt_number = Some(1);
    older.created_at_epoch = 1_000_000_000_000;
    store_summary(&conn, &fill(older, &mem, "project")).unwrap();

    let mut newer = base_input();
    newer.request = Some("Second request".into());
    newer.prompt_number = Some(2);
    newer.created_at_epoch = 2_000_000_000_000;
    store_summary(&conn, &fill(newer, &mem, "project")).unwrap();

    let rows = get_summary_for_session(&conn, &mem).unwrap();
    assert_eq!(rows.len(), 2);
    // DESC order by epoch.
    assert_eq!(rows[0].request.as_deref(), Some("Second request"));
    assert_eq!(rows[0].prompt_number, Some(2));
    assert_eq!(rows[1].request.as_deref(), Some("First request"));
    assert_eq!(rows[1].prompt_number, Some(1));
}

#[test]
fn row_has_all_expected_columns() {
    let conn = open_in_memory().unwrap();
    let mem = create_session_with_memory_id(&conn, "content-fields", "fields-check", "p");
    let mut input = base_input();
    input.prompt_number = Some(1);
    input.discovery_tokens = Some(100);
    input.created_at_epoch = 1_500_000_000_000;
    input.created_at = "2017-07-13T15:20:00Z".into();
    store_summary(&conn, &fill(input, &mem, "p")).unwrap();

    let rows = get_summary_for_session(&conn, &mem).unwrap();
    let r = &rows[0];
    assert!(r.request.is_some());
    assert!(r.investigated.is_some());
    assert!(r.learned.is_some());
    assert!(r.completed.is_some());
    assert!(r.next_steps.is_some());
    assert!(r.notes.is_some());
    assert_eq!(r.prompt_number, Some(1));
    assert_eq!(r.discovery_tokens, 100);
    assert_eq!(r.created_at, "2017-07-13T15:20:00Z");
    assert_eq!(r.created_at_epoch, 1_500_000_000_000);
}
