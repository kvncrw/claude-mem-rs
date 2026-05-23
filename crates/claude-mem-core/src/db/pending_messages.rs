//! Persistent pending-messages processing queue.
//!
//! Port of `src/services/sqlite/PendingMessageStore.ts`. NOT a compression
//! queue — a claim-confirm processing queue for tool messages returned by
//! the LLM.

use rusqlite::{params, Connection, Result};

use crate::types::pending_message::PendingMessageRow;

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

fn json_to_text(v: Option<&serde_json::Value>) -> Result<Option<String>, rusqlite::Error> {
    v.map(serde_json::to_string)
        .transpose()
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e).into()))
}

pub fn enqueue(conn: &Connection, input: &EnqueueInput) -> Result<i64> {
    let tool_in = json_to_text(input.tool_input.as_ref())?;
    let tool_resp = json_to_text(input.tool_response.as_ref())?;
    conn.execute(
        "INSERT INTO pending_messages
         (session_db_id, content_session_id, message_type, tool_name,
          tool_input, tool_response, cwd, last_user_message, last_assistant_message,
          prompt_number, status, retry_count, created_at_epoch,
          agent_type, agent_id)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'pending',0,?11,?12,?13)",
        params![
            input.session_db_id,
            input.content_session_id,
            input.message_type,
            input.tool_name,
            tool_in,
            tool_resp,
            input.cwd,
            input.last_user_message,
            input.last_assistant_message,
            input.prompt_number,
            input.created_at_epoch,
            input.agent_type,
            input.agent_id,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingMessageRow> {
    let tool_in_raw: Option<String> = row.get(4)?;
    let tool_resp_raw: Option<String> = row.get(5)?;
    Ok(PendingMessageRow {
        id: row.get(0)?,
        session_db_id: row.get(1)?,
        content_session_id: row.get(2)?,
        message_type: row.get(3)?,
        tool_name: row.get(4)?,
        tool_input: tool_in_raw.and_then(|s| serde_json::from_str(&s).ok()),
        tool_response: tool_resp_raw.and_then(|s| serde_json::from_str(&s).ok()),
        cwd: row.get(6)?,
        last_user_message: row.get(7)?,
        last_assistant_message: row.get(8)?,
        prompt_number: row.get(9)?,
        status: row.get(10)?,
        retry_count: row.get(11)?,
        created_at_epoch: row.get(12)?,
        started_processing_at_epoch: row.get(13)?,
        completed_at_epoch: row.get(14)?,
        failed_at_epoch: row.get(15)?,
        agent_type: row.get(16)?,
        agent_id: row.get(17)?,
    })
}
