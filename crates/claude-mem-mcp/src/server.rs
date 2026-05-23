//! rmcp tool router: maps MCP tools to worker HTTP endpoints.

use anyhow::Result;
use reqwest::Client;
use rmcp::{
    handler::server::tool::{Parameters, ToolRouter},
    model::*,
    tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;

const IMPORTANT_TEXT: &str = r#"# Memory Search Workflow

1. search(query="...", limit=20, project="...") gets a compact index with IDs.
2. timeline(anchor=<ID>, depth_before=3, depth_after=3) gets nearby context.
3. get_observations(ids=[...]) fetches full details only for filtered IDs.

Use save_memory(text="...", project="...", title="...") only when the user explicitly asks you to remember something.
"#;

#[derive(Debug, Clone)]
pub struct WorkerClient {
    base_url: String,
    client: Client,
}

impl WorkerClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            client: Client::new(),
        }
    }

    pub fn from_env() -> Self {
        if let Ok(url) = std::env::var("CLAUDE_MEM_WORKER_URL") {
            return Self::new(url);
        }
        let host = std::env::var("CLAUDE_MEM_WORKER_HOST").unwrap_or_else(|_| "127.0.0.1".into());
        let port = std::env::var("CLAUDE_MEM_WORKER_PORT").unwrap_or_else(|_| "37777".into());
        Self::new(format!("http://{}:{}", host, port))
    }

    async fn get_json(&self, path: &str, query: Vec<(String, String)>) -> Result<Value, McpError> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .get(url)
            .query(&query)
            .send()
            .await
            .map_err(mcp_internal)?;
        response_json(response).await
    }

    async fn post_json(&self, path: &str, body: Value) -> Result<Value, McpError> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(mcp_internal)?;
        response_json(response).await
    }

    async fn healthy(&self) -> bool {
        self.get_json("/api/health", Vec::new()).await.is_ok()
    }
}

async fn response_json(response: reqwest::Response) -> Result<Value, McpError> {
    let status = response.status();
    let text = response.text().await.map_err(mcp_internal)?;
    if !status.is_success() {
        return Err(McpError::internal_error(
            format!("worker HTTP {status}: {text}"),
            None,
        ));
    }
    serde_json::from_str(&text).map_err(mcp_internal)
}

#[derive(Clone)]
pub struct ClaudeMemMcp {
    worker: WorkerClient,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ClaudeMemMcp {
    pub fn new(worker: WorkerClient) -> Self {
        Self {
            worker,
            tool_router: Self::tool_router(),
        }
    }

    pub fn from_env() -> Self {
        Self::new(WorkerClient::from_env())
    }

    #[tool(
        name = "__IMPORTANT",
        description = "Required claude-mem workflow: search first, then timeline, then fetch selected observations.",
        annotations(title = "Memory Workflow", read_only_hint = true)
    )]
    pub async fn important(&self) -> Result<CallToolResult, McpError> {
        Ok(text_result(IMPORTANT_TEXT))
    }

    #[tool(
        description = "Step 1: Search memory. Returns a compact index with observation IDs. Params: query, q, limit, project, type, obs_type, dateStart, dateEnd, offset, orderBy.",
        annotations(title = "Search Memory", read_only_hint = true)
    )]
    pub async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut query = params.extra;
        if let Some(value) = params.query.or(params.q) {
            query.insert("query".into(), value);
        }
        insert_optional(&mut query, "project", params.project);
        insert_optional_number(&mut query, "limit", params.limit);
        let data = self
            .worker
            .get_json("/api/search", query_pairs(query))
            .await?;
        Ok(json_text_result(&data)?)
    }

    #[tool(
        description = "Step 2: Get chronological context around an observation. Params: anchor or query, depth_before, depth_after, project.",
        annotations(title = "Memory Timeline", read_only_hint = true)
    )]
    pub async fn timeline(
        &self,
        Parameters(params): Parameters<TimelineParams>,
    ) -> Result<CallToolResult, McpError> {
        let mut query = params.extra;
        insert_optional_number(&mut query, "anchor", params.anchor);
        if let Some(value) = params.query.or(params.q) {
            query.insert("query".into(), value);
        }
        insert_optional(&mut query, "project", params.project);
        insert_optional_number(&mut query, "depth_before", params.depth_before);
        insert_optional_number(&mut query, "depth_after", params.depth_after);
        let data = self
            .worker
            .get_json("/api/timeline", query_pairs(query))
            .await?;
        Ok(json_text_result(&data)?)
    }

    #[tool(
        description = "Step 3: Fetch full observation details for filtered IDs. Params: ids.",
        annotations(title = "Get Observations", read_only_hint = true)
    )]
    pub async fn get_observations(
        &self,
        Parameters(params): Parameters<GetObservationsParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.ids.is_empty() {
            return Err(McpError::invalid_params("ids must not be empty", None));
        }
        let data = self
            .worker
            .post_json("/api/observations/batch", json!({ "ids": params.ids }))
            .await?;
        Ok(json_text_result(&data)?)
    }

    #[tool(
        description = "Save a manual memory. Params: text, title, project.",
        annotations(
            title = "Save Memory",
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    pub async fn save_memory(
        &self,
        Parameters(params): Parameters<SaveMemoryParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.text.trim().is_empty() {
            return Err(McpError::invalid_params("text must not be empty", None));
        }
        let data = self
            .worker
            .post_json(
                "/api/memory/save",
                json!({
                    "text": params.text,
                    "title": params.title,
                    "project": params.project,
                }),
            )
            .await?;
        Ok(json_text_result(&data)?)
    }

    pub async fn worker_ready(&self) -> bool {
        self.worker.healthy().await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ClaudeMemMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "claude-mem".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            instructions: Some(
                "Use __IMPORTANT, then search, timeline, and get_observations for efficient memory recall."
                    .into(),
            ),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SearchParams {
    pub query: Option<String>,
    pub q: Option<String>,
    pub project: Option<String>,
    pub limit: Option<i64>,
    #[serde(flatten)]
    pub extra: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct TimelineParams {
    pub anchor: Option<i64>,
    pub query: Option<String>,
    pub q: Option<String>,
    pub project: Option<String>,
    pub depth_before: Option<i64>,
    pub depth_after: Option<i64>,
    #[serde(flatten)]
    pub extra: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct GetObservationsParams {
    pub ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SaveMemoryParams {
    pub text: String,
    pub title: Option<String>,
    pub project: Option<String>,
}

pub async fn run_stdio() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "claude_mem_mcp=info".into()),
        )
        .try_init()
        .ok();
    let service = ClaudeMemMcp::from_env()
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

fn text_result(text: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(text)])
}

fn json_text_result(value: &Value) -> Result<CallToolResult, McpError> {
    let text = serde_json::to_string_pretty(value).map_err(mcp_internal)?;
    Ok(text_result(text))
}

fn insert_optional(query: &mut HashMap<String, String>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        query.insert(key.into(), value);
    }
}

fn insert_optional_number(
    query: &mut HashMap<String, String>,
    key: &str,
    value: Option<impl ToString>,
) {
    if let Some(value) = value {
        query.insert(key.into(), value.to_string());
    }
}

fn query_pairs(query: HashMap<String, String>) -> Vec<(String, String)> {
    query.into_iter().collect()
}

fn mcp_internal(error: impl std::fmt::Display) -> McpError {
    McpError::internal_error(error.to_string(), None)
}
