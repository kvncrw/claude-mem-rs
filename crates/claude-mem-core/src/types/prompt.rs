use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserPromptRow {
    pub id: i64,
    pub content_session_id: String,
    pub prompt_number: i64,
    pub prompt_text: String,
    pub created_at: String,
    pub created_at_epoch: i64,
}
