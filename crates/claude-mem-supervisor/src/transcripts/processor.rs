use super::config::{EventAction, SchemaEvent, TranscriptSchema, WatchTarget};
use super::field_utils::{matches_rule, resolve_field_spec, resolve_fields, ResolveContext};
use crate::hooks::{execute_hook, WorkerClient};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

#[derive(Debug, Clone, Default)]
struct SessionState {
    session_id: String,
    platform_source: String,
    cwd: Option<String>,
    project: Option<String>,
    last_user_message: Option<String>,
    last_assistant_message: Option<String>,
    pending_tools: HashMap<String, PendingTool>,
}

#[derive(Debug, Clone, Default)]
struct PendingTool {
    name: Option<String>,
    input: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessEntryStats {
    pub matched_events: usize,
    pub session_inits: usize,
    pub observations: usize,
    pub summaries: usize,
    pub completions: usize,
}

pub struct TranscriptEventProcessor {
    worker: WorkerClient,
    sessions: HashMap<String, SessionState>,
}

impl TranscriptEventProcessor {
    pub fn new(worker: WorkerClient) -> Self {
        Self {
            worker,
            sessions: HashMap::new(),
        }
    }

    pub async fn process_entry(
        &mut self,
        entry: Value,
        watch: &WatchTarget,
        schema: &TranscriptSchema,
        session_id_override: Option<&str>,
    ) -> Result<ProcessEntryStats> {
        let mut stats = ProcessEntryStats::default();
        for event in &schema.events {
            if !matches_rule(&entry, event.r#match.as_ref(), schema) {
                continue;
            }
            stats.matched_events += 1;
            let event_stats = self
                .handle_event(&entry, watch, schema, event, session_id_override)
                .await?;
            stats.session_inits += event_stats.session_inits;
            stats.observations += event_stats.observations;
            stats.summaries += event_stats.summaries;
            stats.completions += event_stats.completions;
        }
        Ok(stats)
    }

    fn session_key(watch: &WatchTarget, session_id: &str) -> String {
        format!("{}:{session_id}", watch.name)
    }

    fn get_or_create_session(
        &mut self,
        watch: &WatchTarget,
        session_id: &str,
    ) -> &mut SessionState {
        let key = Self::session_key(watch, session_id);
        self.sessions.entry(key).or_insert_with(|| SessionState {
            session_id: session_id.to_owned(),
            platform_source: normalize_platform_source(&watch.name),
            ..Default::default()
        })
    }

    async fn handle_event(
        &mut self,
        entry: &Value,
        watch: &WatchTarget,
        schema: &TranscriptSchema,
        event: &SchemaEvent,
        session_id_override: Option<&str>,
    ) -> Result<ProcessEntryStats> {
        let Some(session_id) =
            self.resolve_session_id(entry, watch, schema, event, session_id_override)
        else {
            return Ok(ProcessEntryStats::default());
        };

        let fields = {
            let session = self.get_or_create_session(watch, &session_id);
            let mut ctx_session = BTreeMap::new();
            ctx_session.insert(
                "sessionId".to_owned(),
                Value::String(session.session_id.clone()),
            );
            if let Some(cwd) = &session.cwd {
                ctx_session.insert("cwd".to_owned(), Value::String(cwd.clone()));
            }
            if let Some(project) = &session.project {
                ctx_session.insert("project".to_owned(), Value::String(project.clone()));
            }
            let ctx = ResolveContext {
                watch,
                schema,
                session: Some(&ctx_session),
            };
            resolve_fields(&event.fields, entry, &ctx)
        };

        {
            let session_snapshot = self.get_or_create_session(watch, &session_id).clone();
            let cwd = self.resolve_cwd(entry, watch, schema, event, &session_snapshot);
            let project = self.resolve_project(entry, watch, schema, event, &session_snapshot);
            let session = self.get_or_create_session(watch, &session_id);
            if let Some(cwd) = cwd {
                session.cwd = Some(cwd);
            }
            if let Some(project) = project {
                session.project = Some(project);
            }
        }

        match event.action {
            EventAction::SessionContext => {
                let session = self.get_or_create_session(watch, &session_id);
                if let Some(cwd) = value_string(fields.get("cwd")) {
                    session.cwd = Some(cwd);
                }
                if let Some(project) = value_string(fields.get("project")) {
                    session.project = Some(project);
                }
                Ok(ProcessEntryStats::default())
            }
            EventAction::SessionInit => self.handle_session_init(watch, &session_id, &fields).await,
            EventAction::UserMessage => {
                let session = self.get_or_create_session(watch, &session_id);
                session.last_user_message = value_string(fields.get("message"))
                    .or_else(|| value_string(fields.get("prompt")));
                Ok(ProcessEntryStats::default())
            }
            EventAction::AssistantMessage => {
                let session = self.get_or_create_session(watch, &session_id);
                if let Some(message) = value_string(fields.get("message")) {
                    session.last_assistant_message = Some(message);
                }
                Ok(ProcessEntryStats::default())
            }
            EventAction::ToolUse => self.handle_tool_use(watch, &session_id, &fields).await,
            EventAction::ToolResult => self.handle_tool_result(watch, &session_id, &fields).await,
            EventAction::Observation => self.send_observation(watch, &session_id, &fields).await,
            EventAction::FileEdit => self.send_file_edit(watch, &session_id, &fields).await,
            EventAction::SessionEnd => self.handle_session_end(watch, &session_id).await,
        }
    }

