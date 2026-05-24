//! Corpus builder — port of upstream-claude-mem
//! `src/services/worker/knowledge/CorpusBuilder.ts`.
//!
//! Composes the existing worker SQL surface to produce a [`CorpusFile`]:
//!  1. Run the filter through `SearchOrchestrator::search` to get IDs.
//!  2. Hydrate via `get_observations_by_ids` (preserves TS' "search returns
//!     IDs, store returns rows" two-step).
//!  3. Map to [`CorpusObservation`] (JSON-array fields, not stringified).
//!  4. Compute stats over the assembled set.
//!  5. Run a second renderer pass to capture the token estimate.
//!  6. Persist via [`CorpusStore`].

use std::collections::HashMap;

use claude_mem_core::db::observations::get::get_observations_by_ids;
use claude_mem_core::types::{
    CorpusDateRange, CorpusFile, CorpusFilter, CorpusObservation, CorpusStats, CorpusVersion,
    ObservationRow,
};
use rusqlite::Connection;
use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use super::renderer::CorpusRenderer;
use super::store::{CorpusStore, CorpusStoreError};
use crate::search::orchestrator::SearchOrchestrator;

#[derive(Debug, Error)]
pub enum CorpusBuilderError {
    #[error(transparent)]
    Store(#[from] CorpusStoreError),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Builds and persists corpora. Each call to `build` is stateless — pass in
/// the connection, the filter, and a previously-constructed store.
#[derive(Debug, Clone, Default)]
pub struct CorpusBuilder {
    orchestrator: SearchOrchestrator,
    renderer: CorpusRenderer,
}

impl CorpusBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a corpus from observations matching `filter`. Persists the
    /// result to `store` before returning.
    pub fn build(
        &self,
        conn: &Connection,
        store: &CorpusStore,
        name: &str,
        description: &str,
        filter: CorpusFilter,
    ) -> Result<CorpusFile, CorpusBuilderError> {
        let search_args = filter_to_search_args(&filter);
        let result = self.orchestrator.search(conn, &search_args);
        let ids: Vec<i64> = result
            .results
            .observations
            .iter()
            .map(|row| row.id)
            .collect();

        let rows = if ids.is_empty() {
            Vec::new()
        } else {
            get_observations_by_ids(conn, &ids)?
        };

        // Apply date_asc ordering on the hydrated rows (TS' `orderBy: 'date_asc'`
        // hint to SessionStore). Same project/types/limit filter that the search
        // already enforced — we re-trim here to mirror TS' explicit
        // `hydrateOptions` shape and to honour `limit` after hydration.
        let mut observations: Vec<CorpusObservation> =
            rows.into_iter().map(map_row).collect();
        observations.sort_by(|a, b| {
            a.created_at_epoch
                .cmp(&b.created_at_epoch)
                .then_with(|| a.id.cmp(&b.id))
        });
        if let Some(limit) = filter.limit {
            let cap = limit.max(0) as usize;
            if observations.len() > cap {
                observations.truncate(cap);
            }
        }

        let stats = calculate_stats(&observations);
        let now = now_iso();
        let mut corpus = CorpusFile {
            version: CorpusVersion,
            name: name.to_owned(),
            description: description.to_owned(),
            created_at: now.clone(),
            updated_at: now,
            filter,
            stats,
            system_prompt: String::new(),
            session_id: None,
            observations,
        };

        // Two-pass: prompt + rendered text both depend on the assembled corpus.
        corpus.system_prompt = self.renderer.generate_system_prompt(&corpus);
        let rendered = self.renderer.render_corpus(&corpus);
        corpus.stats.token_estimate = self.renderer.estimate_tokens(&rendered);

        store.write(&corpus)?;
        Ok(corpus)
    }
}

/// Translate a `CorpusFilter` into the `HashMap<String,String>` shape the
/// `SearchOrchestrator::search` entrypoint expects. Mirrors the
/// `searchArgs` construction in TS `CorpusBuilder.ts:54-62`.
fn filter_to_search_args(filter: &CorpusFilter) -> HashMap<String, String> {
    let mut args = HashMap::new();
    if let Some(project) = &filter.project {
        args.insert("project".into(), project.clone());
    }
    if let Some(types) = &filter.types {
        if !types.is_empty() {
            // TS passes types as `type` (singular) — the orchestrator alias
            // map already handles this via `obs_type` lookup. Use `obs_type`
            // for the Rust orchestrator's normalize_params.
            args.insert("obs_type".into(), types.join(","));
        }
    }
    if let Some(concepts) = &filter.concepts {
        if !concepts.is_empty() {
            args.insert("concepts".into(), concepts.join(","));
        }
    }
    if let Some(files) = &filter.files {
        if !files.is_empty() {
            args.insert("files".into(), files.join(","));
        }
    }
    if let Some(query) = &filter.query {
        if !query.is_empty() {
            args.insert("query".into(), query.clone());
        }
    }
    if let Some(date_start) = &filter.date_start {
        args.insert("dateStart".into(), date_start.clone());
    }
    if let Some(date_end) = &filter.date_end {
        args.insert("dateEnd".into(), date_end.clone());
    }
    if let Some(limit) = filter.limit {
        args.insert("limit".into(), limit.to_string());
    }
    args
}

/// Map a raw `ObservationRow` into the renderer-friendly `CorpusObservation`.
fn map_row(row: ObservationRow) -> CorpusObservation {
    CorpusObservation {
        id: row.id,
        r#type: row.r#type,
        title: row.title.unwrap_or_default(),
        subtitle: row.subtitle,
        narrative: row.narrative,
        facts: row.facts.unwrap_or_default(),
        concepts: row.concepts.unwrap_or_default(),
        files_read: row.files_read.unwrap_or_default(),
        files_modified: row.files_modified.unwrap_or_default(),
        project: row.project,
        created_at: row.created_at,
        created_at_epoch: row.created_at_epoch,
    }
}

fn calculate_stats(observations: &[CorpusObservation]) -> CorpusStats {
    let mut breakdown: HashMap<String, i64> = HashMap::new();
    let mut earliest = i64::MAX;
    let mut latest = i64::MIN;
    for observation in observations {
        *breakdown.entry(observation.r#type.clone()).or_insert(0) += 1;
        if observation.created_at_epoch < earliest {
            earliest = observation.created_at_epoch;
        }
        if observation.created_at_epoch > latest {
            latest = observation.created_at_epoch;
        }
    }
    let (earliest_iso, latest_iso) = if observations.is_empty() {
        let now = now_iso();
        (now.clone(), now)
    } else {
        (epoch_ms_to_iso(earliest), epoch_ms_to_iso(latest))
    };
    CorpusStats {
        observation_count: observations.len() as i64,
        token_estimate: 0, // filled in after rendering pass
        date_range: CorpusDateRange {
            earliest: earliest_iso,
            latest: latest_iso,
        },
        type_breakdown: breakdown,
    }
}

fn now_iso() -> String {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    OffsetDateTime::from_unix_timestamp(now_secs)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

fn epoch_ms_to_iso(epoch_ms: i64) -> String {
    OffsetDateTime::from_unix_timestamp(epoch_ms.div_euclid(1000))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_mem_core::db::open_in_memory;
    use claude_mem_core::db::sessions::{create_session, update_memory_session_id};
    use claude_mem_core::db::transactions::store_batch;
    use claude_mem_core::types::session::CreateSessionInput;
    use claude_mem_core::types::ObservationInput;
    use tempfile::TempDir;

    fn seed_observations(conn: &Connection, project: &str, count: i64) {
        let content_id = format!("content-{project}");
        let memory_id = format!("memses-{project}");
        create_session(
            conn,
            &CreateSessionInput {
                content_session_id: content_id.clone(),
                project: project.to_owned(),
                user_prompt: Some("test".into()),
                started_at: "2026-05-24T00:00:00Z".into(),
                started_at_epoch: 1_716_500_000_000,
            },
        )
        .expect("create_session");
        update_memory_session_id(conn, &content_id, &memory_id).expect("memory id");

        let observations: Vec<ObservationInput> = (0..count)
            .map(|i| ObservationInput {
                memory_session_id: memory_id.clone(),
                project: project.to_owned(),
                r#type: if i % 2 == 0 {
                    "decision".into()
                } else {
                    "bugfix".into()
                },
                title: Some(format!("Title {i}")),
                narrative: Some(format!("Body {i}")),
                facts: Some(vec![format!("fact-{i}")]),
                concepts: Some(vec!["hooks".into()]),
                files_modified: Some(vec![format!("src/file-{i}.rs")]),
                created_at: "2026-05-24T00:00:00Z".into(),
                created_at_epoch: 1_716_500_000_000 + i,
                ..Default::default()
            })
            .collect();
        store_batch(conn, &memory_id, project, &observations, None, None, None, None)
            .expect("store_batch");
    }

    #[test]
    fn builds_corpus_against_in_memory_db() {
        let conn = open_in_memory().unwrap();
        seed_observations(&conn, "alpha", 4);
        seed_observations(&conn, "beta", 2);

        let tmp = TempDir::new().unwrap();
        let store = CorpusStore::new(tmp.path());
        let builder = CorpusBuilder::new();
        let filter = CorpusFilter {
            project: Some("alpha".into()),
            limit: Some(100),
            ..Default::default()
        };
        let corpus = builder.build(&conn, &store, "alpha-all", "all of alpha", filter).unwrap();
        assert_eq!(corpus.stats.observation_count, 4);
        assert_eq!(corpus.observations.len(), 4);
        // Sorted ascending by epoch.
        assert!(corpus.observations.windows(2).all(|w| w[0].created_at_epoch <= w[1].created_at_epoch));
        // Token estimate is non-zero now that we've rendered.
        assert!(corpus.stats.token_estimate > 0);
        // Persisted.
        let loaded = store.read("alpha-all").unwrap().unwrap();
        assert_eq!(loaded, corpus);
    }

    #[test]
    fn empty_corpus_uses_now_for_date_range() {
        let conn = open_in_memory().unwrap();
        let tmp = TempDir::new().unwrap();
        let store = CorpusStore::new(tmp.path());
        let builder = CorpusBuilder::new();
        let corpus = builder
            .build(
                &conn,
                &store,
                "empty",
                "no rows",
                CorpusFilter {
                    project: Some("ghost".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(corpus.stats.observation_count, 0);
        assert!(!corpus.stats.date_range.earliest.is_empty());
        assert_eq!(corpus.stats.date_range.earliest, corpus.stats.date_range.latest);
    }

    #[test]
    fn filter_to_search_args_translates_known_fields() {
        let args = filter_to_search_args(&CorpusFilter {
            project: Some("p".into()),
            types: Some(vec!["decision".into(), "bugfix".into()]),
            concepts: Some(vec!["hooks".into()]),
            files: Some(vec!["src/x.rs".into()]),
            query: Some("hooks".into()),
            date_start: Some("2026-01-01".into()),
            date_end: Some("2026-06-01".into()),
            limit: Some(42),
        });
        assert_eq!(args.get("project"), Some(&"p".to_owned()));
        assert_eq!(args.get("obs_type"), Some(&"decision,bugfix".to_owned()));
        assert_eq!(args.get("concepts"), Some(&"hooks".to_owned()));
        assert_eq!(args.get("files"), Some(&"src/x.rs".to_owned()));
        assert_eq!(args.get("query"), Some(&"hooks".to_owned()));
        assert_eq!(args.get("dateStart"), Some(&"2026-01-01".to_owned()));
        assert_eq!(args.get("dateEnd"), Some(&"2026-06-01".to_owned()));
        assert_eq!(args.get("limit"), Some(&"42".to_owned()));
    }

    #[test]
    fn filter_to_search_args_skips_empty_collections() {
        let args = filter_to_search_args(&CorpusFilter {
            types: Some(vec![]),
            concepts: Some(vec![]),
            files: Some(vec![]),
            query: Some(String::new()),
            ..Default::default()
        });
        assert!(!args.contains_key("obs_type"));
        assert!(!args.contains_key("concepts"));
        assert!(!args.contains_key("files"));
        assert!(!args.contains_key("query"));
    }

    #[test]
    fn calculate_stats_counts_breakdown_and_range() {
        let observations = vec![
            CorpusObservation {
                id: 1,
                r#type: "decision".into(),
                title: "a".into(),
                subtitle: None,
                narrative: None,
                facts: vec![],
                concepts: vec![],
                files_read: vec![],
                files_modified: vec![],
                project: "p".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
                created_at_epoch: 1_700_000_000_000,
            },
            CorpusObservation {
                id: 2,
                r#type: "bugfix".into(),
                title: "b".into(),
                subtitle: None,
                narrative: None,
                facts: vec![],
                concepts: vec![],
                files_read: vec![],
                files_modified: vec![],
                project: "p".into(),
                created_at: "2026-02-01T00:00:00Z".into(),
                created_at_epoch: 1_710_000_000_000,
            },
            CorpusObservation {
                id: 3,
                r#type: "decision".into(),
                title: "c".into(),
                subtitle: None,
                narrative: None,
                facts: vec![],
                concepts: vec![],
                files_read: vec![],
                files_modified: vec![],
                project: "p".into(),
                created_at: "2026-03-01T00:00:00Z".into(),
                created_at_epoch: 1_720_000_000_000,
            },
        ];
        let stats = calculate_stats(&observations);
        assert_eq!(stats.observation_count, 3);
        assert_eq!(stats.type_breakdown.get("decision").copied(), Some(2));
        assert_eq!(stats.type_breakdown.get("bugfix").copied(), Some(1));
        assert_eq!(stats.date_range.earliest, epoch_ms_to_iso(1_700_000_000_000));
        assert_eq!(stats.date_range.latest, epoch_ms_to_iso(1_720_000_000_000));
    }
}
