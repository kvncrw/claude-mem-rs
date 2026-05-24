#![cfg(feature = "qdrant")]

use axum::body::{to_bytes, Body};
use axum::extract::{Path, State};
use axum::http::{Method, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use claude_mem_core::types::ObservationRow;
use claude_mem_worker::http::router::{build_router_with_state, AppState};
use claude_mem_worker::search::qdrant::{QdrantClient, QdrantConfig};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tower::ServiceExt;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Default)]
struct FakeQdrantState {
    collection_exists: Mutex<bool>,
    collection_body: Mutex<Option<Value>>,
    upsert_body: Mutex<Option<Value>>,
}

async fn spawn_fake_qdrant() -> (String, Arc<FakeQdrantState>) {
    let state = Arc::new(FakeQdrantState::default());
    let app = Router::new()
        .route(
            "/collections/:collection",
            get(get_collection).put(put_collection),
        )
        .route("/collections/:collection/points", put(upsert_points))
        .route(
            "/collections/:collection/points/search",
            post(search_points),
        )
        .with_state(Arc::clone(&state));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

async fn get_collection(
    State(state): State<Arc<FakeQdrantState>>,
    Path(_collection): Path<String>,
) -> impl IntoResponse {
    if *state.collection_exists.lock().unwrap() {
        (
            StatusCode::OK,
            Json(json!({ "result": { "status": "green" } })),
        )
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "status": "not_found" })),
        )
    }
}

async fn put_collection(
    State(state): State<Arc<FakeQdrantState>>,
    Path(_collection): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    *state.collection_exists.lock().unwrap() = true;
    *state.collection_body.lock().unwrap() = Some(body);
    (StatusCode::OK, Json(json!({ "result": true })))
}

async fn upsert_points(
    State(state): State<Arc<FakeQdrantState>>,
    Path(_collection): Path<String>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    *state.collection_exists.lock().unwrap() = true;
    *state.upsert_body.lock().unwrap() = Some(body);
    (
        StatusCode::OK,
        Json(json!({ "result": { "operation_id": 1 } })),
    )
}

async fn search_points(
    State(state): State<Arc<FakeQdrantState>>,
    Path(_collection): Path<String>,
    Json(_body): Json<Value>,
) -> impl IntoResponse {
    let id = state
        .upsert_body
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|body| body["points"][0]["payload"]["observation_id"].as_i64())
        .unwrap_or(1);
    (
        StatusCode::OK,
        Json(json!({
            "result": [
                { "id": id, "score": 0.9, "payload": { "observation_id": id } }
            ]
        })),
    )
}

fn config(url: String) -> QdrantConfig {
    QdrantConfig {
        url,
        collection: "test_observations".into(),
        api_key: None,
        vector_size: 16,
    }
}

fn observation(id: i64) -> ObservationRow {
    ObservationRow {
        id,
        memory_session_id: "memory-session".into(),
        project: "cloudy-fork".into(),
        text: None,
        r#type: "discovery".into(),
        title: Some("Dynatron power cap".into()),
        subtitle: Some("Manual memory".into()),
        narrative: Some("Reduce package power before increasing chassis fans.".into()),
        facts: None,
        concepts: Some(vec!["thermal".into()]),
        files_read: Some(vec!["/repo/thermal.md".into()]),
        files_modified: None,
        prompt_number: Some(1),
        discovery_tokens: 10,
        created_at: "2026-05-23T00:00:00Z".into(),
        created_at_epoch: 1,
        generated_by_model: Some("test".into()),
        relevance_count: 0,
        merged_into_project: None,
        agent_type: None,
        agent_id: None,
        content_hash: Some("hash".into()),
    }
}

#[tokio::test]
async fn qdrant_client_creates_collection_upserts_and_searches_points() {
    let (url, state) = spawn_fake_qdrant().await;
    let client = QdrantClient::new(config(url));

    client
        .upsert_observations(&[observation(42)])
        .await
        .unwrap();

    let collection = state.collection_body.lock().unwrap().clone().unwrap();
    assert_eq!(collection["vectors"]["size"], 16);
    assert_eq!(collection["vectors"]["distance"], "Cosine");

    let upsert = state.upsert_body.lock().unwrap().clone().unwrap();
    assert_eq!(upsert["points"][0]["id"], 42);
    assert_eq!(upsert["points"][0]["payload"]["project"], "cloudy-fork");
    assert_eq!(upsert["points"][0]["vector"].as_array().unwrap().len(), 16);

    let ids = client
        .search_observation_ids("package power", 5)
        .await
        .unwrap();
    assert_eq!(ids, vec![42]);
}

async fn json_request(app: Router, method: Method, uri: &str, body: Value) -> (StatusCode, Value) {
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
    (status, serde_json::from_slice(&body).unwrap())
}

