//! `sdk_sessions` table read/write surface.
//!
//! Port of `src/services/sqlite/sessions/{create,get}.ts`.
//!
//! Dual ID semantics (Migration 17): `content_session_id` is the user-visible
//! chat id (immutable); `memory_session_id` is NULL at create and populated
//! async before the first observation insert.

use rusqlite::{params, Connection, Result};

use crate::types::session::{CreateSessionInput, SdkSessionRow};

/// Idempotent session create (port of `createSDKSession`).
/// `INSERT OR IGNORE` so repeat calls don't churn the rowid.
pub fn create_session(conn: &Connection, input: &CreateSessionInput) -> Result<CreateSessionOutcome> {
    conn.execute(
        "INSERT OR IGNORE INTO sdk_sessions
            (content_session_id, project, user_prompt, started_at, started_at_epoch, status)
         VALUES (?1, ?2, ?3, ?4, ?5, 'active')",
        params![
            input.content_session_id,
            input.project,
            input.user_prompt,
            input.started_at,
            input.started_at_epoch,
        ],
    )?;

    if conn.changes() == 0 {
        return Ok(CreateSessionOutcome::AlreadyExisted);
    }
    Ok(CreateSessionOutcome::Created)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateSessionOutcome {
    Created,
    AlreadyExisted,
}

const SELECT_COLS: &str = "
    id, content_session_id, memory_session_id, project, user_prompt,
    started_at, started_at_epoch, completed_at, completed_at_epoch,
    status, worker_port, COALESCE(prompt_counter,0),
    custom_title, platform_source";

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<SdkSessionRow> {
    Ok(SdkSessionRow {
        id: row.get(0)?,
        content_session_id: row.get(1)?,
        memory_session_id: row.get(2)?,
        project: row.get(3)?,
        user_prompt: row.get(4)?,
        started_at: row.get(5)?,
        started_at_epoch: row.get(6)?,
        completed_at: row.get(7)?,
        completed_at_epoch: row.get(8)?,
        status: row.get(9)?,
        worker_port: row.get(10)?,
        prompt_counter: row.get(11)?,
        custom_title: row.get(12)?,
        // Fall back to 'claude' for older rows written before migration 25.
        platform_source: row.get::<_, Option<String>>(13)?.unwrap_or_else(|| "claude".into()),
    })
}

pub fn get_session_by_content_id(
    conn: &Connection,
    content_session_id: &str,
) -> Result<Option<SdkSessionRow>> {
    conn.query_row(
        &format!("SELECT {cols} FROM sdk_sessions WHERE content_session_id = ?", cols = SELECT_COLS),
        params![content_session_id],
        row_from,
    )
    .optional()
}

pub fn get_session_by_memory_id(
    conn: &Connection,
    memory_session_id: &str,
) -> Result<Option<SdkSessionRow>> {
    conn.query_row(
        &format!("SELECT {cols} FROM sdk_sessions WHERE memory_session_id = ?", cols = SELECT_COLS),
        params![memory_session_id],
        row_from,
    )
    .optional()
}

/// Populate the `memory_session_id` for a session that already exists
/// keyed on `content_session_id` (port of `updateMemorySessionId`).
pub fn update_memory_session_id(
    conn: &Connection,
    content_session_id: &str,
    memory_session_id: &str,
) -> Result<bool> {
    conn.execute(
        "UPDATE sdk_sessions SET memory_session_id = ?1 WHERE content_session_id = ?2",
        params![memory_session_id, content_session_id],
    )?;
    Ok(conn.changes() > 0)
}

/// Mark a session as completed (port of
/// `SessionStore.markSessionCompleted` — fixes #1532).
///
/// Sets `status = 'completed'` and `completed_at`/`completed_at_epoch` to
/// the current wall-clock (ISO-8601 / epoch ms). No-op on non-existent
/// rows.
pub fn mark_session_completed(conn: &Connection, id: i64) -> Result<()> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let secs = now_ms / 1000;
    let completed_at = time::OffsetDateTime::from_unix_timestamp(secs)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();

    conn.execute(
        "UPDATE sdk_sessions
            SET status = 'completed',
                completed_at = ?1,
                completed_at_epoch = ?2
          WHERE id = ?3",
        params![completed_at, now_ms, id],
    )?;
    Ok(())
}

trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>>;
}
impl<T> OptionalExt<T> for rusqlite::Result<T> {
    fn optional(self) -> Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
