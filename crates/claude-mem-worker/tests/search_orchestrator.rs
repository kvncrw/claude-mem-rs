use claude_mem_core::db::sessions::{create_session, update_memory_session_id};
use claude_mem_core::db::transactions::store_batch;
use claude_mem_core::db::{open_in_memory, prompts::save_user_prompt, prompts::PromptInput};
use claude_mem_core::types::session::CreateSessionInput;
use claude_mem_core::types::ObservationInput;
use claude_mem_worker::search::orchestrator::SearchOrchestrator;
use claude_mem_worker::search::result_formatter::SearchResults;
use claude_mem_worker::search::strategies::{OrderBy, SearchType};
use std::collections::HashMap;

#[test]
fn normalizes_url_style_params() {
    let orchestrator = SearchOrchestrator::new();
    let args = map([
        ("query", "thermal limits"),
        ("type", "observations"),
        ("obs_type", "decision, discovery"),
        ("concepts", "thermal, power"),
        ("files", "a.rs, b.rs"),
        ("dateStart", "2025-01-01T00:00:00Z"),
        ("dateEnd", "1735776000000"),
        ("orderBy", "date_asc"),
        ("limit", "10"),
        ("offset", "2"),
        ("strategy", "sqlite"),
    ]);

    let options = orchestrator.search_options(&args);

    assert_eq!(options.query.as_deref(), Some("thermal limits"));
    assert_eq!(options.search_type, SearchType::Observations);
    assert_eq!(options.obs_type, vec!["decision", "discovery"]);
    assert_eq!(options.concepts, vec!["thermal", "power"]);
    assert_eq!(options.files, vec!["a.rs", "b.rs"]);
    assert_eq!(
        options.date_range.unwrap().end_epoch,
        Some(1_735_776_000_000)
    );
    assert_eq!(options.order_by, OrderBy::DateAsc);
    assert_eq!(options.limit, Some(10));
    assert_eq!(options.offset, Some(2));
}

#[test]
fn searches_with_sqlite_strategy_and_formats_results() {
    let conn = seeded_db();
    let orchestrator = SearchOrchestrator::new();

    let result = orchestrator.search(
        &conn,
        &map([("query", "Dynatron"), ("project", "cloudy"), ("limit", "5")]),
    );

    assert_eq!(result.used_chroma, false);
    assert_eq!(result.results.observations.len(), 1);
    assert_eq!(
        result.results.observations[0].title.as_deref(),
        Some("Dynatron cap")
    );

    let formatted = orchestrator.format_search_results(&result.results, "Dynatron", false);
    assert!(formatted.contains("Dynatron"));
    assert!(formatted.contains("Dynatron cap"));
}

#[test]
fn find_helpers_delegate_to_sqlite_strategy() {
    let conn = seeded_db();
    let orchestrator = SearchOrchestrator::new();
    let args = map([("project", "cloudy"), ("limit", "10")]);

    let concept = orchestrator
        .find_by_concept(&conn, "thermal", &args)
        .unwrap();
    assert_eq!(concept.results.observations.len(), 1);

    let by_type = orchestrator
        .find_by_type(&conn, &[String::from("decision")], &args)
        .unwrap();
    assert_eq!(by_type.results.observations.len(), 1);

    let by_file = orchestrator
        .find_by_file(&conn, "thermal.rs", &args)
        .unwrap();
    assert_eq!(by_file.observations.len(), 1);
}

#[test]
fn exposes_formatter_and_chroma_availability() {
    let without_chroma = SearchOrchestrator::new();
    assert!(!without_chroma.is_chroma_available());

    let with_chroma = SearchOrchestrator::with_chroma_available(true);
    assert!(with_chroma.is_chroma_available());

    let formatted = with_chroma.format_search_results(&SearchResults::default(), "nothing", true);
    assert!(formatted.contains("Vector search failed"));
}

fn map<const N: usize>(items: [(&str, &str); N]) -> HashMap<String, String> {
    items
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn seeded_db() -> rusqlite::Connection {
    let conn = open_in_memory().unwrap();
    create_session(
        &conn,
        &CreateSessionInput {
            content_session_id: "content-cloudy".into(),
            project: "cloudy".into(),
            user_prompt: Some("Remember Dynatron limits".into()),
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
            prompt_text: "Dynatron limits".into(),
            created_at: "2025-01-01T12:00:00Z".into(),
            created_at_epoch: 1_735_732_800_000,
        },
    )
    .unwrap();
    store_batch(
        &conn,
        "memory-cloudy",
        "cloudy",
        &[ObservationInput {
            r#type: "decision".into(),
            title: Some("Dynatron cap".into()),
            narrative: Some("Lower CPU package wattage.".into()),
            concepts: Some(vec!["thermal".into()]),
            files_read: Some(vec!["src/thermal.rs".into()]),
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
    conn
}
