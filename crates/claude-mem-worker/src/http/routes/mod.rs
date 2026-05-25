use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use claude_mem_core::context::formatters::{format_observation, FormatOptions};
use claude_mem_core::context::observation_compiler::{query_observations, ObservationQuery};
use claude_mem_core::db::observations::get::{
    get_observation_by_id, get_observations_by_file_path, get_observations_by_ids,
    get_observations_for_session,
};
use claude_mem_core::db::pending_messages::{EnqueueInput, PendingMessageStore};
use claude_mem_core::db::prompts::{
    get_latest_user_prompt, get_prompt_number_from_user_prompts, get_user_prompts_by_ids,
    save_user_prompt, PromptInput,
};
use claude_mem_core::db::sessions::{
    create_session, get_session_by_content_id, get_session_by_memory_id, mark_session_completed,
    update_memory_session_id,
};
use claude_mem_core::db::summaries::{
    get_summaries_by_ids, get_summary_for_session, store_summary, SummaryInput,
};
use claude_mem_core::db::transactions::store_batch;
use claude_mem_core::shared::tag_stripping::strip_private_tags;
use claude_mem_core::types::session::CreateSessionInput;
use claude_mem_core::types::{
    CorpusFile, CorpusFilter, ObservationInput, ObservationRow, SdkSessionRow, SessionSummaryRow,
    UserPromptRow,
};
use include_dir::{include_dir, Dir};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use super::router::{default_db_path, AppState};
use crate::agents::observer::{
    process_all_pending, process_pending_message, process_session_init, ObserverConfig,
    QueueProcessStats,
};
use crate::agents::response_processor::parse_summary;
use crate::knowledge::{CorpusBuilder, CorpusStore, CorpusStoreError};
#[cfg(feature = "knowledge-agent")]
use crate::knowledge::{KnowledgeAgent, KnowledgeAgentError};
#[cfg(feature = "qdrant")]
use crate::search::qdrant::{
    MemoryPointRefs, PromptPoint, QdrantClient, QdrantConfig, QdrantStatus,
};
use crate::search::result_formatter::{ResultFormatter, SearchResults};
use crate::search::strategies::{
    DateRange, OrderBy, SearchStrategyHint, SearchType, SqliteSearchStrategy, StrategySearchOptions,
};

type ApiResult<T> = Result<Json<T>, ApiError>;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    status: &'static str,
    initialized: bool,
    #[serde(rename = "mcpReady")]
    mcp_ready: bool,
    platform: &'static str,
    pid: u32,
}

pub async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        initialized: state.initialized,
        mcp_ready: state.mcp_ready,
        platform: std::env::consts::OS,
        pid: std::process::id(),
    })
}

pub async fn readiness(State(state): State<AppState>) -> impl IntoResponse {
    if state.initialized {
        (
            StatusCode::OK,
            Json(json!({ "status": "ready", "mcpReady": state.mcp_ready })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "initializing",
                "mcpReady": state.mcp_ready,
                "message": "Worker is still initializing"
            })),
        )
    }
}

pub async fn version() -> Json<Value> {
    Json(json!({ "version": env!("CARGO_PKG_VERSION") }))
}

pub async fn instructions() -> Json<Value> {
    Json(json!({
        "name": "claude-mem-rs",
        "version": env!("CARGO_PKG_VERSION"),
        "worker": "native Rust worker",
        "memoryLifecycle": {
            "sessionInit": "/api/sessions/init",
            "observation": "/api/sessions/observations",
            "summarize": "/api/sessions/summarize",
            "complete": "/api/sessions/complete",
            "context": "/api/context/inject"
        }
    }))
}

static DASHBOARD_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../apps/dashboard/out");

pub async fn root_viewer() -> Response {
    dashboard_asset_response("index.html")
}

pub async fn dashboard_asset(Path(path): Path<String>) -> Response {
    dashboard_asset_response(&format!("_next/{path}"))
}

fn dashboard_asset_response(path: &str) -> Response {
    let clean_path = path.trim_start_matches('/');
    if clean_path.is_empty()
        || clean_path.contains("..")
        || clean_path.split('/').any(|part| part.starts_with('.'))
    {
        return StatusCode::NOT_FOUND.into_response();
    }

    let Some(file) = DASHBOARD_DIR.get_file(clean_path) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, dashboard_content_type(clean_path))
        .body(Body::from(file.contents().to_vec()))
        .unwrap_or_else(|error| ApiError::internal(error).into_response())
}

fn dashboard_content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

pub async fn stream(State(state): State<AppState>) -> impl IntoResponse {
    let payload = match snapshot(&state, 10) {
        Ok(value) => value,
        Err(error) => json!({ "error": error.message }),
    };
    let initial = tokio_stream::once(Ok::<_, Infallible>(
        Event::default()
            .event("initial_load")
            .data(payload.to_string()),
    ));
    let live = BroadcastStream::new(state.events.subscribe()).filter_map(|message| match message {
        Ok(event) => Some(Ok(Event::default()
            .event(event.event)
            .data(event.data.to_string()))),
        Err(BroadcastStreamRecvError::Lagged(skipped)) => Some(Ok(Event::default()
            .event("stream_lagged")
            .data(json!({ "skipped": skipped }).to_string()))),
    });

    Sse::new(initial.chain(live)).keep_alive(KeepAlive::default())
}

pub async fn admin_shutdown(State(state): State<AppState>) -> Json<Value> {
    if let Some(shutdown) = &state.shutdown {
        shutdown.notify_waiters();
    }
    Json(json!({ "success": true }))
}

pub async fn admin_restart(State(state): State<AppState>) -> Json<Value> {
    if let Some(shutdown) = &state.shutdown {
        shutdown.notify_waiters();
    }
    Json(json!({
        "success": true,
        "message": "Worker shutdown requested; external supervisor is responsible for restart"
    }))
}

