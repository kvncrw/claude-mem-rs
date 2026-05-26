use axum::Router;
use claude_mem_supervisor::hooks::WorkerClient;
use claude_mem_supervisor::installer::{run_install, InstallOptions};
use claude_mem_supervisor::transcripts::config::{sample_config, TranscriptWatchConfig};
use claude_mem_supervisor::transcripts::watcher::TranscriptWatcher;
use claude_mem_worker::http::router::{build_router_with_state, AppState};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Mutex};

// `tokio::sync::Mutex` so the env guard can sit across `.await` without
// tripping `clippy::await_holding_lock`. The test body is fully async.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

async fn spawn_worker() -> (WorkerClient, oneshot::Sender<()>) {
    let app: Router = build_router_with_state(AppState::in_memory().unwrap());
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

#[test]
fn installer_writes_posix_runtime_integration_files() {
    // Sync test → `blocking_lock` on the tokio mutex. The async sibling
    // uses `.await`. Both serialise on the same guard.
    let _guard = ENV_LOCK.blocking_lock();
    let home = tempfile::TempDir::new().unwrap();
    let claude_dir = home.path().join(".claude");
    let cursor_mcp = home.path().join(".cursor/mcp.json");
    let gemini_settings = home.path().join(".gemini/settings.json");
    let codex_agents = home.path().join(".codex/AGENTS.md");
    let opencode_config = home.path().join(".config/opencode/opencode.json");
    let opencode_plugin = home
        .path()
        .join(".config/opencode/claude-mem-rs-plugin.mjs");
    let transcript_config = home.path().join(".claude-mem/transcript-watch.json");
    let systemd_user_dir = home.path().join(".config/systemd/user");
    std::env::set_var("HOME", home.path());
    std::env::set_var("CLAUDE_CONFIG_DIR", &claude_dir);
    std::env::set_var("CURSOR_MCP_CONFIG", &cursor_mcp);
    std::env::set_var("GEMINI_SETTINGS_PATH", &gemini_settings);
    std::env::set_var("CODEX_AGENTS_PATH", &codex_agents);
    std::env::set_var("OPENCODE_CONFIG_PATH", &opencode_config);
    std::env::set_var("OPENCODE_PLUGIN_PATH", &opencode_plugin);
    std::env::set_var("CLAUDE_MEM_TRANSCRIPTS_CONFIG_PATH", &transcript_config);
    std::env::set_var("CLAUDE_MEM_SYSTEMD_USER_DIR", &systemd_user_dir);
    std::env::set_var(
        "CLAUDE_MEM_LAUNCH_AGENTS_DIR",
        home.path().join("Library/LaunchAgents"),
    );
    std::env::set_var(
        "CLAUDE_MEM_WINDOWS_TASKS_DIR",
        home.path().join("AppData/Roaming/claude-mem"),
    );
    std::fs::create_dir_all(gemini_settings.parent().unwrap()).unwrap();
    std::fs::write(
        &gemini_settings,
        r#"{"hooks":{"Stop":[{"command":"old-invalid-hook"}]}}"#,
    )
    .unwrap();

    let report = run_install(InstallOptions {
        ide: Some("claude-code,cursor,gemini-cli,codex-cli,opencode".into()),
        yes: true,
        dry_run: false,
        bin_path: Some("/usr/local/bin/claude-mem".into()),
    })
    .unwrap();

    assert!(report.failed.is_empty());
    assert!(claude_dir
        .join("plugins/marketplaces/kvncrw/plugin/.claude-plugin/plugin.json")
        .exists());
    assert!(claude_dir
        .join("plugins/marketplaces/kvncrw/plugin/hooks/hooks.json")
        .exists());
    assert!(cursor_mcp.exists());
    assert!(gemini_settings.exists());
    assert!(codex_agents.exists());
    assert!(opencode_config.exists());
    assert!(opencode_plugin.exists());
    assert!(transcript_config.exists());
    assert!(systemd_user_dir.join("claude-mem-worker.service").exists());
    assert!(systemd_user_dir
        .join("claude-mem-transcript-watch.service")
        .exists());

    let cursor: Value =
        serde_json::from_str(&std::fs::read_to_string(cursor_mcp).unwrap()).unwrap();
    assert_eq!(
        cursor["mcpServers"]["claude-mem-rs"]["command"],
        "/usr/local/bin/claude-mem"
    );

    let hooks_path = claude_dir.join("plugins/marketplaces/kvncrw/plugin/hooks/hooks.json");
    let hooks: Value = serde_json::from_str(&std::fs::read_to_string(hooks_path).unwrap()).unwrap();
    assert_eq!(
        hooks["hooks"]["SessionStart"][0]["matcher"],
        "startup|clear|compact"
    );
    assert_eq!(hooks["hooks"]["PostToolUse"][0]["matcher"], "*");

    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        settings["hooks"]["SessionStart"][0]["matcher"],
        "startup|clear|compact"
    );
    assert_eq!(settings["hooks"]["PostToolUse"][0]["matcher"], "*");
    assert_eq!(
        settings["mcpServers"]["mcp-search"]["command"],
        "/usr/local/bin/claude-mem"
    );
    assert!(
        settings["mcpServers"]["mcp-search"]["env"]["CLAUDE_MEM_HOME"]
            .as_str()
            .unwrap()
            .ends_with(".claude-mem")
    );
    assert!(settings["mcpServers"].get("claude-mem-rs").is_none());

    let gemini: Value =
        serde_json::from_str(&std::fs::read_to_string(gemini_settings).unwrap()).unwrap();
    assert_eq!(
        gemini["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "\"/usr/local/bin/claude-mem\" hook gemini-cli context"
    );
    assert_eq!(
        gemini["hooks"]["BeforeAgent"][0]["hooks"][0]["command"],
        "\"/usr/local/bin/claude-mem\" hook gemini-cli session-init"
    );
    assert_eq!(
        gemini["hooks"]["AfterTool"][0]["hooks"][0]["command"],
        "\"/usr/local/bin/claude-mem\" hook gemini-cli observation"
    );
    assert_eq!(
        gemini["hooks"]["AfterAgent"][0]["hooks"][0]["command"],
        "\"/usr/local/bin/claude-mem\" hook gemini-cli summarize"
    );
    assert_eq!(
        gemini["hooks"]["SessionEnd"][0]["hooks"][0]["command"],
        "\"/usr/local/bin/claude-mem\" hook gemini-cli session-complete"
    );
    assert!(gemini["hooks"].get("Stop").is_none());

    let claude_state: Value =
        serde_json::from_str(&std::fs::read_to_string(home.path().join(".claude.json")).unwrap())
            .unwrap();
    assert_eq!(
        claude_state["mcpServers"]["mcp-search"]["command"],
        "/usr/local/bin/claude-mem"
    );
    assert!(
        claude_state["mcpServers"]["mcp-search"]["env"]["CLAUDE_MEM_HOME"]
            .as_str()
            .unwrap()
            .ends_with(".claude-mem")
    );
    assert!(claude_state["mcpServers"].get("claude-mem-rs").is_none());

    let opencode: Value =
        serde_json::from_str(&std::fs::read_to_string(opencode_config).unwrap()).unwrap();
    assert_eq!(opencode["mcp"]["claude-mem"]["type"], "local");
    assert_eq!(
        opencode["mcp"]["claude-mem"]["command"][0],
        "/usr/local/bin/claude-mem"
    );
    assert_eq!(opencode["mcp"]["claude-mem"]["command"][1], "mcp");
    assert_eq!(
        opencode["mcp"]["claude-mem"]["environment"]["CLAUDE_MEM_HOME"],
        home.path().join(".claude-mem").display().to_string()
    );
    assert!(opencode["plugin"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry == opencode_plugin.to_str().unwrap()));

    let plugin_text = std::fs::read_to_string(opencode_plugin).unwrap();
    assert!(plugin_text.contains("experimental.chat.system.transform"));
    assert!(plugin_text.contains("\"tool.execute.after\""));
    assert!(plugin_text.contains("\"hook\", \"opencode\", event"));
}

#[tokio::test]
async fn transcript_watcher_processes_codex_jsonl_into_memory() {
    let _guard = ENV_LOCK.lock().await;
    let home = tempfile::TempDir::new().unwrap();
    std::env::set_var("HOME", home.path());
    let (worker, shutdown) = spawn_worker().await;
    let project_dir = home.path().join("cloudy-transcript-project");
    std::fs::create_dir_all(&project_dir).unwrap();
    let transcript_dir = home.path().join(".codex/sessions");
    std::fs::create_dir_all(&transcript_dir).unwrap();
    let session_id = "11111111-2222-3333-4444-555555555555";
    let transcript_path = transcript_dir.join(format!("{session_id}.jsonl"));
    std::fs::write(
        &transcript_path,
        format!(
            r#"{{"type":"session_meta","payload":{{"id":"{session_id}","cwd":"{}"}}}}
{{"payload":{{"type":"user_message","message":"Remember transcript watcher parity for Rust."}}}}
{{"payload":{{"type":"function_call","call_id":"call-1","name":"Read","arguments":{{"file_path":"/tmp/transcript.md"}}}}}}
{{"payload":{{"type":"function_call_output","call_id":"call-1","output":"transcript watcher stored output about wattage limits"}}}}
{{"payload":{{"type":"agent_message","message":"Transcript watcher learned wattage limits beat fan speed."}}}}
{{"payload":{{"type":"turn_completed"}}}}
"#,
            project_dir.display()
        ),
    )
    .unwrap();

    let mut config: TranscriptWatchConfig = sample_config();
    config.watches[0].path = transcript_path.display().to_string();
    config.watches[0].start_at_end = Some(false);
    config.watches[0].context.as_mut().unwrap().path =
        Some(home.path().join(".codex/AGENTS.md").display().to_string());
    let state_path = home.path().join(".claude-mem/transcript-watch-state.json");
    let mut watcher = TranscriptWatcher::new(config, state_path.clone(), worker.clone());
    let stats = watcher.process_once().await.unwrap();

    assert_eq!(stats.files_seen, 1);
    assert!(stats.session_inits >= 1);
    assert!(stats.observations >= 1);
    assert!(stats.completions >= 1);
    assert!(state_path.exists());
    assert!(home.path().join(".codex/AGENTS.md").exists());

    let search: Value = reqwest::get(format!(
        "{}/api/search?query=transcript%20watcher%20stored%20output&project=cloudy-transcript-project&limit=10",
        worker.base_url()
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(search["count"].as_i64().unwrap() >= 1);

    let _ = shutdown.send(());
}
