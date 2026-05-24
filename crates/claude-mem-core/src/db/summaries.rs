//! `session_summaries` read/write surface.
//!
//! Port of `src/services/sqlite/summaries/{store,get,recent}.ts`.

use rusqlite::{params, Connection, Result};

use crate::types::summary::SessionSummaryRow;

#[derive(Debug, Clone, Default, bon::Builder)]
#[builder(on(String, into))]
pub struct SummaryInput {
    pub memory_session_id: String,
    pub project: String,
    pub request: Option<String>,
    pub investigated: Option<String>,
    pub learned: Option<String>,
    pub completed: Option<String>,
    pub next_steps: Option<String>,
    pub files_read: Option<String>,
    pub files_edited: Option<String>,
    pub notes: Option<String>,
    pub prompt_number: Option<i64>,
    pub discovery_tokens: Option<i64>,
    pub created_at: String,
    pub created_at_epoch: i64,
    pub merged_into_project: Option<String>,
}

pub fn store_summary(conn: &Connection, input: &SummaryInput) -> Result<i64> {
    conn.execute(
        "INSERT INTO session_summaries
         (memory_session_id, project, request, investigated, learned, completed,
          next_steps, files_read, files_edited, notes, prompt_number,
          discovery_tokens, created_at, created_at_epoch, merged_into_project)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            input.memory_session_id,
            input.project,
            input.request,
            input.investigated,
            input.learned,
            input.completed,
            input.next_steps,
            input.files_read,
            input.files_edited,
            input.notes,
            input.prompt_number,
            input.discovery_tokens.unwrap_or(0),
            input.created_at,
            input.created_at_epoch,
            input.merged_into_project,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionSummaryRow> {
    Ok(SessionSummaryRow {
        id: row.get(0)?,
        memory_session_id: row.get(1)?,
        project: row.get(2)?,
        request: row.get(3)?,
        investigated: row.get(4)?,
        learned: row.get(5)?,
        completed: row.get(6)?,
        next_steps: row.get(7)?,
        files_read: row.get(8)?,
        files_edited: row.get(9)?,
        notes: row.get(10)?,
        prompt_number: row.get(11)?,
        discovery_tokens: row.get(12)?,
        created_at: row.get(13)?,
        created_at_epoch: row.get(14)?,
        merged_into_project: row.get(15)?,
    })
}

const SELECT_COLS: &str = "
    id, memory_session_id, project, request, investigated, learned, completed,
    next_steps, files_read, files_edited, notes, prompt_number,
    COALESCE(discovery_tokens,0) as discovery_tokens, created_at,
    created_at_epoch, merged_into_project";

pub fn get_summary_by_id(conn: &Connection, id: i64) -> Result<Option<SessionSummaryRow>> {
    conn.query_row(
        &format!(
            "SELECT {cols} FROM session_summaries WHERE id = ?",
            cols = SELECT_COLS
        ),
        params![id],
        row_from,
    )
    .optional()
}

pub fn get_summary_for_session(
    conn: &Connection,
    memory_session_id: &str,
) -> Result<Vec<SessionSummaryRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {cols} FROM session_summaries
         WHERE memory_session_id = ?
         ORDER BY created_at_epoch DESC, id DESC",
        cols = SELECT_COLS
    ))?;
    let rows: Result<Vec<_>> = stmt
        .query_map(params![memory_session_id], row_from)?
        .collect();
    rows
}

pub fn get_summaries_by_ids(conn: &Connection, ids: &[i64]) -> Result<Vec<SessionSummaryRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT {cols} FROM session_summaries WHERE id IN ({})",
        placeholders,
        cols = SELECT_COLS
    ))?;
    let params: Vec<&dyn rusqlite::types::ToSql> = ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), row_from)?;
    let mut out: Vec<SessionSummaryRow> = rows.collect::<Result<_>>()?;
    out.sort_by_key(|r| ids.iter().position(|id| *id == r.id).unwrap_or(usize::MAX));
    Ok(out)
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
