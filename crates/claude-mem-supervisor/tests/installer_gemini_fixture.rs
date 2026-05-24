//! Fixture parity tests for the Gemini CLI settings/hooks the installer
//! writes.
//!
//! Critical asserts in here:
//!
//! - All five lifecycle events (SessionStart, BeforeAgent, AfterTool,
//!   AfterAgent, SessionEnd) are bound with the correct hook names,
//!   matchers, timeouts, and exact `command` strings.
//! - The stale TS-era `Stop` hook is REMOVED when present in the
//!   pre-existing settings file. This is a documented behavior the TS
//!   v12 installer set up and the Rust port has to preserve to avoid
//!   stranding users on a broken legacy hook.
//!
//! Tests run against a temp HOME so the real user environment is not
//! mutated.

use claude_mem_supervisor::installer::{run_install, InstallOptions};
use serde_json::Value;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

const BIN: &str = "/usr/local/bin/claude-mem";

fn isolate_env(home: &std::path::Path) {
    std::env::set_var("HOME", home);
    std::env::set_var("CLAUDE_CONFIG_DIR", home.join(".claude"));
    std::env::set_var("CURSOR_MCP_CONFIG", home.join(".cursor/mcp.json"));
    std::env::set_var("GEMINI_SETTINGS_PATH", home.join(".gemini/settings.json"));
    std::env::set_var("CODEX_AGENTS_PATH", home.join(".codex/AGENTS.md"));
    std::env::set_var(
        "OPENCODE_CONFIG_PATH",
        home.join(".config/opencode/opencode.json"),
    );
    std::env::set_var(
        "OPENCODE_PLUGIN_PATH",
        home.join(".config/opencode/claude-mem-rs-plugin.mjs"),
    );
    std::env::set_var(
        "CLAUDE_MEM_TRANSCRIPTS_CONFIG_PATH",
        home.join(".claude-mem/transcript-watch.json"),
    );
    std::env::remove_var("CLAUDE_MEM_HOME");
    std::env::remove_var("CLAUDE_MEM_DATA_DIR");
    std::env::remove_var("CLAUDE_MEM_WORKER_URL");
    // See installer_cursor_fixture::isolate_env for the rationale.
    #[cfg(windows)]
    {
        std::env::set_var("USERPROFILE", home);
        std::env::remove_var("HOMEDRIVE");
        std::env::remove_var("HOMEPATH");
    }
}

fn install_gemini_only() {
    let report = run_install(InstallOptions {
        ide: Some("gemini-cli".into()),
        yes: true,
        dry_run: false,
        bin_path: Some(BIN.into()),
    })
    .expect("gemini install must succeed");
    assert!(
        report.failed.is_empty(),
        "gemini install reported failures: {:?}",
        report.failed
    );
}

/// Asserts the structural contract of one lifecycle hook entry in the
/// gemini settings file. Pulled into a helper so each event-table row
/// stays one line in the per-event test.
fn assert_gemini_hook(
    settings: &Value,
    event: &str,
    expected_matcher: &str,
    expected_name: &str,
    expected_event_arg: &str,
    expected_timeout_ms: u64,
) {
    let arr = settings["hooks"][event]
        .as_array()
        .unwrap_or_else(|| panic!("hooks.{event} must be an array"));
    assert_eq!(arr.len(), 1, "hooks.{event} must have one entry");
    let entry = &arr[0];
    assert_eq!(
        entry["matcher"], expected_matcher,
        "hooks.{event}[0].matcher must be {expected_matcher:?}"
    );
    let inner = entry["hooks"]
        .as_array()
        .unwrap_or_else(|| panic!("hooks.{event}[0].hooks must be an array"));
    assert_eq!(inner.len(), 1, "hooks.{event}[0].hooks must have one entry");
    let hook = &inner[0];
    assert_eq!(hook["name"], expected_name);
    assert_eq!(hook["type"], "command");
    assert_eq!(
        hook["command"],
        format!("\"{BIN}\" hook gemini-cli {expected_event_arg}"),
        "command for {event} must match expected gemini-cli wiring"
    );
    assert_eq!(hook["timeout"], expected_timeout_ms);
}

#[test]
fn gemini_writes_all_five_lifecycle_hooks() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    install_gemini_only();

    let path = home.path().join(".gemini/settings.json");
    let settings: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    // Each row pins one lifecycle event with its expected hook name,
    // event arg, and timeout. If the installer drops or renames an
    // event the matching row fails immediately.
    assert_gemini_hook(
        &settings,
        "SessionStart",
        "startup|resume|clear",
        "claude-mem-rs-context",
        "context",
        60_000,
    );
    assert_gemini_hook(
        &settings,
        "BeforeAgent",
        "*",
        "claude-mem-rs-session-init",
        "session-init",
        60_000,
    );
    assert_gemini_hook(
        &settings,
        "AfterTool",
        "*",
        "claude-mem-rs-observation",
        "observation",
        120_000,
    );
    assert_gemini_hook(
        &settings,
        "AfterAgent",
        "*",
        "claude-mem-rs-summarize",
        "summarize",
        120_000,
    );
    assert_gemini_hook(
        &settings,
        "SessionEnd",
        "*",
        "claude-mem-rs-complete",
        "session-complete",
        30_000,
    );
}

#[test]
fn gemini_session_start_matcher_pins_resume_clear_set() {
    // The SessionStart matcher controls when the context hook fires;
    // it's the only event whose matcher differs from the wildcard "*"
    // contract used everywhere else. Pin it so changes are visible.
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    install_gemini_only();
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".gemini/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        settings["hooks"]["SessionStart"][0]["matcher"],
        "startup|resume|clear"
    );
}

#[test]
fn gemini_removes_stale_stop_hook_from_existing_settings() {
    // Issue #2 explicitly calls out the stale Stop-hook removal as an
    // acceptance criterion. Seed a settings file that contains a Stop
    // hook left over from the TS v12 era and assert it's gone after
    // install.
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    let path = home.path().join(".gemini/settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"old-claude-mem hook gemini-cli stop"}]}]}}"#,
    )
    .unwrap();

    install_gemini_only();

    let settings: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        settings["hooks"].get("Stop").is_none(),
        "stale Stop hook must be removed by the installer, got: {settings:#}"
    );
}

#[test]
fn gemini_install_preserves_unrelated_top_level_keys() {
    // Users frequently customize Gemini settings (theme, model, etc.).
    // The installer must only touch `hooks.*`, never wipe unrelated
    // settings.
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    let path = home.path().join(".gemini/settings.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"theme":"dracula","model":"gemini-1.5-pro","selectedAuthType":"oauth-personal"}"#,
    )
    .unwrap();

    install_gemini_only();

    let settings: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(settings["theme"], "dracula");
    assert_eq!(settings["model"], "gemini-1.5-pro");
    assert_eq!(settings["selectedAuthType"], "oauth-personal");
    assert!(settings["hooks"]["SessionStart"].is_array());
}

#[test]
fn gemini_install_is_idempotent() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    install_gemini_only();
    let first = std::fs::read_to_string(home.path().join(".gemini/settings.json")).unwrap();

    install_gemini_only();
    let second = std::fs::read_to_string(home.path().join(".gemini/settings.json")).unwrap();

    assert_eq!(first, second, "second gemini install must be a no-op");
}
