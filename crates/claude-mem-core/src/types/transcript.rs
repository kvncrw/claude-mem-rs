//! Claude Code JSONL model types. Port of `types/transcript.ts`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageInfo {
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_read_input_tokens: i64,
    #[serde(default)]
    pub cache_creation_input_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentItem {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: serde_json::Value,
        #[serde(default)]
        is_error: Option<bool>,
    },
    #[serde(rename = "image")]
    Image { source: serde_json::Value },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserTranscriptEntry {
    #[serde(rename = "type")]
    pub typ: String,
    pub message: UserMessage,
    pub timestamp: String,
    #[serde(rename = "sessionUuid")]
    pub session_uuid: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserMessage {
    pub role: String,
    pub content: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantTranscriptEntry {
    #[serde(rename = "type")]
    pub typ: String,
    pub message: AssistantMessage,
    pub timestamp: String,
    #[serde(rename = "sessionUuid")]
    pub session_uuid: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub role: String,
    pub content: Vec<ContentItem>,
    pub usage: Option<UsageInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TranscriptEntry {
    #[serde(rename = "user")]
    User(UserTranscriptEntry),
    #[serde(rename = "assistant")]
    Assistant(AssistantTranscriptEntry),
    #[serde(rename = "summary")]
    Summary(SummaryTranscriptEntry),
    #[serde(rename = "system")]
    System(SystemTranscriptEntry),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryTranscriptEntry {
    #[serde(rename = "type")]
    pub typ: String,
    #[serde(rename = "summary")]
    pub summary: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemTranscriptEntry {
    #[serde(rename = "type")]
    pub typ: String,
    pub cwd: String,
    pub tools: Vec<String>,
    pub timestamp: String,
}
