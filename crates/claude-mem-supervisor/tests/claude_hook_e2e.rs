use claude_mem_supervisor::hooks::{execute_hook, WorkerClient};
use claude_mem_worker::http::router::{build_router_with_state, AppState};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

async fn spawn_worker() -> (WorkerClient, oneshot::Sender<()>) {
    let app = build_router_with_state(AppState::in_memory().unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    (WorkerClient::new(format!("http://{}", addr)), tx)
}

#[tokio::test]
async fn claude_hook_round_trip_creates_memory_and_injects_context() {
    let (worker, shutdown) = spawn_worker().await;

    let session_input = json!({
        "session_id": "claude-hook-content-e2e",
        "cwd": "/home/kcrawley/projects/cloudy-fork",
        "prompt": "Remember the Rust hook should create and recall Claude memory."
    });
    let init = execute_hook("claude-code", "session-init", session_input, &worker)
        .await
        .unwrap();
    assert_eq!(init.exit_code, 0);
    assert!(init.output.hook_specific_output.is_none());

    let observation_input = json!({
        "session_id": "claude-hook-content-e2e",
        "cwd": "/home/kcrawley/projects/cloudy-fork",
        "tool_name": "Read",
        "tool_input": { "file_path": "/repo/src/lib.rs" },
        "tool_response": { "content": "Dynatron thermal memories should be available to Claude." }
    });
    let observation = execute_hook("claude-code", "observation", observation_input, &worker)
        .await
        .unwrap();
    assert_eq!(observation.exit_code, 0);
    assert!(observation.output.hook_specific_output.is_none());

    let context_input = json!({
        "session_id": "claude-hook-content-e2e",
        "cwd": "/home/kcrawley/projects/cloudy-fork"
    });
    let context = execute_hook("claude-code", "context", context_input, &worker)
        .await
        .unwrap();
    assert_eq!(context.exit_code, 0);
    let hook_output = context.output.hook_specific_output.unwrap();
    assert_eq!(hook_output.hook_event_name, "SessionStart");
    assert!(hook_output.additional_context.contains("Read tool use"));
    assert!(hook_output
        .additional_context
        .contains("Dynatron thermal memories"));

    let complete_input = json!({
        "session_id": "claude-hook-content-e2e",
        "cwd": "/home/kcrawley/projects/cloudy-fork"
    });
    let complete = execute_hook("claude-code", "session-complete", complete_input, &worker)
        .await
        .unwrap();
    assert_eq!(complete.exit_code, 0);

    let _ = shutdown.send(());
}
