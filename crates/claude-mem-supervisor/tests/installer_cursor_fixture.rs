//! Fixture parity tests for the Cursor MCP config the installer writes.
//!
//! These pin the structural shape of `.cursor/mcp.json` so any drift from
//! the TS v12 plugin's contract surfaces in CI rather than silently
//! breaking Cursor users. Tests run against a temp HOME so the real user
//! environment is never mutated.
//!
//! Each test grabs `ENV_LOCK` because `std::env::set_var` is process-wide.

use claude_mem_supervisor::installer::{run_install, InstallOptions};
use serde_json::Value;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

const BIN: &str = "/usr/local/bin/claude-mem";

/// Set the env vars the installer reads so it writes inside `home`.
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
    // Keep the worker env stable so the embedded `env` block in mcpServers
    // is deterministic across hosts (CI vs developer laptops). CLAUDE_MEM_HOME
    // is set in many real environments (the user's own shell, agent runners,
    // CI containers); the installer must derive it from the temp HOME we just
    // set, NOT from whatever happens to be in the test process env.
    std::env::remove_var("CLAUDE_MEM_HOME");
    std::env::remove_var("CLAUDE_MEM_DATA_DIR");
    std::env::remove_var("CLAUDE_MEM_WORKER_URL");
}

fn install_cursor_only() {
    let report = run_install(InstallOptions {
        ide: Some("cursor".into()),
        yes: true,
        dry_run: false,
        bin_path: Some(BIN.into()),
    })
    .expect("cursor install must succeed");
    assert!(
        report.failed.is_empty(),
        "cursor install reported failures: {:?}",
        report.failed
    );
}

#[test]
fn cursor_mcp_json_structure_matches_contract() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    install_cursor_only();

    let path = home.path().join(".cursor/mcp.json");
    assert!(path.exists(), "cursor mcp.json must be written");
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    let server = &cfg["mcpServers"]["claude-mem-rs"];
    assert!(
        server.is_object(),
        "mcpServers.claude-mem-rs must be object"
    );

    // Command string is compared explicitly so a casing/quoting change is
    // caught — Cursor invokes the binary directly so the bytes matter.
    assert_eq!(server["command"], BIN);
    assert_eq!(server["args"], serde_json::json!(["mcp"]));

    // env.CLAUDE_MEM_HOME must point at the temp HOME, never the real
    // host. If platform_paths starts ignoring HOME the assertion catches
    // it instantly.
    let env_block = &server["env"];
    assert!(env_block.is_object(), "env must be object, got {env_block}");
    let claude_mem_home = env_block["CLAUDE_MEM_HOME"]
        .as_str()
        .expect("CLAUDE_MEM_HOME must be a string");
    assert!(
        claude_mem_home.ends_with(".claude-mem"),
        "CLAUDE_MEM_HOME should end with .claude-mem, got {claude_mem_home}"
    );
    assert!(
        claude_mem_home.starts_with(home.path().to_str().unwrap()),
        "CLAUDE_MEM_HOME must live under temp HOME, got {claude_mem_home}"
    );
}

#[test]
fn cursor_install_preserves_unrelated_mcp_servers() {
    // Users frequently have other MCP servers configured; the installer
    // must never wipe them. This was a regression risk called out in the
    // adversarial review of the Rust port.
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    let path = home.path().join(".cursor/mcp.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"mcpServers":{"other":{"command":"/opt/other","args":["serve"]}}}"#,
    )
    .unwrap();

    install_cursor_only();

    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        cfg["mcpServers"]["other"]["command"], "/opt/other",
        "pre-existing MCP server must survive install"
    );
    assert_eq!(cfg["mcpServers"]["claude-mem-rs"]["command"], BIN);
}

#[test]
fn cursor_install_is_idempotent() {
    // Running the installer twice must produce the same content; second
    // pass must NOT duplicate or corrupt the `claude-mem-rs` entry.
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    install_cursor_only();
    let first = std::fs::read_to_string(home.path().join(".cursor/mcp.json")).unwrap();

    install_cursor_only();
    let second = std::fs::read_to_string(home.path().join(".cursor/mcp.json")).unwrap();

    assert_eq!(first, second, "second install must be a no-op on contents");
}
