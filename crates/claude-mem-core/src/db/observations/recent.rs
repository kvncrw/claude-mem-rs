//! Recent-observation queries (port of
//! `src/services/sqlite/observations/recent.ts`).

use rusqlite::{params, Connection, Result};

use crate::types::observation::ObservationRow;

use super::get::get_observations_by_ids;

/// Fetch the most recent N observation ids, optionally filtered by project.
fn recent_ids(
    conn: &Connection,
    project: Option<&str>,
    limit: i64,
) -> Result<Vec<i64>> {
    let mut stmt = match project {
        None => conn.prepare(
            "SELECT id FROM observations
             ORDER BY created_at_epoch DESC, id DESC
             LIMIT ?",
        )?,
        Some(_) => conn.prepare(
            "SELECT id FROM observations
             WHERE project = ?1
             ORDER BY created_at_epoch DESC, id DESC
             LIMIT ?2",
        )?,
    };
    let ids: Vec<i64> = if let Some(p) = project {
        stmt.query_map(params![p, limit], |r| r.get(0))?
            .collect::<Result<_>>()?
    } else {
        stmt.query_map(params![limit], |r| r.get(0))?
            .collect::<Result<_>>()?
    };
    Ok(ids)
}

pub fn get_recent_observations(
    conn: &Connection,
    project: Option<&str>,
    limit: i64,
) -> Result<Vec<ObservationRow>> {
    let ids = recent_ids(conn, project, limit)?;
    get_observations_by_ids(conn, &ids)
}

/// Recent observations across **all** projects, joined with session start
/// timestamp.
#[derive(Debug, Clone)]
pub struct AllRecentObservationRow {
    pub row: ObservationRow,
    pub session_started_at: Option<String>,
}

pub fn get_all_recent_observations(
    conn: &Connection,
    limit: i64,
) -> Result<Vec<AllRecentObservationRow>> {
    let mut stmt = conn.prepare(
        "SELECT o.id, o.memory_session_id, o.project, o.text, o.type,
                o.title, o.subtitle, o.narrative,
                COALESCE(o.facts,''), COALESCE(o.concepts,''),
                COALESCE(o.files_read,''), COALESCE(o.files_modified,''),
                o.prompt_number, o.discovery_tokens, o.created_at,
                o.created_at_epoch, o.generated_by_model, o.relevance_count,
                o.merged_into_project, o.agent_type, o.agent_id, o.content_hash,
                s.started_at
         FROM observations o
         LEFT JOIN sdk_sessions s ON s.memory_session_id = o.memory_session_id
         ORDER BY o.created_at_epoch DESC, o.id DESC
         LIMIT ?",
    )?;

    let rows: Result<Vec<_>> = stmt
        .query_map(params![limit], |row| {
            use crate::db::observations::get::row_from_helper;
            Ok(AllRecentObservationRow {
                row: row_from_helper(row, 0)?,
                session_started_at: row.get(22)?,
            })
        })?
        .collect();
    rows
}
