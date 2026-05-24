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
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};

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
        query
            .entry("format".into())
            .or_insert_with(|| "text".into());
        let data = self
            .worker
            .get_json("/api/search", query_pairs(query))
            .await?;
        if let Some(text) = data
            .get("content")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
        {
            return Ok(text_result(text));
        }
        json_text_result(&data)
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
        json_text_result(&data)
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
        json_text_result(&data)
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
        json_text_result(&data)
    }

    #[tool(
        description = "Search source files for matching file names, symbols, and lines. Returns compact folded context instead of full files. Params: query, path, max_results, file_pattern.",
        annotations(title = "Smart Search", read_only_hint = true)
    )]
    pub async fn smart_search(
        &self,
        Parameters(params): Parameters<SmartSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.query.trim().is_empty() {
            return Err(McpError::invalid_params("query must not be empty", None));
        }
        let root = params
            .path
            .map(PathBuf::from)
            .unwrap_or(std::env::current_dir().map_err(mcp_internal)?);
        let result = smart_search_files(
            &root,
            &params.query,
            params.max_results.unwrap_or(20).clamp(1, 100),
            params.file_pattern.as_deref(),
        )
        .map_err(mcp_internal)?;
        Ok(text_result(result))
    }

    #[tool(
        description = "Get a compact structural outline of a source file. Params: file_path.",
        annotations(title = "Smart Outline", read_only_hint = true)
    )]
    pub async fn smart_outline(
        &self,
        Parameters(params): Parameters<SmartOutlineParams>,
    ) -> Result<CallToolResult, McpError> {
        let path = PathBuf::from(params.file_path);
        let content = fs::read_to_string(&path).map_err(mcp_internal)?;
        Ok(text_result(format_outline(&path, &content)))
    }

    #[tool(
        description = "Build a knowledge corpus from filtered observations. Creates a queryable knowledge agent. Params: name (required), description, project, types (comma-separated), concepts (comma-separated), files (comma-separated), query, dateStart, dateEnd, limit.",
        annotations(
            title = "Build Corpus",
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    pub async fn build_corpus(
        &self,
        Parameters(params): Parameters<BuildCorpusParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.name.trim().is_empty() {
            return Err(McpError::invalid_params("name must not be empty", None));
        }
        let body = build_corpus_body(&params);
        let data = self.worker.post_json("/api/corpus", body).await?;
        json_text_result(&data)
    }

    #[tool(
        description = "List all knowledge corpora with their stats and priming status.",
        annotations(title = "List Corpora", read_only_hint = true)
    )]
    pub async fn list_corpora(&self) -> Result<CallToolResult, McpError> {
        let data = self.worker.get_json("/api/corpus", Vec::new()).await?;
        // Worker wraps the list in `{content:[{type:text,text:...}]}`; unwrap
        // when present so the MCP caller sees a clean JSON-encoded array.
        if let Some(text) = data
            .get("content")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
        {
            return Ok(text_result(text));
        }
        json_text_result(&data)
    }

    #[tool(
        description = "Prime a knowledge corpus — creates an AI session loaded with the corpus knowledge. Must be called before query_corpus.",
        annotations(
            title = "Prime Corpus",
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    pub async fn prime_corpus(
        &self,
        Parameters(params): Parameters<NameOnlyParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.name.trim().is_empty() {
            return Err(McpError::invalid_params("name must not be empty", None));
        }
        let path = format!("/api/corpus/{}/prime", urlencode(&params.name));
        let data = self.worker.post_json(&path, json!({})).await?;
        json_text_result(&data)
    }

    #[tool(
        description = "Ask a question to a primed knowledge corpus. The corpus must be primed first with prime_corpus. Params: name, question.",
        annotations(title = "Query Corpus", read_only_hint = true)
    )]
    pub async fn query_corpus(
        &self,
        Parameters(params): Parameters<QueryCorpusParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.name.trim().is_empty() {
            return Err(McpError::invalid_params("name must not be empty", None));
        }
        if params.question.trim().is_empty() {
            return Err(McpError::invalid_params("question must not be empty", None));
        }
        let path = format!("/api/corpus/{}/query", urlencode(&params.name));
        let data = self
            .worker
            .post_json(&path, json!({ "question": params.question }))
            .await?;
        json_text_result(&data)
    }

    #[tool(
        description = "Rebuild a knowledge corpus from its stored filter — re-runs the search to refresh with new observations. Does not re-prime the session.",
        annotations(
            title = "Rebuild Corpus",
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    pub async fn rebuild_corpus(
        &self,
        Parameters(params): Parameters<NameOnlyParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.name.trim().is_empty() {
            return Err(McpError::invalid_params("name must not be empty", None));
        }
        let path = format!("/api/corpus/{}/rebuild", urlencode(&params.name));
        let data = self.worker.post_json(&path, json!({})).await?;
        json_text_result(&data)
    }

    #[tool(
        description = "Create a fresh knowledge agent session for a corpus, clearing prior Q&A context. Use when conversation has drifted or after rebuilding.",
        annotations(
            title = "Reprime Corpus",
            destructive_hint = false,
            idempotent_hint = false
        )
    )]
    pub async fn reprime_corpus(
        &self,
        Parameters(params): Parameters<NameOnlyParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.name.trim().is_empty() {
            return Err(McpError::invalid_params("name must not be empty", None));
        }
        let path = format!("/api/corpus/{}/reprime", urlencode(&params.name));
        let data = self.worker.post_json(&path, json!({})).await?;
        json_text_result(&data)
    }

    #[tool(
        description = "Expand a specific symbol from a source file. Params: file_path, symbol_name.",
        annotations(title = "Smart Unfold", read_only_hint = true)
    )]
    pub async fn smart_unfold(
        &self,
        Parameters(params): Parameters<SmartUnfoldParams>,
    ) -> Result<CallToolResult, McpError> {
        if params.symbol_name.trim().is_empty() {
            return Err(McpError::invalid_params(
                "symbol_name must not be empty",
                None,
            ));
        }
        let path = PathBuf::from(params.file_path);
        let content = fs::read_to_string(&path).map_err(mcp_internal)?;
        Ok(text_result(
            unfold_symbol(&content, &params.symbol_name).unwrap_or_else(|| {
                format!(
                    "No symbol named {} found in {}",
                    params.symbol_name,
                    path.display()
                )
            }),
        ))
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

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SmartSearchParams {
    pub query: String,
    pub path: Option<String>,
    pub max_results: Option<usize>,
    pub file_pattern: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SmartOutlineParams {
    pub file_path: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SmartUnfoldParams {
    pub file_path: String,
    pub symbol_name: String,
}

/// Build-corpus arguments. Mirrors the TS MCP `build_corpus` input schema —
/// list fields accept either JSON arrays or comma-separated strings.
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct BuildCorpusParams {
    pub name: String,
    pub description: Option<String>,
    pub project: Option<String>,
    pub types: Option<String>,
    pub concepts: Option<String>,
    pub files: Option<String>,
    pub query: Option<String>,
    #[serde(rename = "dateStart")]
    pub date_start: Option<String>,
    #[serde(rename = "dateEnd")]
    pub date_end: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct NameOnlyParams {
    pub name: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct QueryCorpusParams {
    pub name: String,
    pub question: String,
}

fn build_corpus_body(params: &BuildCorpusParams) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("name".into(), Value::String(params.name.clone()));
    if let Some(d) = &params.description {
        body.insert("description".into(), Value::String(d.clone()));
    }
    if let Some(p) = &params.project {
        body.insert("project".into(), Value::String(p.clone()));
    }
    if let Some(t) = &params.types {
        body.insert("types".into(), Value::String(t.clone()));
    }
    if let Some(c) = &params.concepts {
        body.insert("concepts".into(), Value::String(c.clone()));
    }
    if let Some(f) = &params.files {
        body.insert("files".into(), Value::String(f.clone()));
    }
    if let Some(q) = &params.query {
        body.insert("query".into(), Value::String(q.clone()));
    }
    if let Some(d) = &params.date_start {
        body.insert("date_start".into(), Value::String(d.clone()));
    }
    if let Some(d) = &params.date_end {
        body.insert("date_end".into(), Value::String(d.clone()));
    }
    if let Some(l) = params.limit {
        body.insert("limit".into(), Value::Number(l.into()));
    }
    Value::Object(body)
}

/// Minimal percent-encoder for a corpus name segment. The route validator
/// rejects path separators upstream, so this only has to handle the small
/// surface of characters the regex allows + any defensive escaping the HTTP
/// client may need.
fn urlencode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{byte:02X}"));
            }
        }
    }
    out
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

fn smart_search_files(
    root: &Path,
    query: &str,
    max_results: usize,
    file_pattern: Option<&str>,
) -> anyhow::Result<String> {
    let root = root.canonicalize()?;
    let terms = query
        .to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut matches = Vec::new();
    let mut scanned = 0usize;
    walk_source_files(&root, 20, &mut |path| {
        if matches.len() >= max_results {
            return;
        }
        let relative = path.strip_prefix(&root).unwrap_or(path);
        let rel_text = relative.to_string_lossy();
        if file_pattern.is_some_and(|pattern| !rel_text.contains(pattern)) {
            return;
        }
        let Ok(content) = fs::read_to_string(path) else {
            return;
        };
        if content.contains('\0') || content.len() > 512 * 1024 {
            return;
        }
        scanned += 1;
        let lower_path = rel_text.to_lowercase();
        let lower_content = content.to_lowercase();
        let score = terms
            .iter()
            .map(|term| {
                usize::from(lower_path.contains(term)) * 5
                    + usize::from(lower_content.contains(term))
            })
            .sum::<usize>();
        if score == 0 {
            return;
        }
        matches.push((score, path.to_path_buf(), content));
    })?;
    matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut out = vec![format!(
        "Smart Search: \"{}\"\nScanned {} files, showing {} matches\n",
        query,
        scanned,
        matches.len().min(max_results)
    )];
    for (_score, path, content) in matches.into_iter().take(max_results) {
        out.push(format_outline(&path, &content));
    }
    Ok(out.join("\n\n"))
}

fn walk_source_files(
    dir: &Path,
    depth: usize,
    visit: &mut impl FnMut(&Path),
) -> anyhow::Result<()> {
    if depth == 0 {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if !ignored_dir(&path) {
                let _ = walk_source_files(&path, depth - 1, visit);
            }
        } else if is_source_file(&path) {
            visit(&path);
        }
    }
    Ok(())
}

