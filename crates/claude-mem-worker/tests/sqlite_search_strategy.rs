use claude_mem_core::db::prompts::{save_user_prompt, PromptInput};
use claude_mem_core::db::sessions::{create_session, update_memory_session_id};
use claude_mem_core::db::summaries::{store_summary, SummaryInput};
use claude_mem_core::db::transactions::store_batch;
use claude_mem_core::db::{observations::get::get_observations_by_ids, open_in_memory};
use claude_mem_core::types::session::CreateSessionInput;
use claude_mem_core::types::ObservationInput;
use claude_mem_worker::search::strategies::{
    DateRange, OrderBy, SearchStrategyHint, SearchType, SqliteSearchStrategy, StrategySearchOptions,
};

#[test]
fn can_handle_filter_only_or_forced_sqlite_queries() {
    let strategy = SqliteSearchStrategy::new();
    assert!(strategy.can_handle(&StrategySearchOptions {
        project: Some("cloudy".into()),
        ..Default::default()
    }));
    assert!(strategy.can_handle(&StrategySearchOptions {
        query: Some(String::new()),
        ..Default::default()
    }));
    assert!(!strategy.can_handle(&StrategySearchOptions {
        query: Some("semantic search query".into()),
        ..Default::default()
    }));
    assert!(strategy.can_handle(&StrategySearchOptions {
        query: Some("semantic search query".into()),
        strategy_hint: Some(SearchStrategyHint::Sqlite),
        ..Default::default()
    }));
}

#[test]
fn searches_all_types_with_project_filter() {
    let conn = seeded_db();
    let strategy = SqliteSearchStrategy::new();

    let result = strategy.search(
        &conn,
        &StrategySearchOptions {
            project: Some("cloudy".into()),
            limit: Some(10),
            ..Default::default()
        },
    );

    assert!(!result.used_chroma);
    assert!(!result.fell_back);
    assert_eq!(result.strategy, SearchStrategyHint::Sqlite);
    assert_eq!(result.results.observations.len(), 2);
    assert_eq!(result.results.sessions.len(), 1);
    assert_eq!(result.results.prompts.len(), 1);
}

#[test]
fn can_scope_search_to_each_result_type() {
    let conn = seeded_db();
    let strategy = SqliteSearchStrategy::new();

    let observations = strategy.search(
        &conn,
        &StrategySearchOptions {
            search_type: SearchType::Observations,
            project: Some("cloudy".into()),
            ..Default::default()
        },
    );
    assert_eq!(observations.results.observations.len(), 2);
    assert!(observations.results.sessions.is_empty());
    assert!(observations.results.prompts.is_empty());

    let sessions = strategy.search(
        &conn,
        &StrategySearchOptions {
            search_type: SearchType::Sessions,
            project: Some("cloudy".into()),
            ..Default::default()
        },
    );
    assert!(sessions.results.observations.is_empty());
    assert_eq!(sessions.results.sessions.len(), 1);

    let prompts = strategy.search(
        &conn,
        &StrategySearchOptions {
            search_type: SearchType::Prompts,
            project: Some("cloudy".into()),
            ..Default::default()
        },
    );
    assert_eq!(prompts.results.prompts.len(), 1);
}

