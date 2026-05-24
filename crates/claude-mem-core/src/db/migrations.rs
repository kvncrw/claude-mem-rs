//! Schema migrations (port of
//! <https://github.com/thedotmack/claude-mem/blob/main/src/services/sqlite/migrations/runner.ts>).
//!
//! Migrations are applied in a single transaction each, idempotent via
//! `INSERT OR IGNORE` into `schema_versions`.

use rusqlite::{params, Connection, Result};

pub(crate) fn is_version_applied(conn: &Connection, version: i32) -> Result<bool> {
    let has_table: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_versions'",
        [],
        |r| r.get::<_, i32>(0),
    )? > 0;
    if !has_table {
        return Ok(false);
    }
    let count: i32 = conn.query_row(
        "SELECT COUNT(*) FROM schema_versions WHERE version = ?1",
        params![version],
        |r| r.get(0),
    )?;
    Ok(count > 0)
}

/// Apply all known migrations (v4..v26) to `conn`. Idempotent: each
/// `apply_vN` dispatcher checks `is_version_applied` first, so a second
/// `apply_all` call on a fully-migrated schema is a no-op.
pub fn apply_all(conn: &Connection) -> Result<()> {
    if !is_version_applied(conn, 4)? {
        apply_v4(conn)?;
    }
    if !is_version_applied(conn, 5)? {
        apply_v5(conn)?;
    }
    if !is_version_applied(conn, 6)? {
        apply_v6(conn)?;
    }
    if !is_version_applied(conn, 7)? {
        apply_v7(conn)?;
    }
    if !is_version_applied(conn, 8)? {
        apply_v8(conn)?;
    }
    if !is_version_applied(conn, 9)? {
        apply_v9(conn)?;
    }
    if !is_version_applied(conn, 10)? {
        apply_v10(conn)?;
    }
    if !is_version_applied(conn, 11)? {
        apply_v11(conn)?;
    }
    if !is_version_applied(conn, 16)? {
        apply_v16(conn)?;
    }
    if !is_version_applied(conn, 17)? {
        apply_v17(conn)?;
    }
    if !is_version_applied(conn, 19)? {
        apply_v19(conn)?;
    }
    if !is_version_applied(conn, 20)? {
        apply_v20(conn)?;
    }
    if !is_version_applied(conn, 21)? {
        apply_v21(conn)?;
    }
    if !is_version_applied(conn, 22)? {
        apply_v22(conn)?;
    }
    if !is_version_applied(conn, 23)? {
        apply_v23(conn)?;
    }
    if !is_version_applied(conn, 24)? {
        apply_v24(conn)?;
    }
    if !is_version_applied(conn, 25)? {
        apply_v25(conn)?;
    }
    if !is_version_applied(conn, 26)? {
        apply_v26(conn)?;
    }
    if !is_version_applied(conn, 26)? {
        apply_v26(conn)?;
    }
    Ok(())
}

