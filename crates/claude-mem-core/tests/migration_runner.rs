//! MigrationRunner tests beyond the basic in-memory suite (port of
//! `tests/services/sqlite/migration-runner.test.ts`, delta over the
//! existing 3 unit tests in `db/migrations.rs`).
//!
//! Validates:
//! - Table schema shape (column names + types) after full migration
//! - Foreign-key ON UPDATE CASCADE semantics (Migration 21)
//! - FTS5 trigger wiring end-to-end (insert base row → SELECT from _fts)
//! - Index presence per table

use claude_mem_core::db::open_in_memory;

fn column_info(conn: &rusqlite::Connection, table: &str) -> Vec<(String, String)> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({})", table))
        .unwrap();
    stmt.query_map([], |row| {
        Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

fn index_names(conn: &rusqlite::Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA index_list({})", table))
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect()
}

fn foreign_keys(conn: &rusqlite::Connection, table: &str) -> Vec<(String, String, String)> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA foreign_key_list({})", table))
        .unwrap();
    stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(2)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
        ))
    })
    .unwrap()
    .filter_map(|r| r.ok())
    .collect()
}

#[test]
fn every_table_is_created_after_apply() {
    let conn = open_in_memory().unwrap();
    for expected in [
        "schema_versions",
        "sdk_sessions",
        "observations",
        "observations_fts",
        "session_summaries",
        "session_summaries_fts",
        "user_prompts",
        "user_prompts_fts",
        "pending_messages",
        "observation_feedback",
    ] {
        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                [expected],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "table {expected} should exist after apply_all");
    }
}

#[test]
fn observations_schema_matches_ts_v9_plus_hierarchical_plus_hash() {
    let conn = open_in_memory().unwrap();
    let cols = column_info(&conn, "observations");
    let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();

    // Required columns after Migration 25.
    for required in [
        "id",
        "memory_session_id", // renamed from sdk_session_id (Migration 17)
        "project",
        "text",
        "type",
        "title",
        "subtitle",
        "facts",
        "narrative",
        "concepts",
        "files_read",
        "files_modified",
        "prompt_number",
        "discovery_tokens",
        "created_at",
        "created_at_epoch",
        "generated_by_model",
        "relevance_count",
        "merged_into_project",
        "agent_type",
        "agent_id",
        "content_hash", // Migration 22
    ] {
        assert!(
            names.contains(&required),
            "observations missing column {required}; have {names:?}"
        );
    }

    // `text` must be nullable after Migration 9.
    let text_notnull: i32 = conn
        .query_row(
            "SELECT [notnull] FROM pragma_table_info('observations') WHERE name='text'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        text_notnull, 0,
        "observations.text should be nullable after Migration 9"
    );
}

#[test]
fn sdk_sessions_schema_includes_dual_id_and_platform_source() {
    let conn = open_in_memory().unwrap();
    let cols = column_info(&conn, "sdk_sessions");
    let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();

    assert!(names.contains(&"content_session_id"));
    assert!(names.contains(&"memory_session_id"));
    assert!(names.contains(&"custom_title"));
    assert!(names.contains(&"platform_source"));

    // platform_source default is 'claude'.
    let default_val: Option<String> = conn
        .query_row(
            "SELECT dflt_value FROM pragma_table_info('sdk_sessions') WHERE name='platform_source'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let dflt = default_val.unwrap_or_default();
    assert!(
        dflt.contains("claude"),
        "platform_source default should mention 'claude', got {dflt}"
    );
}

#[test]
fn observations_fk_cascades_on_update() {
    // Migration 21 recreated observations with ON UPDATE CASCADE so that
    // renaming memory_session_id propagates. Verified via pragma
    // foreign_key_list on the live schema.
    let conn = open_in_memory().unwrap();
    let fks = foreign_keys(&conn, "observations");
    // Expect (table= sdk_sessions, on_update=CASCADE, on_delete=CASCADE).
    let has_update_cascade = fks.iter().any(|(t, on_u, on_d)| {
        t == "sdk_sessions" && on_u == "CASCADE" && on_d == "CASCADE"
    });
    assert!(
        has_update_cascade,
        "observations.FK→sdk_sessions missing CASCADE on both update+delete; got {fks:?}"
    );
}

#[test]
fn session_summaries_fk_cascades_on_update() {
    let conn = open_in_memory().unwrap();
    let fks = foreign_keys(&conn, "session_summaries");
    let ok = fks.iter().any(|(t, on_u, on_d)| {
        t == "sdk_sessions" && on_u == "CASCADE" && on_d == "CASCADE"
    });
    assert!(ok, "session_summaries.FK cascade missing; got {fks:?}");
}

#[test]
fn fts5_triggers_fire_on_observation_insert() {
    let conn = open_in_memory().unwrap();
    // Create a session row (FK), populate memory_session_id, then insert an
    // observation. The trigger should copy its searchable columns into
    // observations_fts — verified via BM25 MATCH.
    conn.execute(
        "INSERT INTO sdk_sessions
            (content_session_id, project, started_at, started_at_epoch, status)
         VALUES ('c1', 'p1', '2026-05-23T00:00:00Z', 1748012400, 'active')",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE sdk_sessions SET memory_session_id = 'm1' WHERE content_session_id = 'c1'",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO observations
            (memory_session_id, project, type, title, narrative,
             created_at, created_at_epoch)
         VALUES ('m1', 'p1', 'discovery', 'quantum flux capacitor',
                 'The flux capacitor achieves quantum coherence via entanglement.',
                 '2026-05-23T15:00:00Z', 1748012400)",
        [],
    )
    .unwrap();

    let hit: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations_fts WHERE observations_fts MATCH 'quantum'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hit, 1, "FTS5 trigger should mirror base-table insert");
}