    fn resolve_session_id(
        &self,
        entry: &Value,
        watch: &WatchTarget,
        schema: &TranscriptSchema,
        event: &SchemaEvent,
        override_id: Option<&str>,
    ) -> Option<String> {
        let ctx = ResolveContext {
            watch,
            schema,
            session: None,
        };
        let field = event
            .fields
            .get("sessionId")
            .or_else(|| event.fields.get("session_id"));
        resolve_field_spec(field, entry, &ctx)
            .and_then(|value| value_string(Some(&value)))
            .or_else(|| {
                schema
                    .session_id_path
                    .as_ref()
                    .and_then(|path| super::field_utils::get_value_by_path(entry, path))
                    .and_then(|value| value_string(Some(&value)))
            })
            .or_else(|| override_id.map(str::to_owned))
    }

    fn resolve_cwd(
        &self,
        entry: &Value,
        watch: &WatchTarget,
        schema: &TranscriptSchema,
        event: &SchemaEvent,
        session: &SessionState,
    ) -> Option<String> {
        let mut ctx_session = BTreeMap::new();
        if let Some(cwd) = &session.cwd {
            ctx_session.insert("cwd".to_owned(), Value::String(cwd.clone()));
        }
        let ctx = ResolveContext {
            watch,
            schema,
            session: Some(&ctx_session),
        };
        event
            .fields
            .get("cwd")
            .and_then(|spec| resolve_field_spec(Some(spec), entry, &ctx))
            .and_then(|value| value_string(Some(&value)))
            .or_else(|| {
                schema
                    .cwd_path
                    .as_ref()
                    .and_then(|path| super::field_utils::get_value_by_path(entry, path))
                    .and_then(|value| value_string(Some(&value)))
            })
            .or_else(|| watch.workspace.clone())
            .or_else(|| session.cwd.clone())
    }

    fn resolve_project(
        &self,
        entry: &Value,
        watch: &WatchTarget,
        schema: &TranscriptSchema,
        event: &SchemaEvent,
        session: &SessionState,
    ) -> Option<String> {
        let ctx = ResolveContext {
            watch,
            schema,
            session: None,
        };
        event
            .fields
            .get("project")
            .and_then(|spec| resolve_field_spec(Some(spec), entry, &ctx))
            .and_then(|value| value_string(Some(&value)))
            .or_else(|| {
                schema
                    .project_path
                    .as_ref()
                    .and_then(|path| super::field_utils::get_value_by_path(entry, path))
                    .and_then(|value| value_string(Some(&value)))
            })
            .or_else(|| watch.project.clone())
            .or_else(|| session.cwd.as_deref().map(project_name))
            .or_else(|| session.project.clone())
    }

    async fn handle_session_init(
        &mut self,
        watch: &WatchTarget,
        session_id: &str,
        fields: &BTreeMap<String, Value>,
    ) -> Result<ProcessEntryStats> {
        let (cwd, prompt, platform) = {
            let session = self.get_or_create_session(watch, session_id);
            let prompt = value_string(fields.get("prompt")).unwrap_or_default();
            if !prompt.is_empty() {
                session.last_user_message = Some(prompt.clone());
            }
            (
                session.cwd.clone().unwrap_or_else(current_dir_string),
                prompt,
                session.platform_source.clone(),
            )
        };
        execute_hook(
            &platform,
            "session-init",
            json!({
                "session_id": session_id,
                "cwd": cwd,
                "prompt": prompt
            }),
            &self.worker,
        )
        .await?;
        self.update_context_if_needed(watch, session_id, "session_start")
            .await?;
        Ok(ProcessEntryStats {
            session_inits: 1,
            ..Default::default()
        })
    }

