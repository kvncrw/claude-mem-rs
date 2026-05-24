//! Batch / transactional storage.
//!
//! Port of `src/services/sqlite/transactions.ts`.
//!
//! Two entry points matching the TS contract:
//!
//! * [`store_batch`] — atomically store N observations plus an optional
//!   summary in one transaction. Returns the IDs assigned (in insertion
//!   order) plus the `createdAtEpoch` shared across all rows.
//!
//! * [`store_batch_and_mark_complete`] — same, but also flips a
//!   `pending_messages` row from `processing` to `processed`. Mirrors the
//!   TS `storeObservationsAndMarkComplete`.

use rusqlite::{params, Connection, Result};

use super::observations::store::{store_observation, StoreObservationResult};
use super::summaries::{store_summary, SummaryInput};
use crate::types::ObservationInput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchStoreResult {
    /// Observation IDs assigned, in insertion order.
    pub observation_ids: Vec<i64>,
    /// Summary ID assigned (if a summary was stored).
    pub summary_id: Option<i64>,
    /// Shared `created_at_epoch` stamped on every row in the batch.
    pub created_at_epoch: i64,
    /// Observations that were inserted vs. content-hash-deduplicated.
    pub inserted: i64,
    pub duplicates: i64,
}

/// Atomically store a batch of observations plus an optional summary.
///
/// `prompt_number`, `discovery_tokens`, and `created_at_epoch` are optional
/// overrides applied to every row. When `created_at_epoch` is `None`, the
/// current wall-clock is used (millisecond precision).
#[allow(clippy::too_many_arguments)]
pub fn store_batch(
    conn: &Connection,
    memory_session_id: &str,
    project: &str,
    observations: &[ObservationInput],
    summary: Option<&SummaryInput>,
    prompt_number: Option<i64>,
    discovery_tokens: Option<i64>,
    created_at_epoch: Option<i64>,
) -> Result<BatchStoreResult> {
    let tx = conn.unchecked_transaction()?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let epoch = created_at_epoch.unwrap_or(now_ms);

    let mut ids = Vec::with_capacity(observations.len());
    let mut inserted = 0i64;
    let mut duplicates = 0i64;

    for mut_obs in observations {
        let mut obs = mut_obs.clone();
        obs.memory_session_id = memory_session_id.into();
        obs.project = project.into();
        if prompt_number.is_some() {
            obs.prompt_number = prompt_number;
        }
        if discovery_tokens.is_some() {
            obs.discovery_tokens = discovery_tokens;
        }
        obs.created_at_epoch = epoch;
        // Recompute content_hash now that session/project may differ from input.
        obs.content_hash = Some(super::observations::compute_observation_content_hash(&obs));

        match store_observation(&tx, &obs)? {
            StoreObservationResult::Inserted(id) => {
                ids.push(id);
                inserted += 1;
            }
            StoreObservationResult::Duplicate(id) => {
                ids.push(id);
                duplicates += 1;
            }
        }
    }

    let summary_id = match summary {
        None => None,
        Some(s) => {
            let mut s = s.clone();
            s.memory_session_id = memory_session_id.into();
            s.project = project.into();
            s.prompt_number = prompt_number;
            s.discovery_tokens = discovery_tokens;
            s.created_at_epoch = epoch;
            Some(store_summary(&tx, &s)?)
        }
    };

    tx.commit()?;

    Ok(BatchStoreResult {
        observation_ids: ids,
        summary_id,
        created_at_epoch: epoch,
        inserted,
        duplicates,
    })
}

