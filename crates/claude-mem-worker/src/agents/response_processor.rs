//! Shared response processing for agent implementations.
//!
//! Port of `src/services/worker/agents/ResponseProcessor.ts`, scoped to the
//! Rust fork's active storage path: parse XML, store atomically, confirm queue
//! rows, broadcast optional events, and clean session state.

use claude_mem_core::db::pending_messages::PendingMessageStore;
use claude_mem_core::db::summaries::SummaryInput;
use claude_mem_core::db::transactions::{store_batch, BatchStoreResult};
use claude_mem_core::types::ObservationInput;
use rusqlite::Connection;
use thiserror::Error;

const FALLBACK_OBSERVATION_TYPE: &str = "discovery";
const VALID_OBSERVATION_TYPES: &[&str] = &[
    "discovery",
    "bugfix",
    "refactor",
    "decision",
    "implementation",
    "constraint",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActiveSession {
    pub session_db_id: i64,
    pub content_session_id: String,
    pub memory_session_id: Option<String>,
    pub project: String,
    pub platform_source: String,
    pub last_prompt_number: Option<i64>,
    pub earliest_pending_timestamp: Option<i64>,
    pub conversation_history: Vec<ConversationMessage>,
    pub processing_message_ids: Vec<i64>,
    pub last_generator_activity_epoch: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedObservation {
    pub r#type: String,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub facts: Vec<String>,
    pub narrative: Option<String>,
    pub concepts: Vec<String>,
    pub files_read: Vec<String>,
    pub files_modified: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSummary {
    pub request: Option<String>,
    pub investigated: Option<String>,
    pub learned: Option<String>,
    pub completed: Option<String>,
    pub next_steps: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationBroadcast {
    pub id: i64,
    pub memory_session_id: String,
    pub session_id: String,
    pub platform_source: String,
    pub r#type: String,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub narrative: Option<String>,
    pub facts: Vec<String>,
    pub concepts: Vec<String>,
    pub files_read: Vec<String>,
    pub files_modified: Vec<String>,
    pub project: String,
    pub prompt_number: Option<i64>,
    pub created_at_epoch: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryBroadcast {
    pub id: i64,
    pub session_id: String,
    pub platform_source: String,
    pub request: Option<String>,
    pub investigated: Option<String>,
    pub learned: Option<String>,
    pub completed: Option<String>,
    pub next_steps: Option<String>,
    pub notes: Option<String>,
    pub project: String,
    pub prompt_number: Option<i64>,
    pub created_at_epoch: i64,
}

pub trait ResponseBroadcaster {
    fn broadcast_observation(&self, observation: ObservationBroadcast);
    fn broadcast_summary(&self, summary: SummaryBroadcast);
    fn broadcast_processing_status(&self);
}

#[derive(Debug, Clone)]
pub struct ProcessAgentResponseOptions {
    pub discovery_tokens: Option<i64>,
    pub original_timestamp: Option<i64>,
    pub agent_name: String,
    pub model_id: Option<String>,
}

impl Default for ProcessAgentResponseOptions {
    fn default() -> Self {
        Self {
            discovery_tokens: None,
            original_timestamp: None,
            agent_name: "agent".to_owned(),
            model_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedAgentResponse {
    pub observations: Vec<ParsedObservation>,
    pub summary: Option<ParsedSummary>,
    pub storage: BatchStoreResult,
    pub discarded_non_xml: bool,
}

#[derive(Debug, Error)]
pub enum ResponseProcessorError {
    #[error("cannot store observations: memory_session_id not yet captured")]
    MissingMemorySessionId,
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
}

pub fn process_agent_response(
    conn: &Connection,
    text: &str,
    session: &mut ActiveSession,
    pending_store: &PendingMessageStore,
    broadcaster: Option<&dyn ResponseBroadcaster>,
    options: ProcessAgentResponseOptions,
) -> Result<ProcessedAgentResponse, ResponseProcessorError> {
    session.last_generator_activity_epoch = Some(now_ms());

    if !text.is_empty() {
        session.conversation_history.push(ConversationMessage {
            role: "assistant".to_owned(),
            content: text.to_owned(),
        });
    }

    let observations = parse_observations(text);
    let summary = parse_summary(text);
    let discarded_non_xml = !text.trim().is_empty()
        && observations.is_empty()
        && summary.is_none()
        && !text.contains("<observation>")
        && !text.contains("<summary>")
        && !text.contains("<skip_summary");

    let memory_session_id = session
        .memory_session_id
        .clone()
        .ok_or(ResponseProcessorError::MissingMemorySessionId)?;

    let observation_inputs = observations
        .iter()
        .map(parsed_observation_to_input)
        .collect::<Vec<_>>();
    let summary_input = summary
        .as_ref()
        .map(|summary| parsed_summary_to_input(summary, &memory_session_id, &session.project));

    let storage = store_batch(
        conn,
        &memory_session_id,
        &session.project,
        &observation_inputs,
        summary_input.as_ref(),
        session.last_prompt_number,
        options.discovery_tokens,
        options.original_timestamp,
    )?;

    for message_id in &session.processing_message_ids {
        pending_store.confirm_processed(conn, *message_id)?;
    }
    session.processing_message_ids.clear();

    if let Some(broadcaster) = broadcaster {
        broadcast_observations(
            broadcaster,
            &observations,
            &storage,
            session,
            &memory_session_id,
        );
        broadcast_summary(broadcaster, summary.as_ref(), &storage, session);
        broadcaster.broadcast_processing_status();
    }
    session.earliest_pending_timestamp = None;

    Ok(ProcessedAgentResponse {
        observations,
        summary,
        storage,
        discarded_non_xml,
    })
}

pub fn parse_observations(text: &str) -> Vec<ParsedObservation> {
    xml_blocks(text, "observation")
        .into_iter()
        .map(|content| {
            let raw_type = extract_field(&content, "type");
            let mut observation_type = raw_type
                .as_deref()
                .map(str::trim)
                .filter(|value| VALID_OBSERVATION_TYPES.contains(value))
                .unwrap_or(FALLBACK_OBSERVATION_TYPE)
                .to_owned();
            if observation_type.is_empty() {
                observation_type = FALLBACK_OBSERVATION_TYPE.to_owned();
            }

            let concepts = extract_array_elements(&content, "concepts", "concept")
                .into_iter()
                .filter(|concept| concept != &observation_type)
                .collect();

            ParsedObservation {
                r#type: observation_type,
                title: extract_field(&content, "title"),
                subtitle: extract_field(&content, "subtitle"),
                facts: extract_array_elements(&content, "facts", "fact"),
                narrative: extract_field(&content, "narrative"),
                concepts,
                files_read: extract_array_elements(&content, "files_read", "file"),
                files_modified: extract_array_elements(&content, "files_modified", "file"),
            }
        })
        .collect()
}

pub fn parse_summary(text: &str) -> Option<ParsedSummary> {
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

fn parsed_observation_to_input(obs: &ParsedObservation) -> ObservationInput {
    ObservationInput {
        r#type: obs.r#type.clone(),
        title: obs.title.clone(),
        subtitle: obs.subtitle.clone(),
        facts: Some(obs.facts.clone()),
        narrative: obs.narrative.clone(),
        concepts: Some(obs.concepts.clone()),
        files_read: Some(obs.files_read.clone()),
        files_modified: Some(obs.files_modified.clone()),
        ..Default::default()
    }
}

fn parsed_summary_to_input(
    summary: &ParsedSummary,
    memory_session_id: &str,
    project: &str,
) -> SummaryInput {
    SummaryInput {
        memory_session_id: memory_session_id.to_owned(),
        project: project.to_owned(),
        request: Some(summary.request.clone().unwrap_or_default()),
        investigated: Some(summary.investigated.clone().unwrap_or_default()),
        learned: Some(summary.learned.clone().unwrap_or_default()),
        completed: Some(summary.completed.clone().unwrap_or_default()),
        next_steps: Some(summary.next_steps.clone().unwrap_or_default()),
        notes: summary.notes.clone(),
        ..Default::default()
    }
}

fn broadcast_observations(
    broadcaster: &dyn ResponseBroadcaster,
    observations: &[ParsedObservation],
    storage: &BatchStoreResult,
    session: &ActiveSession,
    memory_session_id: &str,
) {
    for (index, observation) in observations.iter().enumerate() {
        let Some(id) = storage.observation_ids.get(index).copied() else {
            continue;
        };
        broadcaster.broadcast_observation(ObservationBroadcast {
            id,
            memory_session_id: memory_session_id.to_owned(),
            session_id: session.content_session_id.clone(),
            platform_source: session.platform_source.clone(),
            r#type: observation.r#type.clone(),
            title: observation.title.clone(),
            subtitle: observation.subtitle.clone(),
            narrative: observation.narrative.clone(),
            facts: observation.facts.clone(),
            concepts: observation.concepts.clone(),
            files_read: observation.files_read.clone(),
            files_modified: observation.files_modified.clone(),
            project: session.project.clone(),
            prompt_number: session.last_prompt_number,
            created_at_epoch: storage.created_at_epoch,
        });
    }
}

fn broadcast_summary(
    broadcaster: &dyn ResponseBroadcaster,
    summary: Option<&ParsedSummary>,
    storage: &BatchStoreResult,
    session: &ActiveSession,
) {
    let (Some(summary), Some(id)) = (summary, storage.summary_id) else {
        return;
    };

    broadcaster.broadcast_summary(SummaryBroadcast {
        id,
        session_id: session.content_session_id.clone(),
        platform_source: session.platform_source.clone(),
        request: summary.request.clone(),
        investigated: summary.investigated.clone(),
        learned: summary.learned.clone(),
        completed: summary.completed.clone(),
        next_steps: summary.next_steps.clone(),
        notes: summary.notes.clone(),
        project: session.project.clone(),
        prompt_number: session.last_prompt_number,
        created_at_epoch: storage.created_at_epoch,
    });
}

fn xml_blocks(text: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut rest = text;
    let mut blocks = Vec::new();

    while let Some(start) = rest.find(&open) {
        let content_start = start + open.len();
        let Some(end) = rest[content_start..].find(&close) else {
            break;
        };
        blocks.push(rest[content_start..content_start + end].to_owned());
        rest = &rest[content_start + end + close.len()..];
    }

    blocks
}

fn extract_field(content: &str, field_name: &str) -> Option<String> {
    let open = format!("<{field_name}>");
    let close = format!("</{field_name}>");
    let start = content.find(&open)? + open.len();
    let end = content[start..].find(&close)?;
    let value = content[start..start + end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

fn extract_array_elements(content: &str, array_name: &str, element_name: &str) -> Vec<String> {
    let Some(array_content) = extract_raw_block(content, array_name) else {
        return Vec::new();
    };
    xml_blocks(&array_content, element_name)
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect()
}

fn extract_raw_block(content: &str, field_name: &str) -> Option<String> {
    let open = format!("<{field_name}>");
    let close = format!("</{field_name}>");
    let start = content.find(&open)? + open.len();
    let end = content[start..].find(&close)?;
    Some(content[start..start + end].to_owned())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