pub async fn admin_doctor(State(state): State<AppState>) -> ApiResult<Value> {
    let (stats, activity) = {
        let conn = state.conn.lock().unwrap();
        (db_stats_locked(&conn)?, db_activity_locked(&conn)?)
    };
    let observer = ObserverConfig::from_env();
    let db_path = default_db_path();
    Ok(Json(json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "platform": std::env::consts::OS,
        "dbPath": db_path,
        "dbReachable": true,
        "initialized": state.initialized,
        "mcpReady": state.mcp_ready,
        "counts": stats,
        "activity": activity,
        "qdrant": {
            "compiled": cfg!(feature = "qdrant"),
            "enabled": qdrant_enabled_env()
        },
        "observer": {
            "provider": observer.provider,
            "model": observer.model_id,
            "tierRoutingEnabled": observer.tier_routing_enabled,
            "simpleModel": observer.simple_model,
            "summaryModel": observer.summary_model,
            "maxMessages": observer.max_messages
        },
        "settingsPath": settings_path(),
        "logPath": log_path()
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInitRequest {
    content_session_id: String,
    project: Option<String>,
    prompt: Option<String>,
    #[serde(rename = "platformSource")]
    _platform_source: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInitResponse {
    session_db_id: i64,
    prompt_number: i64,
    skipped: bool,
    reason: Option<&'static str>,
    context_injected: bool,
}

pub async fn sessions_init(
    State(state): State<AppState>,
    Json(req): Json<SessionInitRequest>,
) -> ApiResult<SessionInitResponse> {
    if req.content_session_id.trim().is_empty() {
        return Err(ApiError::bad_request("contentSessionId is required"));
    }

    let project = req.project.unwrap_or_else(|| "unknown".into());
    let prompt = req.prompt.unwrap_or_else(|| "[media prompt]".into());
    let cleaned_prompt = strip_private_tags(&prompt).trim().to_owned();
    let (created_at, created_at_epoch) = now_timestamp();

    let (session_db_id, prompt_number, context_injected, prompt_id) = {
        let conn = state.conn.lock().unwrap();
        create_session(
            &conn,
            &CreateSessionInput {
                content_session_id: req.content_session_id.clone(),
                project,
                user_prompt: Some(prompt),
                started_at: created_at.clone(),
                started_at_epoch: created_at_epoch,
            },
        )
        .map_err(ApiError::internal)?;

        let session = get_session_by_content_id(&conn, &req.content_session_id)
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::internal("session was not created"))?;
        let prompt_number = get_prompt_number_from_user_prompts(&conn, &req.content_session_id)
            .map_err(ApiError::internal)?
            + 1;

        if cleaned_prompt.is_empty() {
            return Ok(Json(SessionInitResponse {
                session_db_id: session.id,
                prompt_number,
                skipped: true,
                reason: Some("private"),
                context_injected: false,
            }));
        }

        let prompt_id = save_user_prompt(
            &conn,
            &PromptInput {
                content_session_id: req.content_session_id.clone(),
                prompt_number,
                prompt_text: cleaned_prompt,
                created_at,
                created_at_epoch,
            },
        )
        .map_err(ApiError::internal)?;

        (
            session.id,
            prompt_number,
            session.memory_session_id.is_some(),
            prompt_id,
        )
    };
    index_prompt_ids_if_enabled(&state, &[prompt_id]).await;

    match process_session_init(
        Arc::clone(&state.conn),
        &req.content_session_id,
        ObserverConfig::from_env(),
    )
    .await
    {
        Ok(stats) => {
            index_observation_ids_if_enabled(&state, &stats.observation_ids).await;
        }
        Err(error) => {
            tracing::warn!(%error, "observer init processing failed");
        }
    }

    state.publish(
        "session_initialized",
        json!({
            "contentSessionId": req.content_session_id,
            "sessionDbId": session_db_id,
            "promptNumber": prompt_number,
            "contextInjected": context_injected
        }),
    );

    Ok(Json(SessionInitResponse {
        session_db_id,
        prompt_number,
        skipped: false,
        reason: None,
        context_injected,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionObservationRequest {
    content_session_id: String,
    #[serde(alias = "tool_name")]
    tool_name: Option<String>,
    #[serde(alias = "tool_input")]
    tool_input: Option<Value>,
    #[serde(alias = "tool_response")]
    tool_response: Option<Value>,
    cwd: Option<String>,
    #[serde(rename = "platformSource")]
    _platform_source: Option<String>,
}

pub async fn sessions_observations(
    State(state): State<AppState>,
    Json(req): Json<SessionObservationRequest>,
) -> ApiResult<Value> {
    if req.content_session_id.trim().is_empty() {
        return Err(ApiError::bad_request("contentSessionId is required"));
    }
    let Some(tool_name) = req.tool_name.as_deref().filter(|s| !s.trim().is_empty()) else {
        return Ok(Json(
            json!({ "success": true, "skipped": true, "reason": "missing_tool_name" }),
        ));
    };
    if should_skip_tool(tool_name) {
        return Ok(Json(
            json!({ "success": true, "skipped": true, "reason": "skip_tool" }),
        ));
    }
    let (message_id, session_db_id) = {
        let conn = state.conn.lock().unwrap();
        let session = match get_session_by_content_id(&conn, &req.content_session_id)
            .map_err(ApiError::internal)?
        {
            Some(session) => session,
            None => {
                let (created_at, created_at_epoch) = now_timestamp();
                let project = req
                    .cwd
                    .as_deref()
                    .map(project_from_path)
                    .unwrap_or_else(|| "unknown".into());
                create_session(
                    &conn,
                    &CreateSessionInput {
                        content_session_id: req.content_session_id.clone(),
                        project,
                        user_prompt: Some(String::new()),
                        started_at: created_at,
                        started_at_epoch: created_at_epoch,
                    },
                )
                .map_err(ApiError::internal)?;
                get_session_by_content_id(&conn, &req.content_session_id)
                    .map_err(ApiError::internal)?
                    .ok_or_else(|| ApiError::internal("session was not created"))?
            }
        };
        let prompt_number = get_prompt_number_from_user_prompts(&conn, &req.content_session_id)
            .map_err(ApiError::internal)?;
        let pending_store = PendingMessageStore::default();
        let message_id = pending_store
            .enqueue(
                &conn,
                &EnqueueInput {
                    session_db_id: session.id,
                    content_session_id: session.content_session_id.clone(),
                    message_type: "observation".into(),
                    tool_name: Some(tool_name.to_owned()),
                    tool_input: req.tool_input.clone(),
                    tool_response: req.tool_response.clone(),
                    cwd: req.cwd.clone(),
                    prompt_number: Some(prompt_number),
                    created_at_epoch: now_timestamp().1,
                    ..Default::default()
                },
            )
            .map_err(ApiError::internal)?;
        (message_id, session.id)
    };

    let stats = match process_pending_message(
        Arc::clone(&state.conn),
        session_db_id,
        message_id,
        ObserverConfig::from_env(),
    )
    .await
    {
        Ok(stats) => {
            index_observation_ids_if_enabled(&state, &stats.observation_ids).await;
            stats
        }
        Err(error) => {
            tracing::warn!(%error, session_db_id, message_id, "observer observation processing failed");
            QueueProcessStats {
                messages_failed: 1,
                ..Default::default()
            }
        }
    };
    let processed = observer_stats_json(&stats);
    state.publish(
        "observation_processed",
        json!({
            "contentSessionId": req.content_session_id,
            "messageId": message_id,
            "sessionDbId": session_db_id,
            "inserted": stats.observations_inserted,
            "observationIds": stats.observation_ids,
            "processed": processed
        }),
    );

    Ok(Json(json!({
        "success": true,
        "status": "queued",
        "messageId": message_id,
        "sessionDbId": session_db_id,
        "inserted": stats.observations_inserted,
        "observationIds": stats.observation_ids.clone(),
        "processed": processed
    })))
}

fn project_from_path(path: &str) -> String {
    PathBuf::from(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
        .to_owned()
}

fn should_skip_tool(tool_name: &str) -> bool {
    configured_skip_tools()
        .iter()
        .any(|name| name.eq_ignore_ascii_case(tool_name.trim()))
}

fn configured_skip_tools() -> Vec<String> {
    std::env::var("CLAUDE_MEM_SKIP_TOOLS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

fn observer_stats_json(stats: &QueueProcessStats) -> Value {
    json!({
        "totalPendingSessions": stats.total_pending_sessions,
        "sessionsStarted": stats.sessions_started,
        "sessionsSkipped": stats.sessions_skipped,
        "startedSessionIds": stats.started_session_ids,
        "messagesProcessed": stats.messages_processed,
        "messagesFailed": stats.messages_failed,
        "observationsInserted": stats.observations_inserted,
        "summariesInserted": stats.summaries_inserted,
        "observationIds": stats.observation_ids
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompleteRequest {
    content_session_id: String,
    #[serde(rename = "platformSource")]
    _platform_source: Option<String>,
}

pub async fn sessions_complete(
    State(state): State<AppState>,
    Json(req): Json<SessionCompleteRequest>,
) -> ApiResult<Value> {
    let mut summary_id = None;
    let mut summary_status = "skipped_no_memory_session";
    let completed = {
        let conn = state.conn.lock().unwrap();
        let Some(session) = get_session_by_content_id(&conn, &req.content_session_id)
            .map_err(ApiError::internal)?
        else {
            return Ok(Json(json!({
                "success": true,
                "completed": false,
                "summaryId": null,
                "summaryStatus": "session_missing"
            })));
        };
        mark_session_completed(&conn, session.id).map_err(ApiError::internal)?;
        if let Some(memory_session_id) = session.memory_session_id.as_deref() {
            let existing =
                get_summary_for_session(&conn, memory_session_id).map_err(ApiError::internal)?;
            if existing.is_empty() {
                summary_id = Some(store_generated_summary(
                    &conn,
                    &req.content_session_id,
                    None,
                )?);
                summary_status = "created";
            } else {
                summary_status = "already_exists";
            }
        }
        true
    };
    if let Some(id) = summary_id {
        index_summary_ids_if_enabled(&state, &[id]).await;
    }
    state.publish(
        "session_completed",
        json!({
            "contentSessionId": req.content_session_id,
            "completed": completed,
            "summaryId": summary_id,
            "summaryStatus": summary_status
        }),
    );
    Ok(Json(json!({
        "success": true,
        "completed": completed,
        "summaryId": summary_id,
        "summaryStatus": summary_status
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummarizeRequest {
    content_session_id: String,
    summary: Option<String>,
    last_assistant_message: Option<String>,
}

pub async fn sessions_summarize(
    State(state): State<AppState>,
    Json(req): Json<SessionSummarizeRequest>,
) -> ApiResult<Value> {
    if req.content_session_id.trim().is_empty() {
        return Err(ApiError::bad_request("contentSessionId is required"));
    }
    let source = req.summary.or(req.last_assistant_message);
    if source
        .as_deref()
        .map(|value| value.contains("<summary>") || value.contains("<skip_summary"))
        .unwrap_or(false)
    {
        let id = {
            let conn = state.conn.lock().unwrap();
            store_generated_summary(&conn, &req.content_session_id, source.as_deref())?
        };
        index_summary_ids_if_enabled(&state, &[id]).await;
        state.publish(
            "summary_stored",
            json!({
                "contentSessionId": req.content_session_id,
                "summaryId": id,
                "status": "stored"
            }),
        );
        return Ok(Json(
            json!({ "success": true, "summaryId": id, "status": "stored" }),
        ));
    }

    let (message_id, session_db_id) = {
        let conn = state.conn.lock().unwrap();
        let session = match get_session_by_content_id(&conn, &req.content_session_id)
            .map_err(ApiError::internal)?
        {
            Some(session) => session,
            None => {
                let (created_at, created_at_epoch) = now_timestamp();
                create_session(
                    &conn,
                    &CreateSessionInput {
                        content_session_id: req.content_session_id.clone(),
                        project: "unknown".into(),
                        user_prompt: Some(String::new()),
                        started_at: created_at,
                        started_at_epoch: created_at_epoch,
                    },
                )
                .map_err(ApiError::internal)?;
                get_session_by_content_id(&conn, &req.content_session_id)
                    .map_err(ApiError::internal)?
                    .ok_or_else(|| ApiError::internal("session was not created"))?
            }
        };
        let pending_store = PendingMessageStore::default();
        let id = pending_store
            .enqueue(
                &conn,
                &EnqueueInput {
                    session_db_id: session.id,
                    content_session_id: session.content_session_id,
                    message_type: "summarize".into(),
                    last_assistant_message: source,
                    created_at_epoch: now_timestamp().1,
                    ..Default::default()
                },
            )
            .map_err(ApiError::internal)?;
        (id, session.id)
    };

    let stats = match process_pending_message(
        Arc::clone(&state.conn),
        session_db_id,
        message_id,
        ObserverConfig::from_env(),
    )
    .await
    {
        Ok(stats) => stats,
        Err(error) => {
            tracing::warn!(%error, session_db_id, message_id, "observer summarize processing failed");
            QueueProcessStats {
                messages_failed: 1,
                ..Default::default()
            }
        }
    };
    let processed = observer_stats_json(&stats);
    state.publish(
        "summary_processed",
        json!({
            "contentSessionId": req.content_session_id,
            "messageId": message_id,
            "sessionDbId": session_db_id,
            "processed": processed
        }),
    );

    Ok(Json(json!({
        "success": true,
        "status": "queued",
        "messageId": message_id,
        "sessionDbId": session_db_id,
        "processed": processed
    })))
}

pub async fn sessions_status(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let content_session_id = query
        .get("contentSessionId")
        .or_else(|| query.get("content_session_id"))
        .ok_or_else(|| ApiError::bad_request("contentSessionId is required"))?;
    let conn = state.conn.lock().unwrap();
    let Some(session) =
        get_session_by_content_id(&conn, content_session_id).map_err(ApiError::internal)?
    else {
        return Ok(Json(json!({ "exists": false })));
    };
    let summaries = if let Some(memory_session_id) = session.memory_session_id.as_deref() {
        get_summary_for_session(&conn, memory_session_id).map_err(ApiError::internal)?
    } else {
        Vec::new()
    };
    let queue_length = count_pending_for_session(&conn, session.id)?;
    Ok(Json(json!({
        "exists": true,
        "session": session,
        "queueLength": queue_length,
        "summaryCount": summaries.len(),
        "hasSummary": !summaries.is_empty()
    })))
}

pub async fn session_legacy_init(
    State(state): State<AppState>,
    Path(session_db_id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let session = get_session_by_id_locked(&conn, session_db_id)?
        .ok_or_else(|| ApiError::bad_request("sessionDbId was not found"))?;
    let prompt = body
        .get("userPrompt")
        .or_else(|| body.get("user_prompt"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(session.user_prompt.clone())
        .unwrap_or_else(|| "[media prompt]".into());
    let prompt_number = body
        .get("promptNumber")
        .or_else(|| body.get("prompt_number"))
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            get_prompt_number_from_user_prompts(&conn, &session.content_session_id).unwrap_or(0) + 1
        });
    let (created_at, created_at_epoch) = now_timestamp();
    save_user_prompt(
        &conn,
        &PromptInput {
            content_session_id: session.content_session_id.clone(),
            prompt_number,
            prompt_text: strip_private_tags(&prompt).trim().to_owned(),
            created_at,
            created_at_epoch,
        },
    )
    .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "status": "initialized",
        "sessionDbId": session_db_id,
        "promptNumber": prompt_number
    })))
}

pub async fn session_legacy_observations(
    State(state): State<AppState>,
    Path(session_db_id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    let id = {
        let conn = state.conn.lock().unwrap();
        let session = get_session_by_id_locked(&conn, session_db_id)?
            .ok_or_else(|| ApiError::bad_request("sessionDbId was not found"))?;
        let pending_store = PendingMessageStore::default();
        pending_store
            .enqueue(
                &conn,
                &EnqueueInput {
                    session_db_id,
                    content_session_id: session.content_session_id,
                    message_type: "observation".into(),
                    tool_name: body
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    tool_input: body.get("tool_input").cloned(),
                    tool_response: body.get("tool_response").cloned(),
                    cwd: body.get("cwd").and_then(Value::as_str).map(str::to_owned),
                    prompt_number: body
                        .get("prompt_number")
                        .or_else(|| body.get("promptNumber"))
                        .and_then(Value::as_i64),
                    created_at_epoch: now_timestamp().1,
                    ..Default::default()
                },
            )
            .map_err(ApiError::internal)?
    };
    let stats = process_pending_message(
        Arc::clone(&state.conn),
        session_db_id,
        id,
        ObserverConfig::from_env(),
    )
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, session_db_id, id, "legacy observer processing failed");
        QueueProcessStats {
            messages_failed: 1,
            ..Default::default()
        }
    });
    index_observation_ids_if_enabled(&state, &stats.observation_ids).await;
    state.publish(
        "observation_processed",
        json!({
            "sessionDbId": session_db_id,
            "messageId": id,
            "processed": observer_stats_json(&stats)
        }),
    );
    Ok(Json(
        json!({ "status": "queued", "messageId": id, "processed": observer_stats_json(&stats) }),
    ))
}

pub async fn session_legacy_summarize(
    State(state): State<AppState>,
    Path(session_db_id): Path<i64>,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    let id = {
        let conn = state.conn.lock().unwrap();
        let session = get_session_by_id_locked(&conn, session_db_id)?
            .ok_or_else(|| ApiError::bad_request("sessionDbId was not found"))?;
        let pending_store = PendingMessageStore::default();
        pending_store
            .enqueue(
                &conn,
                &EnqueueInput {
                    session_db_id,
                    content_session_id: session.content_session_id,
                    message_type: "summarize".into(),
                    last_assistant_message: body
                        .get("last_assistant_message")
                        .or_else(|| body.get("lastAssistantMessage"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    created_at_epoch: now_timestamp().1,
                    ..Default::default()
                },
            )
            .map_err(ApiError::internal)?
    };
    let stats = process_pending_message(
        Arc::clone(&state.conn),
        session_db_id,
        id,
        ObserverConfig::from_env(),
    )
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, session_db_id, id, "legacy summarize processing failed");
        QueueProcessStats {
            messages_failed: 1,
            ..Default::default()
        }
    });
    state.publish(
        "summary_processed",
        json!({
            "sessionDbId": session_db_id,
            "messageId": id,
            "processed": observer_stats_json(&stats)
        }),
    );
    Ok(Json(
        json!({ "status": "queued", "messageId": id, "processed": observer_stats_json(&stats) }),
    ))
}

pub async fn session_legacy_status(
    State(state): State<AppState>,
    Path(session_db_id): Path<i64>,
) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let Some(session) = get_session_by_id_locked(&conn, session_db_id)? else {
        return Ok(Json(json!({ "status": "not_found", "queueLength": 0 })));
    };
    let queue_length = count_pending_for_session(&conn, session_db_id)?;
    Ok(Json(json!({
        "status": session.status,
        "sessionDbId": session_db_id,
        "project": session.project,
        "queueLength": queue_length
    })))
}

pub async fn session_legacy_delete(
    State(state): State<AppState>,
    Path(session_db_id): Path<i64>,
) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    mark_session_completed(&conn, session_db_id).map_err(ApiError::internal)?;
    Ok(Json(
        json!({ "status": "deleted", "sessionDbId": session_db_id }),
    ))
}

pub async fn session_legacy_complete(
    State(state): State<AppState>,
    Path(session_db_id): Path<i64>,
) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    mark_session_completed(&conn, session_db_id).map_err(ApiError::internal)?;
    Ok(Json(
        json!({ "success": true, "sessionDbId": session_db_id }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct MemorySaveRequest {
    text: String,
    title: Option<String>,
    project: Option<String>,
}

pub async fn memory_save(
    State(state): State<AppState>,
    Json(req): Json<MemorySaveRequest>,
) -> ApiResult<Value> {
    let text = strip_private_tags(&req.text).trim().to_owned();
    if text.is_empty() {
        return Err(ApiError::bad_request(
            "text is required and must be non-empty",
        ));
    }
    let project = req.project.unwrap_or_else(|| "manual".into());
    let content_session_id = format!("manual:{}", project);
    let memory_session_id = format!("manual-memory:{}", project);
    let (created_at, created_at_epoch) = now_timestamp();

    let title = req
        .title
        .unwrap_or_else(|| text.chars().take(60).collect::<String>());
    let result = {
        let conn = state.conn.lock().unwrap();
        create_session(
            &conn,
            &CreateSessionInput {
                content_session_id: content_session_id.clone(),
                project: project.clone(),
                user_prompt: Some("Manual memory".into()),
                started_at: created_at.clone(),
                started_at_epoch: created_at_epoch,
            },
        )
        .map_err(ApiError::internal)?;
        update_memory_session_id(&conn, &content_session_id, &memory_session_id)
            .map_err(ApiError::internal)?;

        let observation = ObservationInput {
            r#type: "discovery".into(),
            title: Some(title.clone()),
            subtitle: Some("Manual memory".into()),
            narrative: Some(text),
            created_at,
            created_at_epoch,
            generated_by_model: Some("manual".into()),
            ..Default::default()
        };
        store_batch(
            &conn,
            &memory_session_id,
            &project,
            &[observation],
            None,
            Some(0),
            Some(0),
            Some(created_at_epoch),
        )
        .map_err(ApiError::internal)?
    };
    index_observation_ids_if_enabled(&state, &result.observation_ids).await;
    state.publish(
        "memory_saved",
        json!({
            "id": result.observation_ids[0],
            "title": title,
            "project": project
        }),
    );
    Ok(Json(json!({
        "success": true,
        "id": result.observation_ids[0],
        "title": title,
        "project": project,
        "message": format!("Memory saved as observation #{}", result.observation_ids[0])
    })))
}

pub async fn context_inject(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<String, ApiError> {
    let projects = query
        .get("projects")
        .or_else(|| query.get("project"))
        .ok_or_else(|| ApiError::bad_request("Project(s) parameter is required"))?;
    let limit = parse_limit(query.get("limit"), 20);
    let conn = state.conn.lock().unwrap();
    let mut sections = Vec::new();
    for project in projects.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let observations = query_observations(
            &conn,
            &ObservationQuery {
                project: Some(project.into()),
                limit,
            },
        )
        .map_err(ApiError::internal)?;
        let summary_ids = list_ids(&conn, "session_summaries", Some(project), limit)?;
        let sessions = get_summaries_by_ids(&conn, &summary_ids).map_err(ApiError::internal)?;
        sections.push(ResultFormatter::new().format_recent_context(
            project,
            &SearchResults {
                observations,
                sessions,
                prompts: Vec::new(),
            },
        ));
    }
    Ok(sections.join("\n\n"))
}

#[derive(Debug, Deserialize)]
pub struct SemanticRequest {
    q: Option<String>,
    project: Option<String>,
    limit: Option<i64>,
}

pub async fn semantic_context(
    State(state): State<AppState>,
    Json(req): Json<SemanticRequest>,
) -> ApiResult<Value> {
    let Some(q) = req.q.filter(|q| q.len() >= 20) else {
        return Ok(Json(json!({ "context": "", "count": 0 })));
    };
    let rows = search_observations(&state, &q, req.project.as_deref(), req.limit.unwrap_or(5))?;
    let context = if rows.is_empty() {
        String::new()
    } else {
        let formatted = ResultFormatter::new().format_search_results(
            &SearchResults {
                observations: rows.clone(),
                sessions: Vec::new(),
                prompts: Vec::new(),
            },
            &q,
            false,
        );
        format!("## Relevant Past Work\n\n{formatted}")
    };
    Ok(Json(json!({ "context": context, "count": rows.len() })))
}

pub async fn context_recent(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let project = query.get("project").map(String::as_str);
    let conn = state.conn.lock().unwrap();
    let ids = list_ids(
        &conn,
        "observations",
        project,
        parse_limit(query.get("limit"), 10),
    )?;
    let rows = get_observations_by_ids(&conn, &ids).map_err(ApiError::internal)?;
    let context = rows
        .iter()
        .map(|row| format_observation(row, &FormatOptions::default()))
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(Json(
        json!({ "context": context, "observations": rows, "count": rows.len() }),
    ))
}

pub async fn context_timeline(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    timeline(State(state), Query(query)).await
}

pub async fn context_preview(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<String, ApiError> {
    context_inject(State(state), Query(query)).await
}

pub async fn search(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let q = query
        .get("query")
        .or_else(|| query.get("q"))
        .cloned()
        .unwrap_or_default();
    let options = search_options_from_query(&query, &q);
    #[cfg(feature = "qdrant")]
    let (qdrant_refs, used_qdrant, fell_back) = if should_use_qdrant(&options, &q) {
        match QdrantClient::from_env_if_enabled() {
            Some(client) => match client
                .search_memory_refs(&q, parse_limit(query.get("limit"), 20) * 4)
                .await
            {
                Ok(refs) => (Some(MemoryPointRefs::from_refs(refs)), true, false),
                Err(error) => {
                    tracing::warn!(%error, "qdrant search failed; falling back to sqlite");
                    (None, false, true)
                }
            },
            None => (None, false, true),
        }
    } else {
        (None, false, false)
    };
    #[cfg(not(feature = "qdrant"))]
    let (used_qdrant, fell_back): (bool, bool) = (false, false);

    let conn = state.conn.lock().unwrap();
    let result = SqliteSearchStrategy::new().search(&conn, &options);
    #[cfg(feature = "qdrant")]
    let (rows, sessions, prompts) = if let Some(refs) = qdrant_refs {
        let rows = get_observations_by_ids(&conn, &refs.observations)
            .map_err(ApiError::internal)?
            .into_iter()
            .filter(|row| observation_matches_options(row, &options))
            .take(parse_limit(query.get("limit"), 20) as usize)
            .collect();
        let sessions = get_summaries_by_ids(&conn, &refs.summaries)
            .map_err(ApiError::internal)?
            .into_iter()
            .filter(|row| summary_matches_options(row, &options))
            .take(parse_limit(query.get("limit"), 20) as usize)
            .collect();
        let prompts = get_user_prompts_by_ids(&conn, &refs.prompts)
            .map_err(ApiError::internal)?
            .into_iter()
            .filter(|row| prompt_matches_options(&conn, row, &options))
            .take(parse_limit(query.get("limit"), 20) as usize)
            .collect();
        (rows, sessions, prompts)
    } else {
        (
            result.results.observations,
            result.results.sessions,
            result.results.prompts,
        )
    };
    #[cfg(not(feature = "qdrant"))]
    let (rows, sessions, prompts) = (
        result.results.observations,
        result.results.sessions,
        result.results.prompts,
    );
    let total_results = rows.len() + sessions.len() + prompts.len();
    if query.get("format").is_some_and(|format| format == "text") {
        let text = ResultFormatter::new().format_search_results(
            &SearchResults {
                observations: rows,
                sessions,
                prompts,
            },
            &q,
            false,
        );
        return Ok(Json(
            json!({ "content": [{ "type": "text", "text": text }] }),
        ));
    }
    Ok(Json(json!({
        "observations": rows,
        "sessions": sessions,
        "prompts": prompts,
        "count": total_results,
        "totalResults": total_results,
        "strategy": if used_qdrant { "qdrant" } else { "sqlite" },
        "usedQdrant": used_qdrant,
        "fellBack": fell_back
    })))
}

pub async fn search_observations_route(
    State(state): State<AppState>,
    Query(mut query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    query.insert("type".into(), "observations".into());
    search(State(state), Query(query)).await
}

pub async fn search_sessions_route(
    State(state): State<AppState>,
    Query(mut query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    query.insert("type".into(), "sessions".into());
    search(State(state), Query(query)).await
}

pub async fn search_prompts_route(
    State(state): State<AppState>,
    Query(mut query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    query.insert("type".into(), "prompts".into());
    search(State(state), Query(query)).await
}

pub async fn decisions(
    State(state): State<AppState>,
    Query(mut query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    query.insert("type".into(), "decision".into());
    search_by_type(State(state), Query(query)).await
}

pub async fn changes(
    State(state): State<AppState>,
    Query(mut query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    query.insert(
        "type".into(),
        "change,implementation,refactor,bugfix".into(),
    );
    search_by_type(State(state), Query(query)).await
}

pub async fn how_it_works(
    State(state): State<AppState>,
    Query(mut query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    query.insert("concept".into(), "how-it-works".into());
    search_by_concept(State(state), Query(query)).await
}

pub async fn timeline_by_query(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    timeline(State(state), Query(query)).await
}

pub async fn search_help() -> Json<Value> {
    Json(json!({
        "endpoints": [
            "/api/search?query=...",
            "/api/search/observations?query=...",
            "/api/search/sessions?query=...",
            "/api/search/prompts?query=...",
            "/api/search/by-file?filePath=...",
            "/api/search/by-concept?concept=...",
            "/api/search/by-type?type=...",
            "/api/timeline?anchor=..."
        ]
    }))
}

pub async fn timeline(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let depth_before = parse_limit(query.get("depth_before"), 3);
    let depth_after = parse_limit(query.get("depth_after"), 3);
    let project = query.get("project").map(String::as_str);
    let anchor_id = match query
        .get("anchor")
        .and_then(|value| value.parse::<i64>().ok())
    {
        Some(id) => id,
        None => {
            let q = query
                .get("query")
                .or_else(|| query.get("q"))
                .ok_or_else(|| ApiError::bad_request("anchor or query is required"))?;
            let rows = search_observations(&state, q, project, 1)?;
            rows.first()
                .map(|row| row.id)
                .ok_or_else(|| ApiError::bad_request("query did not match an anchor observation"))?
        }
    };

    let conn = state.conn.lock().unwrap();
    let anchor = get_observation_by_id(&conn, anchor_id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::bad_request("anchor observation was not found"))?;
    if let Some(project) = project {
        if anchor.project != project {
            return Err(ApiError::bad_request(
                "anchor observation does not belong to requested project",
            ));
        }
    }

    let mut before_stmt = conn
        .prepare(
            "SELECT id FROM observations
             WHERE project = ?1
               AND (created_at_epoch < ?2 OR (created_at_epoch = ?2 AND id < ?3))
             ORDER BY created_at_epoch DESC, id DESC
             LIMIT ?4",
        )
        .map_err(ApiError::internal)?;
    let before_ids_desc: Vec<i64> = before_stmt
        .query_map(
            rusqlite::params![
                &anchor.project,
                anchor.created_at_epoch,
                anchor.id,
                depth_before
            ],
            |row| row.get(0),
        )
        .map_err(ApiError::internal)?
        .collect::<Result<_, _>>()
        .map_err(ApiError::internal)?;
    drop(before_stmt);

    let mut after_stmt = conn
        .prepare(
            "SELECT id FROM observations
             WHERE project = ?1
               AND (created_at_epoch > ?2 OR (created_at_epoch = ?2 AND id > ?3))
             ORDER BY created_at_epoch ASC, id ASC
             LIMIT ?4",
        )
        .map_err(ApiError::internal)?;
    let after_ids: Vec<i64> = after_stmt
        .query_map(
            rusqlite::params![
                &anchor.project,
                anchor.created_at_epoch,
                anchor.id,
                depth_after
            ],
            |row| row.get(0),
        )
        .map_err(ApiError::internal)?
        .collect::<Result<_, _>>()
        .map_err(ApiError::internal)?;
    drop(after_stmt);

    let mut ids = before_ids_desc;
    ids.reverse();
    ids.push(anchor.id);
    ids.extend(after_ids);
    let rows = get_observations_by_ids(&conn, &ids).map_err(ApiError::internal)?;

    Ok(Json(json!({
        "anchor": anchor.id,
        "observations": rows,
        "count": rows.len()
    })))
}

pub async fn search_by_file(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let file_path = query
        .get("filePath")
        .or_else(|| query.get("file_path"))
        .ok_or_else(|| ApiError::bad_request("filePath is required"))?;
    let conn = state.conn.lock().unwrap();
    let rows =
        get_observations_by_file_path(&conn, file_path, Some(parse_limit(query.get("limit"), 10)))
            .map_err(ApiError::internal)?;
    Ok(Json(json!({ "observations": rows, "count": rows.len() })))
}

pub async fn search_by_concept(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let concept = query
        .get("concept")
        .or_else(|| query.get("q"))
        .or_else(|| query.get("query"))
        .ok_or_else(|| ApiError::bad_request("concept is required"))?;
    let options = search_options_from_query(&query, "");
    let conn = state.conn.lock().unwrap();
    let rows = SqliteSearchStrategy::new()
        .find_by_concept(&conn, concept, &options)
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "observations": rows, "count": rows.len() })))
}

pub async fn search_by_type(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let types = split_csv(query.get("type").or_else(|| query.get("types")));
    if types.is_empty() {
        return Err(ApiError::bad_request("type is required"));
    }
    let options = search_options_from_query(&query, "");
    let conn = state.conn.lock().unwrap();
    let rows = SqliteSearchStrategy::new()
        .find_by_type(&conn, &types, &options)
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "observations": rows, "count": rows.len() })))
}

#[cfg(feature = "qdrant")]
pub async fn qdrant_health() -> ApiResult<Value> {
    let Some(config) = QdrantConfig::from_env_if_enabled() else {
        return Ok(Json(json!({ "enabled": false })));
    };
    let client = QdrantClient::new(config.clone());
    let reachable = client.ensure_collection().await.is_ok();
    Ok(Json(json!({
        "qdrant": QdrantStatus::from(&config),
        "reachable": reachable
    })))
}

#[cfg(feature = "qdrant")]
#[derive(Debug, Deserialize)]
pub struct QdrantReindexRequest {
    project: Option<String>,
    limit: Option<i64>,
}

#[cfg(feature = "qdrant")]
pub async fn qdrant_reindex(
    State(state): State<AppState>,
    Json(req): Json<QdrantReindexRequest>,
) -> ApiResult<Value> {
    let client = QdrantClient::from_env_if_enabled()
        .ok_or_else(|| ApiError::bad_request("qdrant is not enabled"))?;
    let limit = req.limit.unwrap_or(100).clamp(1, 10_000);
    let project = req.project.clone();
    let (observations, summaries, prompts) = {
        let conn = state.conn.lock().unwrap();
        let observations = query_observations(
            &conn,
            &ObservationQuery {
                project: project.clone(),
                limit,
            },
        )
        .map_err(ApiError::internal)?;
        let summary_ids = list_ids(&conn, "session_summaries", project.as_deref(), limit)?;
        let summaries = get_summaries_by_ids(&conn, &summary_ids).map_err(ApiError::internal)?;
        let prompts = prompt_points_for_reindex(&conn, project.as_deref(), limit)?;
        (observations, summaries, prompts)
    };
    client
        .upsert_memory_points(&observations, &summaries, &prompts)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "success": true,
        "indexed": observations.len() + summaries.len() + prompts.len(),
        "observations": observations.len(),
        "summaries": summaries.len(),
        "prompts": prompts.len(),
        "schemaVersion": 2
    })))
}

#[derive(Debug, Deserialize)]
pub struct ObservationsBatchRequest {
    ids: Vec<i64>,
}

pub async fn observations_batch(
    State(state): State<AppState>,
    Json(req): Json<ObservationsBatchRequest>,
) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let rows = get_observations_by_ids(&conn, &req.ids).map_err(ApiError::internal)?;
    Ok(Json(json!({ "observations": rows })))
}

pub async fn observation_get(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let row = get_observation_by_id(&conn, id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::bad_request("observation was not found"))?;
    Ok(Json(json!(row)))
}

pub async fn observations_get(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let ids = list_ids(
        &conn,
        "observations",
        query.get("project").map(String::as_str),
        parse_limit(query.get("limit"), 100),
    )?;
    let rows = get_observations_by_ids(&conn, &ids).map_err(ApiError::internal)?;
    Ok(Json(json!({ "observations": rows, "count": rows.len() })))
}

pub async fn observations_by_file(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let file_path = query
        .get("path")
        .or_else(|| query.get("filePath"))
        .or_else(|| query.get("file_path"))
        .ok_or_else(|| ApiError::bad_request("path query parameter is required"))?;
    search_by_file(
        State(state),
        Query(HashMap::from([
            ("filePath".to_owned(), file_path.to_owned()),
            (
                "limit".to_owned(),
                query.get("limit").cloned().unwrap_or_else(|| "15".into()),
            ),
        ])),
    )
    .await
}

pub async fn summaries_get(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let ids = list_ids(
        &conn,
        "session_summaries",
        query.get("project").map(String::as_str),
        parse_limit(query.get("limit"), 100),
    )?;
    let rows = get_summaries_by_ids(&conn, &ids).map_err(ApiError::internal)?;
    Ok(Json(json!({ "summaries": rows, "count": rows.len() })))
}

pub async fn prompts_get(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let ids = list_ids(
        &conn,
        "user_prompts",
        None,
        parse_limit(query.get("limit"), 100),
    )?;
    let rows = get_user_prompts_by_ids(&conn, &ids).map_err(ApiError::internal)?;
    Ok(Json(json!({ "prompts": rows, "count": rows.len() })))
}

pub async fn prompt_get(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let rows = get_user_prompts_by_ids(&conn, &[id]).map_err(ApiError::internal)?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::bad_request("prompt was not found"))?;
    Ok(Json(json!(row)))
}

pub async fn session_get(State(state): State<AppState>, Path(id): Path<i64>) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let row = get_session_by_id_locked(&conn, id)?
        .ok_or_else(|| ApiError::bad_request("session was not found"))?;
    Ok(Json(json!(row)))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SdkSessionsBatchRequest {
    #[serde(alias = "memorySessionIds")]
    memory_session_ids: Option<Vec<String>>,
}

pub async fn sdk_sessions_batch(
    State(state): State<AppState>,
    Json(req): Json<SdkSessionsBatchRequest>,
) -> ApiResult<Value> {
    let ids = req.memory_session_ids.unwrap_or_default();
    let conn = state.conn.lock().unwrap();
    let mut sessions = Vec::new();
    for id in ids {
        if let Some(session) = get_session_by_memory_id(&conn, &id).map_err(ApiError::internal)? {
            sessions.push(session);
        }
    }
    Ok(Json(json!(sessions)))
}

pub async fn stats(State(state): State<AppState>) -> ApiResult<Value> {
    Ok(Json(db_stats(&state)?))
}

pub async fn projects(State(state): State<AppState>) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT project, COUNT(*) AS observation_count, MAX(created_at_epoch) AS latest_epoch
             FROM observations
             GROUP BY project
             ORDER BY latest_epoch DESC, project ASC",
        )
        .map_err(ApiError::internal)?;
    let rows: Vec<Value> = stmt
        .query_map([], |row| {
            Ok(json!({
                "project": row.get::<_, String>(0)?,
                "observationCount": row.get::<_, i64>(1)?,
                "latestEpoch": row.get::<_, Option<i64>>(2)?
            }))
        })
        .map_err(ApiError::internal)?
        .collect::<Result<_, _>>()
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "projects": rows, "count": rows.len() })))
}

