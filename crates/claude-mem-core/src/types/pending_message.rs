use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingMessageRow {
    pub id: i64,
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
    pub status: String,
    pub retry_count: i64,
    pub created_at_epoch: i64,
    pub started_processing_at_epoch: Option<i64>,
    pub completed_at_epoch: Option<i64>,
    pub failed_at_epoch: Option<i64>,
    pub agent_type: Option<String>,
    pub agent_id: Option<String>,
}
