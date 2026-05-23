//! Batch / transactional operations.
//!
//! Port of `src/services/sqlite/transactions.ts`.

use rusqlite::{params, Connection, Result};

use super::observations::store::{store_observation, StoreObservationResult};
use crate::types::ObservationInput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchStoreResult {
    pub inserted: i64,
    pub duplicates: i64,
}

/// Store a batch of observations in a single transaction, optionally
/// marking a pending message as processed afterward (port of
/// `storeObservationsAndMarkComplete`).
pub fn store_observations(
    conn: &Connection,
    observations: &[ObservationInput],
    pending_message_id: Option<i64>,
) -> Result<BatchStoreResult> {
    let tx = conn.unchecked_transaction()?;
    let mut inserted = 0i64;
    let mut duplicates = 0i64;
    for obs in observations {
        match store_observation(&tx, obs)? {
            StoreObservationResult::Inserted(_) => inserted += 1,
            StoreObservationResult::Duplicate(_) => duplicates += 1,
        }
    }
    if let Some(id) = pending_message_id {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        tx.execute(
            "UPDATE pending_messages
                SET status = 'processed', completed_at_epoch = ?1
              WHERE id = ?2",
            params![now_secs, id],
        )?;
    }
    tx.commit()?;
    Ok(BatchStoreResult {
        inserted,
        duplicates,
    })
}