pub async fn processing_status(State(state): State<AppState>) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pending_messages WHERE status = 'pending'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    let processing: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pending_messages WHERE status = 'processing'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    Ok(Json(json!({
        "active": processing > 0,
        "pending": pending,
        "processing": processing
    })))
}

pub async fn processing_set(State(state): State<AppState>) -> ApiResult<Value> {
    processing_status(State(state)).await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueMetricsQuery {
    window: Option<String>,
}

pub async fn pending_queue_get(
    State(state): State<AppState>,
    Query(query): Query<QueueMetricsQuery>,
) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let queue = pending_queue_rows(&conn, PendingQueueFilter::All)?;
    Ok(Json(pending_queue_response(
        &conn,
        queue,
        MetricsWindow::parse(query.window.as_deref()),
    )?))
}

pub async fn pending_queue_all_get(
    State(state): State<AppState>,
    Query(query): Query<QueueMetricsQuery>,
) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let queue = pending_queue_rows(&conn, PendingQueueFilter::All)?;
    Ok(Json(pending_queue_response(
        &conn,
        queue,
        MetricsWindow::parse(query.window.as_deref()),
    )?))
}

pub async fn pending_queue_failed_get(State(state): State<AppState>) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let queue = pending_queue_rows(&conn, PendingQueueFilter::Failed)?;
    Ok(Json(pending_queue_response(
        &conn,
        queue,
        MetricsWindow::FifteenMinutes,
    )?))
}

