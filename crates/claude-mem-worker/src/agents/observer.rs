//! Native observer-agent orchestration.
//!
//! This wires the persistent pending-message queue to provider runners and the
//! shared response processor. The default `local` provider keeps the worker
//! useful without external credentials; `claude`, `gemini`, and `openrouter`
//! provide model-backed extraction when configured.

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use claude_mem_core::db::pending_messages::PendingMessageStore;
use claude_mem_core::db::prompts::get_prompt_number_from_user_prompts;
use claude_mem_core::db::sessions::update_memory_session_id;
use claude_mem_core::types::pending_message::PendingMessageRow;
use claude_mem_core::types::session::SdkSessionRow;
use claude_mem_sdk::{
    build_continuation_prompt, build_init_prompt, build_observation_prompt, build_summary_prompt,
    ObservationPromptInput, SummaryPromptInput,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::process::Command;

use super::fallback_error_handler::should_fallback_to_claude_message;
use super::response_processor::{
    process_agent_response, ActiveSession, ConversationMessage, ProcessAgentResponseOptions,
    ResponseProcessorError,
};

const SIMPLE_TOOLS: &[&str] = &["Read", "Glob", "Grep", "LS", "TodoRead"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObserverConfig {
    pub provider: String,
    pub model_id: Option<String>,
    pub tier_routing_enabled: bool,
    pub simple_model: Option<String>,
    pub summary_model: Option<String>,
    pub max_messages: usize,
}

impl ObserverConfig {
    pub fn from_env() -> Self {
        Self {
            provider: std::env::var("CLAUDE_MEM_PROVIDER")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| std::env::var("CLAUDE_MEM_AGENT_PROVIDER").ok())
                .unwrap_or_else(|| "local".to_owned())
                .to_lowercase(),
            model_id: env_non_empty("CLAUDE_MEM_MODEL"),
            tier_routing_enabled: std::env::var("CLAUDE_MEM_TIER_ROUTING_ENABLED")
                .map(|value| value != "false")
                .unwrap_or(true),
            simple_model: env_non_empty("CLAUDE_MEM_TIER_SIMPLE_MODEL"),
            summary_model: env_non_empty("CLAUDE_MEM_TIER_SUMMARY_MODEL"),
            max_messages: std::env::var("CLAUDE_MEM_QUEUE_PROCESS_LIMIT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(50),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueueProcessStats {
    pub total_pending_sessions: usize,
    pub sessions_started: usize,
    pub sessions_skipped: usize,
    pub started_session_ids: Vec<i64>,
    pub messages_processed: usize,
    pub messages_failed: usize,
    pub observations_inserted: usize,
    pub summaries_inserted: usize,
    pub observation_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentOutput {
    pub text: String,
    pub memory_session_id: Option<String>,
    pub provider: String,
    pub model_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum ObserverError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("agent error: {0}")]
    Agent(String),
    #[error("response processor error: {0}")]
    Response(#[from] ResponseProcessorError),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub async fn process_all_pending(
    conn: Arc<Mutex<Connection>>,
    config: ObserverConfig,
) -> Result<QueueProcessStats, ObserverError> {
    let session_ids = {
        let conn = conn.lock().unwrap();
        pending_session_ids(&conn)?
    };

    let mut stats = QueueProcessStats {
        total_pending_sessions: session_ids.len(),
        ..Default::default()
    };

    for session_db_id in session_ids {
        match process_pending_for_session(Arc::clone(&conn), session_db_id, config.clone()).await {
            Ok(session_stats) => merge_stats(&mut stats, session_stats),
            Err(error) => {
                tracing::warn!(session_db_id, %error, "observer processing failed for session");
                stats.sessions_skipped += 1;
            }
        }
    }

    Ok(stats)
}

pub async fn process_pending_for_content_session(
    conn: Arc<Mutex<Connection>>,
    content_session_id: &str,
    config: ObserverConfig,
) -> Result<QueueProcessStats, ObserverError> {
    let session_db_id = {
        let conn = conn.lock().unwrap();
        let Some(session) = get_session_by_content_id_locked(&conn, content_session_id)? else {
            return Ok(QueueProcessStats::default());
        };
        session.id
    };
    process_pending_for_session(conn, session_db_id, config).await
}

pub async fn process_pending_for_session(
    conn: Arc<Mutex<Connection>>,
    session_db_id: i64,
    config: ObserverConfig,
) -> Result<QueueProcessStats, ObserverError> {
    let mut stats = QueueProcessStats {
        sessions_started: 1,
        started_session_ids: vec![session_db_id],
        ..Default::default()
    };
    let pending_store = PendingMessageStore::default();
    let mut processed = 0usize;

    loop {
        if processed >= config.max_messages {
            break;
        }

        let (session, message, model_id) = {
            let conn = conn.lock().unwrap();
            let Some(session) = get_session_by_id_locked(&conn, session_db_id)? else {
                stats.sessions_skipped += 1;
                return Ok(stats);
            };
            let model_id = choose_model_for_pending(&conn, session_db_id, &config)?;
            let Some(message) = pending_store.claim_next_message(&conn, session_db_id)? else {
                break;
            };
            (session, message, model_id)
        };

        let prompt = build_prompt_for_message(&session, &message);
        let mut active_session = active_session_from_row(&session);
        active_session.last_prompt_number = message.prompt_number.or_else(|| {
            latest_prompt_number(&conn, &session.content_session_id)
                .ok()
                .flatten()
        });
        active_session.processing_message_ids.push(message.id);
        active_session
            .conversation_history
            .push(ConversationMessage {
                role: "user".to_owned(),
                content: prompt.clone(),
            });

        let runner = AgentRunner::new(config.provider.clone());
        let agent_result = runner
            .run(&prompt, &session, Some(&message), model_id.clone())
            .await;
        let output = match agent_result {
            Ok(output) => output,
            Err(error) if should_try_claude_fallback(&config, &error) => {
                AgentRunner::new("claude".to_owned())
                    .run(&prompt, &session, Some(&message), model_id.clone())
                    .await?
            }
            Err(error) => {
                let conn = conn.lock().unwrap();
                let _ = pending_store.mark_failed(&conn, message.id)?;
                stats.messages_failed += 1;
                return Err(error);
            }
        };

        {
            let conn = conn.lock().unwrap();
            ensure_memory_session_id(
                &conn,
                &mut active_session,
                output.memory_session_id.as_deref(),
                &output.provider,
            )?;
            let processed_response = process_agent_response(
                &conn,
                &output.text,
                &mut active_session,
                &pending_store,
                None,
                ProcessAgentResponseOptions {
                    original_timestamp: Some(message.created_at_epoch),
                    agent_name: output.provider,
                    model_id: output.model_id,
                    ..Default::default()
                },
            )?;
            stats.messages_processed += 1;
            stats.observations_inserted += processed_response.storage.inserted as usize;
            stats.summaries_inserted += usize::from(processed_response.summary.is_some());
            stats
                .observation_ids
                .extend(processed_response.storage.observation_ids);
        }

        processed += 1;
    }

    Ok(stats)
}

pub async fn process_session_init(
    conn: Arc<Mutex<Connection>>,
    content_session_id: &str,
    config: ObserverConfig,
) -> Result<QueueProcessStats, ObserverError> {
    if !agent_init_enabled() {
        return Ok(QueueProcessStats::default());
    }

    let session = {
        let conn = conn.lock().unwrap();
        let Some(session) = get_session_by_content_id_locked(&conn, content_session_id)? else {
            return Ok(QueueProcessStats::default());
        };
        session
    };
    let prompt_number = latest_prompt_number(&conn, content_session_id)
        .ok()
        .flatten()
        .unwrap_or(1);
    let prompt = if prompt_number <= 1 {
        build_init_prompt(
            &session.project,
            &session.content_session_id,
            session.user_prompt.as_deref().unwrap_or_default(),
        )
    } else {
        build_continuation_prompt(
            session.user_prompt.as_deref().unwrap_or_default(),
            prompt_number,
            &session.content_session_id,
        )
    };
    let model_id = config.model_id.clone();
    let output = AgentRunner::new(config.provider.clone())
        .run(&prompt, &session, None, model_id)
        .await?;

    let mut active_session = active_session_from_row(&session);
    active_session.last_prompt_number = Some(prompt_number);
    active_session
        .conversation_history
        .push(ConversationMessage {
            role: "user".to_owned(),
            content: prompt,
        });

    let pending_store = PendingMessageStore::default();
    let mut stats = QueueProcessStats {
        sessions_started: 1,
        started_session_ids: vec![session.id],
        ..Default::default()
    };
    {
        let conn = conn.lock().unwrap();
        ensure_memory_session_id(
            &conn,
            &mut active_session,
            output.memory_session_id.as_deref(),
            &output.provider,
        )?;
        let processed_response = process_agent_response(
            &conn,
            &output.text,
            &mut active_session,
            &pending_store,
            None,
            ProcessAgentResponseOptions {
                agent_name: output.provider,
                model_id: output.model_id,
                ..Default::default()
            },
        )?;
        stats.observations_inserted = processed_response.storage.inserted as usize;
        stats.summaries_inserted = usize::from(processed_response.summary.is_some());
        stats
            .observation_ids
            .extend(processed_response.storage.observation_ids);
    }
    Ok(stats)
}

#[derive(Debug, Clone)]
struct AgentRunner {
    provider: String,
}

impl AgentRunner {
    fn new(provider: String) -> Self {
        Self { provider }
    }

    async fn run(
        &self,
        prompt: &str,
        session: &SdkSessionRow,
        message: Option<&PendingMessageRow>,
        model_id: Option<String>,
    ) -> Result<AgentOutput, ObserverError> {
        match self.provider.as_str() {
            "fake" => Ok(fake_output(session, model_id)),
            "claude" | "claude-cli" | "claude_code" | "claude-code" => {
                self.run_claude(prompt, session, message, model_id).await
            }
            "gemini" => self.run_gemini(prompt, model_id).await,
            "gemini-cli" | "gemini_cli" => self.run_gemini_cli(prompt, model_id).await,
            "openrouter" => self.run_openrouter(prompt, model_id).await,
            "codex" | "codex-cli" | "codex_cli" => self.run_codex(prompt, model_id).await,
            _ => Ok(local_output(session, message, model_id)),
        }
    }

    async fn run_claude(
        &self,
        prompt: &str,
        session: &SdkSessionRow,
        message: Option<&PendingMessageRow>,
        model_id: Option<String>,
    ) -> Result<AgentOutput, ObserverError> {
        let command = env_non_empty("CLAUDE_MEM_CLAUDE_COMMAND")
            .or_else(|| env_non_empty("CLAUDE_CODE_PATH"))
            .unwrap_or_else(|| "claude".to_owned());
        let mut cmd = Command::new(command);
        let args = std::env::var("CLAUDE_MEM_CLAUDE_ARGS").unwrap_or_else(|_| {
            "-p --output-format json --tools \"\" --permission-mode dontAsk".to_owned()
        });
        let parsed_args = shell_words(&args);
        let has_resume_arg = parsed_args
            .iter()
            .any(|arg| arg == "--resume" || arg == "-r" || arg.starts_with("--resume="));
        let has_output_format = parsed_args
            .iter()
            .any(|arg| arg == "--output-format" || arg.starts_with("--output-format="));
        for arg in &parsed_args {
            cmd.arg(arg);
        }
        if !has_output_format {
            cmd.arg("--output-format").arg("json");
        }
        if should_resume_claude(session, message) && !has_resume_arg {
            if let Some(memory_session_id) = session.memory_session_id.as_deref() {
                cmd.arg("--resume").arg(memory_session_id);
            }
        }
        if std::env::var("CLAUDE_MEM_CLAUDE_INCLUDE_MODEL_ARG").as_deref() == Ok("true") {
            if let Some(model) = model_id.as_deref() {
                cmd.arg("--model").arg(model);
            }
        }
        cmd.arg(prompt)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let timeout = provider_timeout("CLAUDE_MEM_CLAUDE_TIMEOUT_SECS", 180);
        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| {
                ObserverError::Agent(format!("claude timed out after {}s", timeout.as_secs()))
            })??;
        if !output.status.success() {
            return Err(ObserverError::Agent(format!(
                "claude exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let (text, memory_session_id) = parse_provider_text_and_session(&raw)
            .or_else(|| parse_provider_text_and_session(&stderr))
            .ok_or_else(|| ObserverError::Agent("claude produced no parseable text".into()))?;
        Ok(AgentOutput {
            text,
            memory_session_id,
            provider: "Claude".to_owned(),
            model_id,
        })
    }

    async fn run_gemini(
        &self,
        prompt: &str,
        model_id: Option<String>,
    ) -> Result<AgentOutput, ObserverError> {
        let Some(api_key) =
            env_non_empty("CLAUDE_MEM_GEMINI_API_KEY").or_else(|| env_non_empty("GEMINI_API_KEY"))
        else {
            return self.run_gemini_cli(prompt, model_id).await;
        };
        let model = model_id
            .clone()
            .or_else(|| env_non_empty("CLAUDE_MEM_GEMINI_MODEL"))
            .unwrap_or_else(|| "gemini-2.5-flash-lite".to_owned());
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}"
        );
        let body = json!({
            "contents": [{"role": "user", "parts": [{"text": prompt}]}]
        });
        let timeout = provider_timeout("CLAUDE_MEM_GEMINI_TIMEOUT_SECS", 180);
        let value: Value = reqwest::Client::builder()
            .timeout(timeout)
            .build()?
            .post(url)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let text = value["candidates"][0]["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(AgentOutput {
            text,
            memory_session_id: None,
            provider: "Gemini".to_owned(),
            model_id: Some(model),
        })
    }

    async fn run_gemini_cli(
        &self,
        prompt: &str,
        model_id: Option<String>,
    ) -> Result<AgentOutput, ObserverError> {
        let command = env_non_empty("CLAUDE_MEM_GEMINI_COMMAND").unwrap_or_else(|| "gemini".into());
        let mut cmd = Command::new(command);
        cmd.arg("--prompt")
            .arg(prompt)
            .arg("--output-format")
            .arg("json");
        let model = model_id
            .clone()
            .or_else(|| env_non_empty("CLAUDE_MEM_GEMINI_MODEL"));
        if let Some(model) = model.as_deref() {
            cmd.arg("--model").arg(model);
        }
        let timeout = provider_timeout("CLAUDE_MEM_GEMINI_TIMEOUT_SECS", 180);
        let output = tokio::time::timeout(
            timeout,
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).output(),
        )
        .await
        .map_err(|_| {
            ObserverError::Agent(format!("gemini timed out after {}s", timeout.as_secs()))
        })??;
        if !output.status.success() {
            return Err(ObserverError::Agent(format!(
                "gemini exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let text = parse_provider_text_and_session(&raw)
            .map(|(text, _)| text)
            .unwrap_or(raw);
        Ok(AgentOutput {
            text,
            memory_session_id: None,
            provider: "Gemini".to_owned(),
            model_id,
        })
    }

    async fn run_openrouter(
        &self,
        prompt: &str,
        model_id: Option<String>,
    ) -> Result<AgentOutput, ObserverError> {
        let api_key = env_non_empty("CLAUDE_MEM_OPENROUTER_API_KEY")
            .or_else(|| env_non_empty("OPENROUTER_API_KEY"))
            .ok_or_else(|| ObserverError::Agent("OpenRouter API key is not configured".into()))?;
        let model = model_id
            .clone()
            .or_else(|| env_non_empty("CLAUDE_MEM_OPENROUTER_MODEL"))
            .unwrap_or_else(|| "anthropic/claude-3.5-haiku".to_owned());
        let body = json!({
            "model": model,
            "messages": [{"role": "user", "content": prompt}]
        });
        let timeout = provider_timeout("CLAUDE_MEM_OPENROUTER_TIMEOUT_SECS", 180);
        let mut request = reqwest::Client::builder()
            .timeout(timeout)
            .build()?
            .post("https://openrouter.ai/api/v1/chat/completions")
            .bearer_auth(api_key)
            .json(&body);
        if let Some(site) = env_non_empty("CLAUDE_MEM_OPENROUTER_SITE_URL") {
            request = request.header("HTTP-Referer", site);
        }
        if let Some(app) = env_non_empty("CLAUDE_MEM_OPENROUTER_APP_NAME") {
            request = request.header("X-Title", app);
        }
        let value: Value = request.send().await?.error_for_status()?.json().await?;
        let text = value["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        Ok(AgentOutput {
            text,
            memory_session_id: None,
            provider: "OpenRouter".to_owned(),
            model_id: Some(model),
        })
    }

    async fn run_codex(
        &self,
        prompt: &str,
        model_id: Option<String>,
    ) -> Result<AgentOutput, ObserverError> {
        let command = env_non_empty("CLAUDE_MEM_CODEX_COMMAND").unwrap_or_else(|| "codex".into());
        let mut output_path = std::env::temp_dir();
        output_path.push(format!(
            "claude-mem-rs-codex-{}-{}.txt",
            std::process::id(),
            time::OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let mut cmd = Command::new(command);
        cmd.arg("exec")
            .arg("--skip-git-repo-check")
            .arg("--sandbox")
            .arg("read-only")
            .arg("--output-last-message")
            .arg(&output_path);
        if let Some(model) = model_id.as_deref() {
            cmd.arg("--model").arg(model);
        }
        cmd.arg(prompt)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let timeout = provider_timeout("CLAUDE_MEM_CODEX_TIMEOUT_SECS", 240);
        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| {
                ObserverError::Agent(format!("codex timed out after {}s", timeout.as_secs()))
            })??;
        if !output.status.success() {
            let _ = std::fs::remove_file(&output_path);
            return Err(ObserverError::Agent(format!(
                "codex exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        let text = std::fs::read_to_string(&output_path)
            .unwrap_or_else(|_| String::from_utf8_lossy(&output.stdout).to_string());
        let _ = std::fs::remove_file(&output_path);
        Ok(AgentOutput {
            text,
            memory_session_id: None,
            provider: "Codex".to_owned(),
            model_id,
        })
    }
}

fn build_prompt_for_message(session: &SdkSessionRow, message: &PendingMessageRow) -> String {
    match message.message_type.as_str() {
        "summarize" => build_summary_prompt(&SummaryPromptInput {
            session_db_id: session.id,
            memory_session_id: session.memory_session_id.clone(),
            project: session.project.clone(),
            user_prompt: session.user_prompt.clone().unwrap_or_default(),
            last_assistant_message: message.last_assistant_message.clone().unwrap_or_default(),
        }),
        _ => build_observation_prompt(&ObservationPromptInput {
            tool_name: message.tool_name.clone().unwrap_or_else(|| "Tool".into()),
            tool_input: compact_json(message.tool_input.as_ref()),
            tool_output: compact_json(message.tool_response.as_ref()),
            created_at_epoch: message.created_at_epoch,
            cwd: message.cwd.clone(),
        }),
    }
}

fn fake_output(session: &SdkSessionRow, model_id: Option<String>) -> AgentOutput {
    let text = env_non_empty("CLAUDE_MEM_FAKE_AGENT_RESPONSE").unwrap_or_else(|| {
        "<observation><type>discovery</type><title>Fake observer response</title><facts><fact>fake runner produced memory</fact></facts><narrative>Fake runner response.</narrative><concepts><concept>fake</concept></concepts></observation>".to_owned()
    });
    AgentOutput {
        text,
        memory_session_id: Some(format!("fake-memory:{}", session.content_session_id)),
        provider: "Fake".to_owned(),
        model_id,
    }
}

fn local_output(
    session: &SdkSessionRow,
    message: Option<&PendingMessageRow>,
    model_id: Option<String>,
) -> AgentOutput {
    let text = match message.map(|m| m.message_type.as_str()) {
        Some("summarize") => {
            let message = message.expect("checked above");
            format!(
                "<summary><request>{}</request><investigated>Processed queued session work.</investigated><learned>{}</learned><completed>Stored by claude-mem-rs local observer.</completed><next_steps></next_steps><notes>Local deterministic observer output.</notes></summary>",
                xml_escape(session.user_prompt.as_deref().unwrap_or_default()),
                xml_escape(message.last_assistant_message.as_deref().unwrap_or_default())
            )
        }
        Some(_) => {
            let message = message.expect("checked above");
            let tool = message.tool_name.as_deref().unwrap_or("Tool");
            let narrative = format!(
                "Claude tool `{}` ran with input {} and response {}",
                tool,
                compact_json(message.tool_input.as_ref()),
                compact_json(message.tool_response.as_ref())
            );
            let (files_read, files_modified) = local_file_xml(tool, message.tool_input.as_ref());
            format!(
                "<observation><type>discovery</type><title>{} tool use</title><subtitle>Claude Code PostToolUse</subtitle><facts><fact>Tool: {}</fact></facts><narrative>{}</narrative><concepts><concept>claude-code</concept><concept>tool-use</concept></concepts>{}{}</observation>",
                xml_escape(tool),
                xml_escape(tool),
                xml_escape(&narrative),
                files_read,
                files_modified
            )
        }
        None => String::new(),
    };
    AgentOutput {
        text,
        memory_session_id: Some(format!("local-memory:{}", session.content_session_id)),
        provider: "Local".to_owned(),
        model_id,
    }
}

fn local_file_xml(tool: &str, input: Option<&Value>) -> (String, String) {
    let file = input.and_then(|value| {
        value
            .get("file_path")
            .or_else(|| value.get("path"))
            .or_else(|| value.get("notebook_path"))
            .and_then(Value::as_str)
    });
    let Some(file) = file else {
        return (String::new(), String::new());
    };
    let escaped = xml_escape(file);
    match tool {
        "Read" | "Grep" | "Glob" | "LS" => (
            format!("<files_read><file>{escaped}</file></files_read>"),
            String::new(),
        ),
        _ => (
            String::new(),
            format!("<files_modified><file>{escaped}</file></files_modified>"),
        ),
    }
}

fn active_session_from_row(row: &SdkSessionRow) -> ActiveSession {
    ActiveSession {
        session_db_id: row.id,
        content_session_id: row.content_session_id.clone(),
        memory_session_id: row.memory_session_id.clone(),
        project: row.project.clone(),
        platform_source: row.platform_source.clone(),
        ..Default::default()
    }
}

fn ensure_memory_session_id(
    conn: &Connection,
    session: &mut ActiveSession,
    observed_id: Option<&str>,
    provider: &str,
) -> Result<(), rusqlite::Error> {
    if session.memory_session_id.is_none() {
        let id = observed_id
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "{}-memory:{}",
                    provider.to_lowercase().replace(' ', "-"),
                    session.content_session_id
                )
            });
        update_memory_session_id(conn, &session.content_session_id, &id)?;
        session.memory_session_id = Some(id);
    }
    Ok(())
}

fn choose_model_for_pending(
    conn: &Connection,
    session_db_id: i64,
    config: &ObserverConfig,
) -> Result<Option<String>, rusqlite::Error> {
    if !config.tier_routing_enabled {
        return Ok(config.model_id.clone());
    }
    let pending = peek_pending_types(conn, session_db_id)?;
    if pending.is_empty() {
        return Ok(config.model_id.clone());
    }
    if pending
        .iter()
        .any(|(message_type, _)| message_type == "summarize")
    {
        return Ok(config
            .summary_model
            .clone()
            .or_else(|| config.model_id.clone()));
    }
    let all_simple = pending.iter().all(|(message_type, tool)| {
        message_type == "observation"
            && tool
                .as_deref()
                .map(|tool| SIMPLE_TOOLS.contains(&tool))
                .unwrap_or(false)
    });
    if all_simple {
        return Ok(config
            .simple_model
            .clone()
            .or_else(|| config.model_id.clone()));
    }
    Ok(config.model_id.clone())
}

fn peek_pending_types(
    conn: &Connection,
    session_db_id: i64,
) -> Result<Vec<(String, Option<String>)>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT message_type, tool_name
           FROM pending_messages
          WHERE session_db_id = ?1 AND status = 'pending'
          ORDER BY created_at_epoch ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![session_db_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

fn pending_session_ids(conn: &Connection) -> Result<Vec<i64>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT session_db_id
           FROM pending_messages
          WHERE status IN ('pending','processing')
          ORDER BY session_db_id ASC",
    )?;
    let rows = stmt.query_map([], |row| row.get(0))?;
    rows.collect()
}

fn get_session_by_id_locked(
    conn: &Connection,
    id: i64,
) -> Result<Option<SdkSessionRow>, rusqlite::Error> {
    conn.query_row(
        "SELECT id, content_session_id, memory_session_id, project, user_prompt,
                started_at, started_at_epoch, completed_at, completed_at_epoch,
                status, worker_port, COALESCE(prompt_counter,0),
                custom_title, platform_source
           FROM sdk_sessions WHERE id = ?1",
        params![id],
        session_row_from,
    )
    .optional()
}

fn get_session_by_content_id_locked(
    conn: &Connection,
    content_session_id: &str,
) -> Result<Option<SdkSessionRow>, rusqlite::Error> {
    conn.query_row(
        "SELECT id, content_session_id, memory_session_id, project, user_prompt,
                started_at, started_at_epoch, completed_at, completed_at_epoch,
                status, worker_port, COALESCE(prompt_counter,0),
                custom_title, platform_source
           FROM sdk_sessions WHERE content_session_id = ?1",
        params![content_session_id],
        session_row_from,
    )
    .optional()
}

fn session_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<SdkSessionRow> {
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
}

fn latest_prompt_number(
    conn: &Arc<Mutex<Connection>>,
    content_session_id: &str,
) -> Result<Option<i64>, rusqlite::Error> {
    let conn = conn.lock().unwrap();
    Ok(Some(get_prompt_number_from_user_prompts(
        &conn,
        content_session_id,
    )?))
}

fn parse_provider_text_and_session(raw: &str) -> Option<(String, Option<String>)> {
    if raw.trim().is_empty() {
        return None;
    }
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        if let Some(value) = parse_mixed_output_json(raw) {
            let memory_session_id = find_session_id(&value);
            let text = provider_text_from_value(&value).unwrap_or_else(|| raw.to_owned());
            return Some((text, memory_session_id));
        }
        let stream = parse_stream_json(raw);
        return Some((stream.unwrap_or_else(|| raw.to_owned()), None));
    };
    let memory_session_id = find_session_id(&value);
    let text = provider_text_from_value(&value).unwrap_or_else(|| raw.to_owned());
    Some((text, memory_session_id))
}

fn provider_text_from_value(value: &Value) -> Option<String> {
    value
        .get("result")
        .or_else(|| value.get("response"))
        .or_else(|| value.get("text"))
        .or_else(|| value.get("content"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            value
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

fn find_session_id(value: &Value) -> Option<String> {
    value
        .get("session_id")
        .or_else(|| value.get("memorySessionId"))
        .or_else(|| value.get("sessionId"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn parse_stream_json(raw: &str) -> Option<String> {
    let mut text = Vec::new();
    for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(part) = provider_text_from_value(&value) {
            text.push(part);
            continue;
        }
        if let Some(delta) = value
            .get("delta")
            .or_else(|| value.get("partial"))
            .and_then(Value::as_str)
        {
            text.push(delta.to_owned());
        }
    }
    (!text.is_empty()).then(|| text.join("\n"))
}

fn parse_mixed_output_json(raw: &str) -> Option<Value> {
    raw.match_indices('{')
        .rev()
        .find_map(|(index, _)| serde_json::from_str::<Value>(&raw[index..]).ok())
}

fn should_resume_claude(session: &SdkSessionRow, message: Option<&PendingMessageRow>) -> bool {
    if std::env::var("CLAUDE_MEM_CLAUDE_RESUME_ENABLED").as_deref() == Ok("false") {
        return false;
    }
    session
        .memory_session_id
        .as_deref()
        .is_some_and(|id| !id.trim().is_empty())
        && message.is_some()
}

fn provider_timeout(key: &str, default_secs: u64) -> Duration {
    Duration::from_secs(
        std::env::var(key)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default_secs),
    )
}

fn shell_words(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut token_started = false;
    for ch in value.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            token_started = true;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                current.push(ch);
            }
            token_started = true;
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                token_started = true;
            }
            ch if ch.is_whitespace() => {
                if token_started {
                    out.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            _ => {
                current.push(ch);
                token_started = true;
            }
        }
    }
    if token_started {
        out.push(current);
    }
    out
}

fn should_try_claude_fallback(config: &ObserverConfig, error: &ObserverError) -> bool {
    !matches!(
        config.provider.as_str(),
        "claude" | "claude-cli" | "claude_code" | "claude-code"
    ) && should_fallback_to_claude_message(&error.to_string())
}

fn merge_stats(target: &mut QueueProcessStats, source: QueueProcessStats) {
    target.sessions_started += source.sessions_started;
    target.sessions_skipped += source.sessions_skipped;
    target
        .started_session_ids
        .extend(source.started_session_ids);
    target.messages_processed += source.messages_processed;
    target.messages_failed += source.messages_failed;
    target.observations_inserted += source.observations_inserted;
    target.summaries_inserted += source.summaries_inserted;
    target.observation_ids.extend(source.observation_ids);
}

fn compact_json(value: Option<&Value>) -> String {
    value
        .map(|value| serde_json::to_string(value).unwrap_or_else(|_| value.to_string()))
        .unwrap_or_else(|| "{}".to_owned())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn agent_init_enabled() -> bool {
    std::env::var("CLAUDE_MEM_AGENT_PROCESS_INIT")
        .map(|value| value == "true")
        .unwrap_or(false)
}

trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for rusqlite::Result<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_json_and_stream_variants() {
        let (text, session) = parse_provider_text_and_session(
            r#"{"type":"result","session_id":"abc","result":"<observation/>"}"#,
        )
        .unwrap();
        assert_eq!(text, "<observation/>");
        assert_eq!(session.as_deref(), Some("abc"));

        let (text, session) = parse_provider_text_and_session(
            r#"{"session_id":"gemini-session","response":"<observation/>"}"#,
        )
        .unwrap();
        assert_eq!(text, "<observation/>");
        assert_eq!(session.as_deref(), Some("gemini-session"));

        let (text, session) = parse_provider_text_and_session(
            "Warning: Basic terminal detected.\n{\n  \"session_id\": \"gemini-session\",\n  \"response\": \"<observation/>\"\n}",
        )
        .unwrap();
        assert_eq!(text, "<observation/>");
        assert_eq!(session.as_deref(), Some("gemini-session"));

        let (text, session) = parse_provider_text_and_session(
            r#"{"type":"system","session_id":"stream-1"}
{"type":"assistant","message":{"content":"<summary/>"}}"#,
        )
        .unwrap();
        assert_eq!(text, "<summary/>");
        assert!(session.is_none());
    }

    #[test]
    fn parses_quoted_claude_args() {
        assert_eq!(
            shell_words(r#"-p --output-format json --tools "" --permission-mode dontAsk"#),
            vec![
                "-p",
                "--output-format",
                "json",
                "--tools",
                "",
                "--permission-mode",
                "dontAsk"
            ]
        );
    }
}
