use claude_mem_core::db::pending_messages::{EnqueueInput, PendingMessageStore};
use claude_mem_core::db::sessions::{create_session, update_memory_session_id};
use claude_mem_core::db::{open_in_memory, summaries};
use claude_mem_core::types::session::CreateSessionInput;
use claude_mem_worker::agents::response_processor::{
    parse_observations, parse_summary, process_agent_response, ActiveSession, ObservationBroadcast,
    ProcessAgentResponseOptions, ResponseBroadcaster, SummaryBroadcast,
};
use rusqlite::Connection;
use std::sync::Mutex;

#[derive(Default)]
struct CapturingBroadcaster {
    observations: Mutex<Vec<ObservationBroadcast>>,
    summaries: Mutex<Vec<SummaryBroadcast>>,
    status_calls: Mutex<usize>,
}

impl ResponseBroadcaster for CapturingBroadcaster {
    fn broadcast_observation(&self, observation: ObservationBroadcast) {
        self.observations.lock().unwrap().push(observation);
    }

    fn broadcast_summary(&self, summary: SummaryBroadcast) {
        self.summaries.lock().unwrap().push(summary);
    }

    fn broadcast_processing_status(&self) {
        *self.status_calls.lock().unwrap() += 1;
    }
}

fn seed_session(conn: &Connection) -> ActiveSession {
    create_session(
        conn,
        &CreateSessionInput {
            content_session_id: "content-session-123".to_owned(),
            project: "test-project".to_owned(),
            user_prompt: Some("Test prompt".to_owned()),
            started_at: "2026-05-23T00:00:00Z".to_owned(),
            started_at_epoch: 1_748_000_000_000,
        },
    )
    .unwrap();
    update_memory_session_id(conn, "content-session-123", "memory-session-456").unwrap();

    ActiveSession {
        session_db_id: 1,
        content_session_id: "content-session-123".to_owned(),
        memory_session_id: Some("memory-session-456".to_owned()),
        project: "test-project".to_owned(),
        platform_source: "claude".to_owned(),
        last_prompt_number: Some(5),
        earliest_pending_timestamp: Some(1_700_000_000_000),
        ..Default::default()
    }
}

#[test]
fn parses_single_and_multiple_observations() {
    let observations = parse_observations(
        r#"
        <observation>
          <type>discovery</type>
          <title>Found important pattern</title>
          <subtitle>In auth module</subtitle>
          <narrative>Discovered reusable authentication pattern.</narrative>
          <facts><fact>Uses JWT</fact></facts>
          <concepts><concept>authentication</concept><concept>discovery</concept></concepts>
          <files_read><file>src/auth.ts</file></files_read>
          <files_modified></files_modified>
        </observation>
        <observation>
          <type>bugfix</type>
          <title>Fixed null pointer</title>
          <facts></facts>
          <concepts></concepts>
          <files_read></files_read>
          <files_modified></files_modified>
        </observation>
        "#,
    );

    assert_eq!(observations.len(), 2);
    assert_eq!(observations[0].r#type, "discovery");
    assert_eq!(
        observations[0].title.as_deref(),
        Some("Found important pattern")
    );
    assert_eq!(observations[0].concepts, vec!["authentication"]);
    assert_eq!(observations[1].r#type, "bugfix");
}

#[test]
fn parses_summary_and_skip_summary() {
    let summary = parse_summary(
        r#"
        <summary>
          <request>Build login form</request>
          <investigated>Reviewed existing forms</investigated>
          <learned>React Hook Form works well</learned>
          <completed>Form skeleton created</completed>
          <next_steps>Add validation</next_steps>
          <notes>Some notes</notes>
        </summary>
        "#,
    )
    .unwrap();

    assert_eq!(summary.request.as_deref(), Some("Build login form"));
    assert_eq!(summary.notes.as_deref(), Some("Some notes"));
    assert!(parse_summary(r#"<skip_summary reason="no changes" />"#).is_none());
    assert!(parse_summary("<summary>plain text only</summary>").is_none());
}

#[test]
fn stores_observations_and_summary_atomically_and_broadcasts() {
    let conn = open_in_memory().unwrap();
    let mut session = seed_session(&conn);
    let pending_store = PendingMessageStore::default();
    let broadcaster = CapturingBroadcaster::default();

    let processed = process_agent_response(
        &conn,
        r#"
        <observation>
          <type>discovery</type>
          <title>Broadcast Test</title>
          <subtitle>Testing broadcast</subtitle>
          <narrative>Testing SSE broadcast</narrative>
          <facts><fact>Fact 1</fact></facts>
          <concepts><concept>testing</concept></concepts>
          <files_read><file>test.ts</file></files_read>
          <files_modified></files_modified>
        </observation>
        <summary>
          <request>Build feature</request>
          <investigated>Reviewed code</investigated>
          <learned>Found patterns</learned>
          <completed>Feature built</completed>
          <next_steps>Add tests</next_steps>
        </summary>
        "#,
        &mut session,
        &pending_store,
        Some(&broadcaster),
        ProcessAgentResponseOptions {
            discovery_tokens: Some(100),
            original_timestamp: Some(1_700_000_000_000),
            agent_name: "TestAgent".to_owned(),
            model_id: None,
        },
    )
    .unwrap();

    assert_eq!(processed.observations.len(), 1);
    assert_eq!(processed.storage.observation_ids.len(), 1);
    assert!(processed.storage.summary_id.is_some());
    assert_eq!(processed.storage.created_at_epoch, 1_700_000_000_000);

    let observations = broadcaster.observations.lock().unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].title.as_deref(), Some("Broadcast Test"));
    assert_eq!(observations[0].id, processed.storage.observation_ids[0]);

    let summaries = broadcaster.summaries.lock().unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].request.as_deref(), Some("Build feature"));
    assert_eq!(*broadcaster.status_calls.lock().unwrap(), 1);
    assert_eq!(session.earliest_pending_timestamp, None);
}

