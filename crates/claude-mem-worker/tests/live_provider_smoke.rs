use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use axum::Router;
use claude_mem_worker::http::router::{build_router_with_state, AppState};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tower::ServiceExt;

// `tokio::sync::Mutex` so the guard can be held across `.await` without
// tripping `clippy::await_holding_lock`. `std::sync::Mutex` was fine when
// this test was single-pass, but it serialises around live HTTP work
// against the in-process worker, every step of which is async.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

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

#[tokio::test(flavor = "current_thread")]
async fn live_provider_smoke_when_enabled() {
    if std::env::var("CLAUDE_MEM_LIVE_PROVIDER_SMOKE").as_deref() != Ok("1") {
        eprintln!("skipping live provider smoke; set CLAUDE_MEM_LIVE_PROVIDER_SMOKE=1");
        return;
    }
    let _guard = ENV_LOCK.lock().await;
    let providers = live_providers();
    assert!(
        !providers.is_empty(),
        "no live providers available; expected claude, gemini-cli, codex, or openrouter"
    );
    for provider in providers {
        eprintln!("running live provider smoke for {provider}");
        std::env::set_var("CLAUDE_MEM_PROVIDER", provider);
        std::env::set_var("CLAUDE_MEM_QUEUE_PROCESS_LIMIT", "5");
        std::env::set_var("CLAUDE_MEM_CLAUDE_TIMEOUT_SECS", "180");
        std::env::set_var("CLAUDE_MEM_GEMINI_TIMEOUT_SECS", "180");
        std::env::set_var("CLAUDE_MEM_CODEX_TIMEOUT_SECS", "240");
        let app = build_router_with_state(AppState::in_memory().unwrap());
        let session = format!("live-provider-{provider}-{}", std::process::id());
        let (status, _init) = json_request(
            app.clone(),
            Method::POST,
            "/api/sessions/init",
            json!({
                "contentSessionId": session,
                "project": "live-provider-smoke",
                "prompt": "Remember live provider smoke marker."
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{provider} session init failed");
        let marker = format!("{provider}-provider-marker");
        let (status, obs) = json_request(
            app,
            Method::POST,
            "/api/sessions/observations",
            json!({
                "contentSessionId": session,
                "toolName": "Read",
                "toolInput": { "file_path": "/tmp/provider-smoke.txt" },
                "toolResponse": {
                    "content": format!("Store this durable memory marker: {marker}.")
                }
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{provider} observation route failed: {obs}"
        );
        assert_eq!(obs["success"], true);
        assert!(
            obs["inserted"].as_i64().unwrap_or_default() >= 1,
            "{provider} did not insert an observation: {obs}"
        );
    }
}

fn live_providers() -> Vec<&'static str> {
    let mut out = Vec::new();
    if command_exists("claude") {
        out.push("claude");
    }
    if std::env::var("CLAUDE_MEM_GEMINI_API_KEY").is_ok()
        || std::env::var("GEMINI_API_KEY").is_ok()
        || command_exists("gemini")
    {
        out.push("gemini-cli");
    }
    if command_exists("codex") {
        out.push("codex");
    }
    if std::env::var("CLAUDE_MEM_LIVE_OPENROUTER_SMOKE").as_deref() == Ok("1")
        && (std::env::var("CLAUDE_MEM_OPENROUTER_API_KEY").is_ok()
            || std::env::var("OPENROUTER_API_KEY").is_ok())
    {
        out.push("openrouter");
    }
    out
}

fn command_exists(name: &str) -> bool {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