fn format_outline(path: &Path, content: &str) -> String {
    let symbols = extract_symbols(content);
    let language = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("text");
    let mut out = vec![format!(
        "{} ({language}, {} lines)",
        path.display(),
        content.lines().count()
    )];
    if symbols.is_empty() {
        out.push("  No symbols detected; showing first matching lines only.".to_owned());
        for (idx, line) in content.lines().take(12).enumerate() {
            out.push(format!("{:>5}: {}", idx + 1, line.trim_end()));
        }
        return out.join("\n");
    }
    for symbol in symbols {
        out.push(format!(
            "  {:>5}-{:>5} {} {}",
            symbol.start, symbol.end, symbol.kind, symbol.name
        ));
        out.push(format!("        {}", symbol.signature.trim()));
    }
    out.join("\n")
}

fn unfold_symbol(content: &str, symbol_name: &str) -> Option<String> {
    let symbols = extract_symbols(content);
    let symbol = symbols.into_iter().find(|symbol| {
        symbol.name == symbol_name || symbol.name.ends_with(&format!(".{symbol_name}"))
    })?;
    let lines = content.lines().collect::<Vec<_>>();
    let start = symbol.start.saturating_sub(1);
    let end = symbol.end.min(lines.len());
    Some(lines[start..end].join("\n"))
}

