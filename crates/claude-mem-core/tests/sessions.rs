//! Sessions module tests (port of
//! <https://github.com/thedotmack/claude-mem/blob/main/tests/sqlite/sessions.test.ts>).
//!
//! Tests `create_session`, `get_session_by_content_id`,
//! `get_session_by_memory_id`, `update_memory_session_id` against an
//! in-memory schema.

use claude_mem_core::db::{open_in_memory, sessions};
use claude_mem_core::types::session::CreateSessionInput;

fn now_iso() -> String {
    "2026-05-23T15:00:00Z".to_string()
}
fn now_epoch() -> i64 {
    1748012400
}

#[test]
fn create_new_session_returns_numeric_id() {
    let conn = open_in_memory().unwrap();
    let input = CreateSessionInput {
        content_session_id: "content-session-123".into(),
        project: "test-project".into(),
        user_prompt: Some("Initial user prompt".into()),
        started_at: now_iso(),
        started_at_epoch: now_epoch(),
    };
    let outcome = sessions::create_session(&conn, &input).unwrap();
    assert!(matches!(outcome, sessions::CreateSessionOutcome::Created));

    let row = sessions::get_session_by_content_id(&conn, "content-session-123")
        .unwrap()
        .expect("row");
    assert_eq!(row.content_session_id, "content-session-123");
    assert_eq!(row.project, "test-project");
    assert_eq!(row.user_prompt.as_deref(), Some("Initial user prompt"));
    // memory_session_id should be NULL until updateMemorySessionId is called.
    assert!(row.memory_session_id.is_none());
    assert_eq!(row.platform_source, "claude");
    assert_eq!(row.status, "active");
}

#[test]
fn create_is_idempotent_same_content_session_id() {
    let conn = open_in_memory().unwrap();
    let mk = |user_prompt: Option<&str>| CreateSessionInput {
        content_session_id: "content-session-456".into(),
        project: "test-project".into(),
        user_prompt: user_prompt.map(str::to_string),
        started_at: now_iso(),
        started_at_epoch: now_epoch(),
    };

    let out1 = sessions::create_session(&conn, &mk(Some("first prompt"))).unwrap();
    let out2 = sessions::create_session(&conn, &mk(Some("different prompt"))).unwrap();
    assert!(matches!(out1, sessions::CreateSessionOutcome::Created));
    assert!(matches!(
        out2,
        sessions::CreateSessionOutcome::AlreadyExisted
    ));

    // Original fields preserved — second call is a no-op.
    let row = sessions::get_session_by_content_id(&conn, "content-session-456")
        .unwrap()
        .unwrap();
    assert_eq!(row.user_prompt.as_deref(), Some("first prompt"));
}

#[test]
fn create_different_content_ids_produce_different_rows() {
    let conn = open_in_memory().unwrap();
    let mk = |id: &str| CreateSessionInput {
        content_session_id: id.into(),
        project: "project".into(),
        user_prompt: Some("p".into()),
        started_at: now_iso(),
        started_at_epoch: now_epoch(),
    };
    let o1 = sessions::create_session(&conn, &mk("session-a")).unwrap();
    let o2 = sessions::create_session(&conn, &mk("session-b")).unwrap();
    assert!(matches!(o1, sessions::CreateSessionOutcome::Created));
    assert!(matches!(o2, sessions::CreateSessionOutcome::Created));

    let r1 = sessions::get_session_by_content_id(&conn, "session-a")
        .unwrap()
        .unwrap();
    let r2 = sessions::get_session_by_content_id(&conn, "session-b")
        .unwrap()
        .unwrap();
    assert_ne!(r1.id, r2.id);
}

#[test]
fn get_returns_null_for_nonexistent() {
    let conn = open_in_memory().unwrap();
    let row = sessions::get_session_by_content_id(&conn, "does-not-exist").unwrap();
    assert!(row.is_none());
}

#[test]
fn update_memory_session_id_populates_field() {
    let conn = open_in_memory().unwrap();
    let input = CreateSessionInput {
        content_session_id: "update-test".into(),
        project: "project".into(),
        user_prompt: None,
        started_at: now_iso(),
        started_at_epoch: now_epoch(),
    };
    sessions::create_session(&conn, &input).unwrap();

    // Initially null.
    assert!(sessions::get_session_by_content_id(&conn, "update-test")
        .unwrap()
        .unwrap()
        .memory_session_id
        .is_none());

    // Populate it.
    let updated = sessions::update_memory_session_id(&conn, "update-test", "memory-xyz").unwrap();
    assert!(updated);

    let row = sessions::get_session_by_content_id(&conn, "update-test")
        .unwrap()
        .unwrap();
    assert_eq!(row.memory_session_id.as_deref(), Some("memory-xyz"));

    // Lookup by memory id now works.
    let by_memory = sessions::get_session_by_memory_id(&conn, "memory-xyz")
        .unwrap()
        .unwrap();
    assert_eq!(by_memory.content_session_id, "update-test");

    // Updating against a non-existent content id returns false (no rows touched).
    let no_touch = sessions::update_memory_session_id(&conn, "nope", "mem-2").unwrap();
    assert!(!no_touch);
}

#[test]
fn migration_25_defaults_platform_source_to_claude() {
    let conn = open_in_memory().unwrap();
    let input = CreateSessionInput {
        content_session_id: "platform-test".into(),
        project: "project".into(),
        user_prompt: None,
        started_at: now_iso(),
        started_at_epoch: now_epoch(),
    };
    sessions::create_session(&conn, &input).unwrap();

    let row = sessions::get_session_by_content_id(&conn, "platform-test")
        .unwrap()
        .unwrap();
    assert_eq!(row.platform_source, "claude");

    // Non-'claude' sources can still be inserted (cursor, windsurf, gemini-cli)
    // via the raw SQL path; the row_from fallback is what we're verifying.
    conn.execute(
        "INSERT INTO sdk_sessions
            (content_session_id, project, started_at, started_at_epoch, status, platform_source)
         VALUES (?1, ?2, ?3, ?4, 'active', 'cursor')",
        ["cursor-session", "project", "2026-05-23T15:00:00Z", "1748012400"],
    ).unwrap();

    let cursor_row = sessions::get_session_by_content_id(&conn, "cursor-session")
        .unwrap()
        .unwrap();
    assert_eq!(cursor_row.platform_source, "cursor");
}