fn pending_queue_response(
    conn: &rusqlite::Connection,
    queue: Vec<Value>,
    window: MetricsWindow,
) -> Result<Value, ApiError> {
    let mut sessions: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    for row in &queue {
        let Some(content_session_id) = row
            .get("contentSessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        let entry = sessions.entry(content_session_id).or_default();
        match row.get("status").and_then(Value::as_str) {
            Some("pending") => entry.0 += 1,
            Some("processing") => entry.1 += 1,
            Some("failed") => entry.2 += 1,
            _ => {}
        }
    }
    let sessions_with_pending_work = sessions
        .into_iter()
        .map(|(content_session_id, (pending, processing, failed))| {
            json!({
                "contentSessionId": content_session_id,
                "pending": pending,
                "processing": processing,
                "failed": failed,
                "total": pending + processing + failed
            })
        })
        .collect::<Vec<_>>();
    let recent = recent_queue_events(conn, 50, window)?;
    let recent_totals = queue_event_totals(conn, window)?;
    let activity = activity_for_window(conn, window)?;
    let token_metrics = token_metrics_for_window(conn, window)?;
    let total_pending = queue
        .iter()
        .filter(|row| row.get("status").and_then(Value::as_str) == Some("pending"))
        .count();
    let total_processing = queue
        .iter()
        .filter(|row| row.get("status").and_then(Value::as_str) == Some("processing"))
        .count();
    let total_failed = queue
        .iter()
        .filter(|row| row.get("status").and_then(Value::as_str) == Some("failed"))
        .count();
    let stale_cutoff = now_timestamp().1 - 60_000;
    let stuck_count = queue
        .iter()
        .filter(|row| row.get("status").and_then(Value::as_str) == Some("processing"))
        .filter(|row| {
            row.get("startedProcessingAtEpoch")
                .and_then(Value::as_i64)
                .is_some_and(|started| started < stale_cutoff)
        })
        .count();
    Ok(json!({
        "queue": {
            "messages": queue,
            "totalPending": total_pending,
            "totalProcessing": total_processing,
            "totalFailed": total_failed,
            "stuckCount": stuck_count
        },
        "recentlyProcessed": recent,
        "recentlyCompleted": recent_totals,
        "activityWindow": activity,
        "tokenMetrics": token_metrics,
        "sessionsWithPendingWork": sessions_with_pending_work
    }))
}

#[derive(Debug, Clone, Copy)]
enum MetricsWindow {
    FifteenMinutes,
    TwentyFourHours,
    All,
}

impl MetricsWindow {
    fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or("15m").trim().to_ascii_lowercase().as_str() {
            "24h" | "day" | "1d" => Self::TwentyFourHours,
            "all" | "all-time" | "all_time" => Self::All,
            _ => Self::FifteenMinutes,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::FifteenMinutes => "15m",
            Self::TwentyFourHours => "24h",
            Self::All => "all",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::FifteenMinutes => "last 15m",
            Self::TwentyFourHours => "last 24h",
            Self::All => "all time",
        }
    }

    fn millis(self) -> Option<i64> {
        match self {
            Self::FifteenMinutes => Some(15 * 60 * 1000),
            Self::TwentyFourHours => Some(24 * 60 * 60 * 1000),
            Self::All => None,
        }
    }

    fn since(self) -> Option<i64> {
        self.millis().map(|millis| now_timestamp().1 - millis)
    }
}