#[derive(Debug, Clone)]
struct Symbol {
    name: String,
    kind: &'static str,
    signature: String,
    start: usize,
    end: usize,
}

fn extract_symbols(content: &str) -> Vec<Symbol> {
    let lines = content.lines().collect::<Vec<_>>();
    let mut symbols = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let Some((kind, name)) = symbol_from_line(trimmed) else {
            continue;
        };
        let end = find_symbol_end(&lines, idx);
        symbols.push(Symbol {
            name,
            kind,
            signature: trimmed.to_owned(),
            start: idx + 1,
            end,
        });
    }
    symbols
}

fn symbol_from_line(line: &str) -> Option<(&'static str, String)> {
    let prefixes = [
        ("fn ", "function"),
        ("pub fn ", "function"),
        ("async fn ", "function"),
        ("pub async fn ", "function"),
        ("function ", "function"),
        ("class ", "class"),
        ("export class ", "class"),
        ("struct ", "struct"),
        ("pub struct ", "struct"),
        ("enum ", "enum"),
        ("pub enum ", "enum"),
        ("trait ", "trait"),
        ("pub trait ", "trait"),
        ("impl ", "impl"),
        ("def ", "function"),
        ("interface ", "interface"),
        ("type ", "type"),
        ("const ", "const"),
    ];
    for (prefix, kind) in prefixes {
        if let Some(rest) = line.strip_prefix(prefix) {
            let name = rest
                .split(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == ':'))
                .find(|part| !part.is_empty())?
                .to_owned();
            return Some((kind, name));
        }
    }
    None
}

fn find_symbol_end(lines: &[&str], start: usize) -> usize {
    let mut depth = 0isize;
    let mut saw_open = false;
    for (idx, line) in lines.iter().enumerate().skip(start) {
        for ch in line.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    saw_open = true;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        if saw_open && depth <= 0 && idx > start {
            return idx + 1;
        }
        if !saw_open && idx > start && !line.starts_with(' ') && !line.starts_with('\t') {
            return idx;
        }
    }
    lines.len()
}

fn is_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some(
            "rs" | "ts"
                | "tsx"
                | "js"
                | "jsx"
                | "mjs"
                | "cjs"
                | "py"
                | "go"
                | "java"
                | "rb"
                | "cpp"
                | "cc"
                | "cxx"
                | "c"
                | "h"
                | "hpp"
                | "swift"
                | "kt"
                | "kts"
                | "php"
                | "ex"
                | "exs"
                | "lua"
                | "scala"
                | "sh"
                | "bash"
                | "zsh"
                | "zig"
                | "toml"
                | "yaml"
                | "yml"
                | "sql"
                | "md"
                | "mdx"
        )
    )
}

fn ignored_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(
            ".git"
                | "node_modules"
                | "target"
                | "dist"
                | "build"
                | ".next"
                | "__pycache__"
                | ".venv"
                | "venv"
                | ".cache"
                | ".turbo"
                | "coverage"
        )
    )
}
