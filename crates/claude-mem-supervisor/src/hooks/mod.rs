pub mod handlers;

use anyhow::{Context, Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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
    #[serde(skip_serializing_if = "String::is_empty")]
    pub hook_event_name: String,
    pub additional_context: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HookOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#continue: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppress_output: Option<bool>,
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
        "summarize" => summarize_handler(&input, worker).await?,
        "user-message" => user_message_handler(&input, worker).await?,
        _ => return Err(anyhow!("unknown hook event: {event}")),
    };
    Ok(HookExecution {
        exit_code: SUCCESS,
        output: format_output(platform, output),
    })
}

fn normalize_input(platform: &str, raw: Value) -> NormalizedHookInput {
    match platform {
        "cursor" | "cursor-agent" => normalize_cursor_input(platform, raw),
        "gemini" | "gemini-cli" => normalize_gemini_input(platform, raw),
        "codex" | "raw" => normalize_raw_input(platform, raw),
        _ => normalize_raw_input(platform, raw),
    }
}

fn normalize_raw_input(platform: &str, raw: Value) -> NormalizedHookInput {
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

fn normalize_cursor_input(platform: &str, raw: Value) -> NormalizedHookInput {
    let cwd = raw
        .get("workspace_roots")
        .and_then(Value::as_array)
        .and_then(|roots| roots.first())
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| raw.get("cwd").and_then(Value::as_str).map(str::to_owned))
        .or_else(default_cwd);
    let command = raw.get("command").and_then(Value::as_str);
    let is_shell_command = command.is_some() && raw.get("tool_name").is_none();
    NormalizedHookInput {
        session_id: raw
            .get("conversation_id")
            .or_else(|| raw.get("generation_id"))
            .or_else(|| raw.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        cwd,
        prompt: raw
            .get("prompt")
            .or_else(|| raw.get("query"))
            .or_else(|| raw.get("input"))
            .or_else(|| raw.get("message"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        tool_name: if is_shell_command {
            Some("Bash".into())
        } else {
            raw.get("tool_name")
                .and_then(Value::as_str)
                .map(str::to_owned)
        },
        tool_input: if is_shell_command {
            Some(json!({ "command": command.unwrap_or_default() }))
        } else {
            raw.get("tool_input").cloned()
        },
        tool_response: if is_shell_command {
            Some(json!({ "output": raw.get("output").cloned().unwrap_or(Value::Null) }))
        } else {
            raw.get("result_json").cloned()
        },
        transcript_path: None,
        platform: platform.to_owned(),
    }
}

fn normalize_gemini_input(platform: &str, raw: Value) -> NormalizedHookInput {
    let hook_event_name = raw.get("hook_event_name").and_then(Value::as_str);
    let mut tool_name = raw
        .get("tool_name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut tool_input = raw.get("tool_input").cloned();
    let mut tool_response = raw.get("tool_response").cloned();

    if hook_event_name == Some("AfterAgent") && raw.get("prompt_response").is_some() {
        tool_name.get_or_insert_with(|| "GeminiAgent".into());
        tool_input.get_or_insert_with(
            || json!({ "prompt": raw.get("prompt").cloned().unwrap_or(Value::Null) }),
        );
        tool_response.get_or_insert_with(
            || json!({ "response": raw.get("prompt_response").cloned().unwrap_or(Value::Null) }),
        );
    }
    if hook_event_name == Some("BeforeTool") && tool_name.is_some() && tool_response.is_none() {
        tool_response = Some(json!({ "_preExecution": true }));
    }
    if hook_event_name == Some("Notification") {
        tool_name.get_or_insert_with(|| "GeminiNotification".into());
        tool_input.get_or_insert_with(|| {
            json!({
                "notification_type": raw.get("notification_type").cloned().unwrap_or(Value::Null),
                "message": raw.get("message").cloned().unwrap_or(Value::Null)
            })
        });
        tool_response.get_or_insert_with(
            || json!({ "details": raw.get("details").cloned().unwrap_or(Value::Null) }),
        );
    }

    NormalizedHookInput {
        session_id: raw
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| std::env::var("GEMINI_SESSION_ID").ok()),
        cwd: raw
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| std::env::var("GEMINI_CWD").ok())
            .or_else(|| std::env::var("GEMINI_PROJECT_DIR").ok())
            .or_else(|| std::env::var("CLAUDE_PROJECT_DIR").ok())
            .or_else(default_cwd),
        prompt: raw.get("prompt").and_then(Value::as_str).map(str::to_owned),
        tool_name,
        tool_input,
        tool_response,
        transcript_path: raw
            .get("transcript_path")
            .and_then(Value::as_str)
            .map(str::to_owned),
        platform: platform.to_owned(),
    }
}

async fn context_handler(input: &NormalizedHookInput, worker: &WorkerClient) -> Result<HookOutput> {
    if !worker.healthy().await {
        return Ok(context_output("SessionStart", ""));
    }
    let project = project_name(input.cwd.as_deref());
    let path = format!(
        "/api/context/inject?project={}&platformSource={}",
        encode_component(&project),
        encode_component(platform_source(&input.platform))
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
                "platformSource": platform_source(&input.platform)
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
                "platformSource": platform_source(&input.platform),
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
            json!({ "contentSessionId": session_id, "platformSource": platform_source(&input.platform) }),
        )
        .await?;
    Ok(HookOutput::default())
}

async fn summarize_handler(
    input: &NormalizedHookInput,
    worker: &WorkerClient,
) -> Result<HookOutput> {
    if !worker.healthy().await {
        return Ok(HookOutput::default());
    }
    let Some(session_id) = input.session_id.as_deref() else {
        return Ok(HookOutput::default());
    };
    let source = input
        .tool_response
        .as_ref()
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| input.prompt.clone());
    let _ = worker
        .post_json(
            "/api/sessions/summarize",
            json!({
                "contentSessionId": session_id,
                "summary": source,
            }),
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
        r#continue: None,
        suppress_output: None,
        hook_specific_output: Some(HookSpecificOutput {
            hook_event_name: event.to_owned(),
            additional_context: context.to_owned(),
        }),
        system_message: None,
    }
}

fn format_output(platform: &str, output: HookOutput) -> HookOutput {
    if matches!(platform, "gemini" | "gemini-cli") {
        HookOutput {
            r#continue: Some(true),
            suppress_output: output.suppress_output,
            hook_specific_output: output
                .hook_specific_output
                .map(|context| HookSpecificOutput {
                    hook_event_name: String::new(),
                    additional_context: strip_ansi(&context.additional_context),
                }),
            system_message: output.system_message.map(|message| strip_ansi(&message)),
        }
    } else if matches!(platform, "cursor" | "cursor-agent") {
        HookOutput {
            r#continue: Some(true),
            suppress_output: None,
            hook_specific_output: None,
            system_message: None,
        }
    } else {
        output
    }
}

fn platform_source(platform: &str) -> &str {
    match platform {
        "claude-code" | "claude" => "claude",
        "gemini" | "gemini-cli" => "gemini",
        "cursor" | "cursor-agent" => "cursor",
        "codex" => "codex",
        "raw" => "raw",
        other => other,
    }
}

fn default_cwd() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|path| path.display().to_string())
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

fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
        } else {
            output.push(ch);
        }
    }
    output
}
