//! Optional Qdrant vector store.

use claude_mem_core::types::{ObservationRow, SessionSummaryRow, UserPromptRow};
use reqwest::StatusCode;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use crate::search::strategies::StrategySearchOptions;
use crate::search::vector::{embed_text, DEFAULT_EMBEDDING_DIM};

const DEFAULT_QDRANT_URL: &str = "http://127.0.0.1:6333";
const DEFAULT_COLLECTION: &str = "claude_mem_observations";
const SCHEMA_VERSION: i64 = 2;
const SUMMARY_ID_OFFSET: u64 = 1_000_000_000_000;
const PROMPT_ID_OFFSET: u64 = 2_000_000_000_000;

#[derive(Debug, Error)]
pub enum QdrantError {
    #[error("qdrant http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("qdrant returned {status}: {body}")]
    Status { status: StatusCode, body: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QdrantConfig {
    pub url: String,
    pub collection: String,
    pub api_key: Option<String>,
    pub vector_size: usize,
}

impl QdrantConfig {
    pub fn from_env_if_enabled() -> Option<Self> {
        let explicit_url = std::env::var("CLAUDE_MEM_QDRANT_URL").ok();
        let enabled = std::env::var("CLAUDE_MEM_QDRANT_ENABLED")
            .ok()
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"));
        if !enabled && explicit_url.is_none() {
            return None;
        }

        Some(Self {
            url: explicit_url.unwrap_or_else(|| DEFAULT_QDRANT_URL.to_owned()),
            collection: std::env::var("CLAUDE_MEM_QDRANT_COLLECTION")
                .unwrap_or_else(|_| DEFAULT_COLLECTION.to_owned()),
            api_key: std::env::var("CLAUDE_MEM_QDRANT_API_KEY").ok(),
            vector_size: std::env::var("CLAUDE_MEM_QDRANT_VECTOR_SIZE")
                .ok()
                .and_then(|value| value.parse().ok())
                .filter(|value| *value > 0)
                .unwrap_or(DEFAULT_EMBEDDING_DIM),
        })
    }
}

#[derive(Debug, Clone)]
pub struct QdrantClient {
    config: QdrantConfig,
    client: reqwest::Client,
}

impl QdrantClient {
    pub fn new(config: QdrantConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    pub fn from_env_if_enabled() -> Option<Self> {
        QdrantConfig::from_env_if_enabled().map(Self::new)
    }

    pub async fn ensure_collection(&self) -> Result<(), QdrantError> {
        let response = self
            .request(self.client.get(self.collection_url()))
            .send()
            .await?;
        match response.status() {
            StatusCode::OK => Ok(()),
            StatusCode::NOT_FOUND => self.create_collection().await,
            status => Err(QdrantError::Status {
                status,
                body: response.text().await.unwrap_or_default(),
            }),
        }
    }

    pub async fn upsert_observations(&self, rows: &[ObservationRow]) -> Result<(), QdrantError> {
        if rows.is_empty() {
            return Ok(());
        }
        self.ensure_collection().await?;
        let points = rows
            .iter()
            .map(|row| {
                json!({
                    "id": row.id as u64,
                    "vector": embed_text_with_config(&observation_text(row), self.config.vector_size),
                    "payload": {
                        "kind": "observation",
                        "schema_version": SCHEMA_VERSION,
                        "observation_id": row.id,
                        "project": row.project,
                        "type": row.r#type,
                        "title": row.title,
                        "created_at_epoch": row.created_at_epoch,
                        "concepts": row.concepts,
                        "files_read": row.files_read,
                        "files_modified": row.files_modified,
                    }
                })
            })
            .collect::<Vec<_>>();

        let response = self
            .request(
                self.client
                    .put(format!("{}/points?wait=true", self.collection_url())),
            )
            .json(&json!({ "points": points }))
            .send()
            .await?;
        expect_success(response).await
    }

    pub async fn upsert_memory_points(
        &self,
        observations: &[ObservationRow],
        summaries: &[SessionSummaryRow],
        prompts: &[PromptPoint],
    ) -> Result<(), QdrantError> {
        if observations.is_empty() && summaries.is_empty() && prompts.is_empty() {
            return Ok(());
        }
        self.ensure_collection().await?;
        let mut points = Vec::new();
        points.extend(observations.iter().map(|row| {
            json!({
                "id": row.id as u64,
                "vector": embed_text_with_config(&observation_text(row), self.config.vector_size),
                "payload": {
                    "kind": "observation",
                    "schema_version": SCHEMA_VERSION,
                    "observation_id": row.id,
                    "project": row.project,
                    "type": row.r#type,
                    "title": row.title,
                    "created_at_epoch": row.created_at_epoch,
                    "concepts": row.concepts,
                    "files_read": row.files_read,
                    "files_modified": row.files_modified,
                }
            })
        }));
        points.extend(summaries.iter().map(|row| {
            json!({
                "id": SUMMARY_ID_OFFSET + row.id as u64,
                "vector": embed_text_with_config(&summary_text(row), self.config.vector_size),
                "payload": {
                    "kind": "summary",
                    "schema_version": SCHEMA_VERSION,
                    "summary_id": row.id,
                    "project": row.project,
                    "created_at_epoch": row.created_at_epoch,
                    "memory_session_id": row.memory_session_id,
                }
            })
        }));
        points.extend(prompts.iter().map(|row| {
            json!({
                "id": PROMPT_ID_OFFSET + row.prompt.id as u64,
                "vector": embed_text_with_config(&row.prompt.prompt_text, self.config.vector_size),
                "payload": {
                    "kind": "prompt",
                    "schema_version": SCHEMA_VERSION,
                    "prompt_id": row.prompt.id,
                    "project": row.project,
                    "created_at_epoch": row.prompt.created_at_epoch,
                    "content_session_id": row.prompt.content_session_id,
                }
            })
        }));

        let response = self
            .request(
                self.client
                    .put(format!("{}/points?wait=true", self.collection_url())),
            )
            .json(&json!({ "points": points }))
            .send()
            .await?;
        expect_success(response).await
    }

    pub async fn search_observation_ids(
        &self,
        query: &str,
        limit: i64,
    ) -> Result<Vec<i64>, QdrantError> {
        self.ensure_collection().await?;
        let limit = limit.clamp(1, 100) as usize;
        let response = self
            .request(
                self.client
                    .post(format!("{}/points/search", self.collection_url())),
            )
            .json(&json!({
                "vector": embed_text_with_config(query, self.config.vector_size),
                "limit": limit,
                "with_payload": true,
            }))
            .send()
            .await?;
        let body = expect_json::<QdrantSearchResponse>(response).await?;
        Ok(body
            .result
            .into_iter()
            .filter_map(|point| point.observation_id())
            .collect())
    }

    pub async fn search_memory_refs(
        &self,
        query: &str,
        limit: i64,
    ) -> Result<Vec<MemoryPointRef>, QdrantError> {
        self.ensure_collection().await?;
        let limit = limit.clamp(1, 100) as usize;
        let response = self
            .request(
                self.client
                    .post(format!("{}/points/search", self.collection_url())),
            )
            .json(&json!({
                "vector": embed_text_with_config(query, self.config.vector_size),
                "limit": limit,
                "with_payload": true,
            }))
            .send()
            .await?;
        let body = expect_json::<QdrantSearchResponse>(response).await?;
        Ok(body
            .result
            .into_iter()
            .filter_map(|point| point.memory_ref())
            .collect())
    }

    fn collection_url(&self) -> String {
        format!(
            "{}/collections/{}",
            self.config.url.trim_end_matches('/'),
            self.config.collection
        )
    }

    async fn create_collection(&self) -> Result<(), QdrantError> {
        let response = self
            .request(self.client.put(self.collection_url()))
            .json(&json!({
                "vectors": {
                    "size": self.config.vector_size,
                    "distance": "Cosine"
                }
            }))
            .send()
            .await?;
        expect_success(response).await
    }

    fn request(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(api_key) = &self.config.api_key {
            request.header("api-key", api_key)
        } else {
            request
        }
    }
}

pub async fn search_observations(
    conn: &Connection,
    client: &QdrantClient,
    query: &str,
    options: &StrategySearchOptions,
) -> Result<Vec<ObservationRow>, QdrantError> {
    let limit = options.limit.unwrap_or(20).clamp(1, 100);
    let ids = client
        .search_observation_ids(query, (limit * 4).min(100))
        .await?;
    let rows = claude_mem_core::db::observations::get::get_observations_by_ids(conn, &ids)
        .unwrap_or_default()
        .into_iter()
        .filter(|row| matches_options(row, options))
        .take(limit as usize)
        .collect();
    Ok(rows)
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptPoint {
    pub prompt: UserPromptRow,
    pub project: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryPointRef {
    Observation(i64),
    Summary(i64),
    Prompt(i64),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryPointRefs {
    pub observations: Vec<i64>,
    pub summaries: Vec<i64>,
    pub prompts: Vec<i64>,
}

impl MemoryPointRefs {
    pub fn from_refs(refs: Vec<MemoryPointRef>) -> Self {
        let mut out = Self::default();
        for item in refs {
            match item {
                MemoryPointRef::Observation(id) => push_unique(&mut out.observations, id),
                MemoryPointRef::Summary(id) => push_unique(&mut out.summaries, id),
                MemoryPointRef::Prompt(id) => push_unique(&mut out.prompts, id),
            }
        }
        out
    }
}

fn matches_options(row: &ObservationRow, options: &StrategySearchOptions) -> bool {
    options
        .project
        .as_ref()
        .is_none_or(|project| row.project == *project)
        && options.date_range.as_ref().is_none_or(|range| {
            range
                .start_epoch
                .is_none_or(|start| row.created_at_epoch >= start)
                && range
                    .end_epoch
                    .is_none_or(|end| row.created_at_epoch <= end)
        })
        && (options.obs_type.is_empty() || options.obs_type.contains(&row.r#type))
        && (options.concepts.is_empty()
            || row.concepts.as_ref().is_some_and(|concepts| {
                options
                    .concepts
                    .iter()
                    .any(|concept| concepts.contains(concept))
            }))
}

fn observation_text(row: &ObservationRow) -> String {
    [
        row.title.as_deref(),
        row.subtitle.as_deref(),
        row.narrative.as_deref(),
        row.text.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
}

fn summary_text(row: &SessionSummaryRow) -> String {
    [
        row.request.as_deref(),
        row.investigated.as_deref(),
        row.learned.as_deref(),
        row.completed.as_deref(),
        row.next_steps.as_deref(),
        row.notes.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
}

fn push_unique(ids: &mut Vec<i64>, id: i64) {
    if !ids.contains(&id) {
        ids.push(id);
    }
}

fn embed_text_with_config(text: &str, vector_size: usize) -> Vec<f32> {
    if vector_size == DEFAULT_EMBEDDING_DIM {
        embed_text(text)
    } else {
        crate::search::vector::embed_text_with_dim(text, vector_size)
    }
}

async fn expect_success(response: reqwest::Response) -> Result<(), QdrantError> {
    if response.status().is_success() {
        Ok(())
    } else {
        Err(QdrantError::Status {
            status: response.status(),
            body: response.text().await.unwrap_or_default(),
        })
    }
}

async fn expect_json<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
) -> Result<T, QdrantError> {
    if response.status().is_success() {
        Ok(response.json::<T>().await?)
    } else {
        Err(QdrantError::Status {
            status: response.status(),
            body: response.text().await.unwrap_or_default(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct QdrantSearchResponse {
    result: Vec<QdrantPoint>,
}

#[derive(Debug, Deserialize)]
struct QdrantPoint {
    id: Value,
    payload: Option<Value>,
}

impl QdrantPoint {
    fn observation_id(&self) -> Option<i64> {
        self.payload
            .as_ref()
            .and_then(|payload| payload.get("observation_id"))
            .and_then(Value::as_i64)
            .or_else(|| self.id.as_i64())
    }

    fn memory_ref(&self) -> Option<MemoryPointRef> {
        let payload = self.payload.as_ref()?;
        match payload.get("kind").and_then(Value::as_str) {
            Some("observation") => payload
                .get("observation_id")
                .and_then(Value::as_i64)
                .or_else(|| self.id.as_i64())
                .map(MemoryPointRef::Observation),
            Some("summary") => payload
                .get("summary_id")
                .and_then(Value::as_i64)
                .map(MemoryPointRef::Summary),
            Some("prompt") => payload
                .get("prompt_id")
                .and_then(Value::as_i64)
                .map(MemoryPointRef::Prompt),
            _ => self.id.as_u64().and_then(|id| {
                if id >= PROMPT_ID_OFFSET {
                    Some(MemoryPointRef::Prompt((id - PROMPT_ID_OFFSET) as i64))
                } else if id >= SUMMARY_ID_OFFSET {
                    Some(MemoryPointRef::Summary((id - SUMMARY_ID_OFFSET) as i64))
                } else {
                    Some(MemoryPointRef::Observation(id as i64))
                }
            }),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct QdrantStatus {
    pub enabled: bool,
    pub url: String,
    pub collection: String,
    pub vector_size: usize,
}

impl From<&QdrantConfig> for QdrantStatus {
    fn from(config: &QdrantConfig) -> Self {
        Self {
            enabled: true,
            url: config.url.clone(),
            collection: config.collection.clone(),
            vector_size: config.vector_size,
        }
    }
}
