//! Observation / summary parser (port of `sdk/parser.ts`).

use serde::{Deserialize, Serialize};

/// A parsed observation from LLM output text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParsedObservation {
    pub title: String,
    pub narrative: Option<String>,
    pub facts: Vec<String>,
    pub concepts: Vec<String>,
    pub files_read: Vec<String>,
    pub files_modified: Vec<String>,
    pub r#type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParsedSummary {
    pub request: Option<String>,
    pub investigated: Option<String>,
    pub learned: Option<String>,
    pub completed: Option<String>,
    pub next_steps: Option<String>,
    pub files_read: Option<String>,
    pub files_edited: Option<String>,
    pub notes: Option<String>,
}

pub fn parse_observations(_text: &str, _correlation_id: Option<&str>) -> Vec<ParsedObservation> {
    Vec::new()
}

pub fn parse_summary(_text: &str, _session_id: Option<&str>) -> ParsedSummary {
    ParsedSummary {
        request: None,
        investigated: None,
        learned: None,
        completed: None,
        next_steps: None,
        files_read: None,
        files_edited: None,
        notes: None,
    }
}
