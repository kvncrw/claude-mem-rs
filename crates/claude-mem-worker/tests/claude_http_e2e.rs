use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use claude_mem_worker::http::router::{build_router_with_state, AppState};
use serde_json::{json, Value};
use tower::ServiceExt;

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
