use serde::{Deserialize, Serialize};

/// Flattened timeline row — observations + summaries + user prompts unioned
/// by `created_at_epoch`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimelineRow {
    pub kind: TimelineKind,
    pub id: i64,
    pub memory_session_id: Option<String>,
    pub content_session_id: Option<String>,
    pub project: String,
    pub title: Option<String>,
    pub text: Option<String>,
    pub created_at: String,
    pub created_at_epoch: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineKind {
    Observation,
    Summary,
    Prompt,
}
