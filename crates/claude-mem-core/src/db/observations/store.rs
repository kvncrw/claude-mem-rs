//! `observations` write surface (port of
//! `src/services/sqlite/observations/store.ts`).

use rusqlite::{params, Connection, Result};

use crate::types::observation::ObservationInput;

/// Compute a stable content hash for deduplication (port of
/// `computeObservationContentHash`).
pub fn compute_observation_content_hash(obs: &ObservationInput) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut s = DefaultHasher::new();
    obs.memory_session_id.hash(&mut s);
    obs.project.hash(&mut s);
    obs.r#type.hash(&mut s);
    obs.title.hash(&mut s);
    obs.subtitle.hash(&mut s);
    obs.narrative.hash(&mut s);
    format!("{:016x}", s.finish())
}

/// Look up any existing row that shares the same content hash and session
/// (port of `findDuplicateObservation`). Returns `Some(id)` on hit.
pub fn find_duplicate_observation(
    conn: &Connection,
    obs: &ObservationInput,
) -> Result<Option<i64>> {
    match obs.content_hash.as_ref() {
        None => Ok(None),
        Some(hash) => {
            let id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM observations
                 WHERE memory_session_id = ? AND content_hash = ?
                 LIMIT 1",
                    params![obs.memory_session_id, hash],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(id)
        }
    }
}

/// Insert an observation (port of `storeObservation` semantics —
/// `INSERT OR IGNORE` on session + content-hash, returns the new row id).
pub fn store_observation(
    conn: &Connection,
    obs: &ObservationInput,
) -> Result<StoreObservationResult> {
    if let Some(existing) = find_duplicate_observation(conn, obs)? {
        return Ok(StoreObservationResult::Duplicate(existing));
    }

    let mut stmt = conn.prepare_cached(
        "INSERT INTO observations
         (memory_session_id, project, text, type, title, subtitle, facts,
          narrative, concepts, files_read, files_modified, prompt_number,
          discovery_tokens, created_at, created_at_epoch, generated_by_model,
          relevance_count, merged_into_project, agent_type, agent_id,
          content_hash)
         VALUES
         (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
          ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
    )?;

    let json = |v: &Option<Vec<String>>| -> String {
        v.as_ref()
            .map(|xs| serde_json::to_string(xs).unwrap_or_else(|_| "[]".into()))
            .unwrap_or_default()
    };
    let facts_s = json(&obs.facts);
    let concepts_s = json(&obs.concepts);
    let files_read_s = json(&obs.files_read);
    let files_modified_s = json(&obs.files_modified);
    let disc = obs.discovery_tokens.unwrap_or(0);
    let rel = obs.relevance_count.unwrap_or(0);

    stmt.execute(params![
        obs.memory_session_id,
        obs.project,
        obs.text,
        obs.r#type,
        obs.title,
        obs.subtitle,
        facts_s,
        obs.narrative,
        concepts_s,
        files_read_s,
        files_modified_s,
        obs.prompt_number,
        disc,
        obs.created_at,
        obs.created_at_epoch,
        obs.generated_by_model,
        rel,
        obs.merged_into_project,
        obs.agent_type,
        obs.agent_id,
        obs.content_hash,
    ])?;

    Ok(StoreObservationResult::Inserted(conn.last_insert_rowid()))
}

/// Outcome of [`store_observation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreObservationResult {
    Inserted(i64),
    Duplicate(i64),
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
