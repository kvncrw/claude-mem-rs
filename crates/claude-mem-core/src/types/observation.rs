//! Observation row types.
//!
//! Port of `types.ts:204-221`. Two shapes: `ObservationInput` (what the SDK
//! submits) and `ObservationRow` (what's stored). The `id` is assigned by
//! SQLite on insert.

use serde::{Deserialize, Serialize};

/// Observation submission payload. Used by `store_observation`.
///
/// `Option<_>` fields default to `None` via `bon`'s implicit Option handling.
/// `String` fields auto-coerce from `&str` via `#[builder(on(String, into))]`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, bon::Builder)]
#[builder(on(String, into))]
pub struct ObservationInput {
    pub memory_session_id: String,
    pub project: String,
    pub r#type: String,
    pub text: Option<String>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub narrative: Option<String>,
    pub facts: Option<Vec<String>>,
    pub concepts: Option<Vec<String>>,
    pub files_read: Option<Vec<String>>,
    pub files_modified: Option<Vec<String>>,
    pub prompt_number: Option<i64>,
    pub discovery_tokens: Option<i64>,
    pub relevance_count: Option<i64>,
    pub created_at: String,
    pub created_at_epoch: i64,
    pub generated_by_model: Option<String>,
    pub merged_into_project: Option<String>,
    pub agent_type: Option<String>,
    pub agent_id: Option<String>,
    pub content_hash: Option<String>,
}

/// Stored observation. Same payload as `ObservationInput` plus the
/// row `id` SQLite assigned.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObservationRow {
    pub id: i64,
    pub memory_session_id: String,
    pub project: String,
    pub text: Option<String>,
    pub r#type: String,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub narrative: Option<String>,
    pub facts: Option<Vec<String>>,
    pub concepts: Option<Vec<String>>,
    pub files_read: Option<Vec<String>>,
    pub files_modified: Option<Vec<String>>,
    pub prompt_number: Option<i64>,
    pub discovery_tokens: i64,
    pub created_at: String,
    pub created_at_epoch: i64,
    pub generated_by_model: Option<String>,
    pub relevance_count: i64,
    pub merged_into_project: Option<String>,
    pub agent_type: Option<String>,
    pub agent_id: Option<String>,
    pub content_hash: Option<String>,
}
