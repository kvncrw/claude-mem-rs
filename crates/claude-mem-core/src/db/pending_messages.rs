//! Persistent pending-messages processing queue (port of
//! `src/services/sqlite/PendingMessageStore.ts`).
//!
//! NOT a compression queue — a claim-confirm processing queue for tool
//! messages returned by the LLM.
//!
//! Lifecycle: `enqueue(pending)` → `claimNextMessage(processing)` →
//! `confirmProcessed(delete)` OR `markFailed(±retry → pending | failed)`.
//!
//! Self-healing: `claimNextMessage` resets any row stuck in `processing`
//! with `started_processing_at_epoch < now - stale_threshold_ms` back to
//! `pending` before claiming, so a crashed worker leaves no stranded work.

use rusqlite::{params, Connection, Result};
use serde_json::Value;

use crate::types::pending_message::PendingMessageRow;

/// Default: messages older than 60s in `processing` are considered stuck.
const DEFAULT_STALE_THRESHOLD_MS: i64 = 60_000;
const DEFAULT_MAX_RETRIES: i64 = 3;

#[derive(Debug, Clone, Default, bon::Builder)]
#[builder(on(String, into))]
pub struct EnqueueInput {
    pub session_db_id: i64,
    pub content_session_id: String,
    pub message_type: String,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub tool_response: Option<serde_json::Value>,
    pub cwd: Option<String>,
    pub last_user_message: Option<String>,
    pub last_assistant_message: Option<String>,
    pub prompt_number: Option<i64>,
    pub created_at_epoch: i64,
    pub agent_type: Option<String>,
    pub agent_id: Option<String>,
}

/// Persistent pending-message processing queue. Port of
/// `PendingMessageStore` class. Holds no state beyond the `Connection` and
/// tuning knobs; operations take `conn` explicitly for testability.
pub struct PendingMessageStore {
    pub max_retries: i64,
    pub stale_threshold_ms: i64,
}

impl Default for PendingMessageStore {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            stale_threshold_ms: DEFAULT_STALE_THRESHOLD_MS,
        }
    }
}

impl PendingMessageStore {
    pub fn new(max_retries: i64) -> Self {
        Self {
            max_retries,
            stale_threshold_ms: DEFAULT_STALE_THRESHOLD_MS,
        }
    }

