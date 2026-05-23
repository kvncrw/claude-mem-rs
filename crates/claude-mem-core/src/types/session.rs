//! `sdk_sessions` row type.
//!
//! Dual ID: `content_session_id` is the user-visible id (immutable),
//! `memory_session_id` is NULL at create and populated async before any
//! observation insert.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SdkSessionRow {
    pub id: i64,
    pub content_session_id: String,
    pub memory_session_id: Option<String>,
    pub project: String,
    pub user_prompt: Option<String>,
    pub started_at: String,
    pub started_at_epoch: i64,
    pub completed_at: Option<String>,
    pub completed_at_epoch: Option<i64>,
    pub status: String,
    pub worker_port: Option<i64>,
    pub prompt_counter: i64,
    pub custom_title: Option<String>,
    pub platform_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, bon::Builder)]
#[builder(on(String, into))]
pub struct CreateSessionInput {
    pub content_session_id: String,
    pub project: String,
    pub user_prompt: Option<String>,
    pub started_at: String,
    pub started_at_epoch: i64,
}
