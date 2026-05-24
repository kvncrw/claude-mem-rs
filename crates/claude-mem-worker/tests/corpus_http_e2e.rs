//! End-to-end HTTP tests for the corpus subsystem.
//!
//! Mirrors the patterns in `claude_http_e2e.rs` — in-memory SQLite via
//! `AppState::in_memory`, axum oneshot requests via `tower::ServiceExt`,
//! and a `tempfile::TempDir` rooted at `$CLAUDE_MEM_HOME` so each test gets
//! its own corpora directory under `<tmp>/corpora/`.
//!
//! These tests exercise the default-feature build (knowledge-agent OFF), so
//! prime/query/reprime return 501. The 501 assertions are gated behind
//! `#[cfg(not(feature = "knowledge-agent"))]` so the suite stays green under
//! `--features knowledge-agent` as well.

use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use claude_mem_core::db::sessions::{create_session, update_memory_session_id};
use claude_mem_core::db::transactions::store_batch;
use claude_mem_core::types::session::CreateSessionInput;
use claude_mem_core::types::ObservationInput;
use claude_mem_worker::http::router::{build_router_with_state, AppState};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tower::ServiceExt;

/// Serializes mutations of `CLAUDE_MEM_HOME` across tests in this file —
/// `CorpusStore::default()` reads the env on every call, so two parallel tests
/// could otherwise stomp each other's corpora dir. Async mutex so the guard
/// can be held across `.await` boundaries without tripping clippy's
/// `await_holding_lock`.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

async fn json_request(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, value)
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, value)
}

async fn delete_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, value)
}

/// Seed `count` observations for `project`, alternating between the
/// `decision` and `bugfix` observation types. Each row gets a unique
/// `created_at_epoch` so date-range stats are deterministic.
fn seed_observations(state: &AppState, project: &str, count: i64, base_epoch: i64) {
    let conn = state.conn.lock().unwrap();
    let content_id = format!("content-{project}");
    let memory_id = format!("memses-{project}");
    create_session(
        &conn,
        &CreateSessionInput {
            content_session_id: content_id.clone(),
            project: project.to_owned(),
            user_prompt: Some("seed".into()),
            started_at: "2026-05-24T00:00:00Z".into(),
            started_at_epoch: base_epoch,
        },
    )
    .expect("create_session");
    update_memory_session_id(&conn, &content_id, &memory_id).expect("update memory id");

    let observations: Vec<ObservationInput> = (0..count)
        .map(|i| ObservationInput {
            memory_session_id: memory_id.clone(),
            project: project.to_owned(),
            r#type: if i % 2 == 0 {
                "decision".into()
            } else {
                "bugfix".into()
            },
            title: Some(format!("{project}-title-{i}")),
            narrative: Some(format!("{project} narrative body {i}")),
            facts: Some(vec![format!("{project}-fact-{i}")]),
            concepts: Some(vec!["hooks".into()]),
            files_modified: Some(vec![format!("src/{project}/file-{i}.rs")]),
            created_at: "2026-05-24T00:00:00Z".into(),
            created_at_epoch: base_epoch + i,
            ..Default::default()
        })
        .collect();
    store_batch(
        &conn,
        &memory_id,
        project,
        &observations,
        None,
        None,
        None,
        None,
    )
    .expect("store_batch");
}