/// Same as [`store_batch`], but additionally marks `pending_message_id` as
/// `processed` with `completed_at_epoch = epoch` inside the same transaction
/// (port of `storeObservationsAndMarkComplete`).
#[allow(clippy::too_many_arguments)]
pub fn store_batch_and_mark_complete(
    conn: &Connection,
    memory_session_id: &str,
    project: &str,
    observations: &[ObservationInput],
    summary: Option<&SummaryInput>,
    pending_message_id: i64,
    prompt_number: Option<i64>,
    discovery_tokens: Option<i64>,
    created_at_epoch: Option<i64>,
) -> Result<BatchStoreResult> {
    let tx = conn.unchecked_transaction()?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let epoch = created_at_epoch.unwrap_or(now_ms);

    let mut ids = Vec::with_capacity(observations.len());
    let mut inserted = 0i64;
    let mut duplicates = 0i64;

    for mut_obs in observations {
        let mut obs = mut_obs.clone();
        obs.memory_session_id = memory_session_id.into();
        obs.project = project.into();
        if prompt_number.is_some() {
            obs.prompt_number = prompt_number;
        }
        if discovery_tokens.is_some() {
            obs.discovery_tokens = discovery_tokens;
        }
        obs.created_at_epoch = epoch;
        obs.content_hash = Some(super::observations::compute_observation_content_hash(&obs));

        match store_observation(&tx, &obs)? {
            StoreObservationResult::Inserted(id) => {
                ids.push(id);
                inserted += 1;
            }
            StoreObservationResult::Duplicate(id) => {
                ids.push(id);
                duplicates += 1;
            }
        }
    }

    let summary_id = match summary {
        None => None,
        Some(s) => {
            let mut s = s.clone();
            s.memory_session_id = memory_session_id.into();
            s.project = project.into();
            s.prompt_number = prompt_number;
            s.discovery_tokens = discovery_tokens;
            s.created_at_epoch = epoch;
            Some(store_summary(&tx, &s)?)
        }
    };

    tx.execute(
        "UPDATE pending_messages
            SET status = 'processed', completed_at_epoch = ?1
          WHERE id = ?2",
        params![epoch, pending_message_id],
    )?;

    tx.commit()?;

    Ok(BatchStoreResult {
        observation_ids: ids,
        summary_id,
        created_at_epoch: epoch,
        inserted,
        duplicates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory;
    use crate::db::sessions;
    use crate::types::observation::ObservationInput;
    use crate::types::session::CreateSessionInput;

    fn seed_session(conn: &Connection, content: &str, memory: &str, project: &str) -> i64 {
        sessions::create_session(
            conn,
            &CreateSessionInput {
                content_session_id: content.into(),
                project: project.into(),
                user_prompt: Some("initial prompt".into()),
                started_at: "2026-05-23T15:00:00Z".into(),
                started_at_epoch: 1748012400,
            },
        )
        .unwrap();
        let row = sessions::get_session_by_content_id(conn, content)
            .unwrap()
            .unwrap();
        sessions::update_memory_session_id(conn, content, memory).unwrap();
        row.id
    }

    fn obs(title: &str) -> ObservationInput {
        ObservationInput {
            r#type: "discovery".into(),
            title: Some(title.into()),
            subtitle: Some("sub".into()),
            facts: Some(vec!["f".into()]),
            narrative: Some("n".into()),
            concepts: Some(vec!["c".into()]),
            files_read: Some(vec!["/f1.ts".into()]),
            files_modified: Some(vec!["/f2.ts".into()]),
            created_at: "2026-05-23T15:00:00Z".into(),
            created_at_epoch: 0,
            ..Default::default()
        }
    }

    fn sum(req: &str) -> SummaryInput {
        SummaryInput {
            request: Some(req.into()),
            investigated: Some("inv".into()),
            learned: Some("learn".into()),
            completed: Some("done".into()),
            next_steps: Some("next".into()),
            notes: Some("notes".into()),
            created_at: "2026-05-23T15:00:00Z".into(),
            created_at_epoch: 0,
            ..Default::default()
        }
    }

    #[test]
    fn store_batch_returns_all_ids_and_epoch() {
        let conn = open_in_memory().unwrap();
        let mem = "mem-atomic-123";
        let _db_id = seed_session(&conn, "content-atomic-123", mem, "test-project");
        let result = store_batch(
            &conn,
            mem,
            "test-project",
            &[obs("Obs 1"), obs("Obs 2"), obs("Obs 3")],
            None,
            None,
            None,
            None,
        )
        .unwrap();

        assert_eq!(result.observation_ids.len(), 3);
        assert!(result.observation_ids.iter().all(|id| *id > 0));
        assert!(result.summary_id.is_none());
        assert!(result.created_at_epoch > 0);
        assert_eq!(result.inserted, 3);
        assert_eq!(result.duplicates, 0);
    }

    #[test]
    fn store_batch_uses_override_timestamp() {
        let conn = open_in_memory().unwrap();
        let mem = "mem-ts-ov";
        seed_session(&conn, "content-ts-ov", mem, "test-project");
        let fixed = 1_600_000_000_000i64;
        let result = store_batch(
            &conn,
            mem,
            "test-project",
            &[obs("Obs A"), obs("Obs B")],
            None,
            None,
            None,
            Some(fixed),
        )
        .unwrap();

        assert_eq!(result.created_at_epoch, fixed);
        for id in &result.observation_ids {
            use crate::db::observations::get_observation_by_id;
            let row = get_observation_by_id(&conn, *id).unwrap().unwrap();
            assert_eq!(row.created_at_epoch, fixed);
        }
    }

    #[test]
    fn store_batch_with_summary_persists_both() {
        let conn = open_in_memory().unwrap();
        let mem = "mem-with-sum";
        seed_session(&conn, "content-with-sum", mem, "test-project");
        let result = store_batch(
            &conn,
            mem,
            "test-project",
            &[obs("Main Obs")],
            Some(&sum("Test request")),
            None,
            None,
            None,
        )
        .unwrap();

        assert_eq!(result.observation_ids.len(), 1);
        assert!(result.summary_id.is_some());

        use crate::db::summaries::get_summary_for_session;
        let stored = get_summary_for_session(&conn, mem).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].request.as_deref(), Some("Test request"));
    }

    #[test]
    fn store_batch_handles_empty_observations() {
        let conn = open_in_memory().unwrap();
        let mem = "mem-empty";
        seed_session(&conn, "content-empty", mem, "test-project");
        let result = store_batch(&conn, mem, "test-project", &[], None, None, None, None).unwrap();

        assert!(result.observation_ids.is_empty());
        assert!(result.summary_id.is_none());
        assert_eq!(result.inserted, 0);
        assert_eq!(result.duplicates, 0);
    }

    #[test]
    fn store_batch_summary_only_without_observations() {
        let conn = open_in_memory().unwrap();
        let mem = "mem-sum-only";
        seed_session(&conn, "content-sum-only", mem, "test-project");
        let result = store_batch(
            &conn,
            mem,
            "test-project",
            &[],
            Some(&sum("Summary-only request")),
            None,
            None,
            None,
        )
        .unwrap();

        assert!(result.observation_ids.is_empty());
        assert!(result.summary_id.is_some());

        use crate::db::summaries::get_summary_for_session;
        let stored = get_summary_for_session(&conn, mem).unwrap();
        assert_eq!(stored[0].request.as_deref(), Some("Summary-only request"));
    }

    #[test]
    fn store_batch_applies_prompt_number_to_all() {
        let conn = open_in_memory().unwrap();
        let mem = "mem-pn";
        seed_session(&conn, "content-pn", mem, "test-project");
        let prompt = 5i64;
        let result = store_batch(
            &conn,
            mem,
            "test-project",
            &[obs("Obs 1"), obs("Obs 2")],
            None,
            Some(prompt),
            None,
            None,
        )
        .unwrap();

        use crate::db::observations::get_observation_by_id;
        for id in &result.observation_ids {
            let row = get_observation_by_id(&conn, *id).unwrap().unwrap();
            assert_eq!(row.prompt_number, Some(prompt));
        }
    }

    #[test]
    fn store_batch_and_mark_complete_marks_pending_as_processed() {
        let conn = open_in_memory().unwrap();
        let mem = "mem-complete";
        let content = "content-complete";
        let db_id = seed_session(&conn, content, mem, "test-project");

        conn.execute(
            "INSERT INTO pending_messages
             (session_db_id, content_session_id, message_type, created_at_epoch, status)
             VALUES (?1, ?2, 'observation', ?3, 'processing')",
            params![db_id, content, 1748012400i64],
        )
        .unwrap();
        let message_id: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .unwrap();

        let result = store_batch_and_mark_complete(
            &conn,
            mem,
            "test-project",
            &[obs("Complete Obs")],
            Some(&sum("Complete request")),
            message_id,
            None,
            None,
            None,
        )
        .unwrap();

        assert_eq!(result.observation_ids.len(), 1);
        assert!(result.summary_id.is_some());

        let status: String = conn
            .query_row(
                "SELECT status FROM pending_messages WHERE id = ?",
                params![message_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "processed");
    }

    #[test]
    fn store_batch_and_mark_complete_shares_timestamp() {
        let conn = open_in_memory().unwrap();
        let mem = "mem-atomic-ts";
        let content = "content-atomic-ts";
        let db_id = seed_session(&conn, content, mem, "test-project");
        let fixed = 1_700_000_000_000i64;

        conn.execute(
            "INSERT INTO pending_messages
             (session_db_id, content_session_id, message_type, created_at_epoch, status)
             VALUES (?1, ?2, 'observation', ?3, 'processing')",
            params![db_id, content, 1748012400i64],
        )
        .unwrap();

        let result = store_batch_and_mark_complete(
            &conn,
            mem,
            "test-project",
            &[obs("Obs 1"), obs("Obs 2")],
            Some(&sum("Shared TS")),
            conn.query_row("SELECT last_insert_rowid()", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            None,
            None,
            Some(fixed),
        )
        .unwrap();

        assert_eq!(result.created_at_epoch, fixed);
        use crate::db::observations::get_observation_by_id;
        for id in &result.observation_ids {
            let obs = get_observation_by_id(&conn, *id).unwrap().unwrap();
            assert_eq!(obs.created_at_epoch, fixed);
        }
        use crate::db::summaries::get_summary_for_session;
        let sums = get_summary_for_session(&conn, mem).unwrap();
        assert_eq!(sums[0].created_at_epoch, fixed);
    }

    #[test]
    fn store_batch_and_mark_complete_handles_null_summary() {
        let conn = open_in_memory().unwrap();
        let mem = "mem-no-sum";
        let content = "content-no-sum";
        let db_id = seed_session(&conn, content, mem, "test-project");

        conn.execute(
            "INSERT INTO pending_messages
             (session_db_id, content_session_id, message_type, created_at_epoch, status)
             VALUES (?1, ?2, 'observation', ?3, 'processing')",
            params![db_id, content, 1748012400i64],
        )
        .unwrap();
        let message_id: i64 = conn
            .query_row("SELECT last_insert_rowid()", [], |r| r.get(0))
            .unwrap();

        let result = store_batch_and_mark_complete(
            &conn,
            mem,
            "test-project",
            &[obs("Only Obs")],
            None,
            message_id,
            None,
            None,
            None,
        )
        .unwrap();

        assert_eq!(result.observation_ids.len(), 1);
        assert!(result.summary_id.is_none());
    }
}
