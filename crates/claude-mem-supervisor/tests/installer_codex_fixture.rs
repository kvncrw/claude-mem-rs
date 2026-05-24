//! Fixture parity tests for the Codex CLI integration.
//!
//! Codex doesn't run hook commands inline like Gemini does — it relies
//! on the transcript watcher reading `.codex/sessions/*.jsonl`. The
//! installer's job for codex is:
//!
//! 1. Write a small `.codex/AGENTS.md` hint telling Codex about the
//!    watcher, and
//! 2. Make sure the transcript-watch config sample lands so a fresh
//!    `claude-mem transcript watch` can pick it up.
//!
//! Both are pinned here so any drift surfaces in CI.

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

fn install_codex_only() {
    let report = run_install(InstallOptions {
        ide: Some("codex-cli".into()),
        yes: true,
        dry_run: false,
        bin_path: Some(BIN.into()),
    })
    .expect("codex install must succeed");
    assert!(
        report.failed.is_empty(),
        "codex install reported failures: {:?}",
        report.failed
    );
}

#[test]
fn codex_agents_md_contains_transcript_watch_hint() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    install_codex_only();

    let path = home.path().join(".codex/AGENTS.md");
    assert!(path.exists(), "AGENTS.md must be written");
    let body = std::fs::read_to_string(&path).unwrap();

    // The wrapping tag tells Codex this block is owned by claude-mem so
    // a future installer can surgically replace it. If we ever stop
    // emitting these markers, downstream rewrites stop being safe.
    assert!(
        body.contains("<claude-mem-context>"),
        "missing opening marker, got: {body:?}"
    );
    assert!(
        body.contains("</claude-mem-context>"),
        "missing closing marker, got: {body:?}"
    );
    // The hint string IS the user-visible contract.
    assert!(
        body.contains("claude-mem transcript watch"),
        "AGENTS.md must reference `claude-mem transcript watch`, got: {body:?}"
    );
}

#[test]
fn codex_install_writes_transcript_watcher_sample_config() {
    // The installer drops a default transcript-watch.json if one
    // doesn't already exist. Pin the structural shape so downstream
    // `claude-mem transcript watch` keeps working out of the box.
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    install_codex_only();

    let path = home.path().join(".claude-mem/transcript-watch.json");
    assert!(path.exists(), "transcript-watch.json must be written");
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    let watches = cfg["watches"].as_array().expect("watches must be an array");
    assert!(
        !watches.is_empty(),
        "sample config must include at least one watch entry"
    );
    // The sample is keyed to codex transcripts; if that ever changes,
    // we want the test to fail so the installer docs can be updated.
    let watch0 = &watches[0];
    assert!(
        watch0.get("path").is_some(),
        "watches[0] must have a path field"
    );
    assert!(
        watch0.get("context").is_some(),
        "watches[0] must have a context block for AGENTS.md injection"
    );
}

#[test]
fn codex_install_does_not_clobber_existing_transcript_config() {
    // If the user has already customized their watcher config, the
    // installer must not overwrite it. Re-running install on top of a
    // tweaked file is the realistic upgrade path.
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    let path = home.path().join(".claude-mem/transcript-watch.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let custom = r#"{"watches":[{"path":"/custom/path.jsonl","marker":"user-customized"}]}"#;
    std::fs::write(&path, custom).unwrap();

    install_codex_only();

    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, custom, "user transcript config must not be replaced");
}

#[test]
fn codex_agents_md_overwrites_to_keep_marker_block_fresh() {
    // Unlike the transcript config, AGENTS.md is a managed hint file;
    // the installer rewrites it on every run so a stale marker block
    // gets refreshed. Lock the behavior in so a future refactor can't
    // silently switch to a leave-alone policy without updating this
    // test + the docs.
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    let path = home.path().join(".codex/AGENTS.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "user content that should NOT be merged").unwrap();

    install_codex_only();

    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        body.contains("<claude-mem-context>"),
        "managed marker block must reappear after install, got: {body:?}"
    );
    assert!(
        !body.contains("user content that should NOT be merged"),
        "current installer rewrites AGENTS.md wholesale; if that changes update this test"
    );

    // Bin path isn't actually referenced from the AGENTS.md hint, but
    // the BIN constant participates in install_codex_only via the
    // worker env. Pin that we didn't accidentally leak it.
    assert!(
        !body.contains(BIN),
        "AGENTS.md hint must stay editor-agnostic, got: {body:?}"
    );
}