fn queue_event_totals(
    conn: &rusqlite::Connection,
    window: MetricsWindow,
) -> Result<Value, ApiError> {
    let (processed, failed, observations, summaries): (i64, i64, i64, i64) =
        if let Some(since) = window.since() {
            conn.query_row(
                "SELECT
                    COALESCE(SUM(CASE WHEN status = 'processed' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN message_type = 'observation' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN message_type = 'summarize' THEN 1 ELSE 0 END), 0)
                 FROM pending_message_events
                 WHERE completed_at_epoch >= ?1",
                [since],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
        } else {
            conn.query_row(
                "SELECT
                    COALESCE(SUM(CASE WHEN status = 'processed' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN message_type = 'observation' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN message_type = 'summarize' THEN 1 ELSE 0 END), 0)
                 FROM pending_message_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
        }
        .map_err(ApiError::internal)?;
    Ok(json!({
        "window": window.key(),
        "windowLabel": window.label(),
        "windowMs": window.millis(),
        "processed": processed,
        "failed": failed,
        "total": processed + failed,
        "observations": observations,
        "summaries": summaries
    }))
}

fn recent_queue_events(
    conn: &rusqlite::Connection,
    limit: i64,
    window: MetricsWindow,
) -> Result<Vec<Value>, ApiError> {
    let since = window.since();
    let sql = if since.is_some() {
        "SELECT pending_message_id, session_db_id, content_session_id,
                    message_type, tool_name, status, retry_count,
                    created_at_epoch, started_processing_at_epoch,
                    completed_at_epoch, duration_ms, agent_type, agent_id
               FROM pending_message_events
              WHERE completed_at_epoch >= ?1
              ORDER BY completed_at_epoch DESC, id DESC
              LIMIT ?2"
    } else {
        "SELECT pending_message_id, session_db_id, content_session_id,
                    message_type, tool_name, status, retry_count,
                    created_at_epoch, started_processing_at_epoch,
                    completed_at_epoch, duration_ms, agent_type, agent_id
               FROM pending_message_events
              ORDER BY completed_at_epoch DESC, id DESC
              LIMIT ?1"
    };
    let mut stmt = conn.prepare(sql).map_err(ApiError::internal)?;
    let map_row = |row: &rusqlite::Row<'_>| {
        Ok(json!({
            "messageId": row.get::<_, i64>(0)?,
            "sessionDbId": row.get::<_, i64>(1)?,
            "contentSessionId": row.get::<_, String>(2)?,
            "messageType": row.get::<_, String>(3)?,
            "toolName": row.get::<_, Option<String>>(4)?,
            "status": row.get::<_, String>(5)?,
            "retryCount": row.get::<_, i64>(6)?,
            "createdAtEpoch": row.get::<_, i64>(7)?,
            "startedProcessingAtEpoch": row.get::<_, Option<i64>>(8)?,
            "completedAtEpoch": row.get::<_, i64>(9)?,
            "durationMs": row.get::<_, Option<i64>>(10)?,
            "agentType": row.get::<_, Option<String>>(11)?,
            "agentId": row.get::<_, Option<String>>(12)?,
        }))
    };
    let rows = if let Some(since) = since {
        stmt.query_map(rusqlite::params![since, limit], map_row)
    } else {
        stmt.query_map(rusqlite::params![limit], map_row)
    }
    .map_err(ApiError::internal)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(ApiError::internal)?;
    Ok(rows)
}

fn activity_for_window(
    conn: &rusqlite::Connection,
    window: MetricsWindow,
) -> Result<Value, ApiError> {
    let observations = count_for_window(conn, "observations", "created_at_epoch", window)?;
    let summaries = count_for_window(conn, "session_summaries", "created_at_epoch", window)?;
    let prompts = count_for_window(conn, "user_prompts", "created_at_epoch", window)?;
    let sessions = count_for_window(conn, "sdk_sessions", "started_at_epoch", window)?;
    Ok(json!({
        "window": window.key(),
        "windowLabel": window.label(),
        "windowMs": window.millis(),
        "observations": observations,
        "summaries": summaries,
        "prompts": prompts,
        "sessions": sessions,
        "total": observations + summaries + prompts
    }))
}

fn token_metrics_for_window(
    conn: &rusqlite::Connection,
    window: MetricsWindow,
) -> Result<Value, ApiError> {
    let input_tokens = estimate_prompt_tokens(conn, window)?;
    let observation_tokens = estimate_observation_tokens(conn, window)?;
    let summary_tokens = estimate_summary_tokens(conn, window)?;
    let output_tokens = observation_tokens + summary_tokens;
    let input_cost_per_million = 3.0f64;
    let output_cost_per_million = 15.0f64;
    let estimated_cost_usd = ((input_tokens as f64 * input_cost_per_million)
        + (output_tokens as f64 * output_cost_per_million))
        / 1_000_000.0;
    Ok(json!({
        "window": window.key(),
        "windowLabel": window.label(),
        "windowMs": window.millis(),
        "inputTokens": input_tokens,
        "outputTokens": output_tokens,
        "totalTokens": input_tokens + output_tokens,
        "observationTokens": observation_tokens,
        "summaryTokens": summary_tokens,
        "estimatedCostUsd": estimated_cost_usd,
        "inputCostPerMillion": input_cost_per_million,
        "outputCostPerMillion": output_cost_per_million,
        "source": "estimated_from_persisted_text"
    }))
}

fn count_for_window(
    conn: &rusqlite::Connection,
    table: &str,
    epoch_column: &str,
    window: MetricsWindow,
) -> Result<i64, ApiError> {
    if let Some(since) = window.since() {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE {epoch_column} >= ?1"),
            [since],
            |row| row.get(0),
        )
    } else {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
    }
    .map_err(ApiError::internal)
}

fn estimate_prompt_tokens(
    conn: &rusqlite::Connection,
    window: MetricsWindow,
) -> Result<i64, ApiError> {
    estimate_text_tokens_for_window(
        conn,
        "user_prompts",
        "created_at_epoch",
        "prompt_text",
        window,
    )
}

fn estimate_observation_tokens(
    conn: &rusqlite::Connection,
    window: MetricsWindow,
) -> Result<i64, ApiError> {
    estimate_text_tokens_for_window(
        conn,
        "observations",
        "created_at_epoch",
        "coalesce(title,'') || ' ' || coalesce(subtitle,'') || ' ' || coalesce(text,'') || ' ' || coalesce(facts,'') || ' ' || coalesce(narrative,'') || ' ' || coalesce(concepts,'')",
        window,
    )
}

fn estimate_summary_tokens(
    conn: &rusqlite::Connection,
    window: MetricsWindow,
) -> Result<i64, ApiError> {
    estimate_text_tokens_for_window(
        conn,
        "session_summaries",
        "created_at_epoch",
        "coalesce(request,'') || ' ' || coalesce(investigated,'') || ' ' || coalesce(learned,'') || ' ' || coalesce(completed,'') || ' ' || coalesce(next_steps,'') || ' ' || coalesce(notes,'')",
        window,
    )
}

fn estimate_text_tokens_for_window(
    conn: &rusqlite::Connection,
    table: &str,
    epoch_column: &str,
    text_expr: &str,
    window: MetricsWindow,
) -> Result<i64, ApiError> {
    let sql = if window.since().is_some() {
        format!(
            "SELECT COALESCE(SUM((length({text_expr}) + 3) / 4), 0)
               FROM {table}
              WHERE {epoch_column} >= ?1"
        )
    } else {
        format!("SELECT COALESCE(SUM((length({text_expr}) + 3) / 4), 0) FROM {table}")
    };
    if let Some(since) = window.since() {
        conn.query_row(&sql, [since], |row| row.get(0))
    } else {
        conn.query_row(&sql, [], |row| row.get(0))
    }
    .map_err(ApiError::internal)
}

pub async fn pending_queue_process(State(state): State<AppState>) -> ApiResult<Value> {
    let stats = process_all_pending(Arc::clone(&state.conn), ObserverConfig::from_env())
        .await
        .map_err(ApiError::internal)?;
    index_observation_ids_if_enabled(&state, &stats.observation_ids).await;
    let result = observer_stats_json(&stats);
    state.publish("queue_processed", result.clone());
    Ok(Json(json!({
        "success": true,
        "message": "Native Rust observer-agent queue processor ran",
        "result": result,
        "totalPendingSessions": stats.total_pending_sessions,
        "sessionsStarted": stats.sessions_started,
        "sessionsSkipped": stats.sessions_skipped,
        "startedSessionIds": stats.started_session_ids,
        "messagesProcessed": stats.messages_processed,
        "messagesFailed": stats.messages_failed,
        "observationsInserted": stats.observations_inserted,
        "summariesInserted": stats.summaries_inserted,
        "observationIds": stats.observation_ids
    })))
}

pub async fn pending_queue_failed_clear(State(state): State<AppState>) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let count = conn
        .execute("DELETE FROM pending_messages WHERE status = 'failed'", [])
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "success": true, "clearedCount": count })))
}

pub async fn pending_queue_all_clear(State(state): State<AppState>) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let count = conn
        .execute(
            "DELETE FROM pending_messages WHERE status IN ('pending','processing','failed')",
            [],
        )
        .map_err(ApiError::internal)?;
    Ok(Json(json!({ "success": true, "clearedCount": count })))
}

pub async fn export_data(State(state): State<AppState>) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    Ok(Json(json!({
        "format": "claude-mem-rs-export-v1",
        "exportedAt": now_timestamp().0,
        "sdkSessions": export_sdk_sessions(&conn)?,
        "observations": export_observations(&conn)?,
        "sessionSummaries": export_summaries(&conn)?,
        "userPrompts": export_prompts(&conn)?
    })))
}

pub async fn import_data(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    let conn = state.conn.lock().unwrap();
    let sessions = import_rows(&conn, "sdk_sessions", body.get("sdkSessions"))?;
    let observations = import_rows(&conn, "observations", body.get("observations"))?;
    let summaries = import_rows(&conn, "session_summaries", body.get("sessionSummaries"))?;
    let prompts = import_rows(&conn, "user_prompts", body.get("userPrompts"))?;
    Ok(Json(json!({
        "success": true,
        "imported": {
            "sdkSessions": sessions,
            "observations": observations,
            "sessionSummaries": summaries,
            "userPrompts": prompts
        }
    })))
}

pub async fn settings_get() -> ApiResult<Value> {
    Ok(Json(read_json_file(&settings_path(), json!({}))?))
}

pub async fn settings_post(Json(body): Json<Value>) -> ApiResult<Value> {
    write_json_file(&settings_path(), &body)?;
    Ok(Json(json!({ "success": true, "settings": body })))
}

pub async fn mcp_status() -> ApiResult<Value> {
    Ok(Json(json!({
        "enabled": true,
        "managedByRust": true,
        "binary": "claude-mem-mcp",
        "vulcan": vulcan_mcp_status()
    })))
}

pub async fn mcp_toggle(Json(body): Json<Value>) -> ApiResult<Value> {
    Ok(Json(json!({
        "success": true,
        "enabled": body.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        "message": "Rust MCP is a separate stdio binary; no plugin file toggle is required"
    })))
}

pub async fn logs_get(Query(query): Query<HashMap<String, String>>) -> ApiResult<Value> {
    let path = log_path();
    let limit = parse_limit(query.get("limit"), 200) as usize;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let lines = text
        .lines()
        .rev()
        .take(limit)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Ok(Json(
        json!({ "path": path, "lines": lines, "count": lines.len() }),
    ))
}

pub async fn logs_clear() -> ApiResult<Value> {
    let path = log_path();
    ensure_parent(&path)?;
    std::fs::write(&path, "").map_err(ApiError::internal)?;
    Ok(Json(json!({ "success": true, "path": path })))
}

