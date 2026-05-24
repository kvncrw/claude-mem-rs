use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use claude_mem_worker::http::router::{AppState, build_router_with_state};
use serde_json::{Value, json};
use std::sync::Mutex;
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
    assert!(
        formatted_search["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Dynatron power cap")
    );

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
    assert!(
        by_type["observations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["title"] == "Read tool use")
    );

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
    assert!(
        semantic["context"]
            .as_str()
            .unwrap()
            .contains("Dynatron power cap")
    );

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

    let (status, stream) = get_text(app.clone(), "/stream").await;
    assert_eq!(status, StatusCode::OK);
    assert!(stream.contains("event: initial_load"));

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
    assert!(
        blocked_switch["error"]
            .as_str()
            .unwrap()
            .contains("CLAUDE_MEM_ALLOW_BRANCH_MUTATION")
    );

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