#[tokio::test]
async fn corpus_http_routes_build_list_get_rebuild_and_delete() {
    let _guard = ENV_LOCK.lock().await;
    let home = tempfile::TempDir::new().unwrap();
    std::env::set_var("CLAUDE_MEM_HOME", home.path());

    let state = AppState::in_memory().unwrap();
    // 4 alpha obs (2 decisions, 2 bugfixes) + 4 beta obs (2 decisions, 2 bugfixes).
    seed_observations(&state, "alpha", 4, 1_716_500_000_000);
    seed_observations(&state, "beta", 4, 1_716_600_000_000);
    let app = build_router_with_state(state);

    // -- 1. POST /api/corpus — build a corpus filtered by project + types + limit.
    let build_body = json!({
        "name": "alpha-decisions",
        "description": "Alpha project decisions only",
        "project": "alpha",
        "types": ["decision"],
        "limit": 10
    });
    let (status, built) = json_request(app.clone(), Method::POST, "/api/corpus", build_body).await;
    assert_eq!(status, StatusCode::OK, "build failed: {built}");
    assert_eq!(built["name"], "alpha-decisions");
    assert_eq!(built["description"], "Alpha project decisions only");
    assert_eq!(built["version"], 1);
    assert_eq!(built["filter"]["project"], "alpha");
    assert_eq!(built["filter"]["types"][0], "decision");
    assert_eq!(built["filter"]["limit"], 10);
    // Only the 2 alpha decisions should be in the corpus.
    assert_eq!(built["stats"]["observation_count"], 2);
    assert!(built["stats"]["token_estimate"].as_i64().unwrap() > 0);
    assert_eq!(built["stats"]["type_breakdown"]["decision"], 2);
    // /api/corpus and GET /api/corpus/:name both return metadata-only.
    assert!(built.get("observations").is_none());

    // -- 2. GET /api/corpus — list returns the built corpus's metadata.
    let (status, list) = get_json(app.clone(), "/api/corpus").await;
    assert_eq!(status, StatusCode::OK);
    // Worker wraps the list in CallToolResult envelope `{content:[{type,text}]}`.
    let list_text = list["content"][0]["text"]
        .as_str()
        .expect("list envelope text");
    let entries: Vec<Value> = serde_json::from_str(list_text).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "alpha-decisions");
    assert_eq!(entries[0]["description"], "Alpha project decisions only");
    assert_eq!(entries[0]["stats"]["observation_count"], 2);
    // List entries are metadata-only — no observations array.
    assert!(entries[0].get("observations").is_none());

    // -- 3. GET /api/corpus/:name — current implementation returns metadata
    // (same shape as the build/rebuild responses). The full observations array
    // is reachable directly from the on-disk file. Round-trip via the store to
    // confirm.
    let (status, got) = get_json(app.clone(), "/api/corpus/alpha-decisions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["name"], "alpha-decisions");
    assert_eq!(got["stats"]["observation_count"], 2);
    let corpus_path = home
        .path()
        .join("corpora")
        .join("alpha-decisions.corpus.json");
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&corpus_path).unwrap()).unwrap();
    let on_disk_obs = on_disk["observations"].as_array().unwrap();
    assert_eq!(on_disk_obs.len(), 2);
    for obs in on_disk_obs {
        assert_eq!(obs["type"], "decision");
        assert_eq!(obs["project"], "alpha");
    }

    // -- 4. POST /api/corpus/:name/rebuild — re-runs the filter; `updated_at`
    // must change. `now_iso` is second-precision, so sleep just over a second
    // before issuing the rebuild so the timestamp delta is observable.
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let (status, rebuilt) = json_request(
        app.clone(),
        Method::POST,
        "/api/corpus/alpha-decisions/rebuild",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "rebuild failed: {rebuilt}");
    assert_eq!(rebuilt["name"], "alpha-decisions");
    assert_eq!(rebuilt["stats"]["observation_count"], 2);
    // The current builder stamps `created_at` and `updated_at` together on
    // every build, so the meaningful timestamp delta is across calls: the
    // rebuilt corpus's `updated_at` must be strictly later than the original
    // build's `updated_at`.
    let updated_at = rebuilt["updated_at"].as_str().unwrap();
    assert!(
        updated_at > built["updated_at"].as_str().unwrap(),
        "rebuild updated_at should be later than the original build's; got built={} rebuilt={updated_at}",
        built["updated_at"].as_str().unwrap()
    );

    // -- 5. DELETE /api/corpus/:name — removes the corpus file.
    let (status, deleted) = delete_json(app.clone(), "/api/corpus/alpha-decisions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["success"], true);
    assert!(
        !corpus_path.exists(),
        "corpus file should be removed after DELETE"
    );

    // Subsequent GET must 404.
    let (status, missing) = get_json(app.clone(), "/api/corpus/alpha-decisions").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(missing["error"]
        .as_str()
        .unwrap()
        .contains("alpha-decisions"));

    std::env::remove_var("CLAUDE_MEM_HOME");
}

#[cfg(not(feature = "knowledge-agent"))]
#[tokio::test]
async fn corpus_prime_query_reprime_return_501_when_feature_disabled() {
    let _guard = ENV_LOCK.lock().await;
    let home = tempfile::TempDir::new().unwrap();
    std::env::set_var("CLAUDE_MEM_HOME", home.path());

    let state = AppState::in_memory().unwrap();
    seed_observations(&state, "alpha", 2, 1_716_500_000_000);
    let app = build_router_with_state(state);

    // Need an existing corpus so we don't get 404 first.
    let (status, _) = json_request(
        app.clone(),
        Method::POST,
        "/api/corpus",
        json!({ "name": "prime-target", "project": "alpha" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for (method, uri, body) in [
        (Method::POST, "/api/corpus/prime-target/prime", json!({})),
        (
            Method::POST,
            "/api/corpus/prime-target/query",
            json!({ "question": "what changed?" }),
        ),
        (Method::POST, "/api/corpus/prime-target/reprime", json!({})),
    ] {
        let (status, body) = json_request(app.clone(), method.clone(), uri, body).await;
        assert_eq!(
            status,
            StatusCode::NOT_IMPLEMENTED,
            "{uri} should return 501 when knowledge-agent feature is off; body={body}"
        );
        let message = body["error"].as_str().unwrap_or_default();
        assert!(
            message.contains("knowledge-agent") && message.contains("not available"),
            "{uri} 501 body should explain the disabled feature; got {message:?}"
        );
    }

    std::env::remove_var("CLAUDE_MEM_HOME");
}

#[tokio::test]
async fn corpus_build_rejects_invalid_names() {
    let _guard = ENV_LOCK.lock().await;
    let home = tempfile::TempDir::new().unwrap();
    std::env::set_var("CLAUDE_MEM_HOME", home.path());

    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state);

    // Empty after trim — fails the route's own required-field check first.
    let (status, body) = json_request(
        app.clone(),
        Method::POST,
        "/api/corpus",
        json!({ "name": "   " }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("Missing required field: name"));

    // Slash and space — pass the required-field gate, then trip
    // `is_valid_corpus_name` inside the store (also surfaced as 400).
    for bad in ["foo/bar", "foo bar"] {
        let (status, body) = json_request(
            app.clone(),
            Method::POST,
            "/api/corpus",
            json!({ "name": bad, "project": "alpha" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} should be 400");
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("invalid corpus name"),
            "{bad}: expected invalid-name error, got {body}"
        );
    }

    std::env::remove_var("CLAUDE_MEM_HOME");
}

#[tokio::test]
async fn corpus_build_rejects_unknown_observation_type() {
    let _guard = ENV_LOCK.lock().await;
    let home = tempfile::TempDir::new().unwrap();
    std::env::set_var("CLAUDE_MEM_HOME", home.path());

    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state);

    let (status, body) = json_request(
        app.clone(),
        Method::POST,
        "/api/corpus",
        json!({
            "name": "bad-types",
            "project": "alpha",
            "types": ["decision", "made_up"]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = body["error"].as_str().unwrap();
    assert!(
        message.contains("types must contain valid observation types"),
        "expected allow-list error, got {message:?}"
    );

    std::env::remove_var("CLAUDE_MEM_HOME");
}
