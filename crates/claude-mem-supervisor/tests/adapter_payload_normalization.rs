//! Regression coverage for AC #2 (issue #5):
//!
//! Per-IDE adapter payload normalization. Each test feeds a realistic raw
//! platform payload into `execute_hook(...)` against a **capturing worker
//! stub** that records the exact JSON body the hook adapter posts to
//! `/api/sessions/init`, `/api/sessions/observations`, and
//! `/api/sessions/complete`. The captured body is then asserted against
//! the documented adapter contract.
//!
//! Why a capturing stub instead of the real worker:
//!
//! - The real worker's `/api/sessions/observations` route synchronously
//!   drains the pending message via the observer and **deletes** the
//!   `pending_messages` row on success, so post-hoc DB inspection cannot
//!   see the normalized `tool_input`/`tool_response` shapes the adapter
//!   produced.
//! - The observer also rewrites tool fields into an `ObservationRow`
//!   (`title = "<tool_name> tool use"`, narrative, facts, etc.), losing
//!   the raw input/response shape.
//! - The normalization functions
//!   (`normalize_cursor_input`/`normalize_gemini_input`/`normalize_raw_input`)
//!   are private to `claude_mem_supervisor::hooks`, so we cannot call
//!   them directly from an integration test.
//!
//! The capturing stub records each request body, plus the also-asserted
//! `format_output` envelope on the hook execution result. Behavioral /
//! queue-processing coverage already lives in `claude_hook_e2e.rs` — this
//! file fences the input adapter alone.

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use claude_mem_supervisor::hooks::{execute_hook, HookExecution, WorkerClient};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Default, Clone)]
struct CapturedRequests {
    inner: Arc<Mutex<Vec<(String, Value)>>>,
}

impl CapturedRequests {
    fn snapshot(&self) -> Vec<(String, Value)> {
        self.inner.lock().unwrap().clone()
    }

    fn first_body(&self, path: &str) -> Value {
        let snapshot = self.snapshot();
        snapshot
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, body)| body.clone())
            .unwrap_or_else(|| panic!("no captured request for {path}; got {snapshot:?}"))
    }
}

/// Spawns a minimal axum app that records every JSON body POSTed to the
/// session lifecycle endpoints. Responds with the smallest valid payload
/// each hook handler expects so the hook continues executing instead of
/// short-circuiting on a non-2xx response.
async fn spawn_capturing_worker() -> (WorkerClient, CapturedRequests, oneshot::Sender<()>) {
    let captured = CapturedRequests::default();
    let app = Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route(
            "/api/sessions/init",
            post(
                |State(captured): State<CapturedRequests>, Json(body): Json<Value>| async move {
                    captured
                        .inner
                        .lock()
                        .unwrap()
                        .push(("/api/sessions/init".to_owned(), body));
                    Json(json!({
                        "sessionDbId": 1,
                        "promptNumber": 1,
                        "skipped": false,
                        "contextInjected": false
                    }))
                },
            ),
        )
        .route(
            "/api/sessions/observations",
            post(
                |State(captured): State<CapturedRequests>, Json(body): Json<Value>| async move {
                    captured
                        .inner
                        .lock()
                        .unwrap()
                        .push(("/api/sessions/observations".to_owned(), body));
                    Json(json!({ "success": true, "inserted": 0 }))
                },
            ),
        )
        .route(
            "/api/sessions/complete",
            post(
                |State(captured): State<CapturedRequests>, Json(body): Json<Value>| async move {
                    captured
                        .inner
                        .lock()
                        .unwrap()
                        .push(("/api/sessions/complete".to_owned(), body));
                    Json(json!({ "success": true, "completed": true }))
                },
            ),
        )
        .route(
            "/api/context/inject",
            get(|| async { axum::response::Html("# context\n\nseed body\n") }),
        )
        .route(
            "/api/context/semantic",
            post(|Json(_): Json<Value>| async { Json(json!({ "context": "", "count": 0 })) }),
        )
        .with_state(captured.clone());

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
    (WorkerClient::new(format!("http://{}", addr)), captured, tx)
}

fn assert_hook_succeeded(execution: &HookExecution) {
    assert_eq!(
        execution.exit_code, 0,
        "hook must exit 0; got {}",
        execution.exit_code
    );
}

// ---------------------------------------------------------------------
// Claude Code adapter (also covers the `claude` alias)
// ---------------------------------------------------------------------