pub async fn branch_status() -> ApiResult<Value> {
    Ok(Json(json!({
        "repo": git_output(&["rev-parse", "--show-toplevel"]).ok(),
        "branch": git_output(&["branch", "--show-current"]).unwrap_or_else(|_| "unknown".into()),
        "commit": git_output(&["rev-parse", "HEAD"]).ok(),
        "dirty": !git_output(&["status", "--porcelain"]).unwrap_or_default().trim().is_empty(),
        "mutationEnabled": branch_mutation_enabled()
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSwitchRequest {
    branch: String,
}

pub async fn branch_switch(Json(req): Json<BranchSwitchRequest>) -> ApiResult<Value> {
    if !branch_mutation_enabled() {
        return Err(ApiError::forbidden(
            "branch mutation requires CLAUDE_MEM_ALLOW_BRANCH_MUTATION=true",
        ));
    }
    let output = git_output(&["switch", &req.branch]).map_err(ApiError::internal)?;
    Ok(Json(json!({ "success": true, "output": output })))
}

pub async fn branch_update() -> ApiResult<Value> {
    if !branch_mutation_enabled() {
        return Err(ApiError::forbidden(
            "branch mutation requires CLAUDE_MEM_ALLOW_BRANCH_MUTATION=true",
        ));
    }
    let output = git_output(&["pull", "--ff-only"]).map_err(ApiError::internal)?;
    Ok(Json(json!({ "success": true, "output": output })))
}

fn snapshot(state: &AppState, limit: i64) -> Result<Value, ApiError> {
    let conn = state.conn.lock().unwrap();
    let observation_ids = list_ids(&conn, "observations", None, limit)?;
    let summary_ids = list_ids(&conn, "session_summaries", None, limit)?;
    Ok(json!({
        "stats": db_stats_locked(&conn)?,
        "observations": get_observations_by_ids(&conn, &observation_ids).map_err(ApiError::internal)?,
        "summaries": get_summaries_by_ids(&conn, &summary_ids).map_err(ApiError::internal)?
    }))
}

fn db_stats(state: &AppState) -> Result<Value, ApiError> {
    let conn = state.conn.lock().unwrap();
    db_stats_locked(&conn)
}

fn db_stats_locked(conn: &rusqlite::Connection) -> Result<Value, ApiError> {
    Ok(json!({
        "sessions": count_table(conn, "sdk_sessions")?,
        "observations": count_table(conn, "observations")?,
        "summaries": count_table(conn, "session_summaries")?,
        "prompts": count_table(conn, "user_prompts")?,
        "pendingMessages": count_table(conn, "pending_messages").unwrap_or(0)
    }))
}

fn db_activity_locked(conn: &rusqlite::Connection) -> Result<Value, ApiError> {
    let since = now_timestamp().1 - (15 * 60 * 1000);
    Ok(json!({
        "windowMs": 15 * 60 * 1000,
        "observations15m": count_since(conn, "observations", since)?,
        "summaries15m": count_since(conn, "session_summaries", since)?,
        "prompts15m": count_since(conn, "user_prompts", since)?,
        "sessions15m": count_since_column(conn, "sdk_sessions", "started_at_epoch", since)?,
        "latestObservationEpoch": max_epoch(conn, "observations")?,
        "latestSummaryEpoch": max_epoch(conn, "session_summaries")?,
        "latestPromptEpoch": max_epoch(conn, "user_prompts")?,
        "latestSessionEpoch": max_epoch_column(conn, "sdk_sessions", "started_at_epoch")?
    }))
}

fn get_session_by_id_locked(
    conn: &rusqlite::Connection,
    id: i64,
) -> Result<Option<SdkSessionRow>, ApiError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, content_session_id, memory_session_id, project, user_prompt,
                    started_at, started_at_epoch, completed_at, completed_at_epoch,
                    status, worker_port, COALESCE(prompt_counter,0),
                    custom_title, platform_source
             FROM sdk_sessions WHERE id = ?1",
        )
        .map_err(ApiError::internal)?;
    let row = stmt
        .query_row(rusqlite::params![id], |row| {
            Ok(SdkSessionRow {
                id: row.get(0)?,
                content_session_id: row.get(1)?,
                memory_session_id: row.get(2)?,
                project: row.get(3)?,
                user_prompt: row.get(4)?,
                started_at: row.get(5)?,
                started_at_epoch: row.get(6)?,
                completed_at: row.get(7)?,
                completed_at_epoch: row.get(8)?,
                status: row.get(9)?,
                worker_port: row.get(10)?,
                prompt_counter: row.get(11)?,
                custom_title: row.get(12)?,
                platform_source: row
                    .get::<_, Option<String>>(13)?
                    .unwrap_or_else(|| "claude".into()),
            })
        })
        .optional()
        .map_err(ApiError::internal)?;
    Ok(row)
}

fn count_pending_for_session(
    conn: &rusqlite::Connection,
    session_db_id: i64,
) -> Result<i64, ApiError> {
    conn.query_row(
        "SELECT COUNT(*) FROM pending_messages
         WHERE session_db_id = ?1 AND status IN ('pending','processing')",
        rusqlite::params![session_db_id],
        |row| row.get(0),
    )
    .map_err(ApiError::internal)
}

#[derive(Debug, Clone, Copy)]
enum PendingQueueFilter {
    All,
    Failed,
}

fn pending_queue_rows(
    conn: &rusqlite::Connection,
    filter: PendingQueueFilter,
) -> Result<Vec<Value>, ApiError> {
    let status_clause = match filter {
        PendingQueueFilter::All => "status IN ('pending','processing','failed')",
        PendingQueueFilter::Failed => "status = 'failed'",
    };
    let mut stmt = conn
        .prepare(&format!(
            "SELECT id, session_db_id, content_session_id, message_type, tool_name,
                    cwd, prompt_number, status, retry_count, created_at_epoch,
                    started_processing_at_epoch, completed_at_epoch, failed_at_epoch
             FROM pending_messages
             WHERE {status_clause}
             ORDER BY created_at_epoch ASC, id ASC
             LIMIT 500",
        ))
        .map_err(ApiError::internal)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(json!({
                "id": row.get::<_, i64>(0)?,
                "sessionDbId": row.get::<_, i64>(1)?,
                "contentSessionId": row.get::<_, String>(2)?,
                "messageType": row.get::<_, String>(3)?,
                "toolName": row.get::<_, Option<String>>(4)?,
                "cwd": row.get::<_, Option<String>>(5)?,
                "promptNumber": row.get::<_, Option<i64>>(6)?,
                "status": row.get::<_, String>(7)?,
                "retryCount": row.get::<_, i64>(8)?,
                "createdAtEpoch": row.get::<_, i64>(9)?,
                "startedProcessingAtEpoch": row.get::<_, Option<i64>>(10)?,
                "completedAtEpoch": row.get::<_, Option<i64>>(11)?,
                "failedAtEpoch": row.get::<_, Option<i64>>(12)?,
            }))
        })
        .map_err(ApiError::internal)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ApiError::internal)?;
    Ok(rows)
}

fn count_table(conn: &rusqlite::Connection, table: &str) -> Result<i64, ApiError> {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .map_err(ApiError::internal)
}

fn count_since(conn: &rusqlite::Connection, table: &str, since: i64) -> Result<i64, ApiError> {
    count_since_column(conn, table, "created_at_epoch", since)
}

fn count_since_column(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
    since: i64,
) -> Result<i64, ApiError> {
    conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE {column} >= ?1"),
        [since],
        |row| row.get(0),
    )
    .map_err(ApiError::internal)
}

fn max_epoch(conn: &rusqlite::Connection, table: &str) -> Result<Option<i64>, ApiError> {
    max_epoch_column(conn, table, "created_at_epoch")
}

fn max_epoch_column(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
) -> Result<Option<i64>, ApiError> {
    conn.query_row(&format!("SELECT MAX({column}) FROM {table}"), [], |row| {
        row.get(0)
    })
    .map_err(ApiError::internal)
}

fn list_ids(
    conn: &rusqlite::Connection,
    table: &str,
    project: Option<&str>,
    limit: i64,
) -> Result<Vec<i64>, ApiError> {
    let has_project = matches!(table, "observations" | "session_summaries") && project.is_some();
    let sql = if has_project {
        format!(
            "SELECT id FROM {table} WHERE project = ?1 ORDER BY created_at_epoch DESC, id DESC LIMIT ?2"
        )
    } else {
        format!("SELECT id FROM {table} ORDER BY created_at_epoch DESC, id DESC LIMIT ?1")
    };
    let mut stmt = conn.prepare(&sql).map_err(ApiError::internal)?;
    let rows = if let Some(project) = project.filter(|_| has_project) {
        stmt.query_map(rusqlite::params![project, limit], |row| row.get(0))
            .map_err(ApiError::internal)?
            .collect::<Result<Vec<i64>, _>>()
            .map_err(ApiError::internal)?
    } else {
        stmt.query_map(rusqlite::params![limit], |row| row.get(0))
            .map_err(ApiError::internal)?
            .collect::<Result<Vec<i64>, _>>()
            .map_err(ApiError::internal)?
    };
    Ok(rows)
}

fn store_generated_summary(
    conn: &rusqlite::Connection,
    content_session_id: &str,
    source: Option<&str>,
) -> Result<i64, ApiError> {
    let session = get_session_by_content_id(conn, content_session_id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::bad_request("unknown contentSessionId"))?;
    let memory_session_id = match session.memory_session_id {
        Some(id) => id,
        None => {
            let generated = format!("rust-local-memory:{content_session_id}");
            update_memory_session_id(conn, content_session_id, &generated)
                .map_err(ApiError::internal)?;
            generated
        }
    };
    let (created_at, created_at_epoch) = now_timestamp();
    let prompt = get_latest_user_prompt(conn, content_session_id).map_err(ApiError::internal)?;
    let prompt_number = prompt.as_ref().map(|prompt| prompt.prompt_number);

    let input = if let Some(parsed) = source.and_then(parse_summary) {
        SummaryInput {
            memory_session_id,
            project: session.project,
            request: parsed.request.or_else(|| prompt.map(|p| p.prompt_text)),
            investigated: parsed.investigated,
            learned: parsed.learned,
            completed: parsed.completed,
            next_steps: parsed.next_steps,
            notes: parsed.notes,
            prompt_number,
            discovery_tokens: Some(0),
            created_at,
            created_at_epoch,
            ..Default::default()
        }
    } else {
        let observations =
            get_observations_for_session(conn, &memory_session_id).map_err(ApiError::internal)?;
        fallback_summary_input(
            memory_session_id,
            session.project,
            prompt.map(|p| p.prompt_text).or(session.user_prompt),
            prompt_number,
            observations,
            source,
            created_at,
            created_at_epoch,
        )
    };
    store_summary(conn, &input).map_err(ApiError::internal)
}

#[allow(clippy::too_many_arguments)]
fn fallback_summary_input(
    memory_session_id: String,
    project: String,
    prompt: Option<String>,
    prompt_number: Option<i64>,
    observations: Vec<ObservationRow>,
    source: Option<&str>,
    created_at: String,
    created_at_epoch: i64,
) -> SummaryInput {
    let titles = observations
        .iter()
        .take(8)
        .filter_map(|obs| obs.title.clone().or_else(|| obs.narrative.clone()))
        .collect::<Vec<_>>();
    let files_read = observations
        .iter()
        .flat_map(|obs| obs.files_read.clone().unwrap_or_default())
        .collect::<Vec<_>>();
    let files_edited = observations
        .iter()
        .flat_map(|obs| obs.files_modified.clone().unwrap_or_default())
        .collect::<Vec<_>>();
    SummaryInput {
        memory_session_id,
        project,
        request: prompt.or_else(|| Some("Session summary".into())),
        investigated: (!titles.is_empty()).then(|| titles.join("; ")),
        learned: source.map(trim_for_summary),
        completed: Some(format!(
            "Captured {} observation(s) for searchable recall.",
            observations.len()
        )),
        next_steps: None,
        files_read: (!files_read.is_empty())
            .then(|| serde_json::to_string(&files_read).unwrap_or_else(|_| "[]".into())),
        files_edited: (!files_edited.is_empty())
            .then(|| serde_json::to_string(&files_edited).unwrap_or_else(|_| "[]".into())),
        notes: Some("Generated by claude-mem-rs session summary fallback.".into()),
        prompt_number,
        discovery_tokens: Some(0),
        created_at,
        created_at_epoch,
        merged_into_project: None,
    }
}

fn trim_for_summary(value: &str) -> String {
    let cleaned = strip_private_tags(value).trim().to_owned();
    if cleaned.chars().count() > 1200 {
        cleaned.chars().take(1200).collect()
    } else {
        cleaned
    }
}

fn export_sdk_sessions(conn: &rusqlite::Connection) -> Result<Vec<Value>, ApiError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, content_session_id, memory_session_id, project, user_prompt,
                    started_at, started_at_epoch, completed_at, completed_at_epoch, status,
                    worker_port, COALESCE(prompt_counter,0), custom_title, platform_source
             FROM sdk_sessions ORDER BY id ASC",
        )
        .map_err(ApiError::internal)?;
    let rows = stmt
        .query_map([], |row| {
        Ok(json!({
            "id": row.get::<_, i64>(0)?,
            "content_session_id": row.get::<_, String>(1)?,
            "memory_session_id": row.get::<_, Option<String>>(2)?,
            "project": row.get::<_, String>(3)?,
            "user_prompt": row.get::<_, Option<String>>(4)?,
            "started_at": row.get::<_, String>(5)?,
            "started_at_epoch": row.get::<_, i64>(6)?,
            "completed_at": row.get::<_, Option<String>>(7)?,
            "completed_at_epoch": row.get::<_, Option<i64>>(8)?,
            "status": row.get::<_, String>(9)?,
            "worker_port": row.get::<_, Option<i64>>(10)?,
            "prompt_counter": row.get::<_, i64>(11)?,
            "custom_title": row.get::<_, Option<String>>(12)?,
            "platform_source": row.get::<_, Option<String>>(13)?.unwrap_or_else(|| "claude".into())
        }))
    })
    .map_err(ApiError::internal)?
    .collect::<Result<_, _>>()
    .map_err(ApiError::internal)?;
    Ok(rows)
}

fn export_observations(conn: &rusqlite::Connection) -> Result<Vec<ObservationRow>, ApiError> {
    let ids = list_ids(conn, "observations", None, i64::MAX)?;
    get_observations_by_ids(conn, &ids).map_err(ApiError::internal)
}

fn export_summaries(conn: &rusqlite::Connection) -> Result<Vec<SessionSummaryRow>, ApiError> {
    let ids = list_ids(conn, "session_summaries", None, i64::MAX)?;
    get_summaries_by_ids(conn, &ids).map_err(ApiError::internal)
}

fn export_prompts(conn: &rusqlite::Connection) -> Result<Vec<UserPromptRow>, ApiError> {
    let ids = list_ids(conn, "user_prompts", None, i64::MAX)?;
    get_user_prompts_by_ids(conn, &ids).map_err(ApiError::internal)
}

fn import_rows(
    conn: &rusqlite::Connection,
    table: &str,
    rows: Option<&Value>,
) -> Result<usize, ApiError> {
    let Some(rows) = rows.and_then(Value::as_array) else {
        return Ok(0);
    };
    let allowed =
        import_columns(table).ok_or_else(|| ApiError::bad_request("unsupported import table"))?;
    let mut inserted = 0;
    for row in rows {
        let Some(object) = row.as_object() else {
            continue;
        };
        let columns = allowed
            .iter()
            .filter(|column| object.contains_key(**column))
            .copied()
            .collect::<Vec<_>>();
        if columns.is_empty() {
            continue;
        }
        let placeholders = (1..=columns.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "INSERT OR IGNORE INTO {table} ({}) VALUES ({placeholders})",
            columns.join(",")
        );
        let values = columns
            .iter()
            .map(|column| json_to_sql_value(object.get(*column).unwrap_or(&Value::Null)))
            .collect::<Vec<_>>();
        let params = values
            .iter()
            .map(|value| value as &dyn rusqlite::types::ToSql)
            .collect::<Vec<_>>();
        inserted += conn
            .execute(&sql, params.as_slice())
            .map_err(ApiError::internal)?;
    }
    Ok(inserted)
}

