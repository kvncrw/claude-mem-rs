//! Fixture parity tests for the opencode integration.
//!
//! opencode is the most complex non-Claude integration: the installer
//! writes BOTH a JSON MCP config and a generated ES module plugin file.
//! Each lifecycle event (context, session-init, observation,
//! summarize/compacting) has to be wired through both layers or the
//! plugin silently no-ops.
//!
//! These tests pin:
//! - Structural shape of `opencode.json` (mcp block + plugin array)
//! - Exact event-name strings in the generated `.mjs` plugin file
//! - That the plugin file references the BIN path and worker env
//!
//! Strings are compared with `.contains()` because the surrounding JS
//! template uses `format!` and is allowed to evolve cosmetically — what
//! must NOT drift are the hook event names and runtime shape.

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

fn install_opencode_only() {
    let report = run_install(InstallOptions {
        ide: Some("opencode".into()),
        yes: true,
        dry_run: false,
        bin_path: Some(BIN.into()),
    })
    .expect("opencode install must succeed");
    assert!(
        report.failed.is_empty(),
        "opencode install reported failures: {:?}",
        report.failed
    );
}

#[test]
fn opencode_json_mcp_block_pins_shape() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    install_opencode_only();

    let path = home.path().join(".config/opencode/opencode.json");
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    let mcp = &cfg["mcp"]["claude-mem"];
    assert!(mcp.is_object(), "mcp.claude-mem must be object");
    assert_eq!(mcp["type"], "local");
    assert_eq!(mcp["enabled"], true);
    assert_eq!(mcp["timeout"], 120_000);

    // command is an array of [bin, "mcp"] — opencode requires array
    // form, not the string form Cursor uses.
    let cmd = mcp["command"]
        .as_array()
        .expect("mcp.claude-mem.command must be array");
    assert_eq!(cmd.len(), 2, "command array must be 2 entries");
    assert_eq!(cmd[0], BIN);
    assert_eq!(cmd[1], "mcp");

    let env_block = &mcp["environment"];
    assert!(env_block.is_object());
    let home_var = env_block["CLAUDE_MEM_HOME"].as_str().unwrap();
    assert!(home_var.ends_with(".claude-mem"));
    assert!(home_var.starts_with(home.path().to_str().unwrap()));
}

#[test]
fn opencode_json_plugin_array_contains_lifecycle_plugin_path() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    install_opencode_only();

    let cfg: Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".config/opencode/opencode.json")).unwrap(),
    )
    .unwrap();
    let plugins = cfg["plugin"]
        .as_array()
        .expect("plugin must be an array (even with single entry)");
    let expected = home
        .path()
        .join(".config/opencode/claude-mem-rs-plugin.mjs")
        .display()
        .to_string();
    assert!(
        plugins
            .iter()
            .any(|p| p == &Value::String(expected.clone())),
        "plugin array must include the lifecycle plugin path, got: {plugins:?}"
    );
}

#[test]
fn opencode_lifecycle_plugin_wires_all_four_events() {
    // Issue #2 spells out the events to cover: context, session-init,
    // observation, summarize/compacting. The plugin uses opencode's
    // hook namespace for each one — pin every string so a renamed event
    // is caught immediately.
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    install_opencode_only();

    let plugin = std::fs::read_to_string(
        home.path()
            .join(".config/opencode/claude-mem-rs-plugin.mjs"),
    )
    .unwrap();

    // The opencode event keys are the contract with opencode itself.
    assert!(
        plugin.contains("\"experimental.chat.system.transform\""),
        "context wiring (experimental.chat.system.transform) missing"
    );
    assert!(
        plugin.contains("\"chat.message\""),
        "session-init wiring (chat.message) missing"
    );
    assert!(
        plugin.contains("\"tool.execute.after\""),
        "observation wiring (tool.execute.after) missing"
    );
    assert!(
        plugin.contains("\"experimental.session.compacting\""),
        "summarize wiring (experimental.session.compacting) missing"
    );

    // The runHook call dispatches each event by string name to the
    // claude-mem binary. Pin each name pair so a typo in either side
    // fails loudly.
    for event in ["context", "session-init", "observation", "summarize"] {
        assert!(
            plugin.contains(&format!("runHook(\"{event}\"")),
            "plugin must dispatch runHook for {event}, got plugin:\n{plugin}"
        );
    }
}

#[test]
fn opencode_lifecycle_plugin_embeds_bin_and_env() {
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    install_opencode_only();

    let plugin = std::fs::read_to_string(
        home.path()
            .join(".config/opencode/claude-mem-rs-plugin.mjs"),
    )
    .unwrap();

    // BIN gets serialized as JSON, so it appears quoted inside the
    // template.
    assert!(
        plugin.contains(&format!("const BIN = \"{BIN}\";")),
        "plugin must hardcode BIN constant, got plugin:\n{plugin}"
    );

    // EXTRA_ENV must include CLAUDE_MEM_HOME so the spawned hook sees
    // the right state directory.
    assert!(
        plugin.contains("const EXTRA_ENV = "),
        "plugin must declare EXTRA_ENV constant"
    );
    assert!(
        plugin.contains("\"CLAUDE_MEM_HOME\""),
        "EXTRA_ENV must include CLAUDE_MEM_HOME, got plugin:\n{plugin}"
    );

    // The runHook helper shells out via spawnSync against
    // [`hook`, `opencode`, event]; pin that pattern so a refactor
    // doesn't silently change the CLI surface.
    assert!(
        plugin.contains("spawnSync(BIN, [\"hook\", \"opencode\", event]"),
        "runHook must shell out with the hook opencode <event> CLI shape"
    );
}

#[test]
fn opencode_install_does_not_duplicate_plugin_entry_on_rerun() {
    // The plugin array is the most likely place for duplicate-append
    // regressions — if the installer simply pushes its path without
    // deduping, re-running install twice silently double-registers.
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    install_opencode_only();
    install_opencode_only();

    let cfg: Value = serde_json::from_str(
        &std::fs::read_to_string(home.path().join(".config/opencode/opencode.json")).unwrap(),
    )
    .unwrap();
    let plugins = cfg["plugin"].as_array().unwrap();
    let expected = home
        .path()
        .join(".config/opencode/claude-mem-rs-plugin.mjs")
        .display()
        .to_string();
    let count = plugins
        .iter()
        .filter(|p| **p == Value::String(expected.clone()))
        .count();
    assert_eq!(
        count, 1,
        "lifecycle plugin path must appear exactly once after repeated installs, got plugins: {plugins:?}"
    );
}

#[test]
fn opencode_install_preserves_unrelated_top_level_config() {
    // Users customize opencode.json (theme, model picker, other MCP
    // servers). The installer must only touch `mcp.claude-mem` and
    // `plugin`, leaving every other key alone.
    let _guard = ENV_LOCK.lock().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    isolate_env(home.path());

    let path = home.path().join(".config/opencode/opencode.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"theme":"dracula","mcp":{"other":{"type":"local","command":["/opt/other"]}}}"#,
    )
    .unwrap();

    install_opencode_only();

    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        cfg["theme"], "dracula",
        "unrelated top-level key must survive"
    );
    assert_eq!(
        cfg["mcp"]["other"]["command"][0], "/opt/other",
        "pre-existing MCP server must survive install"
    );
    assert_eq!(cfg["mcp"]["claude-mem"]["type"], "local");
}
