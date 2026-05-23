pub mod data;
pub mod logs;
pub mod memory;
pub mod search;
pub mod session;
pub mod settings;
pub mod viewer;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use claude_mem_core::context::formatters::{format_observation, FormatOptions};
use claude_mem_core::context::observation_compiler::{query_observations, ObservationQuery};
use claude_mem_core::db::observations::get::{
    get_observation_by_id, get_observations_by_file_path, get_observations_by_ids,
};
use claude_mem_core::db::prompts::{
    get_prompt_number_from_user_prompts, save_user_prompt, PromptInput,
};
use claude_mem_core::db::sessions::{
    create_session, get_session_by_content_id, mark_session_completed, update_memory_session_id,
};
use claude_mem_core::db::transactions::store_batch;
use claude_mem_core::shared::tag_stripping::strip_private_tags;
use claude_mem_core::types::session::CreateSessionInput;
use claude_mem_core::types::ObservationInput;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

use super::router::AppState;
use crate::search::result_formatter::{ResultFormatter, SearchResults};

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

    save_user_prompt(
        &conn,
        &PromptInput {
            content_session_id: req.content_session_id,
            prompt_number,
            prompt_text: cleaned_prompt,
            created_at,
            created_at_epoch,
        },
    )
    .map_err(ApiError::internal)?;

    Ok(Json(SessionInitResponse {
        session_db_id: session.id,
        prompt_number,
        skipped: false,
        reason: None,
        context_injected: session.memory_session_id.is_some(),
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

    let conn = state.conn.lock().unwrap();
    let session = get_session_by_content_id(&conn, &req.content_session_id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::bad_request("unknown contentSessionId"))?;
    let memory_session_id = match session.memory_session_id {
        Some(id) => id,
        None => {
            let generated = format!("rust-local-memory:{}", req.content_session_id);
            update_memory_session_id(&conn, &req.content_session_id, &generated)
                .map_err(ApiError::internal)?;
            generated
        }
    };
    let project = if session.project.is_empty() {
        req.cwd.unwrap_or_else(|| "unknown".into())
    } else {
        session.project
    };
    let prompt_number = get_prompt_number_from_user_prompts(&conn, &req.content_session_id)
        .map_err(ApiError::internal)?;
    let (created_at, created_at_epoch) = now_timestamp();
    let narrative = format!(
        "Claude tool `{}` ran with input {} and response {}",
        tool_name,
        compact_json(req.tool_input.as_ref()),
        compact_json(req.tool_response.as_ref())
    );
    let observation = ObservationInput {
        r#type: "discovery".into(),
        title: Some(format!("{} tool use", tool_name)),
        subtitle: Some("Claude Code PostToolUse".into()),
        narrative: Some(strip_private_tags(&narrative).into_owned()),
        facts: Some(vec![format!("Tool: {}", tool_name)]),
        concepts: Some(vec!["claude-code".into(), "tool-use".into()]),
        created_at,
        created_at_epoch,
        generated_by_model: Some("rust-local".into()),
        ..Default::default()
    };
    let result = store_batch(
        &conn,
        &memory_session_id,
        &project,
        &[observation],
        None,
        Some(prompt_number),
        Some(0),
        Some(created_at_epoch),
    )
    .map_err(ApiError::internal)?;

    Ok(Json(json!({
        "success": true,
        "memorySessionId": memory_session_id,
        "observationIds": result.observation_ids,
        "inserted": result.inserted,
        "duplicates": result.duplicates
    })))
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
    let conn = state.conn.lock().unwrap();
    if let Some(session) =
        get_session_by_content_id(&conn, &req.content_session_id).map_err(ApiError::internal)?
    {
        mark_session_completed(&conn, session.id).map_err(ApiError::internal)?;
        Ok(Json(json!({ "success": true, "completed": true })))
    } else {
        Ok(Json(json!({ "success": true, "completed": false })))
    }
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

    let title = req
        .title
        .unwrap_or_else(|| text.chars().take(60).collect::<String>());
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
    let result = store_batch(
        &conn,
        &memory_session_id,
        &project,
        &[observation],
        None,
        Some(0),
        Some(0),
        Some(created_at_epoch),
    )
    .map_err(ApiError::internal)?;
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
    let for_human = query.get("colors").is_some_and(|v| v == "true");
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
        for observation in observations {
            sections.push(format_observation(
                &observation,
                &FormatOptions {
                    for_human,
                    ..Default::default()
                },
            ));
        }
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
    let context = rows
        .iter()
        .map(|row| format_observation(row, &FormatOptions::default()))
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok(Json(json!({ "context": context, "count": rows.len() })))
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
    let project = query.get("project").map(String::as_str);
    let limit = parse_limit(query.get("limit"), 20);
    let rows = if q.trim().is_empty() {
        let conn = state.conn.lock().unwrap();
        query_observations(
            &conn,
            &ObservationQuery {
                project: project.map(str::to_owned),
                limit,
            },
        )
        .map_err(ApiError::internal)?
    } else {
        search_observations(&state, &q, project, limit)?
    };
    if query.get("format").is_some_and(|format| format == "text") {
        let text = ResultFormatter::new().format_search_results(
            &SearchResults {
                observations: rows,
                sessions: Vec::new(),
                prompts: Vec::new(),
            },
            &q,
            false,
        );
        return Ok(Json(
            json!({ "content": [{ "type": "text", "text": text }] }),
        ));
    }
    Ok(Json(json!({ "observations": rows, "count": rows.len() })))
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

fn compact_json(value: Option<&Value>) -> String {
    value
        .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "null".into()))
        .unwrap_or_else(|| "null".into())
}
