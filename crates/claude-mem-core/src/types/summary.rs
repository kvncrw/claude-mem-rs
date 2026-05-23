use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionSummaryRow {
    pub id: i64,
    pub memory_session_id: String,
    pub project: String,
    pub request: Option<String>,
    pub investigated: Option<String>,
    pub learned: Option<String>,
    pub completed: Option<String>,
    pub next_steps: Option<String>,
    pub files_read: Option<String>,
    pub files_edited: Option<String>,
    pub notes: Option<String>,
    pub prompt_number: Option<i64>,
    pub discovery_tokens: i64,
    pub created_at: String,
    pub created_at_epoch: i64,
    pub merged_into_project: Option<String>,
}
