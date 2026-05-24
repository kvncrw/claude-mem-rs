//! End-to-end MCP tests for the corpus subsystem.
//!
//! Mirrors `mcp_tools_e2e.rs` — spins up the in-process axum worker on a
//! random TCP port, then drives a `ClaudeMemMcp` instance bound to that
//! worker. The corpora dir is rooted at `$CLAUDE_MEM_HOME` so each test gets
//! its own tempdir.
//!
//! These tests exercise the default-feature build (knowledge-agent OFF), so
//! prime/query/reprime surface the worker's 501 to the MCP caller as an
//! error or wrapped text payload. The 501 assertions are gated behind
//! `#[cfg(not(feature = "knowledge-agent"))]` so the suite stays green under
//! `--features knowledge-agent` as well.

#[cfg(not(feature = "knowledge-agent"))]
use claude_mem_mcp::server::QueryCorpusParams;
use claude_mem_mcp::server::{BuildCorpusParams, ClaudeMemMcp, NameOnlyParams, WorkerClient};
use claude_mem_worker::http::router::{build_router_with_state, AppState};
use rmcp::handler::server::tool::Parameters;
use rmcp::model::{CallToolResult, RawContent};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Serializes mutations of `CLAUDE_MEM_HOME` across tests in this file.
/// `CorpusStore::default()` reads the env on every call, so two parallel tests
/// would otherwise stomp each other's corpora dir. Async mutex so the guard
/// can be held across `.await` boundaries without tripping clippy's
/// `await_holding_lock`.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

fn result_text(result: &CallToolResult) -> String {
    let content = result
        .content
        .as_ref()
        .expect("tool result must have content");
    match &content[0].raw {
        RawContent::Text(text) => text.text.clone(),
        other => panic!("expected text content, got {other:?}"),
    }
}

fn result_json(result: &CallToolResult) -> Value {
    serde_json::from_str(&result_text(result)).expect("tool result must be JSON-encoded text")
}

async fn spawn_worker() -> (String, tokio::task::JoinHandle<()>) {
    let state = AppState::in_memory().unwrap();
    let app = build_router_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), server)
}

#[tokio::test]
async fn list_corpora_empty_store_returns_empty_array() {
    let _guard = ENV_LOCK.lock().await;
    let home = tempfile::TempDir::new().unwrap();
    std::env::set_var("CLAUDE_MEM_HOME", home.path());

    let (base, server) = spawn_worker().await;
    let mcp = ClaudeMemMcp::new(WorkerClient::new(base));
    assert!(mcp.worker_ready().await);

    let list = mcp.list_corpora().await.unwrap();
    let parsed = result_json(&list);
    assert!(
        parsed.as_array().map(|a| a.is_empty()).unwrap_or(false),
        "expected empty array, got {parsed}"
    );

    server.abort();
    std::env::remove_var("CLAUDE_MEM_HOME");
}

#[tokio::test]
async fn build_corpus_proxies_through_worker_and_list_then_includes_it() {
    let _guard = ENV_LOCK.lock().await;
    let home = tempfile::TempDir::new().unwrap();
    std::env::set_var("CLAUDE_MEM_HOME", home.path());

    let (base, server) = spawn_worker().await;
    let mcp = ClaudeMemMcp::new(WorkerClient::new(base.clone()));

    // Seed via the public memory_save tool so the underlying observations
    // table has a row that survives a project filter.
    use claude_mem_mcp::server::SaveMemoryParams;
    let _ = mcp
        .save_memory(Parameters(SaveMemoryParams {
            project: Some("mcp-corpus".into()),
            title: Some("Seed memory".into()),
            text: "A seed observation so the corpus build has something to chew on.".into(),
        }))
        .await
        .unwrap();

    let built = mcp
        .build_corpus(Parameters(BuildCorpusParams {
            name: "mcp-suite".into(),
            description: Some("MCP suite corpus".into()),
            project: Some("mcp-corpus".into()),
            // store_batch persists manual memories as `discovery` type.
            types: Some("discovery".into()),
            limit: Some(50),
            ..Default::default()
        }))
        .await
        .unwrap();
    let built_json = result_json(&built);
    assert_eq!(built_json["name"], "mcp-suite");
    assert_eq!(built_json["description"], "MCP suite corpus");
    assert_eq!(built_json["version"], 1);
    assert_eq!(built_json["filter"]["project"], "mcp-corpus");
    assert!(
        built_json["stats"]["observation_count"]
            .as_i64()
            .unwrap_or(0)
            >= 1
    );

    let list = mcp.list_corpora().await.unwrap();
    let entries: Vec<Value> = serde_json::from_str(&result_text(&list)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "mcp-suite");
    assert_eq!(entries[0]["description"], "MCP suite corpus");
    assert!(entries[0].get("observations").is_none());

    // -- rebuild via MCP succeeds end-to-end.
    let rebuilt = mcp
        .rebuild_corpus(Parameters(NameOnlyParams {
            name: "mcp-suite".into(),
        }))
        .await
        .unwrap();
    let rebuilt_json = result_json(&rebuilt);
    assert_eq!(rebuilt_json["name"], "mcp-suite");
    assert_eq!(
        rebuilt_json["stats"]["observation_count"],
        built_json["stats"]["observation_count"]
    );

    server.abort();
    std::env::remove_var("CLAUDE_MEM_HOME");
}

#[cfg(not(feature = "knowledge-agent"))]
#[tokio::test]
async fn prime_query_reprime_surface_worker_501_when_feature_disabled() {
    let _guard = ENV_LOCK.lock().await;
    let home = tempfile::TempDir::new().unwrap();
    std::env::set_var("CLAUDE_MEM_HOME", home.path());

    let (base, server) = spawn_worker().await;
    let mcp = ClaudeMemMcp::new(WorkerClient::new(base));

    // Build first so prime/query/reprime get past the 404 gate and reach the
    // disabled-feature path.
    let _ = mcp
        .build_corpus(Parameters(BuildCorpusParams {
            name: "disabled-feat".into(),
            project: Some("any".into()),
            ..Default::default()
        }))
        .await
        .unwrap();

    let cases: Vec<(&str, Result<CallToolResult, rmcp::ErrorData>)> = vec![
        (
            "prime",
            mcp.prime_corpus(Parameters(NameOnlyParams {
                name: "disabled-feat".into(),
            }))
            .await,
        ),
        (
            "query",
            mcp.query_corpus(Parameters(QueryCorpusParams {
                name: "disabled-feat".into(),
                question: "what changed?".into(),
            }))
            .await,
        ),
        (
            "reprime",
            mcp.reprime_corpus(Parameters(NameOnlyParams {
                name: "disabled-feat".into(),
            }))
            .await,
        ),
    ];

    for (label, outcome) in cases {
        let payload = match outcome {
            Ok(result) => result_text(&result),
            Err(error) => error.to_string(),
        };
        assert!(
            payload.contains("knowledge-agent") && payload.contains("not available"),
            "{label}: expected disabled-feature explanation, got {payload:?}"
        );
    }

    server.abort();
    std::env::remove_var("CLAUDE_MEM_HOME");
}