    pub fn enqueue(&self, conn: &Connection, input: &EnqueueInput) -> Result<i64> {
        let tool_in = json_to_text(input.tool_input.as_ref().map(redact_json_value).as_ref())?;
        let tool_resp = json_to_text(input.tool_response.as_ref().map(redact_json_value).as_ref())?;
        let last_user_message = input
            .last_user_message
            .as_ref()
            .map(|value| redact_secrets(value));
        let last_assistant_message = input
            .last_assistant_message
            .as_ref()
            .map(|value| redact_secrets(value));
        conn.execute(
            "INSERT INTO pending_messages
             (session_db_id, content_session_id, message_type, tool_name,
              tool_input, tool_response, cwd, last_user_message,
              last_assistant_message, prompt_number, status, retry_count,
              created_at_epoch, agent_type, agent_id)
             VALUES
             (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'pending',0,?11,?12,?13)",
            params![
                input.session_db_id,
                input.content_session_id,
                input.message_type,
                input.tool_name,
                tool_in,
                tool_resp,
                input.cwd,
                last_user_message,
                last_assistant_message,
                input.prompt_number,
                input.created_at_epoch,
                input.agent_type,
                input.agent_id,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Claim the oldest `pending` message for `session_db_id`, after first
    /// self-healing any `processing` messages that have been stuck longer
    /// than `stale_threshold_ms`. Atomic within this call.
    pub fn claim_next_message(
        &self,
        conn: &Connection,
        session_db_id: i64,
    ) -> Result<Option<PendingMessageRow>> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let stale_cutoff = now_ms - self.stale_threshold_ms;

        // Self-heal: any `processing` row for this session whose
        // `started_processing_at_epoch` is older than the stale cutoff is
        // reset back to `pending` so it can be reclaimed.
        conn.execute(
            "UPDATE pending_messages
                SET status = 'pending', started_processing_at_epoch = NULL
              WHERE session_db_id = ?1
                AND status = 'processing'
                AND started_processing_at_epoch IS NOT NULL
                AND started_processing_at_epoch < ?2",
            params![session_db_id, stale_cutoff],
        )?;

        // Claim the oldest pending row for this session.
        let row_id: Option<i64> = conn
            .query_row(
                "SELECT id FROM pending_messages
                 WHERE session_db_id = ?1 AND status = 'pending'
                 ORDER BY created_at_epoch ASC, id ASC LIMIT 1",
                params![session_db_id],
                |r| r.get(0),
            )
            .optional()?;

        let id = match row_id {
            None => return Ok(None),
            Some(id) => id,
        };

        conn.execute(
            "UPDATE pending_messages
                SET status = 'processing', started_processing_at_epoch = ?1
              WHERE id = ?2",
            params![now_ms, id],
        )?;

        get_by_id(conn, id)
    }

    /// Claim one specific pending message by id. This is used by hook-facing
    /// routes so a fresh hook event is not blocked behind old session backlog.
    pub fn claim_message_by_id(
        &self,
        conn: &Connection,
        id: i64,
    ) -> Result<Option<PendingMessageRow>> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let changed = conn.execute(
            "UPDATE pending_messages
                SET status = 'processing', started_processing_at_epoch = ?1
              WHERE id = ?2 AND status = 'pending'",
            params![now_ms, id],
        )?;
        if changed == 0 {
            return Ok(None);
        }

        get_by_id(conn, id)
    }

    /// Mark message as successfully processed; records a durable completion
    /// event and deletes the active queue row.
    pub fn confirm_processed(&self, conn: &Connection, id: i64) -> Result<()> {
        let now_ms = now_epoch_ms();
        record_queue_event(conn, id, "processed", now_ms)?;
        conn.execute("DELETE FROM pending_messages WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Mark a message as failed. If retry count is under `max_retries`, the
    /// row returns to `pending` with `retry_count + 1`; otherwise it's
    /// permanently marked `failed`.
    pub fn mark_failed(&self, conn: &Connection, id: i64) -> Result<MarkFailedOutcome> {
        let now_ms = now_epoch_ms();
        let current: Option<(i64, i64)> = conn
            .query_row(
                "SELECT retry_count, ?1 >= ?2 FROM pending_messages
                   WHERE id = ?3",
                params![now_ms, 0, id],
                |r| Ok((r.get::<_, i64>(0)?, 0i64)),
            )
            .optional()?
            .map(|(rc, _)| (rc, id));

        let (retry_count, _) = match current {
            None => return Ok(MarkFailedOutcome::NotFound),
            Some(v) => v,
        };

        if retry_count + 1 >= self.max_retries {
            conn.execute(
                "UPDATE pending_messages
                    SET status = 'failed', failed_at_epoch = ?1
                  WHERE id = ?2",
                params![now_ms, id],
            )?;
            record_queue_event(conn, id, "failed", now_ms)?;
            Ok(MarkFailedOutcome::PermanentlyFailed)
        } else {
            conn.execute(
                "UPDATE pending_messages
                    SET status = 'pending',
                        retry_count = retry_count + 1,
                        started_processing_at_epoch = NULL
                  WHERE id = ?1",
                params![id],
            )?;
            Ok(MarkFailedOutcome::Retried(retry_count + 1))
        }
    }
}

fn now_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn record_queue_event(
    conn: &Connection,
    pending_message_id: i64,
    status: &str,
    completed_at_epoch: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pending_message_events
            (pending_message_id, session_db_id, content_session_id, message_type,
             tool_name, status, retry_count, created_at_epoch,
             started_processing_at_epoch, completed_at_epoch, duration_ms,
             agent_type, agent_id)
         SELECT id, session_db_id, content_session_id, message_type,
                tool_name, ?1, retry_count, created_at_epoch,
                started_processing_at_epoch, ?2,
                CASE
                    WHEN started_processing_at_epoch IS NOT NULL
                    THEN MAX(0, ?2 - started_processing_at_epoch)
                    ELSE NULL
                END,
                agent_type, agent_id
           FROM pending_messages
          WHERE id = ?3",
        params![status, completed_at_epoch, pending_message_id],
    )?;
    Ok(())
}

fn redact_json_value(value: &Value) -> Value {
    match value {
        Value::String(value) => Value::String(redact_secrets(value)),
        Value::Array(values) => Value::Array(values.iter().map(redact_json_value).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    if key_is_secret(key) {
                        (key.clone(), Value::String("[REDACTED]".to_owned()))
                    } else {
                        (key.clone(), redact_json_value(value))
                    }
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn key_is_secret(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower == "authorization"
}

fn redact_secrets(input: &str) -> String {
    input
        .split_whitespace()
        .map(|part| {
            if looks_like_secret(part) {
                redact_secret_part(part)
            } else {
                part.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn looks_like_secret(part: &str) -> bool {
    let trimmed =
        part.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_');
    trimmed.starts_with("sk-")
        || trimmed.starts_with("sk_")
        || trimmed.starts_with("sk-or-")
        || trimmed.starts_with("ghp_")
        || trimmed.starts_with("github_pat_")
}

fn redact_secret_part(part: &str) -> String {
    let start = part
        .find(|ch: char| ch.is_ascii_alphanumeric())
        .unwrap_or(0);
    let end = part
        .rfind(|ch: char| ch.is_ascii_alphanumeric())
        .map(|idx| idx + 1)
        .unwrap_or(part.len());
    format!("{}[REDACTED]{}", &part[..start], &part[end..])
}

/// Outcome of [`PendingMessageStore::mark_failed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkFailedOutcome {
    NotFound,
    /// Row returned to `pending` with the new `retry_count`.
    Retried(i64),
    /// `max_retries` exceeded; row is permanently `failed`.
    PermanentlyFailed,
}

pub fn get_by_id(conn: &Connection, id: i64) -> Result<Option<PendingMessageRow>> {
    conn.query_row(
        "SELECT id, session_db_id, content_session_id, message_type,
                tool_name, tool_input, tool_response, cwd, last_user_message,
                last_assistant_message, prompt_number, status, retry_count,
                created_at_epoch, started_processing_at_epoch,
                completed_at_epoch, failed_at_epoch, agent_type, agent_id
         FROM pending_messages WHERE id = ?1",
        params![id],
        row_from,
    )
    .optional()
}

fn json_to_text(v: Option<&serde_json::Value>) -> Result<Option<String>, rusqlite::Error> {
    v.map(serde_json::to_string)
        .transpose()
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e).into()))
}

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingMessageRow> {
    // SELECT columns: id(0), session_db_id(1), content_session_id(2), message_type(3),
    // tool_name(4), tool_input(5), tool_response(6), cwd(7), last_user_message(8),
    // last_assistant_message(9), prompt_number(10), status(11), retry_count(12),
    // created_at_epoch(13), started_processing_at_epoch(14),
    // completed_at_epoch(15), failed_at_epoch(16), agent_type(17), agent_id(18).
    let tool_in_raw: Option<String> = row.get(5)?;
    let tool_resp_raw: Option<String> = row.get(6)?;
    Ok(PendingMessageRow {
        id: row.get(0)?,
        session_db_id: row.get(1)?,
        content_session_id: row.get(2)?,
        message_type: row.get(3)?,
        tool_name: row.get(4)?,
        tool_input: tool_in_raw.and_then(|s| serde_json::from_str(&s).ok()),
        tool_response: tool_resp_raw.and_then(|s| serde_json::from_str(&s).ok()),
        cwd: row.get(7)?,
        last_user_message: row.get(8)?,
        last_assistant_message: row.get(9)?,
        prompt_number: row.get(10)?,
        status: row.get(11)?,
        retry_count: row.get(12)?,
        created_at_epoch: row.get(13)?,
        started_processing_at_epoch: row.get(14)?,
        completed_at_epoch: row.get(15)?,
        failed_at_epoch: row.get(16)?,
        agent_type: row.get(17)?,
        agent_id: row.get(18)?,
    })
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
