//! Observations module tests (port of
//! `tests/sqlite/observations.test.ts`).
//!
//! Exercises `store_observation`, `get_observation_by_id`, `get_recent_observations`
//! against an in-memory schema with full migrations applied.

use claude_mem_core::db::observations::{
    get_observation_by_id, get_recent_observations, store_observation,
    compute_observation_content_hash,
};
use claude_mem_core::db::sessions;
use claude_mem_core::db::open_in_memory;
use claude_mem_core::types::observation::ObservationInput;
use claude_mem_core::types::session::CreateSessionInput;

fn base_input() -> ObservationInput {
    ObservationInput {
        r#type: "discovery".into(),
        title: Some("Test Observation".into()),
        subtitle: Some("Test Subtitle".into()),
        facts: Some(vec!["fact1".into(), "fact2".into()]),
        narrative: Some("Test narrative content".into()),
        concepts: Some(vec!["concept1".into(), "concept2".into()]),
        files_read: Some(vec!["/path/to/file1.ts".into()]),
        files_modified: Some(vec!["/path/to/file2.ts".into()]),
        created_at: "2026-05-23T15:00:00Z".into(),
        created_at_epoch: 1748012400000,
        ..Default::default()
    }
}

/// Port of `createSessionWithMemoryId` — idempotent session create + memory
/// id population, returns the memory_session_id string for FK use.
fn create_session_with_memory_id(
    conn: &rusqlite::Connection,
    content_session_id: &str,
    memory_session_id: &str,
    project: &str,
) -> String {
    let input = CreateSessionInput {
        content_session_id: content_session_id.into(),
        project: project.into(),
        user_prompt: Some("initial prompt".into()),
        started_at: "2026-05-23T15:00:00Z".into(),
        started_at_epoch: 1748012400,
    };
    sessions::create_session(conn, &input).unwrap();
    sessions::update_memory_session_id(conn, content_session_id, memory_session_id).unwrap();
    memory_session_id.to_string()
}

fn stored_input_from(obs: ObservationInput, memory_session_id: &str, project: &str) -> ObservationInput {
    let mut input = obs;
    input.memory_session_id = memory_session_id.into();
    input.project = project.into();
    input.content_hash = Some(compute_observation_content_hash(&input));
    input
}

#[test]
fn store_returns_positive_id() {
    let conn = open_in_memory().unwrap();
    let mem = create_session_with_memory_id(&conn, "content-123", "mem-session-123", "test-project");
    let obs = stored_input_from(base_input(), &mem, "test-project");
    match store_observation(&conn, &obs).unwrap() {
        claude_mem_core::db::observations::store::StoreObservationResult::Inserted(id) => {
            assert!(id > 0);
        }
        other => panic!("expected Inserted, got {:?}", other),
    }
}

