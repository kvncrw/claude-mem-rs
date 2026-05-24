//! Search orchestration over the available worker strategies.

use rusqlite::{Connection, Result};
use std::collections::HashMap;

use super::result_formatter::{ResultFormatter, SearchResults};
use super::strategies::{
    DateRange, FileSearchResults, OrderBy, SearchStrategyHint, SearchType, SqliteSearchStrategy,
    StrategySearchOptions, StrategySearchResult,
};

#[derive(Debug, Clone, Default)]
pub struct SearchOrchestrator {
    sqlite_strategy: SqliteSearchStrategy,
    result_formatter: ResultFormatter,
    chroma_available: bool,
}

impl SearchOrchestrator {
    pub fn new() -> Self {
        Self {
            sqlite_strategy: SqliteSearchStrategy::new(),
            result_formatter: ResultFormatter::new(),
            chroma_available: false,
        }
    }

    pub fn with_chroma_available(chroma_available: bool) -> Self {
        Self {
            chroma_available,
            ..Self::new()
        }
    }

    pub fn search(
        &self,
        conn: &Connection,
        args: &HashMap<String, String>,
    ) -> StrategySearchResult {
        let options = self.normalize_params(args);
        self.sqlite_strategy.search(conn, &options)
    }

    pub fn search_options(&self, args: &HashMap<String, String>) -> StrategySearchOptions {
        self.normalize_params(args)
    }

    pub fn find_by_concept(
        &self,
        conn: &Connection,
        concept: &str,
        args: &HashMap<String, String>,
    ) -> Result<StrategySearchResult> {
        let options = self.normalize_params(args);
        let observations = self
            .sqlite_strategy
            .find_by_concept(conn, concept, &options)?;
        Ok(StrategySearchResult {
            results: SearchResults {
                observations,
                sessions: Vec::new(),
                prompts: Vec::new(),
            },
            used_chroma: false,
            fell_back: false,
            strategy: SearchStrategyHint::Sqlite,
        })
    }

    pub fn find_by_type(
        &self,
        conn: &Connection,
        types: &[String],
        args: &HashMap<String, String>,
    ) -> Result<StrategySearchResult> {
        let options = self.normalize_params(args);
        let observations = self.sqlite_strategy.find_by_type(conn, types, &options)?;
        Ok(StrategySearchResult {
            results: SearchResults {
                observations,
                sessions: Vec::new(),
                prompts: Vec::new(),
            },
            used_chroma: false,
            fell_back: false,
            strategy: SearchStrategyHint::Sqlite,
        })
    }

    pub fn find_by_file(
        &self,
        conn: &Connection,
        file_path: &str,
        args: &HashMap<String, String>,
    ) -> Result<FileSearchResults> {
        let options = self.normalize_params(args);
        self.sqlite_strategy.find_by_file(conn, file_path, &options)
    }

    pub fn format_search_results(
        &self,
        results: &SearchResults,
        query: &str,
        chroma_failed: bool,
    ) -> String {
        self.result_formatter
            .format_search_results(results, query, chroma_failed)
    }

    pub fn get_formatter(&self) -> ResultFormatter {
        self.result_formatter
    }

    pub fn is_chroma_available(&self) -> bool {
        self.chroma_available
    }

    fn normalize_params(&self, args: &HashMap<String, String>) -> StrategySearchOptions {
        let query = args
            .get("query")
            .or_else(|| args.get("q"))
            .filter(|value| !value.trim().is_empty())
            .cloned();
        StrategySearchOptions {
            query,
            search_type: args
                .get("type")
                .or_else(|| args.get("searchType"))
                .map(|value| match value.as_str() {
                    "observations" | "observation" => SearchType::Observations,
                    "sessions" | "session" => SearchType::Sessions,
                    "prompts" | "prompt" => SearchType::Prompts,
                    _ => SearchType::All,
                })
                .unwrap_or_default(),
            obs_type: split_csv(args.get("obs_type").or_else(|| args.get("obsType"))),
            concepts: split_csv(args.get("concepts")),
            files: split_csv(args.get("files")),
            project: args.get("project").cloned(),
            date_range: parse_date_range(args),
            limit: args
                .get("limit")
                .and_then(|limit| limit.parse::<i64>().ok())
                .filter(|limit| *limit > 0)
                .map(|limit| limit.min(100)),
            offset: args
                .get("offset")
                .and_then(|offset| offset.parse::<i64>().ok())
                .filter(|offset| *offset >= 0),
            order_by: match args.get("orderBy").map(String::as_str) {
                Some("date_asc") => OrderBy::DateAsc,
                Some("relevance") => OrderBy::Relevance,
                _ => OrderBy::DateDesc,
            },
            strategy_hint: match args.get("strategy").map(String::as_str) {
                Some("sqlite") => Some(SearchStrategyHint::Sqlite),
                Some("chroma") => Some(SearchStrategyHint::Chroma),
                Some("qdrant") => Some(SearchStrategyHint::Qdrant),
                Some("hybrid") => Some(SearchStrategyHint::Hybrid),
                Some("auto") => Some(SearchStrategyHint::Auto),
                _ => None,
            },
        }
    }
}

fn split_csv(value: Option<&String>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_date_range(args: &HashMap<String, String>) -> Option<DateRange> {
    let start = args
        .get("dateStart")
        .or_else(|| args.get("start"))
        .and_then(|value| parse_epoch(value));
    let end = args
        .get("dateEnd")
        .or_else(|| args.get("end"))
        .and_then(|value| parse_epoch(value));
    (start.is_some() || end.is_some()).then_some(DateRange {
        start_epoch: start,
        end_epoch: end,
    })
}

fn parse_epoch(value: &str) -> Option<i64> {
    value.parse::<i64>().ok().or_else(|| {
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .ok()
            .map(|dt| dt.unix_timestamp() * 1000)
    })
}
