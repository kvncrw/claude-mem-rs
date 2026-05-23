//! Timeline union query — observations + summaries + user_prompts.
//!
//! Port of `src/services/sqlite/timeline/queries.ts`.

use rusqlite::{params, Connection, Result};

use crate::types::timeline::{TimelineKind, TimelineRow};

pub fn get_timeline_for_project(
    conn: &Connection,
    project: &str,
    limit: i64,
) -> Result<Vec<TimelineRow>> {
    let mut stmt = conn.prepare(
        "SELECT kind, id, memory_session_id, content_session_id, project,
                title, text, created_at, created_at_epoch
         FROM (
            SELECT 'observation' AS kind, o.id, o.memory_session_id,
                   NULL AS content_session_id, o.project,
                   o.title AS title, o.narrative AS text,
                   o.created_at, o.created_at_epoch
            FROM observations o WHERE o.project = ?1
            UNION ALL
            SELECT 'summary', s.id, s.memory_session_id, NULL, s.project,
                   NULL, s.completed, s.created_at, s.created_at_epoch
            FROM session_summaries s WHERE s.project = ?1
            UNION ALL
            SELECT 'prompt', p.id, NULL, p.content_session_id,
                   (SELECT project FROM sdk_sessions
                    WHERE content_session_id = p.content_session_id LIMIT 1),
                   NULL, p.prompt_text, p.created_at, p.created_at_epoch
            FROM user_prompts p
            WHERE p.content_session_id IN (
                SELECT content_session_id FROM sdk_sessions WHERE project = ?1
            )
         )
         ORDER BY created_at_epoch DESC
         LIMIT ?2",
    )?;
    let rows: Result<Vec<_>> = stmt.query_map(params![project, limit], row_from)?.collect();
    rows
}

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<TimelineRow> {
    let kind_raw: String = row.get(0)?;
    let kind = match kind_raw.as_str() {
        "observation" => TimelineKind::Observation,
        "summary" => TimelineKind::Summary,
        "prompt" => TimelineKind::Prompt,
        _ => TimelineKind::Observation,
    };
    Ok(TimelineRow {
        kind,
        id: row.get(1)?,
        memory_session_id: row.get(2)?,
        content_session_id: row.get(3)?,
        project: row.get(4)?,
        title: row.get(5)?,
        text: row.get(6)?,
        created_at: row.get(7)?,
        created_at_epoch: row.get(8)?,
    })
}