#[test]
fn fts5_triggers_fire_on_delete() {
    let conn = open_in_memory().unwrap();
    conn.execute(
        "INSERT INTO sdk_sessions
            (content_session_id, memory_session_id, project, started_at,
             started_at_epoch, status)
         VALUES ('c2', 'm2', 'p2', '2026-05-23T00:00:00Z', 1748012400, 'active')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO observations
            (memory_session_id, project, type, title, narrative,
             created_at, created_at_epoch)
         VALUES ('m2', 'p2', 'discovery', 'disappearing token',
                 'Token exists then does not.',
                 '2026-05-23T15:00:00Z', 1748012400)",
        [],
    )
    .unwrap();

    // Delete the base row.
    conn.execute(
        "DELETE FROM observations WHERE memory_session_id = 'm2' AND title = 'disappearing token'",
        [],
    )
    .unwrap();

    let hit: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations_fts WHERE observations_fts MATCH 'disappearing'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hit, 0, "FTS5 AD trigger should remove the deleted row from FTS");
}

#[test]
fn fts5_triggers_fire_on_user_prompts() {
    let conn = open_in_memory().unwrap();
    conn.execute(
        "INSERT INTO sdk_sessions
            (content_session_id, project, started_at, started_at_epoch, status)
         VALUES ('c3', 'p3', '2026-05-23T00:00:00Z', 1748012400, 'active')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO user_prompts
            (content_session_id, prompt_number, prompt_text, created_at,
             created_at_epoch)
         VALUES ('c3', 1, 'please refactor the widget factory',
                 '2026-05-23T15:00:00Z', 1748012400)",
        [],
    )
    .unwrap();
    let hit: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM user_prompts_fts WHERE user_prompts_fts MATCH 'widget'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hit, 1, "user_prompts_fts AI trigger should fire on insert");
}

#[test]
fn fts5_triggers_fire_on_session_summaries() {
    let conn = open_in_memory().unwrap();
    conn.execute(
        "INSERT INTO sdk_sessions
            (content_session_id, memory_session_id, project, started_at,
             started_at_epoch, status)
         VALUES ('c4', 'm4', 'p4', '2026-05-23T00:00:00Z', 1748012400, 'active')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session_summaries
            (memory_session_id, project, completed, created_at, created_at_epoch)
         VALUES ('m4', 'p4', 'refactored the quantum router',
                 '2026-05-23T15:00:00Z', 1748012400)",
        [],
    )
    .unwrap();
    let hit: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_summaries_fts
             WHERE session_summaries_fts MATCH 'quantum'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hit, 1, "session_summaries_fts AI trigger should fire on insert");
}

#[test]
fn all_expected_indexes_exist_after_apply() {
    let conn = open_in_memory().unwrap();

    // Observations should have the 8 indexes from Migration 4 (+ content_hash
    // from Migration 22).
    let obs_idx = index_names(&conn, "observations");
    for required in [
        "idx_observations_sdk_session",
        "idx_observations_project",
        "idx_observations_type",
        "idx_observations_created",
        "idx_observations_merged_into",
        "idx_observations_agent_type",
        "idx_observations_agent_id",
        "idx_observations_content_hash",
    ] {
        assert!(
            obs_idx.iter().any(|n| n == required),
            "observations missing index {required}; have {obs_idx:?}"
        );
    }

    let sdk_idx = index_names(&conn, "sdk_sessions");
    for required in [
        "idx_sdk_sessions_claude_id",
        "idx_sdk_sessions_sdk_id",
        "idx_sdk_sessions_project",
        "idx_sdk_sessions_status",
        "idx_sdk_sessions_started",
        "idx_sdk_sessions_platform_source",
    ] {
        assert!(
            sdk_idx.iter().any(|n| n == required),
            "sdk_sessions missing index {required}; have {sdk_idx:?}"
        );
    }

    let prompts_idx = index_names(&conn, "user_prompts");
    assert!(prompts_idx.iter().any(|n| n == "idx_user_prompts_claude_session"));
    assert!(prompts_idx.iter().any(|n| n == "idx_user_prompts_created"));
    assert!(prompts_idx.iter().any(|n| n == "idx_user_prompts_prompt_number"));
    assert!(prompts_idx.iter().any(|n| n == "idx_user_prompts_lookup"));

    let pending_idx = index_names(&conn, "pending_messages");
    assert!(pending_idx.iter().any(|n| n == "idx_pending_messages_session"));
    assert!(pending_idx.iter().any(|n| n == "idx_pending_messages_status"));
    assert!(pending_idx.iter().any(|n| n == "idx_pending_messages_claude_session"));
}