#[test]
fn applies_type_concept_file_date_and_order_filters() {
    let conn = seeded_db();
    let strategy = SqliteSearchStrategy::new();

    let by_type = strategy
        .find_by_type(
            &conn,
            &[String::from("decision")],
            &StrategySearchOptions {
                project: Some("cloudy".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(by_type.len(), 1);
    assert_eq!(by_type[0].r#type, "decision");

    let by_concept = strategy
        .find_by_concept(
            &conn,
            "thermal",
            &StrategySearchOptions {
                project: Some("cloudy".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(by_concept.len(), 1);
    assert_eq!(by_concept[0].title.as_deref(), Some("Dynatron cap"));

    let by_file = strategy
        .find_by_file(
            &conn,
            "thermal.rs",
            &StrategySearchOptions {
                project: Some("cloudy".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(by_file.observations.len(), 1);
    assert_eq!(by_file.sessions.len(), 1);

    let newest_first = strategy.search(
        &conn,
        &StrategySearchOptions {
            search_type: SearchType::Observations,
            project: Some("cloudy".into()),
            order_by: OrderBy::DateDesc,
            ..Default::default()
        },
    );
    assert_eq!(
        newest_first.results.observations[0].title.as_deref(),
        Some("Chassis fans")
    );

    let date_filtered = strategy.search(
        &conn,
        &StrategySearchOptions {
            search_type: SearchType::Observations,
            project: Some("cloudy".into()),
            date_range: Some(DateRange {
                start_epoch: Some(1_735_732_801_000),
                end_epoch: None,
            }),
            ..Default::default()
        },
    );
    assert_eq!(date_filtered.results.observations.len(), 1);
    assert_eq!(
        date_filtered.results.observations[0].title.as_deref(),
        Some("Chassis fans")
    );
}

#[test]
fn fts_query_searches_observations_sessions_and_prompts() {
    let conn = seeded_db();
    let strategy = SqliteSearchStrategy::new();

    let result = strategy.search(
        &conn,
        &StrategySearchOptions {
            query: Some("Dynatron wattage".into()),
            strategy_hint: Some(SearchStrategyHint::Sqlite),
            project: Some("cloudy".into()),
            limit: Some(10),
            ..Default::default()
        },
    );

    assert_eq!(result.results.observations.len(), 1);
    assert_eq!(
        result.results.observations[0].title.as_deref(),
        Some("Dynatron cap")
    );
    assert_eq!(result.results.sessions.len(), 1);
    assert_eq!(result.results.prompts.len(), 1);
}

fn seeded_db() -> rusqlite::Connection {
    let conn = open_in_memory().unwrap();
    create_session(
        &conn,
        &CreateSessionInput {
            content_session_id: "content-cloudy".into(),
            project: "cloudy".into(),
            user_prompt: Some("How should cloudy-k3s handle Dynatron wattage?".into()),
            started_at: "2025-01-01T12:00:00Z".into(),
            started_at_epoch: 1_735_732_800_000,
        },
    )
    .unwrap();
    update_memory_session_id(&conn, "content-cloudy", "memory-cloudy").unwrap();

    save_user_prompt(
        &conn,
        &PromptInput {
            content_session_id: "content-cloudy".into(),
            prompt_number: 1,
            prompt_text: "Dynatron wattage needs attention".into(),
            created_at: "2025-01-01T12:00:00Z".into(),
            created_at_epoch: 1_735_732_800_000,
        },
    )
    .unwrap();

    store_summary(
        &conn,
        &SummaryInput {
            memory_session_id: "memory-cloudy".into(),
            project: "cloudy".into(),
            request: Some("Investigate Dynatron wattage".into()),
            files_read: Some("[\"src/thermal.rs\"]".into()),
            discovery_tokens: Some(500),
            created_at: "2025-01-01T12:00:00Z".into(),
            created_at_epoch: 1_735_732_800_000,
            ..Default::default()
        },
    )
    .unwrap();

    let first = store_batch(
        &conn,
        "memory-cloudy",
        "cloudy",
        &[ObservationInput {
            r#type: "decision".into(),
            title: Some("Dynatron cap".into()),
            narrative: Some("Lower package wattage beats chassis fan speed.".into()),
            concepts: Some(vec!["thermal".into(), "power".into()]),
            files_read: Some(vec!["src/thermal.rs".into()]),
            discovery_tokens: Some(100),
            created_at: "2025-01-01T12:00:00Z".into(),
            created_at_epoch: 1_735_732_800_000,
            ..Default::default()
        }],
        None,
        Some(1),
        Some(0),
        Some(1_735_732_800_000),
    )
    .unwrap();
    let second = store_batch(
        &conn,
        "memory-cloudy",
        "cloudy",
        &[ObservationInput {
            r#type: "discovery".into(),
            title: Some("Chassis fans".into()),
            narrative: Some("Fan noise is ambient and chassis related.".into()),
            concepts: Some(vec!["ambient".into()]),
            files_modified: Some(vec!["src/fans.rs".into()]),
            discovery_tokens: Some(50),
            created_at: "2025-01-01T12:00:01Z".into(),
            created_at_epoch: 1_735_732_801_000,
            ..Default::default()
        }],
        None,
        Some(1),
        Some(0),
        Some(1_735_732_801_000),
    )
    .unwrap();
    assert_eq!(first.inserted + second.inserted, 2);
    let observation_ids = [first.observation_ids[0], second.observation_ids[0]];
    assert_eq!(
        get_observations_by_ids(&conn, &observation_ids)
            .unwrap()
            .len(),
        2
    );
    conn
}
