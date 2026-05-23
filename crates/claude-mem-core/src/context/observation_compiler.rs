//! Observation query façade used by the context compiler and timeline.

use rusqlite::{params, Connection, Result};

use crate::db::observations::get::get_observations_by_ids;
use crate::types::ObservationRow;

#[derive(Debug, Clone, Default)]
pub struct ObservationQuery {
    pub project: Option<String>,
    pub limit: i64,
}

/// Fetch observations matching a project-scoped query, ordered newest-first.
pub fn query_observations(
    conn: &Connection,
    q: &ObservationQuery,
) -> Result<Vec<ObservationRow>> {
    let ids: Vec<i64> = if let Some(p) = q.project.as_deref() {
        let mut stmt = conn.prepare(
            "SELECT id FROM observations WHERE project = ?
             ORDER BY created_at_epoch DESC, id DESC LIMIT ?",
        )?;
        let collected: Result<Vec<i64>> = stmt
            .query_map(params![p, q.limit], |r| r.get(0))?
            .collect();
        collected?
    } else {
        let mut stmt = conn.prepare(
            "SELECT id FROM observations
             ORDER BY created_at_epoch DESC, id DESC LIMIT ?",
        )?;
        let collected: Result<Vec<i64>> = stmt
            .query_map(params![q.limit], |r| r.get(0))?
            .collect();
        collected?
    };
    get_observations_by_ids(conn, &ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_on_zero_limit() {
        let conn = crate::db::open_in_memory().unwrap();
        let rows = query_observations(
            &conn,
            &ObservationQuery {
                project: None,
                limit: 0,
            },
        )
        .unwrap();
        assert!(rows.is_empty());
    }
}