fn import_columns(table: &str) -> Option<&'static [&'static str]> {
    match table {
        "sdk_sessions" => Some(&[
            "id",
            "content_session_id",
            "memory_session_id",
            "project",
            "user_prompt",
            "started_at",
            "started_at_epoch",
            "completed_at",
            "completed_at_epoch",
            "status",
            "worker_port",
            "prompt_counter",
            "custom_title",
            "platform_source",
        ]),
        "observations" => Some(&[
            "id",
            "memory_session_id",
            "project",
            "text",
            "type",
            "title",
            "subtitle",
            "narrative",
            "facts",
            "concepts",
            "files_read",
            "files_modified",
            "prompt_number",
            "discovery_tokens",
            "created_at",
            "created_at_epoch",
            "generated_by_model",
            "relevance_count",
            "merged_into_project",
            "agent_type",
            "agent_id",
            "content_hash",
        ]),
        "session_summaries" => Some(&[
            "id",
            "memory_session_id",
            "project",
            "request",
            "investigated",
            "learned",
            "completed",
            "next_steps",
            "files_read",
            "files_edited",
            "notes",
            "prompt_number",
            "discovery_tokens",
            "created_at",
            "created_at_epoch",
            "merged_into_project",
        ]),
        "user_prompts" => Some(&[
            "id",
            "content_session_id",
            "prompt_number",
            "prompt_text",
            "created_at",
            "created_at_epoch",
        ]),
        _ => None,
    }
}

fn json_to_sql_value(value: &Value) -> rusqlite::types::Value {
    match value {
        Value::Null => rusqlite::types::Value::Null,
        Value::Bool(value) => rusqlite::types::Value::Integer(i64::from(*value)),
        Value::Number(value) => value
            .as_i64()
            .map(rusqlite::types::Value::Integer)
            .or_else(|| value.as_f64().map(rusqlite::types::Value::Real))
            .unwrap_or(rusqlite::types::Value::Null),
        Value::String(value) => rusqlite::types::Value::Text(value.clone()),
        Value::Array(_) | Value::Object(_) => rusqlite::types::Value::Text(value.to_string()),
    }
}

fn claude_mem_home() -> PathBuf {
    claude_mem_core::shared::platform_paths::claude_mem_home()
}

fn settings_path() -> PathBuf {
    claude_mem_home().join("settings.json")
}

fn log_path() -> PathBuf {
    claude_mem_home().join("claude-mem.log")
}

fn read_json_file(path: &std::path::Path, default: Value) -> Result<Value, ApiError> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(ApiError::internal),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(default),
        Err(error) => Err(ApiError::internal(error)),
    }
}

fn write_json_file(path: &std::path::Path, value: &Value) -> Result<(), ApiError> {
    ensure_parent(path)?;
    std::fs::write(
        path,
        serde_json::to_string_pretty(value).map_err(ApiError::internal)?,
    )
    .map_err(ApiError::internal)
}

fn ensure_parent(path: &std::path::Path) -> Result<(), ApiError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(ApiError::internal)?;
    }
    Ok(())
}

fn git_output(args: &[&str]) -> std::io::Result<String> {
    let output = Command::new("git").args(args).output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Ok(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn branch_mutation_enabled() -> bool {
    env_truthy("CLAUDE_MEM_ALLOW_BRANCH_MUTATION")
}

fn qdrant_enabled_env() -> bool {
    env_truthy("CLAUDE_MEM_QDRANT_ENABLED") || std::env::var_os("CLAUDE_MEM_QDRANT_URL").is_some()
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
        .unwrap_or(false)
}

fn search_observations(
    state: &AppState,
    query: &str,
    project: Option<&str>,
    limit: i64,
) -> Result<Vec<claude_mem_core::types::ObservationRow>, ApiError> {
    let query = fts_query(query);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let conn = state.conn.lock().unwrap();
    let sql = if project.is_some() {
        "SELECT o.id
         FROM observations_fts f
         JOIN observations o ON o.id = f.rowid
         WHERE observations_fts MATCH ?1 AND o.project = ?2
         ORDER BY o.created_at_epoch DESC, o.id DESC
         LIMIT ?3"
    } else {
        "SELECT o.id
         FROM observations_fts f
         JOIN observations o ON o.id = f.rowid
         WHERE observations_fts MATCH ?1
         ORDER BY o.created_at_epoch DESC, o.id DESC
         LIMIT ?2"
    };
    let ids: Vec<i64> = if let Some(project) = project {
        let mut stmt = conn.prepare(sql).map_err(ApiError::internal)?;
        let rows = stmt
            .query_map(rusqlite::params![query, project, limit], |row| row.get(0))
            .map_err(ApiError::internal)?
            .collect::<Result<_, _>>()
            .map_err(ApiError::internal)?;
        rows
    } else {
        let mut stmt = conn.prepare(sql).map_err(ApiError::internal)?;
        let rows = stmt
            .query_map(rusqlite::params![query, limit], |row| row.get(0))
            .map_err(ApiError::internal)?
            .collect::<Result<_, _>>()
            .map_err(ApiError::internal)?;
        rows
    };
    get_observations_by_ids(&conn, &ids).map_err(ApiError::internal)
}

fn fts_query(input: &str) -> String {
    input
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|term| term.len() >= 3)
        .take(8)
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn now_timestamp() -> (String, i64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let epoch = now.as_millis() as i64;
    let iso = time::OffsetDateTime::from_unix_timestamp(epoch / 1000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into());
    (iso, epoch)
}

fn parse_limit(value: Option<&String>, default: i64) -> i64 {
    value
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
        .min(100)
}

fn search_options_from_query(query: &HashMap<String, String>, q: &str) -> StrategySearchOptions {
    StrategySearchOptions {
        query: (!q.trim().is_empty()).then(|| q.to_owned()),
        search_type: query
            .get("type")
            .or_else(|| query.get("searchType"))
            .map(|value| match value.as_str() {
                "observations" | "observation" => SearchType::Observations,
                "sessions" | "session" => SearchType::Sessions,
                "prompts" | "prompt" => SearchType::Prompts,
                _ => SearchType::All,
            })
            .unwrap_or_default(),
        obs_type: split_csv(query.get("obs_type").or_else(|| query.get("obsType"))),
        concepts: split_csv(query.get("concepts")),
        files: split_csv(query.get("files")),
        project: query.get("project").cloned(),
        date_range: parse_date_range(query),
        limit: Some(parse_limit(query.get("limit"), 20)),
        offset: query
            .get("offset")
            .and_then(|offset| offset.parse::<i64>().ok())
            .filter(|offset| *offset >= 0),
        order_by: match query.get("orderBy").map(String::as_str) {
            Some("date_asc") => OrderBy::DateAsc,
            Some("relevance") => OrderBy::Relevance,
            _ => OrderBy::DateDesc,
        },
        strategy_hint: match query.get("strategy").map(String::as_str) {
            Some("sqlite") => Some(SearchStrategyHint::Sqlite),
            Some("chroma") => Some(SearchStrategyHint::Chroma),
            Some("qdrant") => Some(SearchStrategyHint::Qdrant),
            Some("hybrid") => Some(SearchStrategyHint::Hybrid),
            Some("auto") => Some(SearchStrategyHint::Auto),
            _ => None,
        },
    }
}

#[cfg(feature = "qdrant")]
fn should_use_qdrant(options: &StrategySearchOptions, query: &str) -> bool {
    !query.trim().is_empty()
        && matches!(
            options.strategy_hint,
            Some(SearchStrategyHint::Qdrant | SearchStrategyHint::Hybrid)
                | Some(SearchStrategyHint::Auto)
        )
        && matches!(
            options.search_type,
            SearchType::All | SearchType::Observations
        )
}

fn split_csv(value: Option<&String>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_date_range(query: &HashMap<String, String>) -> Option<DateRange> {
    let start = query
        .get("dateStart")
        .or_else(|| query.get("start"))
        .and_then(|value| parse_epoch(value));
    let end = query
        .get("dateEnd")
        .or_else(|| query.get("end"))
        .and_then(|value| parse_epoch(value));
    (start.is_some() || end.is_some()).then_some(DateRange {
        start_epoch: start,
        end_epoch: end,
    })
}

fn parse_epoch(value: &str) -> Option<i64> {
    value.parse::<i64>().ok().or_else(|| {
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .ok()
            .map(|dt| dt.unix_timestamp() * 1000)
    })
}

#[cfg(feature = "qdrant")]
fn observation_matches_options(
    row: &claude_mem_core::types::ObservationRow,
    options: &StrategySearchOptions,
) -> bool {
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

#[cfg(feature = "qdrant")]
fn summary_matches_options(row: &SessionSummaryRow, options: &StrategySearchOptions) -> bool {
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
}

#[cfg(feature = "qdrant")]
fn prompt_matches_options(
    conn: &rusqlite::Connection,
    row: &UserPromptRow,
    options: &StrategySearchOptions,
) -> bool {
    let date_matches = options.date_range.as_ref().is_none_or(|range| {
        range
            .start_epoch
            .is_none_or(|start| row.created_at_epoch >= start)
            && range
                .end_epoch
                .is_none_or(|end| row.created_at_epoch <= end)
    });
    if !date_matches {
        return false;
    }
    let Some(project) = &options.project else {
        return true;
    };
    conn.query_row(
        "SELECT 1 FROM sdk_sessions WHERE content_session_id = ?1 AND project = ?2 LIMIT 1",
        rusqlite::params![row.content_session_id, project],
        |_| Ok(()),
    )
    .optional()
    .map(|value| value.is_some())
    .unwrap_or(false)
}

fn vulcan_mcp_status() -> Value {
    let configured_path = std::env::var("CLAUDE_MEM_VULCAN_SDK_PATH").ok();
    let default_path = Some(
        claude_mem_core::shared::platform_paths::home_dir().join("firefly/vulcan-mcp-sdk-rust"),
    );
    let detected_path = configured_path
        .as_ref()
        .map(PathBuf::from)
        .or(default_path)
        .filter(|path| path.exists());

    json!({
        "evaluated": true,
        "available": detected_path.is_some(),
        "sdkPath": detected_path.map(|path| path.display().to_string()),
        "runtime": "rmcp",
        "decision": "Vulcan SDK is detected for future adapter work; Rust MCP remains rmcp-backed to keep public builds self-contained."
    })
}

#[cfg(feature = "qdrant")]
fn prompt_points_for_reindex(
    conn: &rusqlite::Connection,
    project: Option<&str>,
    limit: i64,
) -> Result<Vec<PromptPoint>, ApiError> {
    let mut sql = String::from(
        "SELECT up.id, up.content_session_id, up.prompt_number, up.prompt_text,
                up.created_at, up.created_at_epoch, COALESCE(s.project, '')
           FROM user_prompts up
           LEFT JOIN sdk_sessions s ON s.content_session_id = up.content_session_id",
    );
    if project.is_some() {
        sql.push_str(" WHERE s.project = ?1");
        sql.push_str(" ORDER BY up.created_at_epoch DESC, up.id DESC LIMIT ?2");
    } else {
        sql.push_str(" ORDER BY up.created_at_epoch DESC, up.id DESC LIMIT ?1");
    }
    let mut stmt = conn.prepare(&sql).map_err(ApiError::internal)?;
    let map_row = |row: &rusqlite::Row<'_>| {
        Ok(PromptPoint {
            prompt: UserPromptRow {
                id: row.get(0)?,
                content_session_id: row.get(1)?,
                prompt_number: row.get(2)?,
                prompt_text: row.get(3)?,
                created_at: row.get(4)?,
                created_at_epoch: row.get(5)?,
            },
            project: row.get(6)?,
        })
    };
    let rows = if let Some(project) = project {
        stmt.query_map(rusqlite::params![project, limit], map_row)
            .map_err(ApiError::internal)?
            .collect::<Result<Vec<_>, _>>()
    } else {
        stmt.query_map(rusqlite::params![limit], map_row)
            .map_err(ApiError::internal)?
            .collect::<Result<Vec<_>, _>>()
    }
    .map_err(ApiError::internal)?;
    Ok(rows)
}

#[cfg(feature = "qdrant")]
async fn index_observation_ids_if_enabled(state: &AppState, ids: &[i64]) {
    let Some(client) = QdrantClient::from_env_if_enabled() else {
        return;
    };
    let rows = {
        let conn = state.conn.lock().unwrap();
        match get_observations_by_ids(&conn, ids) {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "failed to load observations for qdrant indexing");
                return;
            }
        }
    };
    if let Err(error) = client.upsert_observations(&rows).await {
        tracing::warn!(%error, "qdrant indexing failed");
    }
}

#[cfg(not(feature = "qdrant"))]
async fn index_observation_ids_if_enabled(_state: &AppState, _ids: &[i64]) {}

#[cfg(feature = "qdrant")]
async fn index_summary_ids_if_enabled(state: &AppState, ids: &[i64]) {
    let Some(client) = QdrantClient::from_env_if_enabled() else {
        return;
    };
    let rows = {
        let conn = state.conn.lock().unwrap();
        match get_summaries_by_ids(&conn, ids) {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "failed to load summaries for qdrant indexing");
                return;
            }
        }
    };
    if let Err(error) = client.upsert_memory_points(&[], &rows, &[]).await {
        tracing::warn!(%error, "qdrant summary indexing failed");
    }
}

