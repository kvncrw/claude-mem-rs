//! Prompts module tests (port of `tests/sqlite/prompts.test.ts`).
//!
//! Exercises `save_user_prompt`, `get_latest_user_prompt`, and
//! `get_prompt_number_from_user_prompts` against an in-memory schema.

use claude_mem_core::db::open_in_memory;
use claude_mem_core::db::prompts::{
    get_latest_user_prompt, get_prompt_number_from_user_prompts, save_user_prompt, PromptInput,
};
use claude_mem_core::db::sessions;
use claude_mem_core::types::session::CreateSessionInput;

fn create_session(conn: &rusqlite::Connection, content_session_id: &str) -> String {
    let input = CreateSessionInput {
        content_session_id: content_session_id.into(),
        project: "test-project".into(),
        user_prompt: Some("initial prompt".into()),
        started_at: "2026-05-23T15:00:00Z".into(),
        started_at_epoch: 1748012400,
    };
    sessions::create_session(conn, &input).unwrap();
    content_session_id.to_string()
}

fn prompt_input(content_session_id: &str, prompt_number: i64, prompt_text: &str) -> PromptInput {
    PromptInput {
        content_session_id: content_session_id.into(),
        prompt_number,
        prompt_text: prompt_text.into(),
        created_at: "2026-05-23T15:00:00Z".into(),
        created_at_epoch: 1748012400000 + prompt_number,
    }
}

#[test]
fn save_returns_positive_id() {
    let conn = open_in_memory().unwrap();
    let content = create_session(&conn, "content-session-prompt-1");
    let id = save_user_prompt(&conn, &prompt_input(&content, 1, "First user prompt")).unwrap();
    assert!(id > 0);
}

#[test]
fn save_multiple_prompts_with_incrementing_ids() {
    let conn = open_in_memory().unwrap();
    let content = create_session(&conn, "content-session-prompt-2");
    let id1 = save_user_prompt(&conn, &prompt_input(&content, 1, "First prompt")).unwrap();
    let id2 = save_user_prompt(&conn, &prompt_input(&content, 2, "Second prompt")).unwrap();
    let id3 = save_user_prompt(&conn, &prompt_input(&content, 3, "Third prompt")).unwrap();
    assert!(id1 > 0);
    assert!(id2 > id1);
    assert!(id3 > id2);
}

#[test]
fn allow_prompts_from_different_sessions() {
    let conn = open_in_memory().unwrap();
    let sa = create_session(&conn, "session-a");
    let sb = create_session(&conn, "session-b");
    let id1 = save_user_prompt(&conn, &prompt_input(&sa, 1, "Prompt A1")).unwrap();
    let id2 = save_user_prompt(&conn, &prompt_input(&sb, 1, "Prompt B1")).unwrap();
    assert_ne!(id1, id2);
}

#[test]
fn prompt_number_returns_zero_when_no_prompts_exist() {
    let conn = open_in_memory().unwrap();
    assert_eq!(
        get_prompt_number_from_user_prompts(&conn, "nonexistent-session").unwrap(),
        0
    );
}

#[test]
fn prompt_number_returns_count_of_prompts_for_session() {
    let conn = open_in_memory().unwrap();
    let content = create_session(&conn, "count-test-session");

    assert_eq!(
        get_prompt_number_from_user_prompts(&conn, &content).unwrap(),
        0
    );
    save_user_prompt(&conn, &prompt_input(&content, 1, "First prompt")).unwrap();
    assert_eq!(
        get_prompt_number_from_user_prompts(&conn, &content).unwrap(),
        1
    );
    save_user_prompt(&conn, &prompt_input(&content, 2, "Second prompt")).unwrap();
    assert_eq!(
        get_prompt_number_from_user_prompts(&conn, &content).unwrap(),
        2
    );
    save_user_prompt(&conn, &prompt_input(&content, 3, "Third prompt")).unwrap();
    assert_eq!(
        get_prompt_number_from_user_prompts(&conn, &content).unwrap(),
        3
    );
}

#[test]
fn prompt_number_maintains_session_isolation() {
    let conn = open_in_memory().unwrap();
    let sa = create_session(&conn, "isolation-session-a");
    let sb = create_session(&conn, "isolation-session-b");

    save_user_prompt(&conn, &prompt_input(&sa, 1, "A1")).unwrap();
    save_user_prompt(&conn, &prompt_input(&sa, 2, "A2")).unwrap();

    save_user_prompt(&conn, &prompt_input(&sb, 1, "B1")).unwrap();

    assert_eq!(get_prompt_number_from_user_prompts(&conn, &sa).unwrap(), 2);
    assert_eq!(get_prompt_number_from_user_prompts(&conn, &sb).unwrap(), 1);

    save_user_prompt(&conn, &prompt_input(&sb, 2, "B2")).unwrap();
    save_user_prompt(&conn, &prompt_input(&sb, 3, "B3")).unwrap();

    assert_eq!(get_prompt_number_from_user_prompts(&conn, &sa).unwrap(), 2);
    assert_eq!(get_prompt_number_from_user_prompts(&conn, &sb).unwrap(), 3);
}

#[test]
fn prompt_number_handles_100_prompts() {
    let conn = open_in_memory().unwrap();
    let content = create_session(&conn, "many-prompts-session");
    for i in 1..=100 {
        save_user_prompt(&conn, &prompt_input(&content, i, &format!("Prompt {i}"))).unwrap();
    }
    assert_eq!(
        get_prompt_number_from_user_prompts(&conn, &content).unwrap(),
        100
    );
}

#[test]
fn get_latest_returns_most_recent_by_prompt_number() {
    let conn = open_in_memory().unwrap();
    let content = create_session(&conn, "latest-prompt");
    save_user_prompt(&conn, &prompt_input(&content, 1, "First")).unwrap();
    save_user_prompt(&conn, &prompt_input(&content, 2, "Second")).unwrap();
    save_user_prompt(&conn, &prompt_input(&content, 3, "Third")).unwrap();

    let row = get_latest_user_prompt(&conn, &content).unwrap().unwrap();
    assert_eq!(row.prompt_number, 3);
    assert_eq!(row.prompt_text, "Third");
}
