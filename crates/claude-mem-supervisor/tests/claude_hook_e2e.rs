use claude_mem_core::db::observations::store::store_observation;
use claude_mem_core::db::sessions::{create_session, update_memory_session_id};
use claude_mem_core::types::session::CreateSessionInput;
use claude_mem_core::types::ObservationInput;
use claude_mem_supervisor::hooks::{execute_hook, WorkerClient};
use claude_mem_worker::http::router::{build_router_with_state, AppState};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::{sleep, Duration};

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

fn seeded_context_state() -> AppState {
    let state = AppState::in_memory().unwrap();
    {
        let conn = state.conn.lock().unwrap();
        create_session(
            &conn,
            &CreateSessionInput {
                content_session_id: "delayed-worker-content".into(),
                project: "cloudy-fork".into(),
                user_prompt: Some("Recall delayed worker boot memory.".into()),
                started_at: "2024-06-01T09:00:00.000Z".into(),
                started_at_epoch: 1_717_232_400_000,
            },
        )
        .unwrap();
        update_memory_session_id(&conn, "delayed-worker-content", "delayed-worker-memory").unwrap();
        store_observation(
            &conn,
            &ObservationInput {
                memory_session_id: "delayed-worker-memory".into(),
                project: "cloudy-fork".into(),
                r#type: "decision".into(),
                text: Some("SessionStart should wait for a worker that is still booting.".into()),
                title: Some("Delayed readiness boot memory".into()),
                subtitle: Some("Hook waits for worker health".into()),
                narrative: Some(
                    "The hook retries health and context requests before giving up.".into(),
                ),
                facts: Some(vec!["Slow startup must still inject context.".into()]),
                concepts: Some(vec!["problem-solution".into()]),
                files_read: Some(vec!["crates/claude-mem-supervisor/src/hooks/mod.rs".into()]),
                files_modified: None,
                prompt_number: Some(1),
                discovery_tokens: Some(500),
                relevance_count: Some(0),
                created_at: "2024-06-01T09:30:00.000Z".into(),
                created_at_epoch: 1_717_234_200_000,
                generated_by_model: None,
                merged_into_project: None,
                agent_type: None,
                agent_id: None,
                content_hash: Some("delayed-readiness".into()),
            },
        )
        .unwrap();
    }
    state
}

async fn spawn_delayed_worker(delay: Duration) -> (WorkerClient, oneshot::Sender<()>) {
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = probe.local_addr().unwrap();
    drop(probe);

    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        sleep(delay).await;
        let app = build_router_with_state(seeded_context_state());
        let listener = TcpListener::bind(addr).await.unwrap();
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
    assert!(hook_output
        .additional_context
        .starts_with("# [cloudy-fork] recent context,"));
    assert!(hook_output.additional_context.contains("Context Index:"));
    assert!(hook_output
        .additional_context
        .contains("Fetch details: get_observations([IDs])"));
    assert!(hook_output.additional_context.contains("Read tool use"));
    let system_message = context.output.system_message.unwrap();
    assert!(system_message.contains("Read tool use"));
    assert!(system_message.contains("Context Index:"));
    assert!(system_message.contains("View Observations Live @"));

    let semantic = execute_hook(
        "claude-code",
        "session-init",
        json!({
            "session_id": "claude-hook-content-e2e-next",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "prompt": "Use prior Dynatron thermal memory when deciding fan and wattage behavior."
        }),
        &worker,
    )
    .await
    .unwrap();
    let semantic_context = semantic
        .output
        .hook_specific_output
        .unwrap()
        .additional_context;
    assert!(semantic_context.starts_with("## Relevant Past Work"));
    assert!(semantic_context.contains("| ID | Time | T | Title | Read |"));
    assert!(semantic_context.contains("Read tool use"));

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
async fn claude_session_start_waits_for_delayed_worker_readiness() {
    let (worker, shutdown) = spawn_delayed_worker(Duration::from_millis(350)).await;

    let context = execute_hook(
        "claude-code",
        "context",
        json!({
            "session_id": "delayed-worker-content",
            "cwd": "/home/kcrawley/projects/cloudy-fork"
        }),
        &worker,
    )
    .await
    .unwrap();

    assert_eq!(context.exit_code, 0);
    let hook_output = context.output.hook_specific_output.unwrap();
    assert_eq!(hook_output.hook_event_name, "SessionStart");
    assert!(hook_output.additional_context.contains("Context Index:"));
    assert!(hook_output
        .additional_context
        .contains("Delayed readiness boot memory"));
    assert!(context
        .output
        .system_message
        .unwrap()
        .contains("View Observations Live @"));

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

    let opencode_init = execute_hook(
        "opencode",
        "session-init",
        json!({
            "session_id": "opencode-content-e2e",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "prompt": "Remember opencode lifecycle plugin memory."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_eq!(opencode_init.exit_code, 0);

    let opencode_obs = execute_hook(
        "opencode",
        "observation",
        json!({
            "session_id": "opencode-content-e2e",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "tool_name": "shell",
            "tool_input": { "command": "cat opencode.md" },
            "tool_response": { "output": "opencode plugin stores tool memory." }
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_eq!(opencode_obs.exit_code, 0);

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
    assert!(additional_context.starts_with("# [cloudy-fork] recent context,"));
    assert!(additional_context.contains("Fetch details: get_observations([IDs])"));
    assert!(additional_context.contains("Bash tool use"));
    assert!(additional_context.contains("GeminiAgent tool use"));
    assert!(additional_context.contains("Read tool use"));
    assert!(additional_context.contains("shell tool use"));

    let _ = shutdown.send(());
}

#[tokio::test]
async fn claude_summarize_hook_reads_transcript_and_completes_session() {
    let (worker, shutdown) = spawn_worker().await;
    let transcript_dir = tempfile::TempDir::new().unwrap();
    let transcript_path = transcript_dir.path().join("session.jsonl");
    std::fs::write(
        &transcript_path,
        r#"{"type":"user","message":{"content":"Summarize transcript memory."}}
{"type":"assistant","message":{"content":[{"type":"text","text":"Transcript summary should mention lower wattage instead of fan speed.\n<system-reminder>hidden reminder</system-reminder>"}]}}
"#,
    )
    .unwrap();

    let init = execute_hook(
        "claude-code",
        "session-init",
        json!({
            "session_id": "claude-transcript-summary-e2e",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "prompt": "Remember transcript summarize hook parity."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_eq!(init.exit_code, 0);

    let summarize = execute_hook(
        "claude-code",
        "summarize",
        json!({
            "session_id": "claude-transcript-summary-e2e",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "transcript_path": transcript_path.display().to_string()
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_eq!(summarize.exit_code, 0);

    let status: serde_json::Value = reqwest::get(format!(
        "{}/api/sessions/status?contentSessionId=claude-transcript-summary-e2e",
        worker.base_url()
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(status["hasSummary"], true);
    assert_eq!(status["queueLength"], 0);
    assert_eq!(status["session"]["status"], "completed");

    let search: serde_json::Value = reqwest::get(format!(
        "{}/api/search?query=lower%20wattage&project=cloudy-fork&limit=10",
        worker.base_url()
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(search["count"].as_i64().unwrap() >= 1);
    assert!(!search.to_string().contains("hidden reminder"));

    let _ = shutdown.send(());
}
