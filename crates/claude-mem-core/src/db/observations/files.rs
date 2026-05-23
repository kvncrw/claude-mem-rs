//! File-path queries over `observations` (port of
//! `src/services/sqlite/observations/files.ts`).
//!
//! In the TypeScript implementation, `files_read` / `files_modified` were
//! stored as JSON arrays. We query them with `json_each` so that LIKE on an
//! exact path works even when the stored value contains a directory prefix.

use rusqlite::{params, Connection, Result};

/// One observation + aggregated files it touched (read and modified).
#[derive(Debug, Clone)]
pub struct SessionFilesResult {
    pub memory_session_id: String,
    pub files_read: Vec<String>,
    pub files_modified: Vec<String>,
}

pub fn get_files_for_session(
    conn: &Connection,
    memory_session_id: &str,
) -> Result<SessionFilesResult> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT j.value
         FROM observations o,
              json_each(o.files_read) j
         WHERE o.memory_session_id = ?1
           AND j.value IS NOT NULL
         ORDER BY j.value",
    )?;
    let reads: Vec<String> = stmt
        .query_map(params![memory_session_id], |r| r.get(0))?
        .collect::<Result<_>>()?;

    let mut stmt = conn.prepare(
        "SELECT DISTINCT j.value
         FROM observations o,
              json_each(o.files_modified) j
         WHERE o.memory_session_id = ?1
           AND j.value IS NOT NULL
         ORDER BY j.value",
    )?;
    let mods: Vec<String> = stmt
        .query_map(params![memory_session_id], |r| r.get(0))?
        .collect::<Result<_>>()?;

    Ok(SessionFilesResult {
        memory_session_id: memory_session_id.to_string(),
        files_read: reads,
        files_modified: mods,
    })
}
