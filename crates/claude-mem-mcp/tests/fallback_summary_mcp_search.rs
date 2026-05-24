//! Regression coverage for AC #4 (issue #5):
//!
//! Verifies the fallback summary created by `POST /api/sessions/complete`
//! is recoverable via the **MCP** `search` tool (the third leg of the
//! parity matrix; the SQLite FTS5 + worker HTTP legs live in
//! `crates/claude-mem-worker/tests/fallback_summary_regression.rs`).
//!
//! Mirrors the MCP harness pattern in `mcp_tools_e2e.rs`.

use claude_mem_core::db::sessions::{create_session, update_memory_session_id};
use claude_mem_core::types::session::CreateSessionInput;
use claude_mem_mcp::server::{ClaudeMemMcp, SearchParams, WorkerClient};
use claude_mem_worker::http::router::{build_router_with_state, AppState};
use rmcp::handler::server::tool::Parameters;
use rmcp::model::RawContent;
use serde_json::json;
use tokio::net::TcpListener;

fn seed_session_with_memory(state: &AppState, content: &str, memory: &str, project: &str) {
    let conn = state.conn.lock().unwrap();
    create_session(
        &conn,
        &CreateSessionInput {
            content_session_id: content.into(),
            project: project.into(),
            user_prompt: Some(format!(
                "Fallback recall prompt for {content}: package wattage beats fan speed."
            )),
            started_at: "2024-06-01T09:00:00.000Z".into(),
            started_at_epoch: 1_717_232_400_000,
        },
    )
    .unwrap();
    update_memory_session_id(&conn, content, memory).unwrap();
}

fn result_text(result: &rmcp::model::CallToolResult) -> String {
    let content = result.content.as_ref().unwrap();
    match &content[0].raw {
        RawContent::Text(text) => text.text.clone(),
        other => panic!("expected text content from MCP search, got {other:?}"),
    }
}

#[tokio::test]
async fn fallback_summary_is_recoverable_via_mcp_search_tool() {
    let state = AppState::in_memory().unwrap();
    seed_session_with_memory(
        &state,
        "fallback-mcp-1",
        "fallback-mcp-memory",
        "cloudy-fork",
    );
    let app = build_router_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Block until the worker is reachable. Without this, the first POST
    // can race startup and intermittently fail with a connection error
    // or non-success status. Codex P2 on PR #16.
    {
        let base = format!("http://{addr}");
        let probe = reqwest::Client::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match probe
                .get(format!("{base}/api/health"))
                .timeout(std::time::Duration::from_millis(250))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => break,
                _ if std::time::Instant::now() >= deadline => {
                    panic!("worker never became ready");
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
    }

    // Trigger the fallback summary path explicitly (no explicit summary
    // was ever posted for this session).
    let complete = reqwest::Client::new()
        .post(format!("http://{addr}/api/sessions/complete"))
        .json(&json!({ "contentSessionId": "fallback-mcp-1" }))
        .send()
        .await
        .unwrap();
    assert!(complete.status().is_success());

    let mcp = ClaudeMemMcp::new(WorkerClient::new(format!("http://{addr}")));
    assert!(mcp.worker_ready().await);

    let search = mcp
        .search(Parameters(SearchParams {
            query: Some("Fallback recall prompt package wattage".into()),
            project: Some("cloudy-fork".into()),
            limit: Some(10),
            ..Default::default()
        }))
        .await
        .unwrap();
    let text = result_text(&search);
    assert!(
        text.contains("Found 1 result(s)") || text.contains("Found 2 result(s)"),
        "MCP search should surface the fallback summary, got: {text}"
    );
    assert!(
        text.contains("package wattage") || text.contains("Fallback recall prompt"),
        "MCP search must echo the indexed prompt body, got: {text}"
    );

    server.abort();
}