#[tokio::test]
async fn claude_code_adapter_posts_raw_session_id_and_cwd_unchanged() {
    let (worker, captured, shutdown) = spawn_capturing_worker().await;

    let init = execute_hook(
        "claude-code",
        "session-init",
        json!({
            "session_id": "claude-adapter-session",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "prompt": "Verify the claude-code adapter normalizes raw session payloads."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&init);
    // claude-code emits the structured hookSpecificOutput unchanged (no
    // gemini-style continue=true wrap, no cursor-style stub).
    assert_eq!(init.output.r#continue, None);

    let body = captured.first_body("/api/sessions/init");
    assert_eq!(body["contentSessionId"], "claude-adapter-session");
    assert_eq!(body["project"], "cloudy-fork");
    assert_eq!(body["platformSource"], "claude");
    assert!(body["prompt"]
        .as_str()
        .unwrap()
        .contains("Verify the claude-code adapter"));

    let observation = execute_hook(
        "claude-code",
        "observation",
        json!({
            "session_id": "claude-adapter-session",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "tool_name": "Read",
            "tool_input": { "file_path": "/repo/src/lib.rs" },
            "tool_response": { "content": "claude-code raw fields stay raw" }
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&observation);

    let obs_body = captured.first_body("/api/sessions/observations");
    assert_eq!(obs_body["contentSessionId"], "claude-adapter-session");
    assert_eq!(obs_body["tool_name"], "Read");
    assert_eq!(obs_body["tool_input"]["file_path"], "/repo/src/lib.rs");
    assert_eq!(
        obs_body["tool_response"]["content"],
        "claude-code raw fields stay raw"
    );
    assert_eq!(obs_body["platformSource"], "claude");

    let _ = shutdown.send(());
}

#[tokio::test]
async fn claude_alias_maps_platform_source_to_claude() {
    let (worker, captured, shutdown) = spawn_capturing_worker().await;

    // `claude` is the documented alias of `claude-code` in
    // `platform_source` / `normalize_input`.
    let init = execute_hook(
        "claude",
        "session-init",
        json!({
            "session_id": "claude-alias-session",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "prompt": "The `claude` platform string must be treated as `claude-code`."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&init);
    assert_eq!(init.output.r#continue, None);

    let body = captured.first_body("/api/sessions/init");
    assert_eq!(body["contentSessionId"], "claude-alias-session");
    assert_eq!(body["platformSource"], "claude");

    let _ = shutdown.send(());
}

// ---------------------------------------------------------------------
// Cursor adapter
// ---------------------------------------------------------------------

#[tokio::test]
async fn cursor_adapter_normalizes_conversation_id_workspace_roots_and_query() {
    let (worker, captured, shutdown) = spawn_capturing_worker().await;

    let init = execute_hook(
        "cursor",
        "session-init",
        json!({
            "conversation_id": "cursor-conv-1",
            "workspace_roots": ["/home/kcrawley/projects/cloudy-fork", "/ignored/second"],
            "query": "Cursor adapter must read conversation_id + workspace_roots[0] + query."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&init);
    // Cursor adapter strips structured output and emits a continue=true
    // stub to satisfy Cursor's hook contract.
    assert_eq!(init.output.r#continue, Some(true));
    assert!(init.output.hook_specific_output.is_none());
    assert!(init.output.system_message.is_none());

    let body = captured.first_body("/api/sessions/init");
    assert_eq!(body["contentSessionId"], "cursor-conv-1");
    // The cwd → project derivation uses workspace_roots[0], not the
    // second entry.
    assert_eq!(body["project"], "cloudy-fork");
    assert_eq!(body["platformSource"], "cursor");
    assert!(body["prompt"]
        .as_str()
        .unwrap()
        .contains("Cursor adapter must read"));

    let _ = shutdown.send(());
}

#[tokio::test]
async fn cursor_adapter_synthesizes_bash_tool_from_shell_command() {
    let (worker, captured, shutdown) = spawn_capturing_worker().await;

    let observation = execute_hook(
        "cursor",
        "observation",
        json!({
            "conversation_id": "cursor-shell-1",
            "workspace_roots": ["/home/kcrawley/projects/cloudy-fork"],
            "command": "cat /repo/notes.md",
            "output": "Cursor shell stdout body"
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&observation);
    assert_eq!(observation.output.r#continue, Some(true));

    let body = captured.first_body("/api/sessions/observations");
    // Cursor `command` (with no explicit tool_name) is normalized into
    // Bash + `{ "command": "..." }`.
    assert_eq!(body["tool_name"], "Bash");
    assert_eq!(body["tool_input"]["command"], "cat /repo/notes.md");
    assert_eq!(body["tool_response"]["output"], "Cursor shell stdout body");

    let _ = shutdown.send(());
}

#[tokio::test]
async fn cursor_adapter_passes_through_explicit_tool_name() {
    let (worker, captured, shutdown) = spawn_capturing_worker().await;

    let observation = execute_hook(
        "cursor",
        "observation",
        json!({
            "conversation_id": "cursor-tool-1",
            "workspace_roots": ["/home/kcrawley/projects/cloudy-fork"],
            "tool_name": "Read",
            "tool_input": { "file_path": "/repo/explicit.rs" },
            "result_json": { "content": "Cursor explicit tool result body" }
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&observation);

    let body = captured.first_body("/api/sessions/observations");
    // Explicit tool_name path bypasses Bash synthesis and reads
    // `result_json` instead of `output`.
    assert_eq!(body["tool_name"], "Read");
    assert_eq!(body["tool_input"]["file_path"], "/repo/explicit.rs");
    assert_eq!(
        body["tool_response"]["content"],
        "Cursor explicit tool result body"
    );

    let _ = shutdown.send(());
}

// ---------------------------------------------------------------------
// Gemini adapter
// ---------------------------------------------------------------------

#[tokio::test]
async fn gemini_adapter_wraps_continue_true_envelope() {
    let (worker, _captured, shutdown) = spawn_capturing_worker().await;

    let init = execute_hook(
        "gemini-cli",
        "session-init",
        json!({
            "session_id": "gemini-session-1",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "prompt": "Gemini adapter must wrap continue=true on every output."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&init);
    // Gemini format_output wraps `continue=true` on every output.
    assert_eq!(init.output.r#continue, Some(true));

    let _ = shutdown.send(());
}

#[tokio::test]
async fn gemini_alias_maps_platform_source_to_gemini() {
    let (worker, captured, shutdown) = spawn_capturing_worker().await;

    // The bare `gemini` string is an alias of `gemini-cli`.
    let init = execute_hook(
        "gemini",
        "session-init",
        json!({
            "session_id": "gemini-alias-1",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "prompt": "Bare `gemini` must route through the Gemini adapter."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&init);
    assert_eq!(init.output.r#continue, Some(true));

    let body = captured.first_body("/api/sessions/init");
    assert_eq!(body["platformSource"], "gemini");
    assert_eq!(body["contentSessionId"], "gemini-alias-1");

    let _ = shutdown.send(());
}

#[tokio::test]
async fn gemini_after_agent_synthesizes_gemini_agent_tool() {
    let (worker, captured, shutdown) = spawn_capturing_worker().await;

    let observation = execute_hook(
        "gemini-cli",
        "observation",
        json!({
            "session_id": "gemini-after-1",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "hook_event_name": "AfterAgent",
            "prompt": "Summarize gemini thermal mitigation work.",
            "prompt_response": "Gemini learned package wattage beats fan speed for tiny coolers."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&observation);

    let body = captured.first_body("/api/sessions/observations");
    assert_eq!(body["tool_name"], "GeminiAgent");
    assert_eq!(
        body["tool_input"]["prompt"],
        "Summarize gemini thermal mitigation work."
    );
    assert_eq!(
        body["tool_response"]["response"],
        "Gemini learned package wattage beats fan speed for tiny coolers."
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn gemini_notification_synthesizes_gemini_notification_tool() {
    let (worker, captured, shutdown) = spawn_capturing_worker().await;

    let observation = execute_hook(
        "gemini-cli",
        "observation",
        json!({
            "session_id": "gemini-notif-1",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "hook_event_name": "Notification",
            "notification_type": "warning",
            "message": "Gemini notification body",
            "details": { "code": "GEMINI_WARN_42" }
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&observation);

    let body = captured.first_body("/api/sessions/observations");
    assert_eq!(body["tool_name"], "GeminiNotification");
    assert_eq!(body["tool_input"]["notification_type"], "warning");
    assert_eq!(body["tool_input"]["message"], "Gemini notification body");
    assert_eq!(body["tool_response"]["details"]["code"], "GEMINI_WARN_42");

    let _ = shutdown.send(());
}

#[tokio::test]
async fn gemini_before_tool_marks_pre_execution_response() {
    let (worker, captured, shutdown) = spawn_capturing_worker().await;

    let observation = execute_hook(
        "gemini-cli",
        "observation",
        json!({
            "session_id": "gemini-before-1",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "hook_event_name": "BeforeTool",
            "tool_name": "Edit",
            "tool_input": { "file_path": "/repo/before.rs", "old_str": "a", "new_str": "b" }
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&observation);

    let body = captured.first_body("/api/sessions/observations");
    assert_eq!(body["tool_name"], "Edit");
    assert_eq!(
        body["tool_response"]["_preExecution"], true,
        "BeforeTool with no tool_response must synthesize a _preExecution marker"
    );

    let _ = shutdown.send(());
}

// ---------------------------------------------------------------------
// Codex adapter
// ---------------------------------------------------------------------

#[tokio::test]
async fn codex_adapter_passes_raw_payload_through() {
    let (worker, captured, shutdown) = spawn_capturing_worker().await;

    let init = execute_hook(
        "codex",
        "session-init",
        json!({
            "session_id": "codex-session-1",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "prompt": "Codex adapter uses raw normalization, no field rewriting."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&init);
    // Codex uses the claude-code-style structured output (no continue=true
    // wrap). format_output's gemini/cursor branches don't apply.
    assert_eq!(init.output.r#continue, None);

    let body = captured.first_body("/api/sessions/init");
    assert_eq!(body["contentSessionId"], "codex-session-1");
    assert_eq!(body["project"], "cloudy-fork");
    assert_eq!(body["platformSource"], "codex");

    let observation = execute_hook(
        "codex",
        "observation",
        json!({
            "session_id": "codex-session-1",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "tool_name": "Read",
            "tool_input": { "file_path": "/repo/codex.md" },
            "tool_response": { "content": "Codex raw observation body" }
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&observation);

    let obs_body = captured.first_body("/api/sessions/observations");
    assert_eq!(obs_body["tool_name"], "Read");
    assert_eq!(
        obs_body["tool_response"]["content"],
        "Codex raw observation body"
    );
    assert_eq!(obs_body["platformSource"], "codex");

    let _ = shutdown.send(());
}

// ---------------------------------------------------------------------
// opencode adapter
// ---------------------------------------------------------------------

#[tokio::test]
async fn opencode_adapter_routes_through_raw_normalizer() {
    let (worker, captured, shutdown) = spawn_capturing_worker().await;

    let init = execute_hook(
        "opencode",
        "session-init",
        json!({
            "session_id": "opencode-session-1",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "prompt": "opencode uses the catchall raw normalizer; verify session creation."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&init);
    // opencode falls through to the raw normalizer + raw format path; no
    // continue=true wrap.
    assert_eq!(init.output.r#continue, None);

    let init_body = captured.first_body("/api/sessions/init");
    assert_eq!(init_body["contentSessionId"], "opencode-session-1");
    // `platform_source` falls through to the literal platform string for
    // anything not in the named map (claude/gemini/cursor/codex/raw).
    assert_eq!(init_body["platformSource"], "opencode");

    let observation = execute_hook(
        "opencode",
        "observation",
        json!({
            "session_id": "opencode-session-1",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "tool_name": "shell",
            "tool_input": { "command": "ls -la /repo" },
            "tool_response": { "output": "opencode plugin tool output" }
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&observation);

    let obs_body = captured.first_body("/api/sessions/observations");
    // opencode keeps the literal tool name from its lifecycle plugin
    // (`shell`, `bash`, etc.) without remapping.
    assert_eq!(obs_body["tool_name"], "shell");
    assert_eq!(obs_body["tool_input"]["command"], "ls -la /repo");
    assert_eq!(
        obs_body["tool_response"]["output"],
        "opencode plugin tool output"
    );

    let _ = shutdown.send(());
}

// ---------------------------------------------------------------------
// raw adapter
// ---------------------------------------------------------------------

#[tokio::test]
async fn raw_adapter_accepts_camel_case_and_snake_case() {
    let (worker, captured, shutdown) = spawn_capturing_worker().await;

    // The raw normalizer falls back from snake_case → camelCase for every
    // string field. Mix the two to prove both arms work.
    let init = execute_hook(
        "raw",
        "session-init",
        json!({
            "sessionId": "raw-session-1",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "prompt": "Raw normalizer must accept camelCase keys as a fallback."
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&init);
    assert_eq!(init.output.r#continue, None);

    let init_body = captured.first_body("/api/sessions/init");
    assert_eq!(init_body["contentSessionId"], "raw-session-1");
    assert_eq!(init_body["platformSource"], "raw");

    let observation = execute_hook(
        "raw",
        "observation",
        json!({
            "session_id": "raw-session-1",
            "cwd": "/home/kcrawley/projects/cloudy-fork",
            "toolName": "Bash",
            "toolInput": { "command": "echo raw camelCase" },
            "toolResponse": { "output": "raw stdout" }
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_hook_succeeded(&observation);

    let obs_body = captured.first_body("/api/sessions/observations");
    assert_eq!(obs_body["tool_name"], "Bash");
    assert_eq!(obs_body["tool_input"]["command"], "echo raw camelCase");
    assert_eq!(obs_body["tool_response"]["output"], "raw stdout");

    let _ = shutdown.send(());
}

// ---------------------------------------------------------------------
// Cross-platform envelope invariant (uses real worker for context body)
// ---------------------------------------------------------------------

#[tokio::test]
async fn context_hook_emits_platform_appropriate_envelope_for_each_adapter() {
    use claude_mem_worker::http::router::{build_router_with_state, AppState};

    // Seed a real worker so the context hook actually has something to
    // inject — the capturing stub only stubs context.
    let state = AppState::in_memory().unwrap();
    {
        let conn = state.conn.lock().unwrap();
        claude_mem_core::db::sessions::create_session(
            &conn,
            &claude_mem_core::types::session::CreateSessionInput {
                content_session_id: "context-seed".into(),
                project: "cloudy-fork".into(),
                user_prompt: Some("Seed context for cross-platform envelope check.".into()),
                started_at: "2024-06-01T09:00:00.000Z".into(),
                started_at_epoch: 1_717_232_400_000,
            },
        )
        .unwrap();
        claude_mem_core::db::sessions::update_memory_session_id(
            &conn,
            "context-seed",
            "context-seed-memory",
        )
        .unwrap();
        claude_mem_core::db::observations::store::store_observation(
            &conn,
            &claude_mem_core::types::ObservationInput {
                memory_session_id: "context-seed-memory".into(),
                project: "cloudy-fork".into(),
                r#type: "decision".into(),
                title: Some("Context envelope seed".into()),
                narrative: Some("Cross-platform envelope check needs at least one row.".into()),
                created_at: "2024-06-01T09:30:00.000Z".into(),
                created_at_epoch: 1_717_234_200_000,
                content_hash: Some("context-envelope-seed".into()),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let app = build_router_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    let worker = WorkerClient::new(format!("http://{}", addr));

    // Claude-code: structured output, no continue wrap.
    let claude_ctx = execute_hook(
        "claude-code",
        "context",
        json!({ "session_id": "ctx-claude", "cwd": "/home/kcrawley/projects/cloudy-fork" }),
        &worker,
    )
    .await
    .unwrap();
    assert_eq!(claude_ctx.output.r#continue, None);
    let claude_hso = claude_ctx
        .output
        .hook_specific_output
        .as_ref()
        .expect("claude-code context must produce hookSpecificOutput");
    assert_eq!(claude_hso.hook_event_name, "SessionStart");
    assert!(!claude_hso.additional_context.is_empty());

    // Gemini: continue=true, hook_event_name blanked.
    let gemini_ctx = execute_hook(
        "gemini-cli",
        "context",
        json!({ "session_id": "ctx-gemini", "cwd": "/home/kcrawley/projects/cloudy-fork" }),
        &worker,
    )
    .await
    .unwrap();
    assert_eq!(gemini_ctx.output.r#continue, Some(true));
    let gemini_hso = gemini_ctx
        .output
        .hook_specific_output
        .as_ref()
        .expect("gemini context must still emit hookSpecificOutput");
    assert_eq!(
        gemini_hso.hook_event_name, "",
        "Gemini format_output must blank the event name"
    );
    assert!(!gemini_hso.additional_context.is_empty());

    // Cursor: continue=true stub only.
    let cursor_ctx = execute_hook(
        "cursor",
        "context",
        json!({
            "conversation_id": "ctx-cursor",
            "workspace_roots": ["/home/kcrawley/projects/cloudy-fork"]
        }),
        &worker,
    )
    .await
    .unwrap();
    assert_eq!(cursor_ctx.output.r#continue, Some(true));
    assert!(cursor_ctx.output.hook_specific_output.is_none());
    assert!(cursor_ctx.output.system_message.is_none());

    let _ = shutdown.send(());
}