/// v4: `schema_versions`, `sdk_sessions`, `observations`, `session_summaries` + indexes.
fn apply_v4(conn: &Connection) -> Result<()> {
    const V: i32 = 4;
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_versions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            version INTEGER UNIQUE NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS sdk_sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            claude_session_id TEXT UNIQUE NOT NULL,
            sdk_session_id TEXT UNIQUE,
            project TEXT NOT NULL,
            user_prompt TEXT,
            started_at TEXT NOT NULL,
            started_at_epoch INTEGER NOT NULL,
            completed_at TEXT,
            completed_at_epoch INTEGER,
            status TEXT NOT NULL DEFAULT 'active'
                CHECK (status IN ('active','completed','failed'))
        );
        CREATE TABLE IF NOT EXISTS observations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            sdk_session_id TEXT NOT NULL,
            project TEXT NOT NULL,
            text TEXT NOT NULL,
            type TEXT NOT NULL,
            created_at TEXT NOT NULL,
            created_at_epoch INTEGER NOT NULL,
            generated_by_model TEXT,
            relevance_count INTEGER NOT NULL DEFAULT 0,
            merged_into_project TEXT,
            agent_type TEXT,
            agent_id TEXT,
            FOREIGN KEY (sdk_session_id) REFERENCES sdk_sessions(sdk_session_id)
                ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS session_summaries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            sdk_session_id TEXT NOT NULL UNIQUE,
            project TEXT NOT NULL,
            request TEXT,
            investigated TEXT,
            learned TEXT,
            completed TEXT,
            next_steps TEXT,
            files_read TEXT,
            files_edited TEXT,
            notes TEXT,
            created_at TEXT NOT NULL,
            created_at_epoch INTEGER NOT NULL,
            merged_into_project TEXT,
            FOREIGN KEY (sdk_session_id) REFERENCES sdk_sessions(sdk_session_id)
                ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_observations_sdk_session
            ON observations(sdk_session_id);
        CREATE INDEX IF NOT EXISTS idx_observations_project
            ON observations(project);
        CREATE INDEX IF NOT EXISTS idx_observations_type
            ON observations(type);
        CREATE INDEX IF NOT EXISTS idx_observations_created
            ON observations(created_at_epoch DESC);
        CREATE INDEX IF NOT EXISTS idx_observations_merged_into
            ON observations(merged_into_project);
        CREATE INDEX IF NOT EXISTS idx_observations_agent_type
            ON observations(agent_type);
        CREATE INDEX IF NOT EXISTS idx_observations_agent_id
            ON observations(agent_id);
        CREATE INDEX IF NOT EXISTS idx_session_summaries_sdk_session
            ON session_summaries(sdk_session_id);
        CREATE INDEX IF NOT EXISTS idx_session_summaries_project
            ON session_summaries(project);
        CREATE INDEX IF NOT EXISTS idx_session_summaries_created
            ON session_summaries(created_at_epoch DESC);
        CREATE INDEX IF NOT EXISTS idx_session_summaries_merged_into
            ON session_summaries(merged_into_project);
        CREATE INDEX IF NOT EXISTS idx_sdk_sessions_claude_id
            ON sdk_sessions(claude_session_id);
        CREATE INDEX IF NOT EXISTS idx_sdk_sessions_sdk_id
            ON sdk_sessions(sdk_session_id);
        CREATE INDEX IF NOT EXISTS idx_sdk_sessions_project
            ON sdk_sessions(project);
        CREATE INDEX IF NOT EXISTS idx_sdk_sessions_status
            ON sdk_sessions(status);
        CREATE INDEX IF NOT EXISTS idx_sdk_sessions_started
            ON sdk_sessions(started_at_epoch DESC);",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v5: `sdk_sessions.worker_port INTEGER`.