    async fn handle_tool_use(
        &mut self,
        watch: &WatchTarget,
        session_id: &str,
        fields: &BTreeMap<String, Value>,
    ) -> Result<ProcessEntryStats> {
        let tool_id =
            value_string(fields.get("toolId")).or_else(|| value_string(fields.get("tool_id")));
        let tool_name =
            value_string(fields.get("toolName")).or_else(|| value_string(fields.get("tool_name")));
        let tool_input = fields
            .get("toolInput")
            .or_else(|| fields.get("tool_input"))
            .cloned();
        let tool_response = fields
            .get("toolResponse")
            .or_else(|| fields.get("tool_response"))
            .cloned();

        if let Some(tool_id) = tool_id {
            let session = self.get_or_create_session(watch, session_id);
            session.pending_tools.insert(
                tool_id,
                PendingTool {
                    name: tool_name.clone(),
                    input: tool_input.clone(),
                },
            );
        }
        if let Some(tool_response) = tool_response {
            if let Some(tool_name) = tool_name {
                return self
                    .send_observation(
                        watch,
                        session_id,
                        &BTreeMap::from([
                            ("toolName".to_owned(), Value::String(tool_name)),
                            ("toolInput".to_owned(), tool_input.unwrap_or(Value::Null)),
                            ("toolResponse".to_owned(), tool_response),
                        ]),
                    )
                    .await;
            }
        }
        Ok(ProcessEntryStats::default())
    }

    async fn handle_tool_result(
        &mut self,
        watch: &WatchTarget,
        session_id: &str,
        fields: &BTreeMap<String, Value>,
    ) -> Result<ProcessEntryStats> {
        let tool_id =
            value_string(fields.get("toolId")).or_else(|| value_string(fields.get("tool_id")));
        let mut tool_name =
            value_string(fields.get("toolName")).or_else(|| value_string(fields.get("tool_name")));
        let mut tool_input = fields
            .get("toolInput")
            .or_else(|| fields.get("tool_input"))
            .cloned();
        let tool_response = fields
            .get("toolResponse")
            .or_else(|| fields.get("tool_response"))
            .cloned()
            .unwrap_or(Value::Null);

        if let Some(tool_id) = tool_id {
            let session = self.get_or_create_session(watch, session_id);
            if let Some(pending) = session.pending_tools.remove(&tool_id) {
                tool_name = tool_name.or(pending.name);
                tool_input = tool_input.or(pending.input);
            }
        }
        let Some(tool_name) = tool_name else {
            return Ok(ProcessEntryStats::default());
        };
        self.send_observation(
            watch,
            session_id,
            &BTreeMap::from([
                ("toolName".to_owned(), Value::String(tool_name)),
                ("toolInput".to_owned(), tool_input.unwrap_or(Value::Null)),
                ("toolResponse".to_owned(), tool_response),
            ]),
        )
        .await
    }

    async fn send_observation(
        &mut self,
        watch: &WatchTarget,
        session_id: &str,
        fields: &BTreeMap<String, Value>,
    ) -> Result<ProcessEntryStats> {
        let Some(tool_name) =
            value_string(fields.get("toolName")).or_else(|| value_string(fields.get("tool_name")))
        else {
            return Ok(ProcessEntryStats::default());
        };
        let (cwd, platform) = {
            let session = self.get_or_create_session(watch, session_id);
            (
                session.cwd.clone().unwrap_or_else(current_dir_string),
                session.platform_source.clone(),
            )
        };
        execute_hook(
            &platform,
            "observation",
            json!({
                "session_id": session_id,
                "cwd": cwd,
                "tool_name": tool_name,
                "tool_input": fields.get("toolInput").or_else(|| fields.get("tool_input")).cloned().unwrap_or(Value::Null),
                "tool_response": fields.get("toolResponse").or_else(|| fields.get("tool_response")).cloned().unwrap_or(Value::Null)
            }),
            &self.worker,
        )
        .await?;
        Ok(ProcessEntryStats {
            observations: 1,
            ..Default::default()
        })
    }