#[cfg(not(feature = "qdrant"))]
async fn index_summary_ids_if_enabled(_state: &AppState, _ids: &[i64]) {}

#[cfg(feature = "qdrant")]
async fn index_prompt_ids_if_enabled(state: &AppState, ids: &[i64]) {
    let Some(client) = QdrantClient::from_env_if_enabled() else {
        return;
    };
    let rows = {
        let conn = state.conn.lock().unwrap();
        match prompt_points_by_ids(&conn, ids) {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(?error, "failed to load prompts for qdrant indexing");
                return;
            }
        }
    };
    if let Err(error) = client.upsert_memory_points(&[], &[], &rows).await {
        tracing::warn!(%error, "qdrant prompt indexing failed");
    }
}

#[cfg(not(feature = "qdrant"))]
async fn index_prompt_ids_if_enabled(_state: &AppState, _ids: &[i64]) {}

#[cfg(feature = "qdrant")]
fn prompt_points_by_ids(
    conn: &rusqlite::Connection,
    ids: &[i64],
) -> Result<Vec<PromptPoint>, ApiError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let mut stmt = conn
        .prepare(&format!(
            "SELECT up.id, up.content_session_id, up.prompt_number, up.prompt_text,
                    up.created_at, up.created_at_epoch, COALESCE(s.project, '')
               FROM user_prompts up
               LEFT JOIN sdk_sessions s ON s.content_session_id = up.content_session_id
              WHERE up.id IN ({placeholders})"
        ))
        .map_err(ApiError::internal)?;
    let params = ids
        .iter()
        .map(|id| id as &dyn rusqlite::types::ToSql)
        .collect::<Vec<_>>();
    let mut rows = stmt
        .query_map(params.as_slice(), |row| {
            Ok(PromptPoint {
                prompt: UserPromptRow {
                    id: row.get(0)?,
                    content_session_id: row.get(1)?,
                    prompt_number: row.get(2)?,
                    prompt_text: row.get(3)?,
                    created_at: row.get(4)?,
                    created_at_epoch: row.get(5)?,
                },
                project: row.get(6)?,
            })
        })
        .map_err(ApiError::internal)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(ApiError::internal)?;
    rows.sort_by_key(|row| {
        ids.iter()
            .position(|id| *id == row.prompt.id)
            .unwrap_or(usize::MAX)
    });
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Corpus routes — port of TS `src/services/worker/http/routes/CorpusRoutes.ts`
// ---------------------------------------------------------------------------

const ALLOWED_CORPUS_TYPES: &[&str] = &[
    "decision",
    "bugfix",
    "feature",
    "refactor",
    "discovery",
    "change",
];

/// Request body for `POST /api/corpus` (build) — `name` is required; every
/// other field maps onto `CorpusFilter`.
#[derive(Debug, Deserialize)]
pub struct CorpusBuildRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub project: Option<String>,
    #[serde(default)]
    pub types: Option<Value>,
    #[serde(default)]
    pub concepts: Option<Value>,
    #[serde(default)]
    pub files: Option<Value>,
    pub query: Option<String>,
    pub date_start: Option<String>,
    pub date_end: Option<String>,
    pub limit: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct CorpusQueryRequest {
    pub question: Option<String>,
}

/// POST /api/corpus — build a new corpus.
pub async fn corpus_build(
    State(state): State<AppState>,
    Json(req): Json<CorpusBuildRequest>,
) -> ApiResult<Value> {
    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request(
                "Missing required field: name. Example body: {\"name\":\"my-corpus\",\"query\":\"hooks\",\"limit\":100}",
            )
        })?;
    let description = req.description.unwrap_or_default();
    let types = coerce_string_array(req.types.as_ref(), "types")?;
    if let Some(values) = &types {
        for value in values {
            if !ALLOWED_CORPUS_TYPES.contains(&value.as_str()) {
                return Err(ApiError::bad_request(
                    "types must contain valid observation types",
                ));
            }
        }
    }
    let concepts = coerce_string_array(req.concepts.as_ref(), "concepts")?;
    let files = coerce_string_array(req.files.as_ref(), "files")?;
    let limit = coerce_positive_int(req.limit.as_ref(), "limit")?;

    let filter = CorpusFilter {
        project: req.project,
        types: types.filter(|v| !v.is_empty()),
        concepts: concepts.filter(|v| !v.is_empty()),
        files: files.filter(|v| !v.is_empty()),
        query: req.query.filter(|q| !q.is_empty()),
        date_start: req.date_start,
        date_end: req.date_end,
        limit,
    };

    let store = CorpusStore::default();
    let builder = CorpusBuilder::new();
    let corpus = {
        let conn = state.conn.lock().unwrap();
        builder
            .build(&conn, &store, name, &description, filter)
            .map_err(corpus_builder_error)?
    };
    Ok(Json(corpus_metadata(&corpus)))
}

/// GET /api/corpus — list every corpus.
pub async fn corpus_list(State(_state): State<AppState>) -> ApiResult<Value> {
    let store = CorpusStore::default();
    let entries = store.list().map_err(corpus_store_error)?;
    // Match TS' MCP-CallToolResult-shaped envelope. See TS CorpusRoutes.ts
    // `handleListCorpora` for the rationale (#1700).
    Ok(Json(json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".into())
        }]
    })))
}

/// GET /api/corpus/:name — return metadata sans `observations`.
pub async fn corpus_get(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Value> {
    let store = CorpusStore::default();
    let corpus = store
        .read(&name)
        .map_err(corpus_store_error)?
        .ok_or_else(|| corpus_not_found(&store, &name))?;
    Ok(Json(corpus_metadata(&corpus)))
}

/// DELETE /api/corpus/:name
pub async fn corpus_delete(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Value> {
    let store = CorpusStore::default();
    let existed = store.delete(&name).map_err(corpus_store_error)?;
    if !existed {
        return Err(corpus_not_found(&store, &name));
    }
    Ok(Json(json!({ "success": true })))
}

/// POST /api/corpus/:name/rebuild
pub async fn corpus_rebuild(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> ApiResult<Value> {
    let store = CorpusStore::default();
    let existing = store
        .read(&name)
        .map_err(corpus_store_error)?
        .ok_or_else(|| corpus_not_found(&store, &name))?;
    let builder = CorpusBuilder::new();
    let corpus = {
        let conn = state.conn.lock().unwrap();
        builder
            .build(
                &conn,
                &store,
                &name,
                &existing.description,
                existing.filter.clone(),
            )
            .map_err(corpus_builder_error)?
    };
    Ok(Json(corpus_metadata(&corpus)))
}

/// POST /api/corpus/:name/prime — feature-gated; returns 501 when off.
pub async fn corpus_prime(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    #[cfg(feature = "knowledge-agent")]
    {
        let store = CorpusStore::default();
        let mut corpus = store
            .read(&name)
            .map_err(corpus_store_error)?
            .ok_or_else(|| corpus_not_found(&store, &name))?;
        let agent = KnowledgeAgent::with_cli().map_err(knowledge_agent_error)?;
        let session_id = agent
            .prime(&mut corpus, &store)
            .await
            .map_err(knowledge_agent_error)?;
        Ok(Json(
            json!({ "session_id": session_id, "name": corpus.name }),
        ))
    }
    #[cfg(not(feature = "knowledge-agent"))]
    {
        let _ = name;
        Err(knowledge_agent_disabled())
    }
}

/// POST /api/corpus/:name/query — feature-gated; returns 501 when off.
pub async fn corpus_query(
    State(_state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<CorpusQueryRequest>,
) -> Result<Json<Value>, ApiError> {
    let question = req
        .question
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request(
                "Missing required field: question. Example: {\"question\":\"What changed?\"}",
            )
        })?;

    #[cfg(feature = "knowledge-agent")]
    {
        let store = CorpusStore::default();
        let mut corpus = store
            .read(&name)
            .map_err(corpus_store_error)?
            .ok_or_else(|| corpus_not_found(&store, &name))?;
        let agent = KnowledgeAgent::with_cli().map_err(knowledge_agent_error)?;
        let result = agent
            .query(&mut corpus, &store, question)
            .await
            .map_err(knowledge_agent_error)?;
        Ok(Json(
            json!({ "answer": result.answer, "session_id": result.session_id }),
        ))
    }
    #[cfg(not(feature = "knowledge-agent"))]
    {
        let _ = (name, question);
        Err(knowledge_agent_disabled())
    }
}

/// POST /api/corpus/:name/reprime — feature-gated; returns 501 when off.
pub async fn corpus_reprime(
    State(_state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    #[cfg(feature = "knowledge-agent")]
    {
        let store = CorpusStore::default();
        let mut corpus = store
            .read(&name)
            .map_err(corpus_store_error)?
            .ok_or_else(|| corpus_not_found(&store, &name))?;
        let agent = KnowledgeAgent::with_cli().map_err(knowledge_agent_error)?;
        let session_id = agent
            .reprime(&mut corpus, &store)
            .await
            .map_err(knowledge_agent_error)?;
        Ok(Json(
            json!({ "session_id": session_id, "name": corpus.name }),
        ))
    }
    #[cfg(not(feature = "knowledge-agent"))]
    {
        let _ = name;
        Err(knowledge_agent_disabled())
    }
}

// --- helpers --------------------------------------------------------------

fn corpus_metadata(corpus: &CorpusFile) -> Value {
    // Mirrors TS' `{ observations, ...metadata } = corpus` destructure.
    let mut obj = serde_json::to_value(corpus).unwrap_or(Value::Null);
    if let Some(map) = obj.as_object_mut() {
        map.remove("observations");
    }
    obj
}

fn corpus_not_found(store: &CorpusStore, name: &str) -> ApiError {
    let available: Vec<String> = store
        .list()
        .map(|entries| entries.into_iter().map(|e| e.name).collect())
        .unwrap_or_default();
    ApiError {
        status: StatusCode::NOT_FOUND,
        message: format!(
            "Corpus \"{name}\" not found. Available: {}",
            if available.is_empty() {
                "(none)".to_owned()
            } else {
                available.join(", ")
            }
        ),
    }
}

fn corpus_store_error(error: CorpusStoreError) -> ApiError {
    match error {
        CorpusStoreError::InvalidName => ApiError::bad_request(error.to_string()),
        other => ApiError::internal(other),
    }
}

fn corpus_builder_error(error: crate::knowledge::builder::CorpusBuilderError) -> ApiError {
    use crate::knowledge::builder::CorpusBuilderError;
    match error {
        CorpusBuilderError::Store(e) => corpus_store_error(e),
        other => ApiError::internal(other),
    }
}

#[cfg(feature = "knowledge-agent")]
fn knowledge_agent_error(error: KnowledgeAgentError) -> ApiError {
    match error {
        KnowledgeAgentError::NotPrimed(_) => ApiError::bad_request(error.to_string()),
        KnowledgeAgentError::ExecutableNotFound => ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: error.to_string(),
        },
        other => ApiError::internal(other),
    }
}

#[cfg(not(feature = "knowledge-agent"))]
fn knowledge_agent_disabled() -> ApiError {
    ApiError {
        status: StatusCode::NOT_IMPLEMENTED,
        message: "Knowledge agent endpoints are not available in this build. Rebuild claude-mem-worker with --features knowledge-agent to enable prime/query/reprime.".into(),
    }
}

fn coerce_string_array(
    value: Option<&Value>,
    field: &str,
) -> Result<Option<Vec<String>>, ApiError> {
    let Some(raw) = value else {
        return Ok(None);
    };
    match raw {
        Value::Null => Ok(None),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Some(text) = item.as_str() else {
                    return Err(ApiError::bad_request(format!(
                        "{field} must be an array of strings"
                    )));
                };
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    out.push(trimmed.to_owned());
                }
            }
            Ok(Some(out))
        }
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            // Try JSON array first, fall back to CSV.
            if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                return coerce_string_array(Some(&parsed), field);
            }
            let parts: Vec<String> = trimmed
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            Ok(Some(parts))
        }
        _ => Err(ApiError::bad_request(format!(
            "{field} must be an array of strings"
        ))),
    }
}

fn coerce_positive_int(value: Option<&Value>, field: &str) -> Result<Option<i64>, ApiError> {
    let Some(raw) = value else { return Ok(None) };
    match raw {
        Value::Null => Ok(None),
        Value::Number(n) => n
            .as_i64()
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or_else(|| ApiError::bad_request(format!("{field} must be a positive integer"))),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            trimmed
                .parse::<i64>()
                .ok()
                .filter(|n| *n > 0)
                .map(Some)
                .ok_or_else(|| ApiError::bad_request(format!("{field} must be a positive integer")))
        }
        _ => Err(ApiError::bad_request(format!(
            "{field} must be a positive integer"
        ))),
    }
}
