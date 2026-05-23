//! Search strategies used by the worker.

use claude_mem_core::db::observations::get::{
    get_observations_by_file_path, get_observations_by_ids,
};
use claude_mem_core::db::prompts::get_user_prompts_by_ids;
use claude_mem_core::db::summaries::get_summaries_by_ids;
use claude_mem_core::types::{ObservationRow, SessionSummaryRow};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params_from_iter, Connection, Result};

use super::result_formatter::SearchResults;

pub const DEFAULT_LIMIT: i64 = 20;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DateRange {
    pub start_epoch: Option<i64>,
    pub end_epoch: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchType {
    All,
    Observations,
    Sessions,
    Prompts,
}

impl Default for SearchType {
    fn default() -> Self {
        Self::All
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderBy {
    DateDesc,
    DateAsc,
    Relevance,
}

impl Default for OrderBy {
    fn default() -> Self {
        Self::DateDesc
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchStrategyHint {
    Sqlite,
    Chroma,
    Hybrid,
    Auto,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StrategySearchOptions {
    pub query: Option<String>,
    pub strategy_hint: Option<SearchStrategyHint>,
    pub search_type: SearchType,
    pub obs_type: Vec<String>,
    pub concepts: Vec<String>,
    pub files: Vec<String>,
    pub project: Option<String>,
    pub date_range: Option<DateRange>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub order_by: OrderBy,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StrategySearchResult {
    pub results: SearchResults,
    pub used_chroma: bool,
    pub fell_back: bool,
    pub strategy: SearchStrategyHint,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileSearchResults {
    pub observations: Vec<ObservationRow>,
    pub sessions: Vec<SessionSummaryRow>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SqliteSearchStrategy;

impl SqliteSearchStrategy {
    pub const NAME: &'static str = "sqlite";

    pub fn new() -> Self {
        Self
    }

    pub fn can_handle(&self, options: &StrategySearchOptions) -> bool {
        options.query.as_deref().is_none_or(str::is_empty)
            || options.strategy_hint == Some(SearchStrategyHint::Sqlite)
    }

    pub fn search(
        &self,
        conn: &Connection,
        options: &StrategySearchOptions,
    ) -> StrategySearchResult {
        let results = self.search_inner(conn, options).unwrap_or_default();
        StrategySearchResult {
            results,
            used_chroma: false,
            fell_back: false,
            strategy: SearchStrategyHint::Sqlite,
        }
    }

    pub fn find_by_concept(
        &self,
        conn: &Connection,
        concept: &str,
        options: &StrategySearchOptions,
    ) -> Result<Vec<ObservationRow>> {
        let mut scoped = options.clone();
        scoped.concepts = vec![concept.to_owned()];
        self.search_observations(conn, None, &scoped)
    }

    pub fn find_by_type(
        &self,
        conn: &Connection,
        types: &[String],
        options: &StrategySearchOptions,
    ) -> Result<Vec<ObservationRow>> {
        let mut scoped = options.clone();
        scoped.obs_type = types.to_vec();
        self.search_observations(conn, None, &scoped)
    }

    pub fn find_by_file(
        &self,
        conn: &Connection,
        file_path: &str,
        options: &StrategySearchOptions,
    ) -> Result<FileSearchResults> {
        let limit = limit(options);
        let mut observations = get_observations_by_file_path(conn, file_path, Some(limit))?;
        observations = filter_observations(observations, options);
        let sessions = self.search_sessions_by_file(conn, file_path, options)?;
        Ok(FileSearchResults {
            observations,
            sessions,
        })
    }

    fn search_inner(
        &self,
        conn: &Connection,
        options: &StrategySearchOptions,
    ) -> Result<SearchResults> {
        let query = options.query.as_deref().filter(|query| !query.is_empty());
        let observations = if matches!(
            options.search_type,
            SearchType::All | SearchType::Observations
        ) {
            self.search_observations(conn, query, options)?
        } else {
            Vec::new()
        };
        let sessions = if matches!(options.search_type, SearchType::All | SearchType::Sessions) {
            self.search_sessions(conn, query, options)?
        } else {
            Vec::new()
        };
        let prompts = if matches!(options.search_type, SearchType::All | SearchType::Prompts) {
            self.search_prompts(conn, query, options)?
        } else {
            Vec::new()
        };
        Ok(SearchResults {
            observations,
            sessions,
            prompts,
        })
    }

    fn search_observations(
        &self,
        conn: &Connection,
        query: Option<&str>,
        options: &StrategySearchOptions,
    ) -> Result<Vec<ObservationRow>> {
        let ids = if let Some(query) = query {
            fts_ids(
                conn,
                "observations_fts",
                "observations",
                "o",
                query,
                options,
                observation_filters,
            )?
        } else {
            filtered_ids(conn, "observations", "o", options, observation_filters)?
        };
        get_observations_by_ids(conn, &ids)
    }

    fn search_sessions(
        &self,
        conn: &Connection,
        query: Option<&str>,
        options: &StrategySearchOptions,
    ) -> Result<Vec<SessionSummaryRow>> {
        let ids = if let Some(query) = query {
            fts_ids(
                conn,
                "session_summaries_fts",
                "session_summaries",
                "s",
                query,
                options,
                session_filters,
            )?
        } else {
            filtered_ids(conn, "session_summaries", "s", options, session_filters)?
        };
        get_summaries_by_ids(conn, &ids)
    }

    fn search_prompts(
        &self,
        conn: &Connection,
        query: Option<&str>,
        options: &StrategySearchOptions,
    ) -> Result<Vec<claude_mem_core::types::UserPromptRow>> {
        let ids = if let Some(query) = query {
            prompt_fts_ids(conn, query, options)?
        } else {
            prompt_filtered_ids(conn, options)?
        };
        get_user_prompts_by_ids(conn, &ids)
    }

    fn search_sessions_by_file(
        &self,
        conn: &Connection,
        file_path: &str,
        options: &StrategySearchOptions,
    ) -> Result<Vec<SessionSummaryRow>> {
        let mut params = Vec::new();
        let mut filters = Vec::new();
        add_project_date_filters("s", options, &mut filters, &mut params);
        filters.push(
            "(EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(s.files_read) THEN s.files_read ELSE '[]' END) WHERE value LIKE ?)
              OR EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid(s.files_edited) THEN s.files_edited ELSE '[]' END) WHERE value LIKE ?))"
                .to_owned(),
        );
        params.push(SqlValue::Text(format!("%{file_path}%")));
        params.push(SqlValue::Text(format!("%{file_path}%")));
        let sql = format!(
            "SELECT s.id FROM session_summaries s
             WHERE {}
             {}
             LIMIT ? OFFSET ?",
            filters.join(" AND "),
            order_clause("s", &options.order_by, false, "")
        );
        params.push(SqlValue::Integer(limit(options)));
        params.push(SqlValue::Integer(options.offset.unwrap_or(0).max(0)));
        let ids = query_ids(conn, &sql, params)?;
        get_summaries_by_ids(conn, &ids)
    }
}

fn filtered_ids(
    conn: &Connection,
    table: &str,
    alias: &str,
    options: &StrategySearchOptions,
    filter_fn: fn(&str, &StrategySearchOptions, &mut Vec<String>, &mut Vec<SqlValue>),
) -> Result<Vec<i64>> {
    let mut filters = Vec::new();
    let mut params = Vec::new();
    filter_fn(alias, options, &mut filters, &mut params);
    let where_clause = if filters.is_empty() {
        "1 = 1".to_owned()
    } else {
        filters.join(" AND ")
    };
    let sql = format!(
        "SELECT {alias}.id FROM {table} {alias}
         WHERE {where_clause}
         {}
         LIMIT ? OFFSET ?",
        order_clause(alias, &options.order_by, false, "")
    );
    params.push(SqlValue::Integer(limit(options)));
    params.push(SqlValue::Integer(options.offset.unwrap_or(0).max(0)));
    query_ids(conn, &sql, params)
}

fn fts_ids(
    conn: &Connection,
    fts_table: &str,
    table: &str,
    alias: &str,
    query: &str,
    options: &StrategySearchOptions,
    filter_fn: fn(&str, &StrategySearchOptions, &mut Vec<String>, &mut Vec<SqlValue>),
) -> Result<Vec<i64>> {
    let Some(query) = fts_query(query) else {
        return Ok(Vec::new());
    };
    let mut filters = vec![format!("{fts_table} MATCH ?")];
    let mut params = vec![SqlValue::Text(query)];
    filter_fn(alias, options, &mut filters, &mut params);
    let sql = format!(
        "SELECT {alias}.id
         FROM {fts_table} f
         JOIN {table} {alias} ON {alias}.id = f.rowid
         WHERE {}
         {}
         LIMIT ? OFFSET ?",
        filters.join(" AND "),
        order_clause(alias, &options.order_by, true, "f")
    );
    params.push(SqlValue::Integer(limit(options)));
    params.push(SqlValue::Integer(options.offset.unwrap_or(0).max(0)));
    query_ids(conn, &sql, params)
}

fn prompt_filtered_ids(conn: &Connection, options: &StrategySearchOptions) -> Result<Vec<i64>> {
    let mut filters = Vec::new();
    let mut params = Vec::new();
    add_prompt_project_date_filters(options, &mut filters, &mut params);
    let where_clause = if filters.is_empty() {
        "1 = 1".to_owned()
    } else {
        filters.join(" AND ")
    };
    let sql = format!(
        "SELECT up.id
         FROM user_prompts up
         JOIN sdk_sessions s ON s.content_session_id = up.content_session_id
         WHERE {where_clause}
         {}
         LIMIT ? OFFSET ?",
        prompt_order_clause(&options.order_by)
    );
    params.push(SqlValue::Integer(limit(options)));
    params.push(SqlValue::Integer(options.offset.unwrap_or(0).max(0)));
    query_ids(conn, &sql, params)
}

fn prompt_fts_ids(
    conn: &Connection,
    query: &str,
    options: &StrategySearchOptions,
) -> Result<Vec<i64>> {
    let Some(query) = fts_query(query) else {
        return Ok(Vec::new());
    };
    let mut filters = vec!["user_prompts_fts MATCH ?".to_owned()];
    let mut params = vec![SqlValue::Text(query)];
    add_prompt_project_date_filters(options, &mut filters, &mut params);
    let sql = format!(
        "SELECT up.id
         FROM user_prompts_fts f
         JOIN user_prompts up ON up.id = f.rowid
         JOIN sdk_sessions s ON s.content_session_id = up.content_session_id
         WHERE {}
         {}
         LIMIT ? OFFSET ?",
        filters.join(" AND "),
        prompt_order_clause(&options.order_by)
    );
    params.push(SqlValue::Integer(limit(options)));
    params.push(SqlValue::Integer(options.offset.unwrap_or(0).max(0)));
    query_ids(conn, &sql, params)
}

fn observation_filters(
    alias: &str,
    options: &StrategySearchOptions,
    filters: &mut Vec<String>,
    params: &mut Vec<SqlValue>,
) {
    add_project_date_filters(alias, options, filters, params);
    if !options.obs_type.is_empty() {
        let placeholders = std::iter::repeat("?")
            .take(options.obs_type.len())
            .collect::<Vec<_>>()
            .join(",");
        filters.push(format!("{alias}.type IN ({placeholders})"));
        params.extend(options.obs_type.iter().cloned().map(SqlValue::Text));
    }
    if !options.concepts.is_empty() {
        let mut clauses = Vec::new();
        for concept in &options.concepts {
            clauses.push(format!(
                "EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid({alias}.concepts) THEN {alias}.concepts ELSE '[]' END) WHERE value = ?)"
            ));
            params.push(SqlValue::Text(concept.clone()));
        }
        filters.push(format!("({})", clauses.join(" OR ")));
    }
    if !options.files.is_empty() {
        let mut clauses = Vec::new();
        for file in &options.files {
            clauses.push(format!(
                "(EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid({alias}.files_read) THEN {alias}.files_read ELSE '[]' END) WHERE value LIKE ?)
                  OR EXISTS (SELECT 1 FROM json_each(CASE WHEN json_valid({alias}.files_modified) THEN {alias}.files_modified ELSE '[]' END) WHERE value LIKE ?))"
            ));
            params.push(SqlValue::Text(format!("%{file}%")));
            params.push(SqlValue::Text(format!("%{file}%")));
        }
        filters.push(format!("({})", clauses.join(" OR ")));
    }
}

fn session_filters(
    alias: &str,
    options: &StrategySearchOptions,
    filters: &mut Vec<String>,
    params: &mut Vec<SqlValue>,
) {
    add_project_date_filters(alias, options, filters, params);
}

fn add_project_date_filters(
    alias: &str,
    options: &StrategySearchOptions,
    filters: &mut Vec<String>,
    params: &mut Vec<SqlValue>,
) {
    if let Some(project) = &options.project {
        filters.push(format!("{alias}.project = ?"));
        params.push(SqlValue::Text(project.clone()));
    }
    if let Some(date_range) = &options.date_range {
        if let Some(start) = date_range.start_epoch {
            filters.push(format!("{alias}.created_at_epoch >= ?"));
            params.push(SqlValue::Integer(start));
        }
        if let Some(end) = date_range.end_epoch {
            filters.push(format!("{alias}.created_at_epoch <= ?"));
            params.push(SqlValue::Integer(end));
        }
    }
}

fn add_prompt_project_date_filters(
    options: &StrategySearchOptions,
    filters: &mut Vec<String>,
    params: &mut Vec<SqlValue>,
) {
    if let Some(project) = &options.project {
        filters.push("s.project = ?".into());
        params.push(SqlValue::Text(project.clone()));
    }
    if let Some(date_range) = &options.date_range {
        if let Some(start) = date_range.start_epoch {
            filters.push("up.created_at_epoch >= ?".into());
            params.push(SqlValue::Integer(start));
        }
        if let Some(end) = date_range.end_epoch {
            filters.push("up.created_at_epoch <= ?".into());
            params.push(SqlValue::Integer(end));
        }
    }
}

fn order_clause(alias: &str, order_by: &OrderBy, has_fts: bool, fts_alias: &str) -> String {
    match (order_by, has_fts) {
        (OrderBy::Relevance, true) => format!("ORDER BY {fts_alias}.rank ASC"),
        (OrderBy::DateAsc, _) => format!("ORDER BY {alias}.created_at_epoch ASC, {alias}.id ASC"),
        _ => format!("ORDER BY {alias}.created_at_epoch DESC, {alias}.id DESC"),
    }
}

fn prompt_order_clause(order_by: &OrderBy) -> &'static str {
    match order_by {
        OrderBy::DateAsc => "ORDER BY up.created_at_epoch ASC, up.id ASC",
        _ => "ORDER BY up.created_at_epoch DESC, up.id DESC",
    }
}

fn query_ids(conn: &Connection, sql: &str, params: Vec<SqlValue>) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(sql)?;
    let rows: Result<Vec<i64>> = stmt
        .query_map(params_from_iter(params), |row| row.get(0))?
        .collect();
    rows
}

fn filter_observations(
    rows: Vec<ObservationRow>,
    options: &StrategySearchOptions,
) -> Vec<ObservationRow> {
    rows.into_iter()
        .filter(|row| {
            options.project.as_ref().is_none_or(|p| row.project == *p)
                && options.date_range.as_ref().is_none_or(|range| {
                    range
                        .start_epoch
                        .is_none_or(|start| row.created_at_epoch >= start)
                        && range
                            .end_epoch
                            .is_none_or(|end| row.created_at_epoch <= end)
                })
                && (options.obs_type.is_empty() || options.obs_type.contains(&row.r#type))
                && (options.concepts.is_empty()
                    || row.concepts.as_ref().is_some_and(|concepts| {
                        options
                            .concepts
                            .iter()
                            .any(|concept| concepts.contains(concept))
                    }))
        })
        .collect()
}

fn limit(options: &StrategySearchOptions) -> i64 {
    options.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, 100)
}

fn fts_query(input: &str) -> Option<String> {
    let query = input
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|term| term.len() >= 3)
        .take(8)
        .collect::<Vec<_>>()
        .join(" OR ");
    (!query.is_empty()).then_some(query)
}
