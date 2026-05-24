//! `observations` read surface (port of
//! `src/services/sqlite/observations/get.ts`).

use rusqlite::{params, Connection, Result};

use crate::types::observation::ObservationRow;

fn parse_json_array(text: &str) -> Option<Vec<String>> {
    if text.is_empty() {
        None
    } else {
        serde_json::from_str(text).ok()
    }
}

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<ObservationRow> {
    let facts_raw: String = row.get(8)?;
    let concepts_raw: String = row.get(9)?;
    let files_read_raw: String = row.get(10)?;
    let files_modified_raw: String = row.get(11)?;
    Ok(ObservationRow {
        id: row.get(0)?,
        memory_session_id: row.get(1)?,
        project: row.get(2)?,
        text: row.get(3)?,
        r#type: row.get(4)?,
        title: row.get(5)?,
        subtitle: row.get(6)?,
        narrative: row.get(7)?,
        facts: parse_json_array(&facts_raw),
        concepts: parse_json_array(&concepts_raw),
        files_read: parse_json_array(&files_read_raw),
        files_modified: parse_json_array(&files_modified_raw),
        prompt_number: row.get(12)?,
        discovery_tokens: row.get(13)?,
        created_at: row.get(14)?,
        created_at_epoch: row.get(15)?,
        generated_by_model: row.get(16)?,
        relevance_count: row.get(17)?,
        merged_into_project: row.get(18)?,
        agent_type: row.get(19)?,
        agent_id: row.get(20)?,
        content_hash: row.get(21)?,
    })
}

const SELECT_COLS: &str = "
    id, memory_session_id, project, text, type, title, subtitle,
    narrative, COALESCE(facts,''), COALESCE(concepts,''),
    COALESCE(files_read,''), COALESCE(files_modified,''),
    prompt_number, discovery_tokens, created_at, created_at_epoch,
    generated_by_model, relevance_count, merged_into_project,
    agent_type, agent_id, content_hash";

pub fn get_observation_by_id(conn: &Connection, id: i64) -> Result<Option<ObservationRow>> {
    let row = conn
        .query_row(
            &format!("SELECT {} FROM observations WHERE id = ?", SELECT_COLS),
            params![id],
            row_from,
        )
        .optional()?;
    Ok(row)
}

/// Load a batch of observations by ids, preserving the input ordering.
pub fn get_observations_by_ids(conn: &Connection, ids: &[i64]) -> Result<Vec<ObservationRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT {} FROM observations WHERE id IN ({})",
        SELECT_COLS, placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect();
    let rows = stmt.query_map(params.as_slice(), row_from)?;
    let mut out: Vec<ObservationRow> = Vec::with_capacity(ids.len());
    for r in rows {
        out.push(r?);
    }
    out.sort_by_key(|r| ids.iter().position(|id| *id == r.id).unwrap_or(usize::MAX));
    Ok(out)
}

pub fn get_observations_for_session(
    conn: &Connection,
    memory_session_id: &str,
) -> Result<Vec<ObservationRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM observations WHERE memory_session_id = ?
         ORDER BY created_at_epoch DESC, id DESC",
        SELECT_COLS
    ))?;
    let rows: Result<Vec<_>> = stmt
        .query_map(params![memory_session_id], row_from)?
        .collect();
    rows
}

/// Get observations that mention a specific file path in either
/// `files_read` or `files_modified`.
pub fn get_observations_by_file_path(
    conn: &Connection,
    file_path: &str,
    limit: Option<i64>,
) -> Result<Vec<ObservationRow>> {
    let pattern = format!("%{}%", file_path);
    let limit_clause = limit.map(|n| format!(" LIMIT {}", n)).unwrap_or_default();
    let mut stmt = conn.prepare(&format!(
        "SELECT {cols} FROM observations
         WHERE files_read LIKE ?1 OR files_modified LIKE ?1
         ORDER BY created_at_epoch DESC{limit}",
        cols = SELECT_COLS,
        limit = limit_clause
    ))?;
    let rows: Result<Vec<_>> = stmt.query_map(params![pattern], row_from)?.collect();
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

/// Helper used by neighbouring modules (`recent`, `get_observations_by_ids`).
/// Start offset is the column index of `id` within the row.
pub(crate) fn row_from_helper(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<ObservationRow> {
    let facts_raw: String = row.get(offset + 8)?;
    let concepts_raw: String = row.get(offset + 9)?;
    let files_read_raw: String = row.get(offset + 10)?;
    let files_modified_raw: String = row.get(offset + 11)?;
    Ok(ObservationRow {
        id: row.get(offset)?,
        memory_session_id: row.get(offset + 1)?,
        project: row.get(offset + 2)?,
        text: row.get(offset + 3)?,
        r#type: row.get(offset + 4)?,
        title: row.get(offset + 5)?,
        subtitle: row.get(offset + 6)?,
        narrative: row.get(offset + 7)?,
        facts: parse_json_array(&facts_raw),
        concepts: parse_json_array(&concepts_raw),
        files_read: parse_json_array(&files_read_raw),
        files_modified: parse_json_array(&files_modified_raw),
        prompt_number: row.get(offset + 12)?,
        discovery_tokens: row.get(offset + 13)?,
        created_at: row.get(offset + 14)?,
        created_at_epoch: row.get(offset + 15)?,
        generated_by_model: row.get(offset + 16)?,
        relevance_count: row.get(offset + 17)?,
        merged_into_project: row.get(offset + 18)?,
        agent_type: row.get(offset + 19)?,
        agent_id: row.get(offset + 20)?,
        content_hash: row.get(offset + 21)?,
    })
}