#[test]
fn handles_empty_and_non_xml_responses_without_storage_rows() {
    let conn = open_in_memory().unwrap();
    let mut session = seed_session(&conn);
    let pending_store = PendingMessageStore::default();

    let empty = process_agent_response(
        &conn,
        "",
        &mut session,
        &pending_store,
        None,
        ProcessAgentResponseOptions::default(),
    )
    .unwrap();
    assert_eq!(empty.observations.len(), 0);
    assert!(empty.summary.is_none());
    assert!(!empty.discarded_non_xml);

    let plain = process_agent_response(
        &conn,
        "This is just plain text without XML tags.",
        &mut session,
        &pending_store,
        None,
        ProcessAgentResponseOptions::default(),
    )
    .unwrap();
    assert!(plain.discarded_non_xml);
    assert_eq!(plain.storage.observation_ids.len(), 0);
}

#[test]
fn missing_memory_session_id_is_an_error() {
    let conn = open_in_memory().unwrap();
    create_session(
        &conn,
        &CreateSessionInput {
            content_session_id: "content-no-memory".to_owned(),
            project: "test-project".to_owned(),
            user_prompt: None,
            started_at: "2026-05-23T00:00:00Z".to_owned(),
            started_at_epoch: 1_748_000_000_000,
        },
    )
    .unwrap();
    let mut session = ActiveSession {
        session_db_id: 1,
        content_session_id: "content-no-memory".to_owned(),
        memory_session_id: None,
        project: "test-project".to_owned(),
        platform_source: "claude".to_owned(),
        ..Default::default()
    };

    let error = process_agent_response(
        &conn,
        "<observation><type>discovery</type></observation>",
        &mut session,
        &PendingMessageStore::default(),
        None,
        ProcessAgentResponseOptions::default(),
    )
    .unwrap_err();

    assert!(error.to_string().contains("memory_session_id"));
}

#[test]
fn confirms_processing_messages_after_successful_storage() {
    let conn = open_in_memory().unwrap();
    let mut session = seed_session(&conn);
    let pending_store = PendingMessageStore::default();
    let pending_id = pending_store
        .enqueue(
            &conn,
            &EnqueueInput {
                session_db_id: session.session_db_id,
                content_session_id: session.content_session_id.clone(),
                message_type: "observation".to_owned(),
                created_at_epoch: 1_700_000_000_000,
                ..Default::default()
            },
        )
        .unwrap();
    session.processing_message_ids.push(pending_id);

    process_agent_response(
        &conn,
        "<observation><type>discovery</type><title>Test</title></observation>",
        &mut session,
        &pending_store,
        None,
        ProcessAgentResponseOptions::default(),
    )
    .unwrap();

    let remaining: i64 = conn
        .query_row("SELECT COUNT(*) FROM pending_messages", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(remaining, 0);
    assert!(session.processing_message_ids.is_empty());
}

#[test]
fn stored_summary_can_be_read_back() {
    let conn = open_in_memory().unwrap();
    let mut session = seed_session(&conn);
    let pending_store = PendingMessageStore::default();

    let processed = process_agent_response(
        &conn,
        r#"<summary><request>Req</request><investigated>Inv</investigated></summary>"#,
        &mut session,
        &pending_store,
        None,
        ProcessAgentResponseOptions::default(),
    )
    .unwrap();

    let summary = summaries::get_summary_by_id(&conn, processed.storage.summary_id.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(summary.request.as_deref(), Some("Req"));
    assert_eq!(summary.investigated.as_deref(), Some("Inv"));
}
