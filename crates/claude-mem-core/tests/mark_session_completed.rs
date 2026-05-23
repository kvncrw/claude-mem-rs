//! `SessionStore.markSessionCompleted` tests (port of
//! `tests/services/sqlite/session-store-mark-completed.test.ts`).

use claude_mem_core::db::open_in_memory;
use claude_mem_core::db::sessions;
use claude_mem_core::types::session::CreateSessionInput;

fn create(conn: &rusqlite::Connection, content_id: &str) -> i64 {
    sessions::create_session(
        conn,
        &CreateSessionInput {
            content_session_id: content_id.into(),
            project: "project".into(),
            user_prompt: Some("prompt".into()),
            started_at: "2026-05-23T15:00:00Z".into(),
            started_at_epoch: 1_748_012_400,
        },
    )
    .unwrap();
    sessions::get_session_by_content_id(conn, content_id)
        .unwrap()
        .unwrap()
        .id
}

#[test]
fn sets_status_to_completed_and_records_timestamps() {
    let conn = open_in_memory().unwrap();
    let before_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let id = create(&conn, "session-1");

    sessions::mark_session_completed(&conn, id).unwrap();

    let row = sessions::get_session_by_content_id(&conn, "session-1")
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "completed");
    assert!(row.completed_at.is_some());
    assert!(row.completed_at_epoch.is_some());
    let epoch = row.completed_at_epoch.unwrap();
    assert!(
        epoch >= before_ms,
        "epoch {epoch} should be >= before_ms {before_ms}"
    );
    let after_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    assert!(epoch <= after_ms);
}

#[test]
fn leaves_other_sessions_unaffected() {
    let conn = open_in_memory().unwrap();
    let id1 = create(&conn, "session-a");
    let _id2 = create(&conn, "session-b");

    sessions::mark_session_completed(&conn, id1).unwrap();

    let row2 = sessions::get_session_by_content_id(&conn, "session-b")
        .unwrap()
        .unwrap();
    assert_eq!(row2.status, "active");
    assert!(row2.completed_at.is_none());
}

#[test]
fn no_throw_on_nonexistent_id() {
    let conn = open_in_memory().unwrap();
    // Should be a no-op, not a panic.
    sessions::mark_session_completed(&conn, 99_999).unwrap();
}

#[test]
fn completed_at_is_valid_iso_timestamp() {
    let conn = open_in_memory().unwrap();
    let id = create(&conn, "session-iso");
    sessions::mark_session_completed(&conn, id).unwrap();

    let row = sessions::get_session_by_content_id(&conn, "session-iso")
        .unwrap()
        .unwrap();
    let completed = row.completed_at.expect("should be set");
    // Time crate should parse it as RFC3339.
    let parsed = time::OffsetDateTime::parse(
        &completed,
        &time::format_description::well_known::Rfc3339,
    );
    assert!(
        parsed.is_ok(),
        "completed_at {completed} is not a valid RFC3339 timestamp: {:?}",
        parsed.err()
    );
}