fn apply_v5(conn: &Connection) -> Result<()> {
    const V: i32 = 5;
    let tx = conn.unchecked_transaction()?;
    add_column_if_missing(&tx, "sdk_sessions", "worker_port", "INTEGER")?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v6: prompt tracking columns.
fn apply_v6(conn: &Connection) -> Result<()> {
    const V: i32 = 6;
    let tx = conn.unchecked_transaction()?;
    add_column_if_missing(&tx, "sdk_sessions", "prompt_counter", "INTEGER DEFAULT 0")?;
    add_column_if_missing(&tx, "observations", "prompt_number", "INTEGER")?;
    add_column_if_missing(&tx, "session_summaries", "prompt_number", "INTEGER")?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v7: drop unique constraint on `session_summaries.sdk_session_id` via table rebuild.
fn apply_v7(conn: &Connection) -> Result<()> {
    const V: i32 = 7;
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("PRAGMA foreign_keys = OFF")?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_summaries_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            sdk_session_id TEXT NOT NULL,
            project TEXT NOT NULL,
            request TEXT,
            investigated TEXT,
            learned TEXT,
            completed TEXT,
            next_steps TEXT,
            files_read TEXT,
            files_edited TEXT,
            notes TEXT,
            prompt_number INTEGER,
            created_at TEXT NOT NULL,
            created_at_epoch INTEGER NOT NULL,
            merged_into_project TEXT,
            FOREIGN KEY (sdk_session_id) REFERENCES sdk_sessions(sdk_session_id)
                ON DELETE CASCADE
        );
        INSERT INTO session_summaries_new
            SELECT id, sdk_session_id, project, request, investigated, learned,
                completed, next_steps, files_read, files_edited, notes, prompt_number,
                created_at, created_at_epoch, merged_into_project
            FROM session_summaries;
        DROP TABLE session_summaries;
        ALTER TABLE session_summaries_new RENAME TO session_summaries;",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v8: hierarchical observation fields.
fn apply_v8(conn: &Connection) -> Result<()> {
    const V: i32 = 8;
    let tx = conn.unchecked_transaction()?;
    for col in [
        ("title", "TEXT"),
        ("subtitle", "TEXT"),
        ("facts", "TEXT"),
        ("narrative", "TEXT"),
        ("concepts", "TEXT"),
        ("files_read", "TEXT"),
        ("files_modified", "TEXT"),
    ] {
        add_column_if_missing(&tx, "observations", col.0, col.1)?;
    }
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v9: make `observations.text` nullable (deprecate free-text).
fn apply_v9(conn: &Connection) -> Result<()> {
    const V: i32 = 9;
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("PRAGMA foreign_keys = OFF")?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS observations_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            sdk_session_id TEXT NOT NULL,
            project TEXT NOT NULL,
            text TEXT,
            type TEXT NOT NULL,
            title TEXT,
            subtitle TEXT,
            facts TEXT,
            narrative TEXT,
            concepts TEXT,
            files_read TEXT,
            files_modified TEXT,
            prompt_number INTEGER,
            created_at TEXT NOT NULL,
            created_at_epoch INTEGER NOT NULL,
            generated_by_model TEXT,
            relevance_count INTEGER NOT NULL DEFAULT 0,
            merged_into_project TEXT,
            agent_type TEXT,
            agent_id TEXT,
            FOREIGN KEY (sdk_session_id) REFERENCES sdk_sessions(sdk_session_id)
                ON DELETE CASCADE
        );
        INSERT INTO observations_new
            SELECT id, sdk_session_id, project, text, type, title, subtitle, facts, narrative,
                concepts, files_read, files_modified, prompt_number,
                created_at, created_at_epoch, generated_by_model,
                relevance_count, merged_into_project, agent_type, agent_id
            FROM observations;
        DROP TABLE observations;
        ALTER TABLE observations_new RENAME TO observations;",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v10: `user_prompts` + FTS5 mirror + AI/AD/AU triggers.
fn apply_v10(conn: &Connection) -> Result<()> {
    const V: i32 = 10;
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS user_prompts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            claude_session_id TEXT NOT NULL,
            prompt_number INTEGER NOT NULL,
            prompt_text TEXT NOT NULL,
            created_at TEXT NOT NULL,
            created_at_epoch INTEGER NOT NULL,
            FOREIGN KEY (claude_session_id) REFERENCES sdk_sessions(claude_session_id)
                ON DELETE CASCADE
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS user_prompts_fts USING fts5(
            prompt_text,
            content='user_prompts',
            content_rowid='id'
        );
        CREATE INDEX IF NOT EXISTS idx_user_prompts_claude_session
            ON user_prompts(claude_session_id);
        CREATE INDEX IF NOT EXISTS idx_user_prompts_created
            ON user_prompts(created_at_epoch DESC);
        CREATE INDEX IF NOT EXISTS idx_user_prompts_prompt_number
            ON user_prompts(claude_session_id, prompt_number);
        CREATE TRIGGER IF NOT EXISTS user_prompts_ai AFTER INSERT ON user_prompts BEGIN
            INSERT INTO user_prompts_fts(rowid, prompt_text) VALUES (new.id, new.prompt_text);
        END;
        CREATE TRIGGER IF NOT EXISTS user_prompts_ad AFTER DELETE ON user_prompts BEGIN
            INSERT INTO user_prompts_fts(user_prompts_fts, rowid, prompt_text)
                VALUES('delete', old.id, old.prompt_text);
        END;
        CREATE TRIGGER IF NOT EXISTS user_prompts_au AFTER UPDATE ON user_prompts BEGIN
            INSERT INTO user_prompts_fts(user_prompts_fts, rowid, prompt_text)
                VALUES('delete', old.id, old.prompt_text);
            INSERT INTO user_prompts_fts(rowid, prompt_text) VALUES (new.id, new.prompt_text);
        END;
        CREATE INDEX IF NOT EXISTS idx_user_prompts_lookup
            ON user_prompts(claude_session_id, prompt_number DESC);",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v11: discovery tokens.
fn apply_v11(conn: &Connection) -> Result<()> {
    const V: i32 = 11;
    let tx = conn.unchecked_transaction()?;
    add_column_if_missing(&tx, "observations", "discovery_tokens", "INTEGER DEFAULT 0")?;
    add_column_if_missing(
        &tx,
        "session_summaries",
        "discovery_tokens",
        "INTEGER DEFAULT 0",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v16: persistent pending messages queue.
fn apply_v16(conn: &Connection) -> Result<()> {
    const V: i32 = 16;
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS pending_messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_db_id INTEGER NOT NULL,
            claude_session_id TEXT NOT NULL,
            message_type TEXT NOT NULL CHECK (message_type IN ('observation','summarize')),
            tool_name TEXT,
            tool_input TEXT,
            tool_response TEXT,
            cwd TEXT,
            last_user_message TEXT,
            last_assistant_message TEXT,
            prompt_number INTEGER,
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending','processing','processed','failed')),
            retry_count INTEGER NOT NULL DEFAULT 0,
            created_at_epoch INTEGER NOT NULL,
            started_processing_at_epoch INTEGER,
            completed_at_epoch INTEGER,
            agent_type TEXT,
            agent_id TEXT,
            FOREIGN KEY (session_db_id) REFERENCES sdk_sessions(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_pending_messages_session
            ON pending_messages(session_db_id);
        CREATE INDEX IF NOT EXISTS idx_pending_messages_status
            ON pending_messages(status);
        CREATE INDEX IF NOT EXISTS idx_pending_messages_claude_session
            ON pending_messages(claude_session_id);",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v17: dual session IDs — rename `claude_session_id→content_session_id`,
/// `sdk_session_id→memory_session_id` across 5 tables.
fn apply_v17(conn: &Connection) -> Result<()> {
    const V: i32 = 17;
    let tx = conn.unchecked_transaction()?;
    rename_column_if(
        &tx,
        "sdk_sessions",
        "claude_session_id",
        "content_session_id",
    )?;
    rename_column_if(&tx, "sdk_sessions", "sdk_session_id", "memory_session_id")?;
    rename_column_if(
        &tx,
        "pending_messages",
        "claude_session_id",
        "content_session_id",
    )?;
    rename_column_if(&tx, "observations", "sdk_session_id", "memory_session_id")?;
    rename_column_if(
        &tx,
        "session_summaries",
        "sdk_session_id",
        "memory_session_id",
    )?;
    rename_column_if(
        &tx,
        "user_prompts",
        "claude_session_id",
        "content_session_id",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v19: no-op stub for backward compat.
fn apply_v19(conn: &Connection) -> Result<()> {
    const V: i32 = 19;
    conn.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    Ok(())
}

/// v20: `pending_messages.failed_at_epoch`.
fn apply_v20(conn: &Connection) -> Result<()> {
    const V: i32 = 20;
    let tx = conn.unchecked_transaction()?;
    add_column_if_missing(&tx, "pending_messages", "failed_at_epoch", "INTEGER")?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v21: add `ON UPDATE CASCADE` to FK via table rebuild.
fn apply_v21(conn: &Connection) -> Result<()> {
    const V: i32 = 21;
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("PRAGMA foreign_keys = OFF")?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS observations_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            memory_session_id TEXT NOT NULL,
            project TEXT NOT NULL,
            text TEXT,
            type TEXT NOT NULL,
            title TEXT,
            subtitle TEXT,
            facts TEXT,
            narrative TEXT,
            concepts TEXT,
            files_read TEXT,
            files_modified TEXT,
            prompt_number INTEGER,
            discovery_tokens INTEGER DEFAULT 0,
            created_at TEXT NOT NULL,
            created_at_epoch INTEGER NOT NULL,
            generated_by_model TEXT,
            relevance_count INTEGER NOT NULL DEFAULT 0,
            merged_into_project TEXT,
            agent_type TEXT,
            agent_id TEXT,
            FOREIGN KEY (memory_session_id) REFERENCES sdk_sessions(memory_session_id)
                ON DELETE CASCADE ON UPDATE CASCADE
        );
        INSERT INTO observations_new SELECT * FROM observations;
        DROP TABLE observations;
        ALTER TABLE observations_new RENAME TO observations;
        CREATE TABLE IF NOT EXISTS session_summaries_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            memory_session_id TEXT NOT NULL,
            project TEXT NOT NULL,
            request TEXT,
            investigated TEXT,
            learned TEXT,
            completed TEXT,
            next_steps TEXT,
            files_read TEXT,
            files_edited TEXT,
            notes TEXT,
            prompt_number INTEGER,
            discovery_tokens INTEGER DEFAULT 0,
            created_at TEXT NOT NULL,
            created_at_epoch INTEGER NOT NULL,
            merged_into_project TEXT,
            FOREIGN KEY (memory_session_id) REFERENCES sdk_sessions(memory_session_id)
                ON DELETE CASCADE ON UPDATE CASCADE
        );
        INSERT INTO session_summaries_new SELECT * FROM session_summaries;
        DROP TABLE session_summaries;
        ALTER TABLE session_summaries_new RENAME TO session_summaries;
        -- Recreate indexes that were dropped with the rebuild.
        CREATE INDEX IF NOT EXISTS idx_observations_sdk_session ON observations(memory_session_id);
        CREATE INDEX IF NOT EXISTS idx_observations_project ON observations(project);
        CREATE INDEX IF NOT EXISTS idx_observations_type ON observations(type);
        CREATE INDEX IF NOT EXISTS idx_observations_created ON observations(created_at_epoch DESC);
        CREATE INDEX IF NOT EXISTS idx_observations_merged_into ON observations(merged_into_project);
        CREATE INDEX IF NOT EXISTS idx_observations_agent_type ON observations(agent_type);
        CREATE INDEX IF NOT EXISTS idx_observations_agent_id ON observations(agent_id);
        CREATE INDEX IF NOT EXISTS idx_session_summaries_sdk_session ON session_summaries(memory_session_id);
        CREATE INDEX IF NOT EXISTS idx_session_summaries_project ON session_summaries(project);
        CREATE INDEX IF NOT EXISTS idx_session_summaries_created ON session_summaries(created_at_epoch DESC);
        CREATE INDEX IF NOT EXISTS idx_session_summaries_merged_into ON session_summaries(merged_into_project);",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v22: `observations.content_hash` + backfill + index.
fn apply_v22(conn: &Connection) -> Result<()> {
    const V: i32 = 22;
    let tx = conn.unchecked_transaction()?;
    add_column_if_missing(&tx, "observations", "content_hash", "TEXT")?;
    tx.execute(
        "CREATE INDEX IF NOT EXISTS idx_observations_content_hash
            ON observations(content_hash)",
        [],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v23: `sdk_sessions.custom_title`.
fn apply_v23(conn: &Connection) -> Result<()> {
    const V: i32 = 23;
    let tx = conn.unchecked_transaction()?;
    add_column_if_missing(&tx, "sdk_sessions", "custom_title", "TEXT")?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v24: `observation_feedback` table.
fn apply_v24(conn: &Connection) -> Result<()> {
    const V: i32 = 24;
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS observation_feedback (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            observation_id INTEGER NOT NULL,
            signal_type TEXT NOT NULL,
            session_db_id INTEGER,
            created_at_epoch INTEGER NOT NULL,
            metadata TEXT,
            FOREIGN KEY (observation_id) REFERENCES observations(id) ON DELETE CASCADE
        );",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v25: `sdk_sessions.platform_source`.
fn apply_v25(conn: &Connection) -> Result<()> {
    const V: i32 = 25;
    let tx = conn.unchecked_transaction()?;
    add_column_if_missing(
        &tx,
        "sdk_sessions",
        "platform_source",
        "TEXT NOT NULL DEFAULT 'claude'",
    )?;
    tx.execute(
        "CREATE INDEX IF NOT EXISTS idx_sdk_sessions_platform_source
            ON sdk_sessions(platform_source)",
        [],
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

/// v26: backfill FTS5 tables for `observations` and `session_summaries`
/// (v10 only created `user_prompts_fts`). Mirrors the TS runner's
/// AI/AD/AU trigger set that keeps each FTS shadow in sync with its base
/// table. Safe on already-migrated DBs that ran v9/v17/v21 — the
/// `CREATE VIRTUAL TABLE IF NOT EXISTS` + `CREATE TRIGGER IF NOT EXISTS`
/// guards make this idempotent.
fn apply_v26(conn: &Connection) -> Result<()> {
    const V: i32 = 26;
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS observations_fts USING fts5(
            title, subtitle, narrative, text, facts, concepts,
            content='observations',
            content_rowid='id'
        );
        CREATE TABLE IF NOT EXISTS session_summaries_fts_cfg (id INTEGER PRIMARY KEY);
        CREATE VIRTUAL TABLE IF NOT EXISTS session_summaries_fts USING fts5(
            request, investigated, learned, completed, next_steps, notes,
            content='session_summaries',
            content_rowid='id'
        );
        CREATE TRIGGER IF NOT EXISTS observations_ai AFTER INSERT ON observations BEGIN
            INSERT INTO observations_fts(rowid, title, subtitle, narrative, text, facts, concepts)
                VALUES (new.id, new.title, new.subtitle, new.narrative, new.text,
                        new.facts, new.concepts);
        END;
        CREATE TRIGGER IF NOT EXISTS observations_ad AFTER DELETE ON observations BEGIN
            INSERT INTO observations_fts(observations_fts, rowid, title, subtitle, narrative, text, facts, concepts)
                VALUES ('delete', old.id, old.title, old.subtitle, old.narrative, old.text,
                        old.facts, old.concepts);
        END;
        CREATE TRIGGER IF NOT EXISTS observations_au AFTER UPDATE ON observations BEGIN
            INSERT INTO observations_fts(observations_fts, rowid, title, subtitle, narrative, text, facts, concepts)
                VALUES ('delete', old.id, old.title, old.subtitle, old.narrative, old.text,
                        old.facts, old.concepts);
            INSERT INTO observations_fts(rowid, title, subtitle, narrative, text, facts, concepts)
                VALUES (new.id, new.title, new.subtitle, new.narrative, new.text,
                        new.facts, new.concepts);
        END;
        CREATE TRIGGER IF NOT EXISTS session_summaries_ai AFTER INSERT ON session_summaries BEGIN
            INSERT INTO session_summaries_fts(rowid, request, investigated, learned, completed, next_steps, notes)
                VALUES (new.id, new.request, new.investigated, new.learned, new.completed,
                        new.next_steps, new.notes);
        END;
        CREATE TRIGGER IF NOT EXISTS session_summaries_ad AFTER DELETE ON session_summaries BEGIN
            INSERT INTO session_summaries_fts(session_summaries_fts, rowid, request, investigated, learned, completed, next_steps, notes)
                VALUES ('delete', old.id, old.request, old.investigated, old.learned, old.completed,
                        old.next_steps, old.notes);
        END;
        CREATE TRIGGER IF NOT EXISTS session_summaries_au AFTER UPDATE ON session_summaries BEGIN
            INSERT INTO session_summaries_fts(session_summaries_fts, rowid, request, investigated, learned, completed, next_steps, notes)
                VALUES ('delete', old.id, old.request, old.investigated, old.learned, old.completed,
                        old.next_steps, old.notes);
            INSERT INTO session_summaries_fts(rowid, request, investigated, learned, completed, next_steps, notes)
                VALUES (new.id, new.request, new.investigated, new.learned, new.completed,
                        new.next_steps, new.notes);
        END;",
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO schema_versions (version) VALUES (?)",
        [V],
    )?;
    tx.commit()
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<()> {
    let exists: bool = conn
        .prepare(&format!("PRAGMA table_info({})", table))?
        .query_map([], |row| row.get::<_, String>(1))?
        .any(|n| n.as_deref() == Ok(column));
    if !exists {
        conn.execute(
            &format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, definition),
            [],
        )?;
    }
    Ok(())
}

fn rename_column_if(conn: &Connection, table: &str, old_name: &str, new_name: &str) -> Result<()> {
    let has_old: bool = conn
        .prepare(&format!("PRAGMA table_info({})", table))?
        .query_map([], |row| row.get::<_, String>(1))?
        .any(|n| n.as_deref() == Ok(old_name));
    if has_old {
        conn.execute(
            &format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {}",
                table, old_name, new_name
            ),
            [],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_all_succeeds_on_fresh_db() {
        let conn = Connection::open_in_memory().unwrap();
        apply_all(&conn).unwrap();
        let applied: i32 = conn
            .query_row("SELECT COUNT(*) FROM schema_versions", [], |r| r.get(0))
            .unwrap();
        // 18 migrations: v4,5,6,7,8,9,10,11,16,17,19,20,21,22,23,24,25,26.
        // (v1-v3 never existed; v12-v15, v18 are gaps in the TS runner.)
        assert_eq!(applied, 18);
    }

    #[test]
    fn apply_all_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        apply_all(&conn).unwrap();
        apply_all(&conn).unwrap();
        let applied: i32 = conn
            .query_row("SELECT COUNT(*) FROM schema_versions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(applied, 18);
    }

    #[test]
    fn all_tables_exist_after_apply() {
        let conn = Connection::open_in_memory().unwrap();
        apply_all(&conn).unwrap();
        for table in [
            "schema_versions",
            "sdk_sessions",
            "observations",
            "session_summaries",
            "user_prompts",
            "user_prompts_fts",
            "pending_messages",
            "observation_feedback",
        ] {
            let count: i32 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {table} should exist");
        }
    }
}
