//! Markdown formatter for worker search results.

use claude_mem_core::types::{ObservationRow, SessionSummaryRow, UserPromptRow};
use time::OffsetDateTime;

const CHARS_PER_TOKEN_ESTIMATE: f64 = 4.0;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchResults {
    pub observations: Vec<ObservationRow>,
    pub sessions: Vec<SessionSummaryRow>,
    pub prompts: Vec<UserPromptRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CombinedData {
    Observation(ObservationRow),
    Session(SessionSummaryRow),
    Prompt(UserPromptRow),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CombinedResult {
    pub result_type: &'static str,
    pub data: CombinedData,
    pub epoch: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ResultFormatter;

impl ResultFormatter {
    pub fn new() -> Self {
        Self
    }

    pub fn format_search_results(
        &self,
        results: &SearchResults,
        query: &str,
        chroma_failed: bool,
    ) -> String {
        let total_results =
            results.observations.len() + results.sessions.len() + results.prompts.len();
        if total_results == 0 {
            if chroma_failed {
                return self.format_chroma_failure_message();
            }
            return format!("No results found matching \"{query}\"");
        }

        let mut combined = self.combine_results(results);
        combined.sort_by(|a, b| b.epoch.cmp(&a.epoch));

        let mut lines = Vec::new();
        lines.push(format!(
            "Found {total_results} result(s) matching \"{query}\" ({} obs, {} sessions, {} prompts)",
            results.observations.len(),
            results.sessions.len(),
            results.prompts.len()
        ));
        lines.push(String::new());

        let mut current_day = String::new();
        let mut current_file = String::new();
        let mut last_time = String::new();

        for result in combined {
            let day = format_date(result.epoch);
            if day != current_day {
                current_day = day.clone();
                current_file.clear();
                last_time.clear();
                lines.push(format!("### {day}"));
                lines.push(String::new());
            }

            let file = match &result.data {
                CombinedData::Observation(obs) => extract_first_file(obs),
                _ => "General".to_owned(),
            };
            if file != current_file {
                current_file = file.clone();
                last_time.clear();
                lines.push(format!("**{file}**"));
                lines.push(self.format_search_table_header());
            }

            match &result.data {
                CombinedData::Observation(obs) => {
                    let formatted = self.format_observation_search_row(obs, &last_time);
                    last_time = formatted.time;
                    lines.push(formatted.row);
                }
                CombinedData::Session(session) => {
                    let formatted = self.format_session_search_row(session, &last_time);
                    last_time = formatted.time;
                    lines.push(formatted.row);
                }
                CombinedData::Prompt(prompt) => {
                    let formatted = self.format_prompt_search_row(prompt, &last_time);
                    last_time = formatted.time;
                    lines.push(formatted.row);
                }
            }
        }

        lines.join("\n")
    }

    pub fn combine_results(&self, results: &SearchResults) -> Vec<CombinedResult> {
        let mut combined = Vec::with_capacity(
            results.observations.len() + results.sessions.len() + results.prompts.len(),
        );
        combined.extend(
            results
                .observations
                .iter()
                .cloned()
                .map(|obs| CombinedResult {
                    result_type: "observation",
                    epoch: obs.created_at_epoch,
                    created_at: obs.created_at.clone(),
                    data: CombinedData::Observation(obs),
                }),
        );
        combined.extend(
            results
                .sessions
                .iter()
                .cloned()
                .map(|session| CombinedResult {
                    result_type: "session",
                    epoch: session.created_at_epoch,
                    created_at: session.created_at.clone(),
                    data: CombinedData::Session(session),
                }),
        );
        combined.extend(
            results
                .prompts
                .iter()
                .cloned()
                .map(|prompt| CombinedResult {
                    result_type: "prompt",
                    epoch: prompt.created_at_epoch,
                    created_at: prompt.created_at.clone(),
                    data: CombinedData::Prompt(prompt),
                }),
        );
        combined
    }

    pub fn format_search_table_header(&self) -> String {
        "| ID | Time | T | Title | Read |\n|----|------|---|-------|------|".into()
    }

    pub fn format_table_header(&self) -> String {
        "| ID | Time | T | Title | Read | Work |\n|-----|------|---|-------|------|------|".into()
    }

    pub fn format_observation_search_row(
        &self,
        obs: &ObservationRow,
        last_time: &str,
    ) -> FormattedRow {
        let id = format!("#{}", obs.id);
        let time = format_time(obs.created_at_epoch);
        let icon = type_icon(&obs.r#type);
        let title = obs.title.as_deref().unwrap_or("Untitled");
        let read_tokens = self.estimate_read_tokens(obs);
        let time_display = if time == last_time { "\"" } else { &time };

        FormattedRow {
            row: format!(
                "| {id} | {time_display} | {icon} | {} | ~{read_tokens} |",
                table_text(title)
            ),
            time,
        }
    }

    pub fn format_session_search_row(
        &self,
        session: &SessionSummaryRow,
        last_time: &str,
    ) -> FormattedRow {
        let id = format!("#S{}", session.id);
        let time = format_time(session.created_at_epoch);
        let title = session.request.clone().unwrap_or_else(|| {
            format!(
                "Session {}",
                session
                    .memory_session_id
                    .chars()
                    .take(8)
                    .collect::<String>()
            )
        });
        let time_display = if time == last_time { "\"" } else { &time };

        FormattedRow {
            row: format!("| {id} | {time_display} | S | {} | - |", table_text(&title)),
            time,
        }
    }

    pub fn format_prompt_search_row(
        &self,
        prompt: &UserPromptRow,
        last_time: &str,
    ) -> FormattedRow {
        let id = format!("#P{}", prompt.id);
        let time = format_time(prompt.created_at_epoch);
        let title = truncate(&prompt.prompt_text, 60);
        let time_display = if time == last_time { "\"" } else { &time };

        FormattedRow {
            row: format!("| {id} | {time_display} | P | {} | - |", table_text(&title)),
            time,
        }
    }

    pub fn format_observation_index(&self, obs: &ObservationRow, _index: usize) -> String {
        let id = format!("#{}", obs.id);
        let time = format_time(obs.created_at_epoch);
        let icon = type_icon(&obs.r#type);
        let title = obs.title.as_deref().unwrap_or("Untitled");
        let read_tokens = self.estimate_read_tokens(obs);
        let work_display = if obs.discovery_tokens > 0 {
            format!("W {}", obs.discovery_tokens)
        } else {
            "-".into()
        };

        format!(
            "| {id} | {time} | {icon} | {} | ~{read_tokens} | {work_display} |",
            table_text(title)
        )
    }

    pub fn format_session_index(&self, session: &SessionSummaryRow, _index: usize) -> String {
        let id = format!("#S{}", session.id);
        let time = format_time(session.created_at_epoch);
        let title = session.request.clone().unwrap_or_else(|| {
            format!(
                "Session {}",
                session
                    .memory_session_id
                    .chars()
                    .take(8)
                    .collect::<String>()
            )
        });
        format!("| {id} | {time} | S | {} | - | - |", table_text(&title))
    }

    pub fn format_prompt_index(&self, prompt: &UserPromptRow, _index: usize) -> String {
        let id = format!("#P{}", prompt.id);
        let time = format_time(prompt.created_at_epoch);
        let title = truncate(&prompt.prompt_text, 60);
        format!("| {id} | {time} | P | {} | - | - |", table_text(&title))
    }

    pub fn format_search_tips(&self) -> String {
        r#"---
Search Strategy:
1. Search with index to see titles, dates, IDs
2. Use timeline to get context around interesting results
3. Batch fetch full details: get_observations(ids=[...])

Tips:
- Filter by type: obs_type="bugfix,feature"
- Filter by date: dateStart="2025-01-01"
- Sort: orderBy="date_desc" or "date_asc""#
            .into()
    }

    fn estimate_read_tokens(&self, obs: &ObservationRow) -> usize {
        let facts_len: usize = obs
            .facts
            .as_ref()
            .map(|facts| facts.iter().map(String::len).sum())
            .unwrap_or(0);
        let size = obs.title.as_ref().map(String::len).unwrap_or(0)
            + obs.subtitle.as_ref().map(String::len).unwrap_or(0)
            + obs.narrative.as_ref().map(String::len).unwrap_or(0)
            + facts_len;
        ((size as f64) / CHARS_PER_TOKEN_ESTIMATE).ceil() as usize
    }

    fn format_chroma_failure_message(&self) -> String {
        "Vector search failed - semantic search unavailable.\n\nYou can still use filter-only searches without a query term.".into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattedRow {
    pub row: String,
    pub time: String,
}

fn format_time(epoch_ms: i64) -> String {
    let timestamp = epoch_ms.div_euclid(1000);
    let dt = OffsetDateTime::from_unix_timestamp(timestamp).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let hour = dt.hour();
    let minute = dt.minute();
    let suffix = if hour >= 12 { "PM" } else { "AM" };
    let hour12 = match hour % 12 {
        0 => 12,
        value => value,
    };
    format!("{hour12}:{minute:02} {suffix}")
}

fn format_date(epoch_ms: i64) -> String {
    let timestamp = epoch_ms.div_euclid(1000);
    let dt = OffsetDateTime::from_unix_timestamp(timestamp).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let month = match dt.month() as u8 {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        _ => "Dec",
    };
    format!("{} {}, {}", month, dt.day(), dt.year())
}

fn extract_first_file(obs: &ObservationRow) -> String {
    obs.files_modified
        .as_ref()
        .and_then(|files| files.first())
        .or_else(|| obs.files_read.as_ref().and_then(|files| files.first()))
        .cloned()
        .unwrap_or_else(|| "General".into())
}

fn type_icon(kind: &str) -> &'static str {
    match kind {
        "decision" => "D",
        "bugfix" => "B",
        "feature" => "F",
        "refactor" => "R",
        "discovery" => "I",
        "change" => "C",
        _ => "?",
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let keep = max_chars.saturating_sub(3);
    format!("{}...", text.chars().take(keep).collect::<String>())
}

fn table_text(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}