#[test]
fn store_persists_all_fields() {
    let conn = open_in_memory().unwrap();
    let mem = create_session_with_memory_id(&conn, "content-456", "mem-session-456", "test-project");
    let mut obs = stored_input_from(
        base_input(),
        &mem,
        "test-project",
    );
    obs.r#type = "bugfix".into();
    obs.title = Some("Fixed critical bug".into());
    obs.subtitle = Some("Memory leak".into());
    obs.facts = Some(vec!["leak found".into(), "patched".into()]);
    obs.narrative = Some("Fixed memory leak in parser".into());
    obs.concepts = Some(vec!["memory".into(), "gc".into()]);
    obs.files_read = Some(vec!["/src/parser.ts".into()]);
    obs.files_modified = Some(vec![
        "/src/parser.ts".into(),
        "/tests/parser.test.ts".into(),
    ]);
    obs.prompt_number = Some(1);
    obs.discovery_tokens = Some(100);

    let id = match store_observation(&conn, &obs).unwrap() {
        claude_mem_core::db::observations::store::StoreObservationResult::Inserted(id) => id,
        other => panic!("expected Inserted, got {:?}", other),
    };

    let stored = get_observation_by_id(&conn, id).unwrap().expect("row");
    assert_eq!(stored.r#type, "bugfix");
    assert_eq!(stored.title.as_deref(), Some("Fixed critical bug"));
    assert_eq!(stored.memory_session_id, "mem-session-456");
    assert_eq!(stored.project, "test-project");
}

#[test]
fn override_timestamp_epoch_honours_caller_value() {
    let conn = open_in_memory().unwrap();
    let mem = create_session_with_memory_id(&conn, "content-789", "mem-session-789", "test-project");
    let past = 1_600_000_000_000i64; // Sep 13, 2020
    let mut obs = stored_input_from(base_input(), &mem, "test-project");
    obs.created_at_epoch = past;
    obs.created_at = "2020-09-13T12:26:40.000Z".into();

    let id = match store_observation(&conn, &obs).unwrap() {
        claude_mem_core::db::observations::store::StoreObservationResult::Inserted(id) => id,
        other => panic!("{:?}", other),
    };
    let stored = get_observation_by_id(&conn, id).unwrap().unwrap();
    assert_eq!(stored.created_at_epoch, past);
}

#[test]
fn null_subtitle_and_narrative_round_trip() {
    let conn = open_in_memory().unwrap();
    let mem = create_session_with_memory_id(&conn, "content-null", "session-null", "p");
    let mut obs = stored_input_from(base_input(), &mem, "p");
    obs.subtitle = None;
    obs.narrative = None;

    let id = match store_observation(&conn, &obs).unwrap() {
        claude_mem_core::db::observations::store::StoreObservationResult::Inserted(id) => id,
        other => panic!("{:?}", other),
    };
    let stored = get_observation_by_id(&conn, id).unwrap().unwrap();
    assert_eq!(stored.id, id);
    assert!(stored.subtitle.is_none());
    assert!(stored.narrative.is_none());
}

#[test]
fn get_by_id_returns_none_for_missing() {
    let conn = open_in_memory().unwrap();
    assert!(get_observation_by_id(&conn, 99999).unwrap().is_none());
}

#[test]
fn recent_ordered_desc_and_respects_limit_and_project_filter() {
    let conn = open_in_memory().unwrap();
    let project = "test-project";
    let m1 = create_session_with_memory_id(&conn, "c1", "s1", project);
    let m2 = create_session_with_memory_id(&conn, "c2", "s2", project);
    let m3 = create_session_with_memory_id(&conn, "c3", "s3", project);

    for (mem_id, pn, epoch) in [
        (&m1, 1, 1_000_000_000_000i64),
        (&m2, 2, 2_000_000_000_000i64),
        (&m3, 3, 3_000_000_000_000i64),
    ] {
        let mut obs = stored_input_from(base_input(), mem_id, project);
        obs.prompt_number = Some(pn);
        obs.created_at_epoch = epoch;
        obs.created_at = format!("epoch-{}", epoch);
        store_observation(&conn, &obs).unwrap();
    }

    let recent = get_recent_observations(&conn, Some(project), 10).unwrap();
    assert_eq!(recent.len(), 3);
    assert_eq!(recent[0].prompt_number, Some(3));
    assert_eq!(recent[1].prompt_number, Some(2));
    assert_eq!(recent[2].prompt_number, Some(1));

    assert_eq!(
        get_recent_observations(&conn, Some(project), 2).unwrap().len(),
        2
    );

    // Project filter.
    let ma = create_session_with_memory_id(&conn, "ca", "sa", "project-a");
    let mb = create_session_with_memory_id(&conn, "cb", "sb", "project-b");
    store_observation(
        &conn,
        &stored_input_from(base_input(), &ma, "project-a"),
    ).unwrap();
    store_observation(
        &conn,
        &stored_input_from(base_input(), &mb, "project-b"),
    ).unwrap();
    // project-a has only the obs stored under `ma` above; the 3 earlier obs
    // are scoped to "test-project" and don't cross project boundaries.
    assert_eq!(
        get_recent_observations(&conn, Some("project-a"), 10).unwrap().len(),
        1,
    );
    assert_eq!(
        get_recent_observations(&conn, Some("project-b"), 10).unwrap().len(),
        1,
    );

    assert!(get_recent_observations(&conn, Some("nonexistent-project"), 10)
        .unwrap()
        .is_empty());
}

#[test]
fn content_hash_dedup_returns_duplicate() {
    let conn = open_in_memory().unwrap();
    let mem = create_session_with_memory_id(&conn, "cd", "sd", "p");
    let obs = stored_input_from(base_input(), &mem, "p");
    let first = store_observation(&conn, &obs).unwrap();
    let second = store_observation(&conn, &obs).unwrap();
    match (first, second) {
        (
            claude_mem_core::db::observations::store::StoreObservationResult::Inserted(a),
            claude_mem_core::db::observations::store::StoreObservationResult::Duplicate(b),
        ) => assert_eq!(a, b, "duplicate should point back to original rowid"),
        other => panic!("expected (Inserted, Duplicate), got {:?}", other),
    }
}
