use axum::http::StatusCode;
use claude_mem_mcp::server::{
    ClaudeMemMcp, GetObservationsParams, SaveMemoryParams, SearchParams, SmartOutlineParams,
    SmartSearchParams, SmartUnfoldParams, TimelineParams, WorkerClient,
};
use claude_mem_worker::http::router::{build_router_with_state, AppState};
use rmcp::handler::server::tool::Parameters;
use rmcp::model::RawContent;
use rmcp::ServerHandler;
use serde_json::{json, Value};
use tokio::net::TcpListener;

#[tokio::test]
async fn mcp_tools_save_search_timeline_and_fetch_worker_memory() {
    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let mcp = ClaudeMemMcp::new(WorkerClient::new(format!("http://{addr}")));
    assert!(mcp.worker_ready().await);

    let info = mcp.get_info();
    assert_eq!(info.server_info.name, "claude-mem");
    assert!(info.capabilities.tools.is_some());

    let save = mcp
        .save_memory(Parameters(SaveMemoryParams {
            project: Some("cloudy-mcp".into()),
            title: Some("Dynatron cap".into()),
            text: "Dynatron 1U coolers need a lower CPU package wattage to keep cloudy-k3s stable."
                .into(),
        }))
        .await
        .unwrap();
    let save_json = result_json(&save);
    assert_eq!(save_json["success"], true);

    let _ = mcp
        .save_memory(Parameters(SaveMemoryParams {
            project: Some("cloudy-mcp".into()),
            title: Some("Chassis fans".into()),
            text: "Chassis fans blasting will not materially fix a CPU cooler wattage mismatch."
                .into(),
        }))
        .await
        .unwrap();

    let search = mcp
        .search(Parameters(SearchParams {
            query: Some("Dynatron wattage cloudy-k3s".into()),
            project: Some("cloudy-mcp".into()),
            limit: Some(10),
            ..Default::default()
        }))
        .await
        .unwrap();
    let search_text = result_text(&search);
    assert!(search_text.contains("Found 2 result(s)"));
    assert!(search_text.contains("Dynatron cap"));
    assert!(!search_text.contains("\"observations\""));
    let anchor = save_json["id"].as_i64().unwrap();

    let timeline = mcp
        .timeline(Parameters(TimelineParams {
            anchor: Some(anchor),
            project: Some("cloudy-mcp".into()),
            depth_before: Some(1),
            depth_after: Some(1),
            ..Default::default()
        }))
        .await
        .unwrap();
    let timeline_json = result_json(&timeline);
    assert_eq!(timeline_json["anchor"], anchor);
    assert_eq!(timeline_json["count"], 2);

    let observations = mcp
        .get_observations(Parameters(GetObservationsParams { ids: vec![anchor] }))
        .await
        .unwrap();
    let observations_json = result_json(&observations);
    assert_eq!(
        observations_json["observations"][0]["title"],
        "Dynatron cap"
    );

    let important = mcp.important().await.unwrap();
    assert!(result_text(&important).contains("search"));

    let source = tempfile::TempDir::new().unwrap();
    let src = source.path().join("thermal.rs");
    std::fs::write(
        &src,
        "pub fn package_power_cap() -> u32 {\n    65\n}\n\nstruct FanCurve;\n",
    )
    .unwrap();
    let smart = mcp
        .smart_search(Parameters(SmartSearchParams {
            query: "package power".into(),
            path: Some(source.path().display().to_string()),
            max_results: Some(5),
            file_pattern: None,
        }))
        .await
        .unwrap();
    assert!(result_text(&smart).contains("package_power_cap"));
    let outline = mcp
        .smart_outline(Parameters(SmartOutlineParams {
            file_path: src.display().to_string(),
        }))
        .await
        .unwrap();
    assert!(result_text(&outline).contains("FanCurve"));
    let unfold = mcp
        .smart_unfold(Parameters(SmartUnfoldParams {
            file_path: src.display().to_string(),
            symbol_name: "package_power_cap".into(),
        }))
        .await
        .unwrap();
    assert!(result_text(&unfold).contains("65"));

    let empty_ids = mcp
        .get_observations(Parameters(GetObservationsParams { ids: Vec::new() }))
        .await;
    assert!(empty_ids.is_err());

    let health = reqwest::get(format!("http://{addr}/api/health"))
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    server.abort();
}

#[tokio::test]
async fn mcp_search_returns_compact_index_when_matching_huge_prompts() {
    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let huge_prompt = format!(
        "searchable kiwifarms prompt\n{}\noversized-prompt-tail",
        "x".repeat(250_000)
    );
    let response = reqwest::Client::new()
        .post(format!("http://{addr}/api/sessions/init"))
        .json(&json!({
            "contentSessionId": "huge-prompt-session",
            "project": "huge-mcp",
            "prompt": huge_prompt
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mcp = ClaudeMemMcp::new(WorkerClient::new(format!("http://{addr}")));
    let search = mcp
        .search(Parameters(SearchParams {
            query: Some("kiwifarms".into()),
            project: Some("huge-mcp".into()),
            limit: Some(20),
            ..Default::default()
        }))
        .await
        .unwrap();

    let text = result_text(&search);
    assert!(text.contains("Found 1 result(s)"));
    assert!(text.contains("#P"));
    assert!(text.contains("searchable kiwifarms prompt"));
    assert!(!text.contains("\"prompt_text\""));
    assert!(!text.contains("oversized-prompt-tail"));
    assert!(
        text.len() < 5_000,
        "MCP search returned oversized text: {} bytes",
        text.len()
    );

    server.abort();
}

fn result_json(result: &rmcp::model::CallToolResult) -> Value {
    serde_json::from_str(&result_text(result)).unwrap()
}

fn result_text(result: &rmcp::model::CallToolResult) -> String {
    let content = result.content.as_ref().unwrap();
    match &content[0].raw {
        RawContent::Text(text) => text.text.clone(),
        other => panic!("expected text content, got {other:?}"),
    }
}
