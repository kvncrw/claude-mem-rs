use claude_mem_supervisor::hooks::{WorkerClient, execute_hook};
use claude_mem_worker::http::router::{AppState, build_router_with_state};
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
    assert!(
        hook_output
            .additional_context
            .contains("Dynatron thermal memories")
    );

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

#[tokio::test]
async fn cursor_gemini_and_codex_adapters_create_searchable_memory() {
    let (worker, shutdown) = spawn_worker().await;

    let cursor_init = execute_hook(
        "cursor",
        "session-init",
        json!({
            "conversation_id": "cursor-content-e2e",
            "workspace_roots": ["/home/kcrawley/projects/cloudy-fork"],
            "query": "Remember Cursor adapter memory for package power caps."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_eq!(cursor_init.output.r#continue, Some(true));

    let cursor_obs = execute_hook(
        "cursor",
        "observation",
        json!({
            "conversation_id": "cursor-content-e2e",
            "workspace_roots": ["/home/kcrawley/projects/cloudy-fork"],
            "command": "cat thermal.md",
            "output": "Cursor found package power cap memory."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_eq!(cursor_obs.output.r#continue, Some(true));

    let gemini_init = execute_hook(
        "gemini-cli",
        "session-init",
        json!({
            "session_id": "gemini-content-e2e",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "prompt": "Remember Gemini adapter thermal lifecycle memory."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_eq!(gemini_init.output.r#continue, Some(true));

    let gemini_obs = execute_hook(
        "gemini-cli",
        "observation",
        json!({
            "session_id": "gemini-content-e2e",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "hook_event_name": "AfterAgent",
            "prompt": "Summarize fans",
            "prompt_response": "Gemini learned power caps matter more than chassis fans."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_eq!(gemini_obs.output.r#continue, Some(true));

    let codex_init = execute_hook(
        "codex",
        "session-init",
        json!({
            "session_id": "codex-content-e2e",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "prompt": "Remember Codex raw adapter memory."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_eq!(codex_init.exit_code, 0);

    let codex_obs = execute_hook(
        "codex",
        "observation",
        json!({
            "session_id": "codex-content-e2e",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "tool_name": "Read",
            "tool_input": { "file_path": "/repo/codex.md" },
            "tool_response": { "content": "Codex adapter stores raw hook memory." }
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_eq!(codex_obs.exit_code, 0);

    let context = execute_hook(
        "claude-code",
        "context",
        json!({
            "session_id": "adapter-context",
            "cwd": "/home/kcrawley/projects/cloudy-fork"
        }),
        &worker,
    )
    .await
    .unwrap();
    let additional_context = context
        .output
        .hook_specific_output
        .unwrap()
        .additional_context;
    assert!(additional_context.contains("Cursor found package power cap memory"));
    assert!(additional_context.contains("Gemini learned power caps"));
    assert!(additional_context.contains("Codex adapter stores raw hook memory"));

    let _ = shutdown.send(());
}
