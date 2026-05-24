use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum FieldSpec {
    Path(String),
    Spec {
        path: Option<String>,
        value: Option<Value>,
        coalesce: Option<Vec<FieldSpec>>,
        default: Option<Value>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct MatchRule {
    pub path: Option<String>,
    pub equals: Option<Value>,
    #[serde(rename = "in")]
    pub in_values: Option<Vec<Value>>,
    pub contains: Option<String>,
    pub exists: Option<bool>,
    pub regex: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum EventAction {
    SessionInit,
    SessionContext,
    UserMessage,
    AssistantMessage,
    ToolUse,
    ToolResult,
    Observation,
    FileEdit,
    SessionEnd,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SchemaEvent {
    pub name: String,
    #[serde(default)]
    pub r#match: Option<MatchRule>,
    pub action: EventAction,
    #[serde(default)]
    pub fields: BTreeMap<String, FieldSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSchema {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub event_type_path: Option<String>,
    pub session_id_path: Option<String>,
    pub cwd_path: Option<String>,
    pub project_path: Option<String>,
    pub events: Vec<SchemaEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WatchContextConfig {
    pub mode: String,
    pub path: Option<String>,
    #[serde(default)]
    pub update_on: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum WatchSchema {
    Named(String),
    Inline(TranscriptSchema),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WatchTarget {
    pub name: String,
    pub path: String,
    pub schema: WatchSchema,
    pub workspace: Option<String>,
    pub project: Option<String>,
    pub context: Option<WatchContextConfig>,
    pub rescan_interval_ms: Option<u64>,
    pub start_at_end: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptWatchConfig {
    pub version: u32,
    #[serde(default)]
    pub schemas: BTreeMap<String, TranscriptSchema>,
    pub watches: Vec<WatchTarget>,
    pub state_file: Option<String>,
}

pub fn default_config_path() -> PathBuf {
    claude_mem_core::shared::platform_paths::transcript_config_path()
}

pub fn default_state_path() -> PathBuf {
    claude_mem_core::shared::platform_paths::transcript_state_path()
}

pub fn expand_home_path(input: impl AsRef<str>) -> PathBuf {
    let value = input.as_ref();
    if value == "~" {
        return home_dir();
    }
    // Accept either POSIX (`~/foo`) or Windows (`~\foo`) separators so the
    // same config JSON works on both hosts.
    if let Some(rest) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return home_dir().join(rest);
    }
    PathBuf::from(value)
}

#[cfg(test)]
mod expand_home_path_tests {
    use super::*;

    #[test]
    fn tilde_alone_returns_home() {
        assert_eq!(expand_home_path("~"), home_dir());
    }

    #[test]
    fn tilde_slash_expands_under_home() {
        assert_eq!(expand_home_path("~/foo"), home_dir().join("foo"));
    }

    #[cfg(windows)]
    #[test]
    fn tilde_backslash_expands_under_home_on_windows() {
        assert_eq!(expand_home_path(r"~\foo\bar"), home_dir().join(r"foo\bar"));
    }

    #[test]
    fn non_tilde_passes_through() {
        assert_eq!(expand_home_path("/etc/hosts"), PathBuf::from("/etc/hosts"));
    }
}

pub fn load_config(path: &Path) -> Result<TranscriptWatchConfig> {
    let text = fs::read_to_string(path)?;
    let mut config: TranscriptWatchConfig = serde_json::from_str(&text)?;
    if config.version != 1 {
        return Err(anyhow!(
            "unsupported transcript config version {}",
            config.version
        ));
    }
    if config.state_file.is_none() {
        config.state_file = Some(default_state_path().display().to_string());
    }
    Ok(config)
}

pub fn write_sample_config(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&sample_config())?),
    )?;
    Ok(())
}

pub fn sample_config() -> TranscriptWatchConfig {
    let mut schemas = BTreeMap::new();
    schemas.insert("codex".to_owned(), codex_schema());
    TranscriptWatchConfig {
        version: 1,
        schemas,
        watches: vec![WatchTarget {
            name: "codex".to_owned(),
            path: "~/.codex/sessions/**/*.jsonl".to_owned(),
            schema: WatchSchema::Named("codex".to_owned()),
            workspace: None,
            project: None,
            context: Some(WatchContextConfig {
                mode: "agents".to_owned(),
                path: Some("~/.codex/AGENTS.md".to_owned()),
                update_on: vec!["session_start".to_owned(), "session_end".to_owned()],
            }),
            rescan_interval_ms: Some(5000),
            start_at_end: Some(true),
        }],
        state_file: Some(default_state_path().display().to_string()),
    }
}

fn codex_schema() -> TranscriptSchema {
    TranscriptSchema {
        name: "codex".to_owned(),
        version: Some("0.3".to_owned()),
        description: Some(
            "Schema for Codex session JSONL files under ~/.codex/sessions.".to_owned(),
        ),
        event_type_path: None,
        session_id_path: None,
        cwd_path: None,
        project_path: None,
        events: vec![
            event(
                "session-meta",
                Some(match_eq("type", "session_meta")),
                EventAction::SessionContext,
                [
                    ("sessionId", path("payload.id")),
                    ("cwd", path("payload.cwd")),
                ],
            ),
            event(
                "turn-context",
                Some(match_eq("type", "turn_context")),
                EventAction::SessionContext,
                [("cwd", path("payload.cwd"))],
            ),
            event(
                "user-message",
                Some(match_eq("payload.type", "user_message")),
                EventAction::SessionInit,
                [("prompt", path("payload.message"))],
            ),
            event(
                "assistant-message",
                Some(match_eq("payload.type", "agent_message")),
                EventAction::AssistantMessage,
                [("message", path("payload.message"))],
            ),
            event(
                "tool-use",
                Some(match_in(
                    "payload.type",
                    &[
                        "function_call",
                        "custom_tool_call",
                        "web_search_call",
                        "exec_command",
                    ],
                )),
                EventAction::ToolUse,
                [
                    ("toolId", path("payload.call_id")),
                    (
                        "toolName",
                        coalesce(vec![
                            path("payload.name"),
                            path("payload.type"),
                            value("web_search"),
                        ]),
                    ),
                    (
                        "toolInput",
                        coalesce(vec![
                            path("payload.arguments"),
                            path("payload.input"),
                            path("payload.command"),
                            path("payload.action"),
                        ]),
                    ),
                ],
            ),
            event(
                "tool-result",
                Some(match_in(
                    "payload.type",
                    &[
                        "function_call_output",
                        "custom_tool_call_output",
                        "exec_command_output",
                    ],
                )),
                EventAction::ToolResult,
                [
                    ("toolId", path("payload.call_id")),
                    ("toolResponse", path("payload.output")),
                ],
            ),
            event(
                "session-end",
                Some(match_in(
                    "payload.type",
                    &["turn_aborted", "turn_completed"],
                )),
                EventAction::SessionEnd,
                [],
            ),
        ],
    }
}

fn event<const N: usize>(
    name: &str,
    r#match: Option<MatchRule>,
    action: EventAction,
    fields: [(&str, FieldSpec); N],
) -> SchemaEvent {
    SchemaEvent {
        name: name.to_owned(),
        r#match,
        action,
        fields: fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
    }
}

fn path(path: &str) -> FieldSpec {
    FieldSpec::Path(path.to_owned())
}

fn value(value: &str) -> FieldSpec {
    FieldSpec::Spec {
        path: None,
        value: Some(Value::String(value.to_owned())),
        coalesce: None,
        default: None,
    }
}

fn coalesce(coalesce: Vec<FieldSpec>) -> FieldSpec {
    FieldSpec::Spec {
        path: None,
        value: None,
        coalesce: Some(coalesce),
        default: None,
    }
}

fn match_eq(path: &str, value: &str) -> MatchRule {
    MatchRule {
        path: Some(path.to_owned()),
        equals: Some(Value::String(value.to_owned())),
        ..Default::default()
    }
}

fn match_in(path: &str, values: &[&str]) -> MatchRule {
    MatchRule {
        path: Some(path.to_owned()),
        in_values: Some(
            values
                .iter()
                .map(|value| Value::String((*value).to_owned()))
                .collect(),
        ),
        ..Default::default()
    }
}

fn home_dir() -> PathBuf {
    claude_mem_core::shared::platform_paths::home_dir()
}
