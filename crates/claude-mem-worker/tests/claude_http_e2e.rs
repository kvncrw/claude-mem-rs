use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use claude_mem_core::db::observations::get::get_observation_by_id;
use claude_mem_core::db::pending_messages::{EnqueueInput, PendingMessageStore};
use claude_mem_core::db::sessions::get_session_by_content_id;
use claude_mem_worker::agents::observer::{
    process_pending_for_session, ObserverConfig, QueueProcessStats,
};
use claude_mem_worker::http::router::{build_router_with_state, AppState};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::time::{timeout, Duration};
use tower::ServiceExt;

static ENV_LOCK: Mutex<()> = Mutex::new(());

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
    let value = serde_json::from_slice(&body).unwrap_or_else(|_| json!(null));
    (status, value)
}

async fn get_text(app: axum::Router, uri: &str) -> (StatusCode, String) {
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
    (status, String::from_utf8(body.to_vec()).unwrap())
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
    (status, serde_json::from_slice(&body).unwrap())
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
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn claude_hook_facing_http_routes_create_and_recall_memory() {
    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state);

    let (status, health) = get_json(app.clone(), "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["status"], "ok");
    assert_eq!(health["initialized"], true);

    let init_body = json!({
        "contentSessionId": "claude-http-content-e2e",
        "project": "cloudy-fork",
        "prompt": "Remember that the Rust fork should preserve Claude memory.",
        "platformSource": "claude"
    });
    let (status, init) =
        json_request(app.clone(), Method::POST, "/api/sessions/init", init_body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(init["skipped"], false);
    assert_eq!(init["promptNumber"], 1);
    assert!(init["sessionDbId"].as_i64().unwrap() > 0);

    let observation_body = json!({
        "contentSessionId": "claude-http-content-e2e",
        "platformSource": "claude",
        "tool_name": "Read",
        "tool_input": { "file_path": "/repo/src/lib.rs" },
        "tool_response": { "content": "Dynatron thermal mitigation lives here" },
        "cwd": "/home/kcrawley/projects/cloudy-fork"
    });
    let (status, observation) = json_request(
        app.clone(),
        Method::POST,
        "/api/sessions/observations",
        observation_body,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(observation["success"], true);
    assert_eq!(observation["inserted"], 1);
    assert_eq!(observation["observationIds"].as_array().unwrap().len(), 1);

    let manual_body = json!({
        "project": "cloudy-fork",
        "title": "Dynatron power cap",
        "text": "Tiny 1U Dynatron coolers need lower package wattage for stable Claude memory recall."
    });
    let (status, manual) =
        json_request(app.clone(), Method::POST, "/api/memory/save", manual_body).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(manual["success"], true);

    let (status, search) = get_json(
        app.clone(),
        "/api/search?query=Dynatron&project=cloudy-fork&limit=10",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(search["count"], 2);
    let anchor = search["observations"][0]["id"].as_i64().unwrap();

    let (status, formatted_search) = get_json(
        app.clone(),
        "/api/search?query=Dynatron&project=cloudy-fork&limit=10&format=text",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(formatted_search["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Dynatron power cap"));

    let (status, timeline) = get_json(
        app.clone(),
        &format!("/api/timeline?anchor={anchor}&project=cloudy-fork&depth_before=1&depth_after=1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(timeline["anchor"], anchor);
    assert_eq!(timeline["count"], 2);

    let (status, by_concept) = get_json(
        app.clone(),
        "/api/search/by-concept?concept=tool-use&project=cloudy-fork&limit=10",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(by_concept["count"], 1);
    assert_eq!(by_concept["observations"][0]["title"], "Read tool use");

    let (status, by_type) = get_json(
        app.clone(),
        "/api/search/by-type?type=discovery&project=cloudy-fork&limit=10",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(by_type["count"], 2);
    assert!(by_type["observations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["title"] == "Read tool use"));

    let (status, by_file) = get_json(
        app.clone(),
        "/api/search/by-file?filePath=/repo/src/lib.rs&limit=10",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(by_file["count"], 1);
    assert_eq!(by_file["observations"][0]["title"], "Read tool use");

    let semantic_body = json!({
        "q": "What should we remember about Dynatron cooler power limits in cloudy-k3s?",
        "project": "cloudy-fork",
        "limit": 5
    });
    let (status, semantic) = json_request(
        app.clone(),
        Method::POST,
        "/api/context/semantic",
        semantic_body,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(semantic["count"], 2);
    assert!(semantic["context"]
        .as_str()
        .unwrap()
        .contains("Dynatron power cap"));

    let (status, context) = get_text(app.clone(), "/api/context/inject?project=cloudy-fork").await;
    assert_eq!(status, StatusCode::OK);
    assert!(context.contains("Read tool use"));
    assert!(context.contains("Dynatron power cap"));

    let (status, complete) = json_request(
        app.clone(),
        Method::POST,
        "/api/sessions/complete",
        json!({ "contentSessionId": "claude-http-content-e2e", "platformSource": "claude" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(complete["completed"], true);

    let (status, shutdown) =
        json_request(app, Method::POST, "/api/admin/shutdown", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shutdown["success"], true);
}

#[tokio::test]
async fn viewer_admin_import_export_settings_logs_and_summary_routes_work() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    std::env::set_var("CLAUDE_MEM_HOME", home.path());

    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state);

    let (status, html) = get_text(app.clone(), "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("claude-mem-rs"));
    assert!(html.contains("Save Manual Memory"));
    assert!(html.contains("Process Pending Queue"));
    assert!(html.contains("Context Preview"));
    assert!(html.contains("EventSource('/stream')"));
    assert!(html.contains("/api/pending-queue"));
    assert!(html.contains("/api/settings"));
    assert!(html.contains("/api/logs?limit=200"));
    assert!(html.contains("/api/branch/status"));

    let (status, settings) = json_request(
        app.clone(),
        Method::POST,
        "/api/settings",
        json!({ "viewer": { "theme": "system" }, "qdrant": { "enabled": false } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["settings"]["viewer"]["theme"], "system");

    let (status, settings) = get_json(app.clone(), "/api/settings").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(settings["viewer"]["theme"], "system");

    let (status, logs) =
        json_request(app.clone(), Method::POST, "/api/logs/clear", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(logs["success"], true);

    let (status, logs) = get_json(app.clone(), "/api/logs?limit=5").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(logs["count"], 0);

    let (status, branch) = get_json(app.clone(), "/api/branch/status").await;
    assert_eq!(status, StatusCode::OK);
    assert!(branch["mutationEnabled"].is_boolean());

    let (status, blocked_switch) = json_request(
        app.clone(),
        Method::POST,
        "/api/branch/switch",
        json!({ "branch": "main" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(blocked_switch["error"]
        .as_str()
        .unwrap()
        .contains("CLAUDE_MEM_ALLOW_BRANCH_MUTATION"));

    let (status, _) = json_request(
        app.clone(),
        Method::POST,
        "/api/sessions/init",
        json!({
            "contentSessionId": "summary-content-e2e",
            "project": "cloudy-fork",
            "prompt": "Summarize the thermal mitigation work for search."
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, observation) = json_request(
        app.clone(),
        Method::POST,
        "/api/sessions/observations",
        json!({
            "contentSessionId": "summary-content-e2e",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/repo/thermal.rs" },
            "tool_response": { "content": "Power caps beat fan speed for tiny 1U coolers." },
            "cwd": "/home/kcrawley/projects/cloudy-fork"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(observation["success"], true);

    let (status, summarize) = json_request(
        app.clone(),
        Method::POST,
        "/api/sessions/summarize",
        json!({
            "contentSessionId": "summary-content-e2e",
            "summary": "<summary><request>Thermal mitigation</request><learned>Power caps beat chassis fans.</learned><completed>Added recallable summary.</completed></summary>"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(summarize["summaryId"].as_i64().unwrap() > 0);

    let (status, complete) = json_request(
        app.clone(),
        Method::POST,
        "/api/sessions/complete",
        json!({ "contentSessionId": "summary-content-e2e" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(complete["completed"], true);
    assert_eq!(complete["summaryId"], Value::Null);

    let (status, session_status) = get_json(
        app.clone(),
        "/api/sessions/status?contentSessionId=summary-content-e2e",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(session_status["hasSummary"], true);

    let (status, summaries) = get_json(app.clone(), "/api/summaries?project=cloudy-fork").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summaries["count"], 1);

    let (status, session_search) = get_json(
        app.clone(),
        "/api/search?query=Power&project=cloudy-fork&type=sessions",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(session_search["sessions"].as_array().unwrap().len(), 1);

    let (status, projects) = get_json(app.clone(), "/api/projects").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(projects["projects"][0]["project"], "cloudy-fork");

    let (status, doctor) = get_json(app.clone(), "/api/admin/doctor").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doctor["ok"], true);
    assert_eq!(doctor["counts"]["summaries"], 1);

    let (status, export) = get_json(app.clone(), "/api/export").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(export["format"], "claude-mem-rs-export-v1");
    assert_eq!(export["sessionSummaries"].as_array().unwrap().len(), 1);

    let imported_app = build_router_with_state(AppState::in_memory().unwrap());
    let (status, imported) =
        json_request(imported_app.clone(), Method::POST, "/api/import", export).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(imported["success"], true);

    let (status, imported_stats) = get_json(imported_app, "/api/stats").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(imported_stats["summaries"], 1);
    assert_eq!(imported_stats["observations"], 1);

    std::env::remove_var("CLAUDE_MEM_HOME");
}

#[tokio::test]
async fn sse_stream_emits_initial_snapshot_and_live_memory_events() {
    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let mut stream_response = client
        .get(format!("http://{addr}/stream"))
        .send()
        .await
        .unwrap();
    assert_eq!(stream_response.status(), reqwest::StatusCode::OK);
    assert!(stream_response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));

    let initial = timeout(Duration::from_secs(2), stream_response.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&initial).contains("event: initial_load"));

    let saved: Value = client
        .post(format!("http://{addr}/api/memory/save"))
        .json(&json!({
            "project": "sse-e2e",
            "title": "SSE live memory",
            "text": "The SSE stream must emit live memory save events."
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(saved["success"], true);

    let mut streamed = String::new();
    for _ in 0..10 {
        let chunk = timeout(Duration::from_secs(2), stream_response.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        streamed.push_str(&String::from_utf8_lossy(&chunk));
        if streamed.contains("event: memory_saved") {
            break;
        }
    }
    assert!(streamed.contains("event: memory_saved"));
    assert!(streamed.contains("SSE live memory"));

    server.abort();
}

#[tokio::test]
async fn v12_compatibility_routes_are_available() {
    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state.clone());

    let (status, instructions) = get_json(app.clone(), "/api/instructions").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(instructions["name"], "claude-mem-rs");

    let (status, init) = json_request(
        app.clone(),
        Method::POST,
        "/api/sessions/init",
        json!({
            "contentSessionId": "compat-content-e2e",
            "project": "compat-project",
            "prompt": "Remember compatibility route coverage."
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session_id = init["sessionDbId"].as_i64().unwrap();

    let (status, observation) = json_request(
        app.clone(),
        Method::POST,
        "/api/memory/save",
        json!({
            "project": "compat-project",
            "title": "Compat route memory",
            "text": "Compatibility routes should fetch prompts, observations, sessions, and queue data."
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let observation_id = observation["id"].as_i64().unwrap();

    let (status, obs) = get_json(app.clone(), &format!("/api/observation/{observation_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(obs["title"], "Compat route memory");

    let (status, prompts) = get_json(app.clone(), "/api/prompts?limit=10").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(prompts["count"], 1);
    let prompt_id = prompts["prompts"][0]["id"].as_i64().unwrap();

    let (status, prompt) = get_json(app.clone(), &format!("/api/prompt/{prompt_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(prompt["prompt_text"]
        .as_str()
        .unwrap()
        .contains("compatibility route"));

    let (status, session) = get_json(app.clone(), &format!("/api/session/{session_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(session["content_session_id"], "compat-content-e2e");

    let (status, legacy_obs) = json_request(
        app.clone(),
        Method::POST,
        &format!("/sessions/{session_id}/observations"),
        json!({
            "tool_name": "Read",
            "tool_input": { "file_path": "/repo/compat.rs" },
            "tool_response": { "content": "queued compat observation" },
            "cwd": "/repo"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(legacy_obs["status"], "queued");

    let (status, pending) = get_json(app.clone(), "/api/pending-queue").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pending["queue"]["totalPending"], 0);
    assert_eq!(
        legacy_obs["processed"]["messagesProcessed"], 1,
        "legacy compatibility route should enqueue and drain through the native observer"
    );

    let (status, process) = json_request(
        app.clone(),
        Method::POST,
        "/api/pending-queue/process",
        json!({ "sessionLimit": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(process["success"], true);
    assert_eq!(process["result"]["messagesProcessed"], 0);
    assert_eq!(process["messagesProcessed"], 0);

    {
        let conn = state.conn.lock().unwrap();
        let session = get_session_by_content_id(&conn, "compat-content-e2e")
            .unwrap()
            .unwrap();
        let store = PendingMessageStore::new(1);
        store
            .enqueue(
                &conn,
                &EnqueueInput {
                    session_db_id: session.id,
                    content_session_id: session.content_session_id.clone(),
                    message_type: "observation".into(),
                    tool_name: Some("Read".into()),
                    tool_input: Some(json!({ "file_path": "/repo/pending.rs" })),
                    tool_response: Some(json!({ "content": "still pending" })),
                    cwd: Some("/repo".into()),
                    prompt_number: Some(1),
                    created_at_epoch: 1_717_234_200_000,
                    ..Default::default()
                },
            )
            .unwrap();
        let failed_id = store
            .enqueue(
                &conn,
                &EnqueueInput {
                    session_db_id: session.id,
                    content_session_id: session.content_session_id.clone(),
                    message_type: "summarize".into(),
                    last_assistant_message: Some("failed queue item".into()),
                    prompt_number: Some(1),
                    created_at_epoch: 1_717_234_300_000,
                    ..Default::default()
                },
            )
            .unwrap();
        store.mark_failed(&conn, failed_id).unwrap();
    }

    let (status, queue) = get_json(app.clone(), "/api/pending-queue").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(queue["queue"]["totalPending"], 1);
    assert_eq!(queue["queue"]["totalFailed"], 1);
    assert_eq!(queue["queue"]["messages"].as_array().unwrap().len(), 2);

    let (status, all) = get_json(app.clone(), "/api/pending-queue/all").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all["queue"]["totalPending"], 1);
    assert_eq!(all["queue"]["totalFailed"], 1);

    let (status, failed) = get_json(app.clone(), "/api/pending-queue/failed").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(failed["queue"]["totalPending"], 0);
    assert_eq!(failed["queue"]["totalFailed"], 1);
    assert_eq!(
        failed["queue"]["messages"][0]["status"].as_str().unwrap(),
        "failed"
    );

    let (status, clear_failed) = delete_json(app.clone(), "/api/pending-queue/failed").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(clear_failed["success"], true);
    assert_eq!(clear_failed["clearedCount"], 1);

    let (status, all_after_failed_clear) = get_json(app.clone(), "/api/pending-queue/all").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all_after_failed_clear["queue"]["totalPending"], 1);
    assert_eq!(all_after_failed_clear["queue"]["totalFailed"], 0);

    let (status, clear) = delete_json(app.clone(), "/api/pending-queue/all").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(clear["success"], true);
    assert_eq!(clear["clearedCount"], 1);

    let (status, all_empty) = get_json(app.clone(), "/api/pending-queue/all").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all_empty["queue"]["totalPending"], 0);
    assert_eq!(all_empty["queue"]["totalFailed"], 0);

    let (status, mcp) = get_json(app.clone(), "/api/mcp/status").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(mcp["enabled"], true);

    let (status, search_help) = get_json(app, "/api/search/help").await;
    assert_eq!(status, StatusCode::OK);
    assert!(search_help["endpoints"].as_array().unwrap().len() > 3);
}

#[tokio::test]
async fn queued_summarize_without_explicit_xml_is_agent_processed() {
    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state);

    let (status, _) = json_request(
        app.clone(),
        Method::POST,
        "/api/sessions/init",
        json!({
            "contentSessionId": "implicit-summary-e2e",
            "project": "cloudy-fork",
            "prompt": "Track power mitigation summary generation."
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, summarize) = json_request(
        app.clone(),
        Method::POST,
        "/api/sessions/summarize",
        json!({
            "contentSessionId": "implicit-summary-e2e",
            "lastAssistantMessage": "Package wattage reduction was selected over chassis fan speed."
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summarize["status"], "queued");
    assert_eq!(summarize["processed"]["messagesProcessed"], 1);
    assert_eq!(summarize["processed"]["summariesInserted"], 1);

    let (status, pending) = get_json(app.clone(), "/api/pending-queue").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pending["queue"]["totalPending"], 0);

    let (status, summaries) = get_json(app, "/api/summaries?project=cloudy-fork").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(summaries["count"], 1);
    assert!(summaries["summaries"][0]["learned"]
        .as_str()
        .unwrap()
        .contains("Package wattage reduction"));
}

#[tokio::test]
async fn fake_agent_runner_processes_queued_observation_xml() {
    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state.clone());

    let (status, init) = json_request(
        app.clone(),
        Method::POST,
        "/api/sessions/init",
        json!({
            "contentSessionId": "fake-agent-e2e",
            "project": "cloudy-fork",
            "prompt": "Use fake provider for observer queue proof."
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session_db_id = init["sessionDbId"].as_i64().unwrap();

    {
        let conn = state.conn.lock().unwrap();
        let session = get_session_by_content_id(&conn, "fake-agent-e2e")
            .unwrap()
            .unwrap();
        let pending_store = PendingMessageStore::default();
        pending_store
            .enqueue(
                &conn,
                &EnqueueInput {
                    session_db_id,
                    content_session_id: session.content_session_id,
                    message_type: "observation".into(),
                    tool_name: Some("Read".into()),
                    tool_input: Some(json!({ "file_path": "/repo/fake.rs" })),
                    tool_response: Some(json!({ "content": "ignored by fake response" })),
                    cwd: Some("/home/kcrawley/projects/cloudy-fork".into()),
                    created_at_epoch: 1,
                    ..Default::default()
                },
            )
            .unwrap();
    }

    let stats = process_pending_for_session(
        Arc::clone(&state.conn),
        session_db_id,
        ObserverConfig {
            provider: "fake".into(),
            model_id: None,
            tier_routing_enabled: true,
            simple_model: None,
            summary_model: None,
            max_messages: 10,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        stats,
        QueueProcessStats {
            sessions_started: 1,
            started_session_ids: vec![session_db_id],
            messages_processed: 1,
            observations_inserted: 1,
            observation_ids: stats.observation_ids.clone(),
            ..Default::default()
        }
    );

    let (status, search) = get_json(
        app,
        "/api/search?query=fake&project=cloudy-fork&type=observations",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(search["observations"][0]["title"], "Fake observer response");
}

#[tokio::test]
async fn observer_tier_routing_applies_simple_model_metadata() {
    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state.clone());

    let (status, init) = json_request(
        app,
        Method::POST,
        "/api/sessions/init",
        json!({
            "contentSessionId": "tier-routing-e2e",
            "project": "cloudy-fork",
            "prompt": "Use tier routing for simple read observations."
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session_db_id = init["sessionDbId"].as_i64().unwrap();

    {
        let conn = state.conn.lock().unwrap();
        let session = get_session_by_content_id(&conn, "tier-routing-e2e")
            .unwrap()
            .unwrap();
        PendingMessageStore::default()
            .enqueue(
                &conn,
                &EnqueueInput {
                    session_db_id,
                    content_session_id: session.content_session_id,
                    message_type: "observation".into(),
                    tool_name: Some("Read".into()),
                    tool_input: Some(json!({ "file_path": "/repo/tier.rs" })),
                    tool_response: Some(json!({ "content": "simple read should use simple tier" })),
                    cwd: Some("/home/kcrawley/projects/cloudy-fork".into()),
                    created_at_epoch: 1,
                    ..Default::default()
                },
            )
            .unwrap();
    }

    let stats = process_pending_for_session(
        Arc::clone(&state.conn),
        session_db_id,
        ObserverConfig {
            provider: "local".into(),
            model_id: Some("default-model".into()),
            tier_routing_enabled: true,
            simple_model: Some("simple-model".into()),
            summary_model: Some("summary-model".into()),
            max_messages: 10,
        },
    )
    .await
    .unwrap();
    assert_eq!(stats.messages_processed, 1);
    assert_eq!(stats.observations_inserted, 1);

    let conn = state.conn.lock().unwrap();
    let observation = get_observation_by_id(&conn, stats.observation_ids[0])
        .unwrap()
        .unwrap();
    assert_eq!(
        observation.generated_by_model.as_deref(),
        Some("Local:simple-model")
    );
}
