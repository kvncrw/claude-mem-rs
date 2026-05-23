//! `user_prompts` read/write surface.
//!
//! Port of `src/services/sqlite/prompts/{store,get}.ts`.

use rusqlite::{params, Connection, Result};

use crate::types::prompt::UserPromptRow;

#[derive(Debug, Clone, bon::Builder)]
#[builder(on(String, into))]
pub struct PromptInput {
    pub content_session_id: String,
    pub prompt_number: i64,
    pub prompt_text: String,
    pub created_at: String,
    pub created_at_epoch: i64,
}

pub fn save_user_prompt(conn: &Connection, input: &PromptInput) -> Result<i64> {
    conn.execute(
        "INSERT INTO user_prompts
         (content_session_id, prompt_number, prompt_text, created_at, created_at_epoch)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            input.content_session_id,
            input.prompt_number,
            input.prompt_text,
            input.created_at,
            input.created_at_epoch,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<UserPromptRow> {
    Ok(UserPromptRow {
        id: row.get(0)?,
        content_session_id: row.get(1)?,
        prompt_number: row.get(2)?,
        prompt_text: row.get(3)?,
        created_at: row.get(4)?,
        created_at_epoch: row.get(5)?,
    })
}

const SELECT_COLS: &str = "
    id, content_session_id, prompt_number, prompt_text, created_at, created_at_epoch";

pub fn get_latest_user_prompt(
    conn: &Connection,
    content_session_id: &str,
) -> Result<Option<UserPromptRow>> {
    conn.query_row(
        &format!(
            "SELECT {cols} FROM user_prompts
             WHERE content_session_id = ?1
             ORDER BY prompt_number DESC LIMIT 1",
            cols = SELECT_COLS
        ),
        params![content_session_id],
        row_from,
    )
    .optional()
}

/// Return the count of user prompts stored for this content session id
/// (port of `getPromptNumberFromUserPrompts`). The TS API treats this as the
/// *next* prompt number (count), not the highest `prompt_number` — the two
/// differ when prompt numbers are non-contiguous.
pub fn get_prompt_number_from_user_prompts(
    conn: &Connection,
    content_session_id: &str,
) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM user_prompts WHERE content_session_id = ?1",
        params![content_session_id],
        |r| r.get(0),
    )
}

pub fn get_user_prompts_for_session(
    conn: &Connection,
    content_session_id: &str,
) -> Result<Vec<UserPromptRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {cols} FROM user_prompts
         WHERE content_session_id = ?
         ORDER BY prompt_number ASC, id ASC",
        cols = SELECT_COLS
    ))?;
    let rows: Result<Vec<_>> = stmt
        .query_map(params![content_session_id], row_from)?
        .collect();
    rows
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