async fn get_json(app: Router, uri: &str) -> (StatusCode, Value) {
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
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test(flavor = "current_thread")]
async fn worker_memory_save_indexes_and_searches_with_qdrant_when_enabled() {
    let _guard = ENV_LOCK.lock().unwrap();
    let (url, _state) = spawn_fake_qdrant().await;
    std::env::set_var("CLAUDE_MEM_QDRANT_URL", url);
    std::env::set_var("CLAUDE_MEM_QDRANT_COLLECTION", "worker_e2e");

    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state);

    let (status, save) = json_request(
        app.clone(),
        Method::POST,
        "/api/memory/save",
        json!({
            "project": "cloudy-fork",
            "title": "Dynatron power cap",
            "text": "Tiny 1U Dynatron coolers need lower package wattage."
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(save["success"], true);

    let (status, search) = get_json(
        app,
        "/api/search?strategy=qdrant&query=package%20wattage&project=cloudy-fork&limit=5",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(search["usedQdrant"], true);
    assert_eq!(search["fellBack"], false);
    assert_eq!(search["count"], 1);
    assert_eq!(search["observations"][0]["title"], "Dynatron power cap");

    std::env::remove_var("CLAUDE_MEM_QDRANT_URL");
    std::env::remove_var("CLAUDE_MEM_QDRANT_COLLECTION");
}

#[tokio::test(flavor = "current_thread")]
async fn worker_qdrant_strategy_falls_back_to_sqlite_when_not_enabled() {
    let _guard = ENV_LOCK.lock().unwrap();
    std::env::remove_var("CLAUDE_MEM_QDRANT_URL");
    std::env::remove_var("CLAUDE_MEM_QDRANT_ENABLED");
    std::env::remove_var("CLAUDE_MEM_QDRANT_COLLECTION");

    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state);

    let (status, save) = json_request(
        app.clone(),
        Method::POST,
        "/api/memory/save",
        json!({
            "project": "cloudy-fork",
            "title": "SQLite fallback",
            "text": "Dynatron fallback search should still work without Qdrant."
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(save["success"], true);

    let (status, search) = get_json(
        app,
        "/api/search?strategy=qdrant&query=Dynatron&project=cloudy-fork&limit=5",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(search["usedQdrant"], false);
    assert_eq!(search["fellBack"], true);
    assert_eq!(search["count"], 1);
    assert_eq!(search["observations"][0]["title"], "SQLite fallback");
}

#[tokio::test(flavor = "current_thread")]
async fn worker_qdrant_reindex_populates_observations_summaries_and_prompts() {
    let _guard = ENV_LOCK.lock().unwrap();
    let (url, state) = spawn_fake_qdrant().await;
    std::env::set_var("CLAUDE_MEM_QDRANT_URL", url);
    std::env::set_var("CLAUDE_MEM_QDRANT_COLLECTION", "worker_reindex_all");

    let app = build_router_with_state(AppState::in_memory().unwrap());
    let (status, _init) = json_request(
        app.clone(),
        Method::POST,
        "/api/sessions/init",
        json!({
            "contentSessionId": "qdrant-full-session",
            "project": "cloudy-fork",
            "prompt": "Remember prompt qdrant population."
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _save) = json_request(
        app.clone(),
        Method::POST,
        "/api/memory/save",
        json!({
            "project": "cloudy-fork",
            "title": "Qdrant observation",
            "text": "Observation vector population works."
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _summary) = json_request(
        app.clone(),
        Method::POST,
        "/api/sessions/summarize",
        json!({
            "contentSessionId": "qdrant-full-session",
            "summary": "<summary><request>Qdrant</request><learned>Summary vector population works.</learned><completed>Done</completed></summary>"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, reindex) = json_request(
        app,
        Method::POST,
        "/api/vector/qdrant/reindex",
        json!({ "project": "cloudy-fork", "limit": 100 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reindex["success"], true);
    assert_eq!(reindex["observations"], 1);
    assert_eq!(reindex["summaries"], 1);
    assert_eq!(reindex["prompts"], 1);

    let upsert = state.upsert_body.lock().unwrap().clone().unwrap();
    let kinds = upsert["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|point| point["payload"]["kind"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(kinds.contains(&"observation".to_owned()));
    assert!(kinds.contains(&"summary".to_owned()));
    assert!(kinds.contains(&"prompt".to_owned()));

    std::env::remove_var("CLAUDE_MEM_QDRANT_URL");
    std::env::remove_var("CLAUDE_MEM_QDRANT_COLLECTION");
}

#[tokio::test]
async fn real_qdrant_smoke_when_url_is_supplied() {
    let Ok(url) = std::env::var("QDRANT_URL") else {
        eprintln!("skipping real qdrant smoke; set QDRANT_URL to run it");
        return;
    };
    let client = QdrantClient::new(QdrantConfig {
        url,
        collection: format!("claude_mem_rs_test_{}", std::process::id()),
        api_key: std::env::var("QDRANT_API_KEY")
            .or_else(|_| std::env::var("CLAUDE_MEM_QDRANT_API_KEY"))
            .ok(),
        vector_size: 16,
    });
    client.upsert_observations(&[observation(7)]).await.unwrap();
    let ids = client
        .search_observation_ids("package power", 3)
        .await
        .unwrap();
    assert!(ids.contains(&7));
}
