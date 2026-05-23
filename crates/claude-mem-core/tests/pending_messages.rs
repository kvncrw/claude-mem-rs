//! PendingMessageStore self-healing tests (port of
//! `tests/services/sqlite/PendingMessageStore.test.ts`).
//!
//! Exercises: `enqueue`, `claim_next_message` (including the self-healing
//! of stuck `processing` rows > 60s old), and per-session isolation.

use claude_mem_core::db::open_in_memory;
use claude_mem_core::db::pending_messages::{EnqueueInput, PendingMessageStore};
use claude_mem_core::db::sessions;
use claude_mem_core::types::session::CreateSessionInput;

const CONTENT: &str = "test-self-heal";

fn create_session(conn: &rusqlite::Connection, content: &str) -> i64 {
    sessions::create_session(
        conn,
        &CreateSessionInput {
            content_session_id: content.into(),
            project: "test-project".into(),
            user_prompt: Some("Test prompt".into()),
            started_at: "2026-05-23T15:00:00Z".into(),
            started_at_epoch: 1748012400,
        },
    ).unwrap();
    sessions::get_session_by_content_id(conn, content)
        .unwrap()
        .unwrap()
        .id
}

fn enqueue_msg(store: &PendingMessageStore, conn: &rusqlite::Connection, db_id: i64) -> i64 {
    store
        .enqueue(
            conn,
            &EnqueueInput {
                session_db_id: db_id,
                content_session_id: CONTENT.into(),
                message_type: "observation".into(),
                tool_name: Some("TestTool".into()),
                tool_input: Some(serde_json::json!({"test": "input"})),
                tool_response: Some(serde_json::json!({"test": "response"})),
                cwd: None,
                last_user_message: None,
                last_assistant_message: None,
                prompt_number: Some(1),
                created_at_epoch: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as i64,
                agent_type: None,
                agent_id: None,
            },
        ).unwrap()
}

/// Set `status = 'processing', started_processing_at_epoch = epoch_ms` for a row.
fn set_processing_at(conn: &rusqlite::Connection, id: i64, epoch_ms: i64) {
    conn.execute(
        "UPDATE pending_messages SET status = 'processing',
                started_processing_at_epoch = ?2 WHERE id = ?1",
        rusqlite::params![id, epoch_ms],
    )
    .unwrap();
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn stuck_processing_messages_are_recovered_on_next_claim() {
    let conn = open_in_memory().unwrap();
    let store = PendingMessageStore::new(3);
    let db_id = create_session(&conn, CONTENT);

    let msg_id = enqueue_msg(&store, &conn, db_id);
    // Simulate 2-minute-old stuck processing row.
    set_processing_at(&conn, msg_id, now_ms() - 120_000);

    let status: String = conn
        .query_row(
            "SELECT status FROM pending_messages WHERE id = ?",
            [msg_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "processing");

    let claimed = store.claim_next_message(&conn, db_id).unwrap().unwrap();
    assert_eq!(claimed.id, msg_id);

    let status: String = conn
        .query_row(
            "SELECT status FROM pending_messages WHERE id = ?",
            [msg_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(status, "processing");
}

#[test]
fn actively_processing_messages_are_not_recovered() {
    let conn = open_in_memory().unwrap();
    let store = PendingMessageStore::new(3);
    let db_id = create_session(&conn, CONTENT);

    let active_id = enqueue_msg(&store, &conn, db_id);
    let pending_id = enqueue_msg(&store, &conn, db_id);

    // 5-second-old activity — well within 60s threshold.
    set_processing_at(&conn, active_id, now_ms() - 5_000);

    let claimed = store.claim_next_message(&conn, db_id).unwrap().unwrap();
    assert_eq!(claimed.id, pending_id, "should skip the actively-processing one");

    let active_status: String = conn
        .query_row(
            "SELECT status FROM pending_messages WHERE id = ?",
            [active_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(active_status, "processing");
}

#[test]
fn recovery_and_claim_is_atomic_within_single_call() {
    let conn = open_in_memory().unwrap();
    let store = PendingMessageStore::new(3);
    let db_id = create_session(&conn, CONTENT);

    let stuck_id = enqueue_msg(&store, &conn, db_id);
    let pending1 = enqueue_msg(&store, &conn, db_id);
    let pending2 = enqueue_msg(&store, &conn, db_id);

    set_processing_at(&conn, stuck_id, now_ms() - 120_000);

    let claimed = store.claim_next_message(&conn, db_id).unwrap().unwrap();
    // Stuck message was recovered to `pending`, and being oldest, it's claimed.
    assert_eq!(claimed.id, stuck_id);

    let s1: String = conn
        .query_row("SELECT status FROM pending_messages WHERE id = ?", [pending1], |r| r.get(0))
        .unwrap();
    let s2: String = conn
        .query_row("SELECT status FROM pending_messages WHERE id = ?", [pending2], |r| r.get(0))
        .unwrap();
    assert_eq!(s1, "pending");
    assert_eq!(s2, "pending");
}

#[test]
fn no_messages_returns_none_without_error() {
    let conn = open_in_memory().unwrap();
    let store = PendingMessageStore::new(3);
    let db_id = create_session(&conn, CONTENT);

    assert!(store.claim_next_message(&conn, db_id).unwrap().is_none());
}

#[test]
fn self_healing_is_scoped_to_specified_session() {
    let conn = open_in_memory().unwrap();
    let store = PendingMessageStore::new(3);

    let db_id1 = create_session(&conn, "session-1");
    let db_id2 = create_session(&conn, "other-session");

    // Enqueue + make stuck in session 1 (must use session 1's content id in input).
    let s1_content = "session-1";
    let stuck_in_s1 = store.enqueue(
        &conn,
        &EnqueueInput {
            session_db_id: db_id1,
            content_session_id: s1_content.into(),
            message_type: "observation".into(),
            tool_name: Some("TestTool".into()),
            tool_input: Some(serde_json::json!({"test": "input"})),
            tool_response: Some(serde_json::json!({"test": "response"})),
            prompt_number: Some(1),
            created_at_epoch: now_ms(),
            ..Default::default()
        },
    ).unwrap();
    set_processing_at(&conn, stuck_in_s1, now_ms() - 120_000);

    // Enqueue + make stuck in session 2.
    let s2_msg = store.enqueue(
        &conn,
        &EnqueueInput {
            session_db_id: db_id2,
            content_session_id: "other-session".into(),
            message_type: "observation".into(),
            tool_name: Some("TestTool".into()),
            tool_input: Some(serde_json::json!({"test": "input"})),
            tool_response: Some(serde_json::json!({"test": "response"})),
            prompt_number: Some(1),
            created_at_epoch: now_ms(),
            ..Default::default()
        },
    ).unwrap();
    set_processing_at(&conn, s2_msg, now_ms() - 120_000);

    // Claim for session 2 — only heals session 2.
    let claimed = store.claim_next_message(&conn, db_id2).unwrap().unwrap();
    assert_eq!(claimed.id, s2_msg);

    // Session 1's stuck row must remain `processing` (not healed by session 2).
    let s1_status: String = conn
        .query_row(
            "SELECT status FROM pending_messages WHERE id = ?",
            [stuck_in_s1],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(s1_status, "processing");
}
