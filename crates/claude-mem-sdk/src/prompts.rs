//! LLM prompt builders (port of `sdk/prompts.ts`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationPromptInput {
    pub tool_name: String,
    pub tool_input: String,
    pub tool_output: String,
    pub created_at_epoch: i64,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SummaryPromptInput {
    pub session_db_id: i64,
    pub memory_session_id: Option<String>,
    pub project: String,
    pub user_prompt: String,
    pub last_assistant_message: String,
}

pub fn build_init_prompt(project: &str, session_id: &str, user_prompt: &str) -> String {
    format!(
        r#"You are the claude-mem observer for project `{project}`.

<observed_from_primary_session>
  <session_id>{session_id}</session_id>
  <user_request>{user_prompt}</user_request>
</observed_from_primary_session>

Observe the primary Claude Code session and extract durable coding memory only.
Do not use tools. Do not change files. Record decisions, constraints, discoveries,
bugs, implementation details, and follow-up work that will matter in a later session.

Return only XML observations in this format, or return an empty response when there is
nothing durable to store:

<observation>
  <type>[ discovery | bugfix | refactor | decision | implementation | constraint ]</type>
  <title>short durable title</title>
  <subtitle>optional context</subtitle>
  <facts>
    <fact>specific fact</fact>
  </facts>
  <narrative>concise explanation of what was learned</narrative>
  <concepts>
    <concept>searchable concept</concept>
  </concepts>
  <files_read>
    <file>/path/read</file>
  </files_read>
  <files_modified>
    <file>/path/modified</file>
  </files_modified>
</observation>
"#
    )
}

pub fn build_observation_prompt(obs: &ObservationPromptInput) -> String {
    let occurred_at = chrono_like_iso(obs.created_at_epoch);
    let cwd = obs
        .cwd
        .as_ref()
        .map(|cwd| format!("\n  <working_directory>{cwd}</working_directory>"))
        .unwrap_or_default();

    format!(
        r#"<observed_from_primary_session>
  <what_happened>{}</what_happened>
  <occurred_at>{}</occurred_at>{}
  <parameters>{}</parameters>
  <outcome>{}</outcome>
</observed_from_primary_session>

Return either one or more <observation>...</observation> blocks, or an empty response if this tool use should be skipped.
Concrete debugging findings from logs, queue state, database rows, session routing, or code-path inspection count as durable discoveries and should be recorded.
If the observed outcome explicitly asks to remember, store, persist, or recall a durable marker or fact, record it as an observation.
Each stored observation must include a specific <title>, at least one concrete <fact>, and a <narrative> explaining what changed or was learned.
Do not emit title-only or generic tool-use observations. Titles like "Bash tool use" are invalid unless the durable fact is only that a shell command ran.

Use this exact XML shape:

<observation>
  <type>discovery</type>
  <title>short durable title</title>
  <subtitle>one-sentence context</subtitle>
  <facts>
    <fact>specific durable fact from the event</fact>
  </facts>
  <narrative>concise explanation of why the fact matters for future work</narrative>
  <concepts>
    <concept>searchable-concept</concept>
  </concepts>
  <files_read>
    <file>/path/read</file>
  </files_read>
  <files_modified>
    <file>/path/modified</file>
  </files_modified>
</observation>

Do not use tools. Do not inspect files. You are observing the provided event only.
Never reply with prose such as "Skipping", "No substantive tool executions", or any explanation outside XML. Non-XML text is discarded."#,
        obs.tool_name, occurred_at, cwd, obs.tool_input, obs.tool_output
    )
}

pub fn build_summary_prompt(session: &SummaryPromptInput) -> String {
    format!(
        r#"--- MODE SWITCH: PROGRESS SUMMARY ---
Do NOT output <observation> tags. This is a summary request, not an observation request.
Your response MUST use <summary> tags ONLY. Any <observation> output will be discarded.

Summarize the observed Claude Code session for later recall.

<session>
  <id>{}</id>
  <memory_session_id>{}</memory_session_id>
  <project>{}</project>
  <user_prompt>{}</user_prompt>
</session>

<last_assistant_message>
{}
</last_assistant_message>

<summary>
  <request>what the user asked for</request>
  <investigated>what was inspected or tried</investigated>
  <learned>durable facts or constraints learned</learned>
  <completed>what changed or was completed</completed>
  <next_steps>remaining follow-up</next_steps>
  <notes>optional notes</notes>
</summary>"#,
        session.session_db_id,
        session.memory_session_id.as_deref().unwrap_or(""),
        session.project,
        session.user_prompt,
        session.last_assistant_message
    )
}

pub fn build_continuation_prompt(
    user_prompt: &str,
    prompt_number: i64,
    content_session_id: &str,
) -> String {
    format!(
        r#"Continue observing the same primary Claude Code session.

<observed_from_primary_session>
  <session_id>{content_session_id}</session_id>
  <prompt_number>{prompt_number}</prompt_number>
  <user_request>{user_prompt}</user_request>
</observed_from_primary_session>

Use the existing observer rules: return only <observation> XML blocks or an empty response.
Track updates, corrections, temporal changes, durable decisions, and useful file context."#
    )
}

fn chrono_like_iso(epoch_ms: i64) -> String {
    let seconds = epoch_ms.div_euclid(1000);
    let Ok(dt) = time::OffsetDateTime::from_unix_timestamp(seconds) else {
        return epoch_ms.to_string();
    };
    dt.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| epoch_ms.to_string())
}
