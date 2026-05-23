//! Claude memory lifecycle e2e coverage for the Rust fork.
//!
//! This flexes the complete core memory path under the worker and hook
//! e2e coverage, including session isolation and recall formatting.

use claude_mem_core::context::formatters::{format_observation, FormatOptions};
use claude_mem_core::context::observation_compiler::{query_observations, ObservationQuery};
use claude_mem_core::db::observations::get::{
    get_observations_by_file_path, get_observations_for_session,
};
use claude_mem_core::db::prompts::{
    get_latest_user_prompt, get_prompt_number_from_user_prompts, save_user_prompt, PromptInput,
};
use claude_mem_core::db::sessions::{
    create_session, get_session_by_content_id, get_session_by_memory_id, mark_session_completed,
    update_memory_session_id, CreateSessionOutcome,
};
use claude_mem_core::db::summaries::{get_summary_for_session, SummaryInput};
use claude_mem_core::db::transactions::store_batch;
use claude_mem_core::types::session::CreateSessionInput;
use claude_mem_core::types::ObservationInput;

fn obs(
    title: &str,
    narrative: &str,
    created_at_epoch: i64,
    files_read: &[&str],
    files_modified: &[&str],
) -> ObservationInput {
    ObservationInput {
        r#type: "discovery".into(),
        title: Some(title.into()),
        subtitle: Some("Claude Code fork validation".into()),
        facts: Some(vec![
            "Claude-specific memory was created by the Rust fork".into(),
            "Recall should use the captured memory session id".into(),
        ]),
        narrative: Some(narrative.into()),
        concepts: Some(vec!["claude-code".into(), "memory-lifecycle".into()]),
        files_read: Some(files_read.iter().map(|s| (*s).into()).collect()),
        files_modified: Some(files_modified.iter().map(|s| (*s).into()).collect()),
        created_at: "2026-05-23T15:00:00.000Z".into(),
        created_at_epoch,
        generated_by_model: Some("claude-agent-sdk".into()),
        ..Default::default()
    }
}

