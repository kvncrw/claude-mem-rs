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

pub fn parse_observations(text: &str, _correlation_id: Option<&str>) -> Vec<ParsedObservation> {
    xml_blocks(text, "observation")
        .into_iter()
        .map(|content| {
            let observation_type = extract_field(&content, "type")
                .map(|value| value.trim().to_owned())
                .filter(|value| is_valid_observation_type(value))
                .unwrap_or_else(|| "discovery".to_owned());
            let concepts = extract_array_elements(&content, "concepts", "concept")
                .into_iter()
                .filter(|concept| concept != &observation_type)
                .collect();

            ParsedObservation {
                title: extract_field(&content, "title").unwrap_or_default(),
                narrative: extract_field(&content, "narrative"),
                facts: extract_array_elements(&content, "facts", "fact"),
                concepts,
                files_read: extract_array_elements(&content, "files_read", "file"),
                files_modified: extract_array_elements(&content, "files_modified", "file"),
                r#type: observation_type,
            }
        })
        .collect()
}

pub fn parse_summary(text: &str, _session_id: Option<&str>) -> Option<ParsedSummary> {
    if text.contains("<skip_summary") {
        return None;
    }

    let content = xml_blocks(text, "summary").into_iter().next()?;
    let summary = ParsedSummary {
        request: extract_field(&content, "request"),
        investigated: extract_field(&content, "investigated"),
        learned: extract_field(&content, "learned"),
        completed: extract_field(&content, "completed"),
        next_steps: extract_field(&content, "next_steps"),
        files_read: extract_field(&content, "files_read"),
        files_edited: extract_field(&content, "files_edited"),
        notes: extract_field(&content, "notes"),
    };

    if summary.request.is_none()
        && summary.investigated.is_none()
        && summary.learned.is_none()
        && summary.completed.is_none()
        && summary.next_steps.is_none()
    {
        return None;
    }

    Some(summary)
}

fn is_valid_observation_type(value: &str) -> bool {
    matches!(
        value,
        "discovery" | "bugfix" | "refactor" | "decision" | "implementation" | "constraint"
    )
}

fn xml_blocks(text: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut rest = text;
    let mut blocks = Vec::new();

    while let Some(start) = rest.find(&open) {
        let after_open = start + open.len();
        let Some(end_rel) = rest[after_open..].find(&close) else {
            break;
        };
        let end = after_open + end_rel;
        blocks.push(rest[after_open..end].to_owned());
        rest = &rest[end + close.len()..];
    }

    blocks
}

fn extract_field(content: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = content.find(&open)? + open.len();
    let end = content[start..].find(&close)? + start;
    let value = content[start..end].trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn extract_array_elements(content: &str, array_tag: &str, element_tag: &str) -> Vec<String> {
    let Some(array) = extract_field(content, array_tag) else {
        return Vec::new();
    };
    xml_blocks(&array, element_tag)
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect()
}
