pub mod handlers;

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;

const SUCCESS: i32 = 0;

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

    async fn get_text(&self, path: &str) -> Result<Option<String>> {
        let url = format!("{}{}", self.base_url, path);
        let response = match self.client.get(url).send().await {
            Ok(response) => response,
            Err(_) => return Ok(None),
        };
        if !response.status().is_success() {
            return Ok(None);
        }
        Ok(Some(response.text().await?))
    }

    async fn post_json(&self, path: &str, body: Value) -> Result<Option<Value>> {
        let url = format!("{}{}", self.base_url, path);
        let response = match self.client.post(url).json(&body).send().await {
            Ok(response) => response,
            Err(_) => return Ok(None),
        };
        if !response.status().is_success() {
            return Ok(None);
        }
        Ok(Some(response.json::<Value>().await?))
    }

    async fn healthy(&self) -> bool {
        matches!(self.get_text("/api/health").await, Ok(Some(_)))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalizedHookInput {
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub prompt: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    pub tool_response: Option<Value>,
    pub transcript_path: Option<String>,
    pub platform: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HookSpecificOutput {
    pub hook_event_name: String,
    pub additional_context: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HookOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<HookSpecificOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookExecution {
    pub exit_code: i32,
    pub output: HookOutput,
}

pub async fn run_hook_from_env() -> Result<i32> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "hook") {
        args.remove(0);
    }
    let platform = args.first().ok_or_else(|| anyhow!("missing platform"))?;
    let event = args.get(1).ok_or_else(|| anyhow!("missing event"))?;

    let mut stdin = String::new();
    std::io::stdin().read_to_string(&mut stdin)?;
    let raw = if stdin.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(&stdin).context("failed to parse hook stdin JSON")?
    };

    let execution = execute_hook(platform, event, raw, &WorkerClient::from_env()).await?;
    println!("{}", serde_json::to_string(&execution.output)?);
    Ok(execution.exit_code)
}

pub async fn execute_hook(
    platform: &str,
    event: &str,
    raw_input: Value,
    worker: &WorkerClient,
) -> Result<HookExecution> {
    let input = normalize_input(platform, raw_input);
    let output = match event {
        "context" => context_handler(&input, worker).await?,
        "session-init" => session_init_handler(&input, worker).await?,
        "observation" => observation_handler(&input, worker).await?,
        "session-complete" => session_complete_handler(&input, worker).await?,
        "user-message" => user_message_handler(&input, worker).await?,
        _ => return Err(anyhow!("unknown hook event: {event}")),
    };
    Ok(HookExecution {
        exit_code: SUCCESS,
        output,
    })
}

fn normalize_input(platform: &str, raw: Value) -> NormalizedHookInput {
    let field = |snake: &str, camel: &str| -> Option<String> {
        raw.get(snake)
            .or_else(|| raw.get(camel))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    NormalizedHookInput {
        session_id: field("session_id", "sessionId").or_else(|| field("id", "id")),
        cwd: field("cwd", "cwd").or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string())
        }),
        prompt: field("prompt", "prompt"),
        tool_name: field("tool_name", "toolName"),
        tool_input: raw
            .get("tool_input")
            .or_else(|| raw.get("toolInput"))
            .cloned(),
        tool_response: raw
            .get("tool_response")
            .or_else(|| raw.get("toolResponse"))
            .cloned(),
        transcript_path: field("transcript_path", "transcriptPath"),
        platform: platform.to_owned(),
    }
}

async fn context_handler(input: &NormalizedHookInput, worker: &WorkerClient) -> Result<HookOutput> {
    if !worker.healthy().await {
        return Ok(context_output("SessionStart", ""));
    }
    let project = project_name(input.cwd.as_deref());
    let path = format!(
        "/api/context/inject?project={}&platformSource=claude",
        encode_component(&project)
    );
    let context = worker.get_text(&path).await?.unwrap_or_default();
    Ok(context_output("SessionStart", context.trim()))
}

async fn session_init_handler(
    input: &NormalizedHookInput,
    worker: &WorkerClient,
) -> Result<HookOutput> {
    if !worker.healthy().await {
        return Ok(HookOutput::default());
    }
    let Some(session_id) = input.session_id.as_deref() else {
        return Ok(HookOutput::default());
    };
    let prompt = input
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty())
        .unwrap_or("[media prompt]");
    let project = project_name(input.cwd.as_deref());
    let _ = worker
        .post_json(
            "/api/sessions/init",
            json!({
                "contentSessionId": session_id,
                "project": project,
                "prompt": prompt,
                "platformSource": "claude"
            }),
        )
        .await?;

    if prompt.len() < 20 || prompt == "[media prompt]" {
        return Ok(HookOutput::default());
    }
    let semantic = worker
        .post_json(
            "/api/context/semantic",
            json!({ "q": prompt, "project": project, "limit": 5 }),
        )
        .await?;
    let additional_context = semantic
        .and_then(|value| {
            value
                .get("context")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default();
    if additional_context.trim().is_empty() {
        Ok(HookOutput::default())
    } else {
        Ok(context_output(
            "UserPromptSubmit",
            additional_context.trim(),
        ))
    }
}

async fn observation_handler(
    input: &NormalizedHookInput,
    worker: &WorkerClient,
) -> Result<HookOutput> {
    if !worker.healthy().await {
        return Ok(HookOutput::default());
    }
    let Some(session_id) = input.session_id.as_deref() else {
        return Ok(HookOutput::default());
    };
    if input.tool_name.as_deref().unwrap_or_default().is_empty() {
        return Ok(HookOutput::default());
    }
    let _ = worker
        .post_json(
            "/api/sessions/observations",
            json!({
                "contentSessionId": session_id,
                "platformSource": "claude",
                "tool_name": input.tool_name,
                "tool_input": input.tool_input,
                "tool_response": input.tool_response,
                "cwd": input.cwd,
            }),
        )
        .await?;
    Ok(HookOutput::default())
}

async fn session_complete_handler(
    input: &NormalizedHookInput,
    worker: &WorkerClient,
) -> Result<HookOutput> {
    if !worker.healthy().await {
        return Ok(HookOutput::default());
    }
    let Some(session_id) = input.session_id.as_deref() else {
        return Ok(HookOutput::default());
    };
    let _ = worker
        .post_json(
            "/api/sessions/complete",
            json!({ "contentSessionId": session_id, "platformSource": "claude" }),
        )
        .await?;
    Ok(HookOutput::default())
}

async fn user_message_handler(
    input: &NormalizedHookInput,
    worker: &WorkerClient,
) -> Result<HookOutput> {
    let _ = context_handler(input, worker).await?;
    Ok(HookOutput::default())
}

fn context_output(event: &str, context: &str) -> HookOutput {
    HookOutput {
        hook_specific_output: Some(HookSpecificOutput {
            hook_event_name: event.to_owned(),
            additional_context: context.to_owned(),
        }),
        system_message: None,
    }
}

fn project_name(cwd: Option<&str>) -> String {
    cwd.and_then(|cwd| Path::new(cwd).file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown")
        .to_owned()
}

fn encode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
