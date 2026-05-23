use claude_mem_core::types::{ObservationRow, SessionSummaryRow, UserPromptRow};
use claude_mem_worker::search::result_formatter::{CombinedData, ResultFormatter, SearchResults};

fn mock_observation() -> ObservationRow {
    ObservationRow {
        id: 1,
        memory_session_id: "session-123".into(),
        project: "test-project".into(),
        text: Some("Test observation text".into()),
        r#type: "decision".into(),
        title: Some("Test Decision Title".into()),
        subtitle: Some("A descriptive subtitle".into()),
        narrative: Some("This is the narrative description".into()),
        facts: Some(vec!["fact1".into(), "fact2".into()]),
        concepts: Some(vec!["concept1".into(), "concept2".into()]),
        files_read: Some(vec!["src/file1.ts".into()]),
        files_modified: Some(vec!["src/file2.ts".into()]),
        prompt_number: Some(1),
        discovery_tokens: 100,
        created_at: "2025-01-01T12:00:00.000Z".into(),
        created_at_epoch: 1_735_732_800_000,
        generated_by_model: None,
        relevance_count: 0,
        merged_into_project: None,
        agent_type: None,
        agent_id: None,
        content_hash: None,
    }
}

fn mock_session() -> SessionSummaryRow {
    SessionSummaryRow {
        id: 1,
        memory_session_id: "session-123".into(),
        project: "test-project".into(),
        request: Some("Implement feature X".into()),
        investigated: Some("Looked at code structure".into()),
        learned: Some("Learned about the architecture".into()),
        completed: Some("Added new feature".into()),
        next_steps: Some("Write tests".into()),
        files_read: Some("[\"src/index.ts\"]".into()),
        files_edited: Some("[\"src/feature.ts\"]".into()),
        notes: Some("Additional notes".into()),
        prompt_number: Some(1),
        discovery_tokens: 500,
        created_at: "2025-01-01T12:00:00.000Z".into(),
        created_at_epoch: 1_735_732_800_000,
        merged_into_project: None,
    }
}

fn mock_prompt() -> UserPromptRow {
    UserPromptRow {
        id: 1,
        content_session_id: "content-123".into(),
        prompt_number: 1,
        prompt_text: "Can you help me implement feature X?".into(),
        created_at: "2025-01-01T12:00:00.000Z".into(),
        created_at_epoch: 1_735_732_800_000,
    }
}

#[test]
fn formats_search_results_for_all_result_types() {
    let formatter = ResultFormatter::new();
    let results = SearchResults {
        observations: vec![mock_observation()],
        sessions: vec![mock_session()],
        prompts: vec![mock_prompt()],
    };

    let formatted = formatter.format_search_results(&results, "mixed query", false);

    assert!(formatted.contains("mixed query"));
    assert!(formatted.contains("3 result(s)"));
    assert!(formatted.contains("1 obs"));
    assert!(formatted.contains("1 sessions"));
    assert!(formatted.contains("1 prompts"));
    assert!(formatted.contains("#1"));
    assert!(formatted.contains("#S1"));
    assert!(formatted.contains("#P1"));
    assert!(formatted.contains("Test Decision Title"));
    assert!(formatted.contains("Implement feature X"));
    assert!(formatted.contains("Can you help me implement"));
    assert!(formatted.contains("| ID | Time | T | Title | Read |"));
}

#[test]
fn formats_empty_results_and_chroma_failure() {
    let formatter = ResultFormatter::new();
    let results = SearchResults::default();

    let empty = formatter.format_search_results(&results, "no matches", false);
    assert!(empty.contains("No results found"));
    assert!(empty.contains("no matches"));

    let failed = formatter.format_search_results(&results, "test", true);
    assert!(failed.contains("Vector search failed"));
    assert!(failed.contains("semantic search unavailable"));
}

#[test]
fn combines_results_with_type_epoch_and_created_at() {
    let formatter = ResultFormatter::new();
    let observation = mock_observation();
    let session = mock_session();
    let prompt = mock_prompt();
    let results = SearchResults {
        observations: vec![observation.clone()],
        sessions: vec![session],
        prompts: vec![prompt],
    };

    let combined = formatter.combine_results(&results);

    assert_eq!(combined.len(), 3);
    assert!(matches!(combined[0].data, CombinedData::Observation(_)));
    assert!(combined.iter().any(|row| row.result_type == "session"));
    assert!(combined.iter().any(|row| row.result_type == "prompt"));
    assert_eq!(combined[0].epoch, observation.created_at_epoch);
    assert_eq!(combined[0].created_at, observation.created_at);
}

#[test]
fn table_headers_match_search_and_index_modes() {
    let formatter = ResultFormatter::new();

    let search_header = formatter.format_search_table_header();
    assert!(search_header.contains("| Read |"));
    assert!(!search_header.contains("| Work |"));

    let full_header = formatter.format_table_header();
    assert!(full_header.contains("| Work |"));
    assert!(full_header.contains("| ID |"));
    assert!(full_header.contains("| Time |"));
}

#[test]
fn formats_rows_and_repeated_times() {
    let formatter = ResultFormatter::new();
    let observation = mock_observation();

    let first = formatter.format_observation_search_row(&observation, "");
    assert!(first.row.contains("#1"));
    assert!(first.row.contains("Test Decision Title"));
    assert!(first.row.contains("~"));

    let repeated = formatter.format_observation_search_row(&observation, &first.time);
    assert!(repeated.row.contains('"'));
    assert_eq!(repeated.time, first.time);

    let session = formatter.format_session_search_row(&mock_session(), "");
    assert!(session.row.contains("#S1"));
    assert!(session.row.contains("Implement feature X"));

    let mut no_request = mock_session();
    no_request.request = None;
    let fallback = formatter.format_session_search_row(&no_request, "");
    assert!(fallback.row.contains("Session session-"));

    let prompt = formatter.format_prompt_search_row(&mock_prompt(), "");
    assert!(prompt.row.contains("#P1"));
    assert!(prompt.row.contains("Can you help me implement"));
}

#[test]
fn truncates_long_prompts_and_formats_index_rows() {
    let formatter = ResultFormatter::new();
    let mut prompt = mock_prompt();
    prompt.prompt_text = "A".repeat(100);

    let prompt_row = formatter.format_prompt_search_row(&prompt, "");
    assert!(prompt_row.row.contains("..."));
    assert!(prompt_row.row.len() < prompt.prompt_text.len() + 50);

    let observation = mock_observation();
    let observation_index = formatter.format_observation_index(&observation, 0);
    assert!(observation_index.contains("#1"));
    assert!(observation_index.contains("100"));

    let mut no_tokens = observation;
    no_tokens.discovery_tokens = 0;
    assert!(formatter
        .format_observation_index(&no_tokens, 0)
        .contains("-"));

    assert!(formatter
        .format_session_index(&mock_session(), 0)
        .contains("#S1"));
    assert!(formatter
        .format_prompt_index(&mock_prompt(), 0)
        .contains("#P1"));
}

#[test]
fn search_tips_include_workflow_and_filters() {
    let formatter = ResultFormatter::new();
    let tips = formatter.format_search_tips();

    assert!(tips.contains("Search Strategy"));
    assert!(tips.contains("timeline"));
    assert!(tips.contains("get_observations"));
    assert!(tips.contains("obs_type"));
    assert!(tips.contains("dateStart"));
    assert!(tips.contains("orderBy"));
}