    async fn send_file_edit(
        &mut self,
        watch: &WatchTarget,
        session_id: &str,
        fields: &BTreeMap<String, Value>,
    ) -> Result<ProcessEntryStats> {
        let file_path =
            value_string(fields.get("filePath")).or_else(|| value_string(fields.get("file_path")));
        let Some(file_path) = file_path else {
            return Ok(ProcessEntryStats::default());
        };
        self.send_observation(
            watch,
            session_id,
            &BTreeMap::from([
                ("toolName".to_owned(), Value::String("Edit".to_owned())),
                ("toolInput".to_owned(), json!({ "file_path": file_path })),
                (
                    "toolResponse".to_owned(),
                    fields.get("edits").cloned().unwrap_or(Value::Null),
                ),
            ]),
        )
        .await
    }

    async fn handle_session_end(
        &mut self,
        watch: &WatchTarget,
        session_id: &str,
    ) -> Result<ProcessEntryStats> {
        let (cwd, platform, last_assistant) = {
            let session = self.get_or_create_session(watch, session_id);
            (
                session.cwd.clone().unwrap_or_else(current_dir_string),
                session.platform_source.clone(),
                session.last_assistant_message.clone().unwrap_or_default(),
            )
        };
        if !last_assistant.trim().is_empty() {
            let _ = self
                .worker
                .post_json(
                    "/api/sessions/summarize",
                    json!({
                        "contentSessionId": session_id,
                        "lastAssistantMessage": last_assistant,
                        "platformSource": platform
                    }),
                )
                .await?;
        }
        execute_hook(
            &platform,
            "session-complete",
            json!({
                "session_id": session_id,
                "cwd": cwd
            }),
            &self.worker,
        )
        .await?;
        self.update_context_if_needed(watch, session_id, "session_end")
            .await?;
        let key = Self::session_key(watch, session_id);
        self.sessions.remove(&key);
        Ok(ProcessEntryStats {
            summaries: usize::from(!last_assistant.trim().is_empty()),
            completions: 1,
            ..Default::default()
        })
    }

    async fn update_context_if_needed(
        &mut self,
        watch: &WatchTarget,
        session_id: &str,
        event: &str,
    ) -> Result<()> {
        let Some(context) = &watch.context else {
            return Ok(());
        };
        if context.mode != "agents" || !context.update_on.iter().any(|item| item == event) {
            return Ok(());
        }
        let (cwd, platform) = {
            let session = self.get_or_create_session(watch, session_id);
            (
                session.cwd.clone().unwrap_or_else(current_dir_string),
                session.platform_source.clone(),
            )
        };
        let project = project_name(&cwd);
        let path = format!(
            "/api/context/inject?projects={}&platformSource={}",
            encode_component(&project),
            encode_component(&platform)
        );
        let Some(content) = self.worker.get_text(&path).await? else {
            return Ok(());
        };
        if content.trim().is_empty() {
            return Ok(());
        }
        let agents_path = context
            .path
            .as_deref()
            .map(super::config::expand_home_path)
            .unwrap_or_else(|| Path::new(&cwd).join("AGENTS.md"));
        write_tagged_file(&agents_path, &content)?;
        Ok(())
    }
}

fn write_tagged_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let start = "<claude-mem-context>";
    let end = "</claude-mem-context>";
    let block = format!("{start}\n{}\n{end}", content.trim());
    let next = if let (Some(start_idx), Some(end_idx)) = (existing.find(start), existing.find(end))
    {
        format!(
            "{}{}{}",
            &existing[..start_idx],
            block,
            &existing[end_idx + end.len()..]
        )
    } else if existing.trim().is_empty() {
        block
    } else {
        format!("{}\n\n{}", existing.trim_end(), block)
    };
    std::fs::write(path, next)?;
    Ok(())
}

fn value_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn normalize_platform_source(value: &str) -> String {
    match value {
        "transcript" => "codex",
        "claude-code" | "claude" => "claude",
        "gemini-cli" | "gemini" => "gemini",
        "cursor-agent" | "cursor" => "cursor",
        other => other,
    }
    .to_owned()
}

fn project_name(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown")
        .to_owned()
}

fn current_dir_string() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| ".".to_owned())
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