fn summary() -> SummaryInput {
    SummaryInput {
        request: Some("Validate Claude memory lifecycle in the Rust fork".into()),
        investigated: Some(
            "Session creation, prompt capture, observation writes, and recall".into(),
        ),
        learned: Some("The core memory path preserves Claude session isolation".into()),
        completed: Some("Stored and recalled Claude-specific memories".into()),
        next_steps: Some("Wire the same path through the Rust hook and worker HTTP layers".into()),
        files_read: Some(r#"["crates/claude-mem-core/src/db/transactions.rs"]"#.into()),
        files_edited: Some(r#"["crates/claude-mem-core/tests/claude_memory_e2e.rs"]"#.into()),
        notes: Some("E2E coverage for implemented fork layers".into()),
        created_at: "2026-05-23T15:00:02.000Z".into(),
        created_at_epoch: 1_748_012_402_000,
        ..Default::default()
    }
}

#[test]
fn claude_session_creates_memory_and_recalls_it_without_cross_contamination() {
    let conn = claude_mem_core::db::open_in_memory().unwrap();

    let content_session_id = "claude-code-content-session-e2e";
    let memory_session_id = "claude-agent-sdk-memory-session-e2e";
    let project = "/home/kcrawley/projects/cloudy-fork";

    let outcome = create_session(
        &conn,
        &CreateSessionInput {
            content_session_id: content_session_id.into(),
            project: project.into(),
            user_prompt: Some(
                "Remember that cloudy-k3s AMD fan noise is CPU-thermal driven, not ambient.".into(),
            ),
            started_at: "2026-05-23T15:00:00.000Z".into(),
            started_at_epoch: 1_748_012_400_000,
        },
    )
    .unwrap();
    assert_eq!(outcome, CreateSessionOutcome::Created);

    let created_session = get_session_by_content_id(&conn, content_session_id)
        .unwrap()
        .expect("Claude session should be persisted");
    assert_eq!(created_session.platform_source, "claude");
    assert!(created_session.memory_session_id.is_none());

    let prompt_id = save_user_prompt(
        &conn,
        &PromptInput {
            content_session_id: content_session_id.into(),
            prompt_number: 0,
            prompt_text: "Use memory to keep the fork's thermal diagnosis available.".into(),
            created_at: "2026-05-23T15:00:01.000Z".into(),
            created_at_epoch: 1_748_012_401_000,
        },
    )
    .unwrap();
    assert!(prompt_id > 0);
    assert_eq!(
        get_prompt_number_from_user_prompts(&conn, content_session_id).unwrap(),
        1
    );

    assert!(update_memory_session_id(&conn, content_session_id, memory_session_id).unwrap());
    let resume_ready_session = get_session_by_memory_id(&conn, memory_session_id)
        .unwrap()
        .expect("captured memory session id should be usable for resume");
    assert_eq!(resume_ready_session.content_session_id, content_session_id);
    assert_eq!(resume_ready_session.platform_source, "claude");

    let batch = store_batch(
        &conn,
        memory_session_id,
        project,
        &[
            obs(
                "Cloudy AMD fan diagnosis",
                "CPU package temperature is the fan trigger; chassis ambient readings stayed low.",
                1_748_012_403_000,
                &["/notes/cloudy-thermal.md"],
                &[],
            ),
            obs(
                "Dynatron cooler power cap",
                "The tiny 1U Dynatron coolers cannot dissipate full 105W Ryzen load, so power limiting is the durable mitigation.",
                1_748_012_404_000,
                &["/etc/rc.local"],
                &["/etc/rc.local"],
            ),
        ],
        Some(&summary()),
        Some(0),
        Some(321),
        Some(1_748_012_405_000),
    )
    .unwrap();

    assert_eq!(batch.inserted, 2);
    assert_eq!(batch.duplicates, 0);
    assert_eq!(batch.observation_ids.len(), 2);
    assert!(batch.summary_id.is_some());

    let latest_prompt = get_latest_user_prompt(&conn, content_session_id)
        .unwrap()
        .expect("latest Claude prompt should be readable");
    assert_eq!(
        latest_prompt.prompt_text,
        "Use memory to keep the fork's thermal diagnosis available."
    );

    let session_observations = get_observations_for_session(&conn, memory_session_id).unwrap();
    assert_eq!(session_observations.len(), 2);
    assert!(session_observations
        .iter()
        .all(|row| row.memory_session_id == memory_session_id));
    assert!(session_observations
        .iter()
        .all(|row| row.project == project));
    assert!(session_observations
        .iter()
        .all(|row| row.prompt_number == Some(0)));
    assert!(session_observations
        .iter()
        .all(|row| row.discovery_tokens == 321));

    let summaries = get_summary_for_session(&conn, memory_session_id).unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(
        summaries[0].request.as_deref(),
        Some("Validate Claude memory lifecycle in the Rust fork")
    );

    let fts_hits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM observations_fts WHERE observations_fts MATCH 'dynatron'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        fts_hits, 1,
        "created observations should be recallable through FTS"
    );

    let project_hits = query_observations(
        &conn,
        &ObservationQuery {
            project: Some(project.into()),
            limit: 10,
        },
    )
    .unwrap();
    assert_eq!(project_hits.len(), 2);
    assert_eq!(
        project_hits[0].title.as_deref(),
        Some("Dynatron cooler power cap")
    );

    let file_hits = get_observations_by_file_path(&conn, "/etc/rc.local", Some(10)).unwrap();
    assert_eq!(file_hits.len(), 1);
    assert_eq!(
        file_hits[0].title.as_deref(),
        Some("Dynatron cooler power cap")
    );

    let context_payload = project_hits
        .iter()
        .map(|row| format_observation(row, &FormatOptions::default()))
        .collect::<Vec<_>>()
        .join("\n\n");
    assert!(context_payload.contains("Dynatron cooler power cap"));
    assert!(context_payload.contains("power limiting is the durable mitigation"));

    let other_content_session_id = "claude-code-content-session-other";
    let other_memory_session_id = "claude-agent-sdk-memory-session-other";
    let other_project = "/home/kcrawley/projects/other";
    create_session(
        &conn,
        &CreateSessionInput {
            content_session_id: other_content_session_id.into(),
            project: other_project.into(),
            user_prompt: Some("Unrelated Claude prompt".into()),
            started_at: "2026-05-23T16:00:00.000Z".into(),
            started_at_epoch: 1_748_016_000_000,
        },
    )
    .unwrap();
    update_memory_session_id(&conn, other_content_session_id, other_memory_session_id).unwrap();
    store_batch(
        &conn,
        other_memory_session_id,
        other_project,
        &[obs(
            "Unrelated memory",
            "This should not appear in cloudy-fork project recall.",
            1_748_016_001_000,
            &[],
            &[],
        )],
        None,
        Some(0),
        Some(1),
        Some(1_748_016_001_000),
    )
    .unwrap();

    let isolated_hits = query_observations(
        &conn,
        &ObservationQuery {
            project: Some(project.into()),
            limit: 10,
        },
    )
    .unwrap();
    assert_eq!(isolated_hits.len(), 2);
    assert!(!isolated_hits
        .iter()
        .any(|row| row.title.as_deref() == Some("Unrelated memory")));

    mark_session_completed(&conn, resume_ready_session.id).unwrap();
    let completed_session = get_session_by_content_id(&conn, content_session_id)
        .unwrap()
        .expect("completed Claude session should still be readable");
    assert_eq!(completed_session.status, "completed");
    assert!(completed_session.completed_at_epoch.is_some());
}
