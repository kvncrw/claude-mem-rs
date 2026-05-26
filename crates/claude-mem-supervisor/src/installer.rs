//! Cross-platform installer UX for the Rust runtime.
//!
//! Unix and macOS get POSIX `sh` launcher scripts; Windows gets `.cmd`
//! batch shims. All path resolution flows through
//! `claude_mem_core::shared::platform_paths` so that `USERPROFILE`,
//! `HOMEDRIVE`+`HOMEPATH`, `HOME`, and the env-var overrides
//! (`CLAUDE_MEM_HOME`, `CLAUDE_MEM_DATA_DIR`, `CLAUDE_CONFIG_DIR`) are
//! resolved identically across the runtime.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const PLUGIN_ID: &str = "claude-mem-rs@kvncrw";
const MARKETPLACE_ID: &str = "kvncrw";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOptions {
    pub ide: Option<String>,
    pub yes: bool,
    pub dry_run: bool,
    pub bin_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallOptions {
    pub yes: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct IdeInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub detected: bool,
    pub supported: bool,
    pub hint: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstallReport {
    pub version: String,
    pub selected_ides: Vec<String>,
    pub dry_run: bool,
    pub actions: Vec<String>,
    pub failed: Vec<String>,
}

pub fn run_install(options: InstallOptions) -> Result<InstallReport> {
    let version = env!("CARGO_PKG_VERSION").to_owned();
    let selected_ides = select_ides(options.ide.as_deref(), options.yes)?;
    let bin_path = options.bin_path.unwrap_or_else(current_binary_path);
    let mut actions = Vec::new();
    let mut failed = Vec::new();

    let cli_path = install_cli_launcher(&bin_path, options.dry_run, &mut actions)?;
    register_claude_plugin(&version, &cli_path, options.dry_run, &mut actions)?;
    for ide in &selected_ides {
        if let Err(error) = install_ide(ide, &cli_path, options.dry_run, &mut actions) {
            failed.push(format!("{ide}: {error}"));
        }
    }
    write_transcript_sample_if_needed(options.dry_run, &mut actions)?;
    if let Err(error) = install_background_services(&cli_path, options.dry_run, &mut actions) {
        failed.push(format!("services: {error}"));
    }

    Ok(InstallReport {
        version,
        selected_ides,
        dry_run: options.dry_run,
        actions,
        failed,
    })
}

pub fn run_uninstall(options: UninstallOptions) -> Result<InstallReport> {
    if !options.yes && stdin_is_tty() && !confirm("Uninstall claude-mem runtime integrations?")? {
        return Err(anyhow!("uninstall cancelled"));
    }
    let mut actions = Vec::new();
    remove_claude_plugin(options.dry_run, &mut actions)?;
    remove_file_if_exists(gemini_settings_path(), options.dry_run, &mut actions)?;
    remove_file_if_exists(cursor_mcp_path(), options.dry_run, &mut actions)?;
    remove_file_if_exists(codex_agents_path(), options.dry_run, &mut actions)?;
    remove_file_if_exists(opencode_plugin_path(), options.dry_run, &mut actions)?;
    remove_background_services(options.dry_run, &mut actions)?;
    Ok(InstallReport {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        selected_ides: Vec::new(),
        dry_run: options.dry_run,
        actions,
        failed: Vec::new(),
    })
}

pub fn detect_ides() -> Vec<IdeInfo> {
    let home = home_dir();
    vec![
        IdeInfo {
            id: "claude-code",
            label: "Claude Code",
            detected: home.join(".claude").exists() || command_exists("claude"),
            supported: true,
            hint: "native hook/plugin files",
        },
        IdeInfo {
            id: "cursor",
            label: "Cursor",
            detected: home.join(".cursor").exists(),
            supported: true,
            hint: "MCP config",
        },
        IdeInfo {
            id: "gemini-cli",
            label: "Gemini CLI",
            detected: home.join(".gemini").exists() || command_exists("gemini"),
            supported: true,
            hint: "settings hook command",
        },
        IdeInfo {
            id: "codex-cli",
            label: "Codex CLI",
            detected: home.join(".codex").exists(),
            supported: true,
            hint: "transcript watcher + AGENTS.md",
        },
        IdeInfo {
            id: "opencode",
            label: "opencode",
            detected: home.join(".config/opencode").exists() || command_exists("opencode"),
            supported: true,
            hint: "MCP config + lifecycle plugin",
        },
    ]
}

pub fn print_install_report(report: &InstallReport) {
    println!("claude-mem-rs {}", report.version);
    println!("selected: {}", report.selected_ides.join(", "));
    if report.dry_run {
        println!("mode: dry-run");
    }
    for action in &report.actions {
        println!("  {action}");
    }
    for failure in &report.failed {
        eprintln!("  failed: {failure}");
    }
}

fn install_ide(ide: &str, bin_path: &Path, dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    match ide {
        "claude-code" => {
            write_claude_hook_manifest(bin_path, dry_run, actions)?;
            write_claude_settings(bin_path, dry_run, actions)?;
            write_claude_state_mcp(bin_path, dry_run, actions)?;
        }
        "cursor" => {
            write_cursor_mcp(bin_path, dry_run, actions)?;
        }
        "gemini-cli" => {
            write_gemini_settings(bin_path, dry_run, actions)?;
        }
        "codex-cli" => {
            write_codex_agents(dry_run, actions)?;
        }
        "opencode" => {
            write_opencode_config(bin_path, dry_run, actions)?;
        }
        other => return Err(anyhow!("unsupported IDE: {other}")),
    }
    Ok(())
}

fn install_cli_launcher(
    bin_path: &Path,
    dry_run: bool,
    actions: &mut Vec<String>,
) -> Result<PathBuf> {
    let cli_path = stable_cli_path(bin_path);
    if cli_path == bin_path {
        actions.push(format!("using CLI binary {}", cli_path.display()));
        return Ok(cli_path);
    }

    if !dry_run {
        if let Some(parent) = cli_path.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::symlink_metadata(&cli_path) {
            Ok(meta) if meta.is_dir() => {
                return Err(anyhow!(
                    "cannot install CLI launcher over directory {}",
                    cli_path.display()
                ));
            }
            Ok(_) => {
                fs::remove_file(&cli_path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect CLI launcher {}", cli_path.display())
                })
            }
        }
        fs::write(&cli_path, plugin_launcher_contents(bin_path))?;
        set_executable(&cli_path)?;
    }
    actions.push(format!(
        "installed compatibility CLI launcher {} -> {}",
        cli_path.display(),
        bin_path.display()
    ));
    Ok(cli_path)
}

fn stable_cli_path(bin_path: &Path) -> PathBuf {
    let file_name = bin_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if matches!(
        file_name,
        "claude-mem" | "claude-mem.exe" | "claude-mem.cmd"
    ) && bin_path.parent().is_some_and(is_stable_cli_dir)
    {
        return bin_path.to_path_buf();
    }

    #[cfg(windows)]
    let launcher = "claude-mem.cmd";
    #[cfg(not(windows))]
    let launcher = "claude-mem";

    home_dir().join(".local").join("bin").join(launcher)
}

fn is_stable_cli_dir(path: &Path) -> bool {
    path == Path::new("/usr/local/bin")
        || path == Path::new("/usr/bin")
        || path.ends_with(Path::new(".local/bin"))
}

fn select_ides(requested: Option<&str>, yes: bool) -> Result<Vec<String>> {
    let available = detect_ides();
    if let Some(requested) = requested {
        let ids = requested
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        for id in &ids {
            if !available.iter().any(|ide| ide.id == id && ide.supported) {
                return Err(anyhow!("unsupported or unknown IDE: {id}"));
            }
        }
        return Ok(ids);
    }

    let detected = available
        .iter()
        .filter(|ide| ide.detected && ide.supported)
        .map(|ide| ide.id.to_owned())
        .collect::<Vec<_>>();
    if yes || !stdin_is_tty() {
        return Ok(if detected.is_empty() {
            vec!["claude-code".to_owned()]
        } else {
            detected
        });
    }

    println!("Detected integrations:");
    for (idx, ide) in available.iter().enumerate() {
        let marker = if ide.detected { "*" } else { " " };
        println!("  {}. [{}] {} - {}", idx + 1, marker, ide.label, ide.hint);
    }
    print!("Install for comma-separated numbers [detected]: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let selected = line.trim();
    if selected.is_empty() {
        return Ok(if detected.is_empty() {
            vec!["claude-code".to_owned()]
        } else {
            detected
        });
    }
    let mut ids = Vec::new();
    for token in selected.split(',').map(str::trim) {
        let index = token
            .parse::<usize>()
            .context("selection must be numbers")?;
        let ide = available
            .get(index.saturating_sub(1))
            .ok_or_else(|| anyhow!("selection out of range: {token}"))?;
        ids.push(ide.id.to_owned());
    }
    Ok(ids)
}

fn register_claude_plugin(
    version: &str,
    bin_path: &Path,
    dry_run: bool,
    actions: &mut Vec<String>,
) -> Result<()> {
    let plugin_dir = claude_marketplace_dir().join("plugin");
    let plugin_json = plugin_dir.join(".claude-plugin").join("plugin.json");
    let scripts_dir = plugin_dir.join("scripts");
    let launcher = scripts_dir.join(plugin_launcher_filename());
    if !dry_run {
        fs::create_dir_all(plugin_json.parent().unwrap())?;
        fs::create_dir_all(&scripts_dir)?;
        write_json_atomic(
            &plugin_json,
            &json!({
                "name": "claude-mem-rs",
                "version": version,
                "description": "Rust claude-mem runtime",
                "author": "kvncrw"
            }),
        )?;
        fs::write(&launcher, plugin_launcher_contents(bin_path))?;
        set_executable(&launcher)?;
        register_marketplace(version)?;
        register_installed_plugin(version, &plugin_dir)?;
        enable_claude_settings()?;
    }
    actions.push(format!(
        "registered Claude plugin at {}",
        plugin_dir.display()
    ));
    Ok(())
}

fn write_claude_hook_manifest(
    bin_path: &Path,
    dry_run: bool,
    actions: &mut Vec<String>,
) -> Result<()> {
    let path = claude_marketplace_dir()
        .join("plugin")
        .join("hooks")
        .join("hooks.json");
    let command = format!("\"{}\" hook claude-code", bin_path.display());
    let manifest = json!({
        "description": "claude-mem-rs memory system hooks",
        "hooks": {
            "SessionStart": [{
                "matcher": "startup|clear|compact",
                "hooks": [{"type": "command", "command": format!("{command} context"), "timeout": 60}]
            }],
            "UserPromptSubmit": [{
                "hooks": [{"type": "command", "command": format!("{command} session-init"), "timeout": 60}]
            }],
            "PostToolUse": [{
                "matcher": "*",
                "hooks": [{"type": "command", "command": format!("{command} observation"), "timeout": 120}]
            }],
            "Stop": [{
                "hooks": [{"type": "command", "command": format!("{command} summarize"), "timeout": 120}]
            }],
            "SessionEnd": [{
                "hooks": [{"type": "command", "command": format!("{command} session-complete"), "timeout": 30}]
            }]
        }
    });
    if !dry_run {
        write_json_atomic(&path, &manifest)?;
    }
    actions.push(format!("wrote Claude hook manifest {}", path.display()));
    Ok(())
}

fn write_claude_settings(bin_path: &Path, dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    let path = claude_config_dir().join("settings.json");
    let mut settings = read_json_or_empty(&path);
    settings["env"]["CLAUDE_MEM_HOME"] = json!(claude_mem_home_string());
    settings["env"]["CLAUDE_MEM_WORKER_URL"] = json!(std::env::var("CLAUDE_MEM_WORKER_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:37777".to_owned()));

    settings["enabledPlugins"][PLUGIN_ID] = json!(true);
    settings["enabledPlugins"]["claude-mem@thedotmack"] = json!(false);
    settings["enabledPlugins"]["claude-mem@kvncrw"] = json!(false);

    if let Some(servers) = settings
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
    {
        servers.remove("claude-mem-rs");
    }
    settings["mcpServers"]["mcp-search"] = json!({
        "command": bin_path.display().to_string(),
        "args": ["mcp"],
        "env": worker_env_json()
    });

    let command = format!("\"{}\" hook claude-code", bin_path.display());
    upsert_claude_hook(
        &mut settings,
        "SessionStart",
        Some("startup|clear|compact"),
        &format!("{command} context"),
        60,
    );
    upsert_claude_hook(
        &mut settings,
        "UserPromptSubmit",
        None,
        &format!("{command} session-init"),
        60,
    );
    upsert_claude_hook(
        &mut settings,
        "PostToolUse",
        Some("*"),
        &format!("{command} observation"),
        120,
    );
    upsert_claude_hook(
        &mut settings,
        "Stop",
        None,
        &format!("{command} summarize"),
        120,
    );
    upsert_claude_hook(
        &mut settings,
        "SessionEnd",
        None,
        &format!("{command} session-complete"),
        30,
    );

    if !dry_run {
        write_json_atomic(&path, &settings)?;
    }
    actions.push(format!("configured Claude settings {}", path.display()));
    Ok(())
}

fn write_claude_state_mcp(bin_path: &Path, dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    let path = home_dir().join(".claude.json");
    let mut state = read_json_or_empty(&path);
    if let Some(servers) = state.get_mut("mcpServers").and_then(Value::as_object_mut) {
        servers.remove("claude-mem-rs");
    }
    state["mcpServers"]["mcp-search"] = json!({
        "command": bin_path.display().to_string(),
        "args": ["mcp"],
        "env": worker_env_json()
    });
    if !dry_run {
        write_json_atomic(&path, &state)?;
    }
    actions.push(format!("configured Claude MCP state {}", path.display()));
    Ok(())
}

fn upsert_claude_hook(
    settings: &mut Value,
    event: &str,
    matcher: Option<&str>,
    command: &str,
    timeout: u64,
) {
    let Some(hooks) = settings["hooks"][event].as_array_mut() else {
        settings["hooks"][event] = json!([]);
        return upsert_claude_hook(settings, event, matcher, command, timeout);
    };
    hooks.retain(|entry| !is_managed_claude_mem_hook(entry));
    let mut entry = json!({
        "hooks": [{"type": "command", "command": command, "timeout": timeout}]
    });
    if let Some(matcher) = matcher {
        entry["matcher"] = json!(matcher);
    }
    hooks.push(entry);
}

fn is_managed_claude_mem_hook(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|hook| {
            hook.get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| {
                    command.contains("claude-mem-rs") && command.contains(" hook claude-code ")
                })
        })
}

fn write_cursor_mcp(bin_path: &Path, dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    let path = cursor_mcp_path();
    let mut config = read_json_or_empty(&path);
    config["mcpServers"]["claude-mem-rs"] = json!({
        "command": bin_path.display().to_string(),
        "args": ["mcp"],
        "env": worker_env_json()
    });
    if !dry_run {
        write_json_atomic(&path, &config)?;
    }
    actions.push(format!("configured Cursor MCP {}", path.display()));
    Ok(())
}

fn write_gemini_settings(bin_path: &Path, dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    let path = gemini_settings_path();
    let mut config = read_json_or_empty(&path);
    let command = |event: &str| format!("\"{}\" hook gemini-cli {event}", bin_path.display());
    if let Some(hooks) = config.get_mut("hooks").and_then(Value::as_object_mut) {
        hooks.remove("Stop");
    }
    config["hooks"]["SessionStart"] = json!([{
        "matcher": "startup|resume|clear",
        "hooks": [{
            "name": "claude-mem-rs-context",
            "type": "command",
            "command": command("context"),
            "timeout": 60000
        }]
    }]);
    config["hooks"]["BeforeAgent"] = json!([{
        "matcher": "*",
        "hooks": [{
            "name": "claude-mem-rs-session-init",
            "type": "command",
            "command": command("session-init"),
            "timeout": 60000
        }]
    }]);
    config["hooks"]["AfterTool"] = json!([{
        "matcher": "*",
        "hooks": [{
            "name": "claude-mem-rs-observation",
            "type": "command",
            "command": command("observation"),
            "timeout": 120000
        }]
    }]);
    config["hooks"]["AfterAgent"] = json!([{
        "matcher": "*",
        "hooks": [{
            "name": "claude-mem-rs-summarize",
            "type": "command",
            "command": command("summarize"),
            "timeout": 120000
        }]
    }]);
    config["hooks"]["SessionEnd"] = json!([{
        "matcher": "*",
        "hooks": [{
            "name": "claude-mem-rs-complete",
            "type": "command",
            "command": command("session-complete"),
            "timeout": 30000
        }]
    }]);
    if !dry_run {
        write_json_atomic(&path, &config)?;
    }
    actions.push(format!("configured Gemini hooks {}", path.display()));
    Ok(())
}

fn write_codex_agents(dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    let path = codex_agents_path();
    let content = "<claude-mem-context>\nRun `claude-mem transcript watch` to keep Codex transcript memory synchronized.\n</claude-mem-context>\n";
    if !dry_run {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, content)?;
    }
    actions.push(format!("wrote Codex AGENTS hint {}", path.display()));
    Ok(())
}

fn write_opencode_config(bin_path: &Path, dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    let config_path = opencode_config_path();
    let plugin_path = opencode_plugin_path();
    let mut config = read_json_or_empty(&config_path);
    config["mcp"]["claude-mem"] = json!({
        "type": "local",
        "command": [bin_path.display().to_string(), "mcp"],
        "environment": worker_env_json(),
        "enabled": true,
        "timeout": 120000
    });

    let plugin_entry = Value::String(plugin_path.display().to_string());
    let mut plugins = config
        .get("plugin")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    plugins.retain(|entry| entry != &plugin_entry);
    plugins.push(plugin_entry);
    config["plugin"] = Value::Array(plugins);

    if !dry_run {
        write_opencode_plugin(bin_path, &plugin_path)?;
        write_json_atomic(&config_path, &config)?;
    }
    actions.push(format!(
        "configured opencode MCP/plugin {}",
        config_path.display()
    ));
    Ok(())
}

fn write_opencode_plugin(bin_path: &Path, plugin_path: &Path) -> Result<()> {
    if let Some(parent) = plugin_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bin = serde_json::to_string(&bin_path.display().to_string())?;
    let env = serde_json::to_string(&worker_env_json())?;
    fs::write(
        plugin_path,
        format!(
            r#"import {{ spawnSync }} from "node:child_process";

const BIN = {bin};
const EXTRA_ENV = {env};

function textFromParts(parts) {{
  if (!Array.isArray(parts)) return "";
  return parts.map((part) => {{
    if (typeof part?.text === "string") return part.text;
    if (typeof part?.content === "string") return part.content;
    return "";
  }}).filter(Boolean).join("\n\n").trim();
}}

function runHook(event, payload) {{
  const result = spawnSync(BIN, ["hook", "opencode", event], {{
    input: JSON.stringify(payload || {{}}),
    encoding: "utf8",
    timeout: 120000,
    env: {{ ...process.env, ...EXTRA_ENV }},
  }});
  if (result.error || result.status !== 0 || !result.stdout) return null;
  try {{
    return JSON.parse(result.stdout);
  }} catch {{
    return null;
  }}
}}

export const ClaudeMemRsPlugin = async (ctx) => ({{
  "experimental.chat.system.transform": async (input, output) => {{
    const response = runHook("context", {{
      session_id: input.sessionID,
      cwd: ctx.directory,
    }});
    const context = response?.systemMessage || response?.hookSpecificOutput?.additionalContext;
    if (context) output.system.push(context);
  }},
  "chat.message": async (input, output) => {{
    runHook("session-init", {{
      session_id: input.sessionID,
      cwd: ctx.directory,
      prompt: textFromParts(output.parts) || output.message?.content || "",
    }});
  }},
  "tool.execute.after": async (input, output) => {{
    runHook("observation", {{
      session_id: input.sessionID,
      cwd: ctx.directory,
      tool_name: input.tool,
      tool_input: input.args,
      tool_response: {{
        title: output.title,
        output: output.output,
        metadata: output.metadata,
      }},
    }});
  }},
  "experimental.session.compacting": async (input, output) => {{
    runHook("summarize", {{
      session_id: input.sessionID,
      cwd: ctx.directory,
      prompt: [output.prompt, ...(output.context || [])].filter(Boolean).join("\n\n"),
    }});
  }},
}});

export default ClaudeMemRsPlugin;
"#
        ),
    )?;
    Ok(())
}

fn write_transcript_sample_if_needed(dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    let path = transcript_config_path();
    if path.exists() {
        return Ok(());
    }
    if !dry_run {
        crate::transcripts::config::write_sample_config(&path)?;
    }
    actions.push(format!(
        "created transcript watcher config {}",
        path.display()
    ));
    Ok(())
}

fn install_background_services(
    cli_path: &Path,
    dry_run: bool,
    actions: &mut Vec<String>,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        return install_systemd_user_services(cli_path, dry_run, actions);
    }
    #[cfg(target_os = "macos")]
    {
        return install_launch_agents(cli_path, dry_run, actions);
    }
    #[cfg(windows)]
    {
        return install_windows_tasks(cli_path, dry_run, actions);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (cli_path, dry_run);
        actions.push("background services are not managed on this platform".to_owned());
        Ok(())
    }
}

fn remove_background_services(dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        return remove_systemd_user_services(dry_run, actions);
    }
    #[cfg(target_os = "macos")]
    {
        return remove_launch_agents(dry_run, actions);
    }
    #[cfg(windows)]
    {
        return remove_windows_tasks(dry_run, actions);
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = dry_run;
        actions.push("background services are not managed on this platform".to_owned());
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn install_systemd_user_services(
    cli_path: &Path,
    dry_run: bool,
    actions: &mut Vec<String>,
) -> Result<()> {
    let dir = systemd_user_dir();
    let worker_path = dir.join("claude-mem-worker.service");
    let watch_path = dir.join("claude-mem-transcript-watch.service");
    if !dry_run {
        fs::create_dir_all(&dir)?;
        fs::write(&worker_path, systemd_worker_service(cli_path))?;
        fs::write(&watch_path, systemd_transcript_watch_service(cli_path))?;
        if should_activate_services() && systemd_user_dir_override().is_none() {
            run_command(
                Command::new("systemctl").args(["--user", "daemon-reload"]),
                "reload systemd user units",
            )?;
            run_command(
                Command::new("systemctl").args([
                    "--user",
                    "enable",
                    "--now",
                    "claude-mem-worker.service",
                    "claude-mem-transcript-watch.service",
                ]),
                "enable claude-mem user services",
            )?;
        }
    }
    actions.push(format!(
        "installed Linux user services {}, {}",
        worker_path.display(),
        watch_path.display()
    ));
    Ok(())
}

#[cfg(target_os = "linux")]
fn remove_systemd_user_services(dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    if !dry_run && should_activate_services() && systemd_user_dir_override().is_none() {
        let _ = Command::new("systemctl")
            .args([
                "--user",
                "disable",
                "--now",
                "claude-mem-worker.service",
                "claude-mem-transcript-watch.service",
            ])
            .status();
    }
    remove_file_if_exists(
        systemd_user_dir().join("claude-mem-worker.service"),
        dry_run,
        actions,
    )?;
    remove_file_if_exists(
        systemd_user_dir().join("claude-mem-transcript-watch.service"),
        dry_run,
        actions,
    )?;
    if !dry_run && should_activate_services() && systemd_user_dir_override().is_none() {
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn systemd_user_dir() -> PathBuf {
    systemd_user_dir_override()
        .unwrap_or_else(|| home_dir().join(".config").join("systemd").join("user"))
}

#[cfg(target_os = "linux")]
fn systemd_user_dir_override() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_MEM_SYSTEMD_USER_DIR").map(PathBuf::from)
}

#[cfg(target_os = "linux")]
fn systemd_worker_service(cli_path: &Path) -> String {
    format!(
        "[Unit]\n\
Description=Claude-mem persistent memory worker\n\
After=network.target\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart={} worker --daemon\n\
WorkingDirectory={}\n\
Environment=CLAUDE_MEM_HOME={}\n\
Environment=CLAUDE_MEM_WORKER_PORT=37777\n\
Environment=CLAUDE_MEM_WORKER_URL=http://127.0.0.1:37777\n\
Restart=on-failure\n\
RestartSec=10\n\
RestartSteps=5\n\
RestartMaxDelaySec=300\n\
Nice=10\n\
MemoryMax=1G\n\
\n\
[Install]\n\
WantedBy=default.target\n",
        systemd_exec_path(cli_path),
        systemd_exec_path(&home_dir()),
        claude_mem_home_string()
    )
}

#[cfg(target_os = "linux")]
fn systemd_transcript_watch_service(cli_path: &Path) -> String {
    format!(
        "[Unit]\n\
Description=Claude-mem Codex transcript watcher\n\
After=claude-mem-worker.service\n\
Wants=claude-mem-worker.service\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart={} transcript watch\n\
WorkingDirectory={}\n\
Environment=CLAUDE_MEM_HOME={}\n\
Environment=CLAUDE_MEM_WORKER_URL=http://127.0.0.1:37777\n\
Restart=on-failure\n\
RestartSec=10\n\
Nice=10\n\
MemoryMax=512M\n\
\n\
[Install]\n\
WantedBy=default.target\n",
        systemd_exec_path(cli_path),
        systemd_exec_path(&home_dir()),
        claude_mem_home_string()
    )
}

#[cfg(target_os = "linux")]
fn systemd_exec_path(path: &Path) -> String {
    let value = path.display().to_string();
    if value.contains(char::is_whitespace) {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        value
    }
}

#[cfg(target_os = "macos")]
fn install_launch_agents(cli_path: &Path, dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    let dir = launch_agents_dir();
    let worker_path = dir.join("ai.claude-mem.worker.plist");
    let watch_path = dir.join("ai.claude-mem.transcript-watch.plist");
    if !dry_run {
        fs::create_dir_all(&dir)?;
        fs::write(
            &worker_path,
            launch_agent_plist("ai.claude-mem.worker", cli_path, &["worker", "--daemon"]),
        )?;
        fs::write(
            &watch_path,
            launch_agent_plist(
                "ai.claude-mem.transcript-watch",
                cli_path,
                &["transcript", "watch"],
            ),
        )?;
        if should_activate_services() && launch_agents_dir_override().is_none() {
            bootstrap_launch_agent(&worker_path, "ai.claude-mem.worker")?;
            bootstrap_launch_agent(&watch_path, "ai.claude-mem.transcript-watch")?;
        }
    }
    actions.push(format!(
        "installed macOS LaunchAgents {}, {}",
        worker_path.display(),
        watch_path.display()
    ));
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_launch_agents(dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    if !dry_run && should_activate_services() && launch_agents_dir_override().is_none() {
        let domain = launchctl_domain();
        let _ = Command::new("launchctl")
            .args(["bootout", &domain, "ai.claude-mem.worker"])
            .status();
        let _ = Command::new("launchctl")
            .args(["bootout", &domain, "ai.claude-mem.transcript-watch"])
            .status();
    }
    remove_file_if_exists(
        launch_agents_dir().join("ai.claude-mem.worker.plist"),
        dry_run,
        actions,
    )?;
    remove_file_if_exists(
        launch_agents_dir().join("ai.claude-mem.transcript-watch.plist"),
        dry_run,
        actions,
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn launch_agents_dir() -> PathBuf {
    launch_agents_dir_override().unwrap_or_else(|| home_dir().join("Library").join("LaunchAgents"))
}

#[cfg(target_os = "macos")]
fn launch_agents_dir_override() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_MEM_LAUNCH_AGENTS_DIR").map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn bootstrap_launch_agent(path: &Path, label: &str) -> Result<()> {
    let domain = launchctl_domain();
    let path = path.display().to_string();
    let service = format!("{domain}/{label}");
    let _ = Command::new("launchctl")
        .args(["bootout", &domain, label])
        .status();
    run_command(
        Command::new("launchctl")
            .arg("bootstrap")
            .arg(&domain)
            .arg(&path),
        "bootstrap claude-mem LaunchAgent",
    )?;
    run_command(
        Command::new("launchctl").arg("enable").arg(&service),
        "enable claude-mem LaunchAgent",
    )?;
    run_command(
        Command::new("launchctl")
            .arg("kickstart")
            .arg("-k")
            .arg(&service),
        "start claude-mem LaunchAgent",
    )
}

#[cfg(target_os = "macos")]
fn launchctl_domain() -> String {
    format!("gui/{}", current_uid())
}

#[cfg(target_os = "macos")]
fn launch_agent_plist(label: &str, cli_path: &Path, args: &[&str]) -> String {
    let args_xml = std::iter::once(cli_path.display().to_string())
        .chain(args.iter().map(|arg| (*arg).to_owned()))
        .map(|arg| format!("        <string>{}</string>\n", xml_escape(&arg)))
        .collect::<String>();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n\
<dict>\n\
    <key>Label</key>\n\
    <string>{}</string>\n\
    <key>ProgramArguments</key>\n\
    <array>\n{}    </array>\n\
    <key>EnvironmentVariables</key>\n\
    <dict>\n\
        <key>CLAUDE_MEM_HOME</key>\n\
        <string>{}</string>\n\
        <key>CLAUDE_MEM_WORKER_URL</key>\n\
        <string>http://127.0.0.1:37777</string>\n\
    </dict>\n\
    <key>RunAtLoad</key>\n\
    <true/>\n\
    <key>KeepAlive</key>\n\
    <true/>\n\
    <key>StandardOutPath</key>\n\
    <string>{}</string>\n\
    <key>StandardErrorPath</key>\n\
    <string>{}</string>\n\
</dict>\n\
</plist>\n",
        xml_escape(label),
        args_xml,
        xml_escape(&claude_mem_home_string()),
        xml_escape(&claude_mem_home_string_with_file("logs/launchd.out.log")),
        xml_escape(&claude_mem_home_string_with_file("logs/launchd.err.log"))
    )
}

#[cfg(windows)]
fn install_windows_tasks(cli_path: &Path, dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    let dir = windows_tasks_dir();
    let worker_script = dir.join("claude-mem-worker.cmd");
    let watch_script = dir.join("claude-mem-transcript-watch.cmd");
    if !dry_run {
        fs::create_dir_all(&dir)?;
        fs::write(
            &worker_script,
            windows_task_script(cli_path, &["worker", "--daemon"]),
        )?;
        fs::write(
            &watch_script,
            windows_task_script(cli_path, &["transcript", "watch"]),
        )?;
        if should_activate_services() && windows_tasks_dir_override().is_none() {
            create_windows_task("claude-mem-worker", &worker_script)?;
            create_windows_task("claude-mem-transcript-watch", &watch_script)?;
        }
    }
    actions.push(format!(
        "installed Windows scheduled task scripts {}, {}",
        worker_script.display(),
        watch_script.display()
    ));
    Ok(())
}

#[cfg(windows)]
fn remove_windows_tasks(dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    if !dry_run && should_activate_services() && windows_tasks_dir_override().is_none() {
        let _ = Command::new("schtasks.exe")
            .args(["/Delete", "/TN", "claude-mem-worker", "/F"])
            .status();
        let _ = Command::new("schtasks.exe")
            .args(["/Delete", "/TN", "claude-mem-transcript-watch", "/F"])
            .status();
    }
    remove_file_if_exists(
        windows_tasks_dir().join("claude-mem-worker.cmd"),
        dry_run,
        actions,
    )?;
    remove_file_if_exists(
        windows_tasks_dir().join("claude-mem-transcript-watch.cmd"),
        dry_run,
        actions,
    )?;
    Ok(())
}

#[cfg(windows)]
fn windows_tasks_dir() -> PathBuf {
    windows_tasks_dir_override().unwrap_or_else(|| {
        home_dir()
            .join("AppData")
            .join("Roaming")
            .join("claude-mem")
    })
}

#[cfg(windows)]
fn windows_tasks_dir_override() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_MEM_WINDOWS_TASKS_DIR").map(PathBuf::from)
}

#[cfg(windows)]
fn create_windows_task(name: &str, script: &Path) -> Result<()> {
    let command = format!("\"{}\"", script.display());
    run_command(
        Command::new("schtasks.exe")
            .arg("/Create")
            .arg("/TN")
            .arg(name)
            .arg("/SC")
            .arg("ONLOGON")
            .arg("/TR")
            .arg(&command)
            .arg("/F"),
        "create claude-mem scheduled task",
    )?;
    let _ = Command::new("schtasks.exe")
        .args(["/Run", "/TN", name])
        .status();
    Ok(())
}

#[cfg(windows)]
fn windows_task_script(cli_path: &Path, args: &[&str]) -> String {
    let args = args.join(" ");
    format!(
        "@echo off\r\nset CLAUDE_MEM_HOME={}\r\nset CLAUDE_MEM_WORKER_URL=http://127.0.0.1:37777\r\n\"{}\" {}\r\n",
        claude_mem_home_string(),
        cli_path.display(),
        args
    )
}

fn should_activate_services() -> bool {
    !matches!(
        std::env::var("CLAUDE_MEM_INSTALL_SERVICES").ok().as_deref(),
        Some("0" | "false" | "False" | "no" | "NO")
    )
}

fn run_command(command: &mut Command, label: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to {label}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("{label} exited with status {status}"))
    }
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "macos")]
fn claude_mem_home_string_with_file(relative: &str) -> String {
    claude_mem_core::shared::platform_paths::claude_mem_home()
        .join(relative)
        .display()
        .to_string()
}

#[cfg(target_os = "macos")]
fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

fn remove_claude_plugin(dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    let marketplace = claude_marketplace_dir();
    if !dry_run {
        let _ = fs::remove_dir_all(&marketplace);
        remove_plugin_registration()?;
    }
    actions.push(format!("removed Claude plugin {}", marketplace.display()));
    Ok(())
}

fn remove_file_if_exists(path: PathBuf, dry_run: bool, actions: &mut Vec<String>) -> Result<()> {
    if path.exists() {
        if !dry_run {
            fs::remove_file(&path)?;
        }
        actions.push(format!("removed {}", path.display()));
    }
    Ok(())
}

fn register_marketplace(version: &str) -> Result<()> {
    let path = claude_plugins_dir().join("known_marketplaces.json");
    let mut value = read_json_or_empty(&path);
    value[MARKETPLACE_ID] = json!({
        "source": {"source": "github", "repo": "kvncrw/claude-mem-rs"},
        "installLocation": claude_marketplace_dir(),
        "lastUpdated": now_string(),
        "autoUpdate": true,
        "version": version
    });
    write_json_atomic(&path, &value)
}

fn register_installed_plugin(version: &str, plugin_dir: &Path) -> Result<()> {
    let path = claude_plugins_dir().join("installed_plugins.json");
    let mut value = read_json_or_empty(&path);
    value["version"] = json!(2);
    value["plugins"][PLUGIN_ID] = json!([{
        "scope": "user",
        "installPath": plugin_dir,
        "version": version,
        "installedAt": now_string(),
        "lastUpdated": now_string(),
        "gitCommitSha": std::env::var("CLAUDE_MEM_GIT_COMMIT")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "local".to_owned())
    }]);
    write_json_atomic(&path, &value)
}

fn enable_claude_settings() -> Result<()> {
    let path = claude_config_dir().join("settings.json");
    let mut settings = read_json_or_empty(&path);
    settings["enabledPlugins"][PLUGIN_ID] = json!(true);
    write_json_atomic(&path, &settings)
}

fn remove_plugin_registration() -> Result<()> {
    for path in [
        claude_plugins_dir().join("known_marketplaces.json"),
        claude_plugins_dir().join("installed_plugins.json"),
        claude_config_dir().join("settings.json"),
    ] {
        if !path.exists() {
            continue;
        }
        let mut value = read_json_or_empty(&path);
        remove_keys_recursive(&mut value, &[PLUGIN_ID, MARKETPLACE_ID]);
        write_json_atomic(&path, &value)?;
    }
    Ok(())
}

fn remove_keys_recursive(value: &mut Value, keys: &[&str]) {
    match value {
        Value::Object(map) => {
            for key in keys {
                map.remove(*key);
            }
            for nested in map.values_mut() {
                remove_keys_recursive(nested, keys);
            }
        }
        Value::Array(items) => {
            for item in items {
                remove_keys_recursive(item, keys);
            }
        }
        _ => {}
    }
}

fn read_json_or_empty(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| Value::Object(Default::default()))
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    // `fs::rename` on Windows does NOT reliably replace an existing
    // destination — second-run installs (or upgrades) would otherwise
    // fail mid-rewrite of `settings.json` / `mcp.json` and leave
    // integrations partially configured. Remove first; the brief race
    // between `remove_file` and `rename` matches the TS installer's
    // behaviour and is acceptable for a single-user install path.
    #[cfg(windows)]
    {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
    fs::rename(tmp, path)?;
    Ok(())
}

fn worker_env_json() -> Value {
    let mut env = BTreeMap::new();
    env.insert("CLAUDE_MEM_HOME", claude_mem_home_string());
    if let Ok(value) = std::env::var("CLAUDE_MEM_DATA_DIR") {
        env.insert("CLAUDE_MEM_DATA_DIR", value);
    }
    if let Ok(value) = std::env::var("CLAUDE_MEM_WORKER_URL") {
        env.insert("CLAUDE_MEM_WORKER_URL", value);
    }
    json!(env)
}

/// Returns the canonical `CLAUDE_MEM_HOME` directory as a display string,
/// routing through `platform_paths` so Windows resolves `USERPROFILE`
/// (or `HOMEDRIVE`+`HOMEPATH`) the same way as the rest of the runtime.
fn claude_mem_home_string() -> String {
    claude_mem_core::shared::platform_paths::claude_mem_home()
        .display()
        .to_string()
}

/// Filename of the plugin launcher that Claude Code spawns. POSIX uses a
/// bare `sh` script; Windows uses a `.cmd` batch shim so the shell can
/// invoke it directly.
fn plugin_launcher_filename() -> &'static str {
    #[cfg(windows)]
    {
        "claude-mem.cmd"
    }
    #[cfg(not(windows))]
    {
        "claude-mem"
    }
}

/// Body of the plugin launcher. Matches the convention used by
/// `plugin/scripts/smart-install.js` in the TypeScript v12 plugin:
/// emit a POSIX `sh` exec on Unix and a `@echo off` cmd shim on Windows.
fn plugin_launcher_contents(bin_path: &Path) -> String {
    #[cfg(windows)]
    {
        // CRLF line endings keep `cmd.exe` happy across older Windows
        // hosts, and `%*` forwards every argument verbatim including
        // quoted strings.
        format!("@echo off\r\n\"{}\" %*\r\n", bin_path.display())
    }
    #[cfg(not(windows))]
    {
        format!(
            "#!/usr/bin/env sh\nexec \"{}\" \"$@\"\n",
            bin_path.display()
        )
    }
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "YES"))
}

fn stdin_is_tty() -> bool {
    #[cfg(unix)]
    unsafe {
        libc::isatty(libc::STDIN_FILENO) == 1
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn command_exists(command: &str) -> bool {
    #[cfg(windows)]
    {
        // `where` is the Windows equivalent of `command -v` for locating
        // executables on PATH. Returns 0 on success, 1 when not found.
        Command::new("where")
            .arg(command)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {command} >/dev/null 2>&1"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

fn current_binary_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("claude-mem"))
}

#[cfg_attr(windows, allow(unused_variables))]
fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

fn now_string() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "now".to_owned())
}

fn home_dir() -> PathBuf {
    claude_mem_core::shared::platform_paths::home_dir()
}

fn claude_config_dir() -> PathBuf {
    claude_mem_core::shared::platform_paths::claude_config_dir()
}

fn claude_plugins_dir() -> PathBuf {
    claude_config_dir().join("plugins")
}

fn claude_marketplace_dir() -> PathBuf {
    claude_plugins_dir()
        .join("marketplaces")
        .join(MARKETPLACE_ID)
}

fn cursor_mcp_path() -> PathBuf {
    std::env::var_os("CURSOR_MCP_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".cursor").join("mcp.json"))
}

fn gemini_settings_path() -> PathBuf {
    std::env::var_os("GEMINI_SETTINGS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".gemini").join("settings.json"))
}

fn codex_agents_path() -> PathBuf {
    std::env::var_os("CODEX_AGENTS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".codex").join("AGENTS.md"))
}

fn opencode_config_path() -> PathBuf {
    std::env::var_os("OPENCODE_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            home_dir()
                .join(".config")
                .join("opencode")
                .join("opencode.json")
        })
}

fn opencode_plugin_path() -> PathBuf {
    std::env::var_os("OPENCODE_PLUGIN_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            home_dir()
                .join(".config")
                .join("opencode")
                .join("claude-mem-rs-plugin.mjs")
        })
}

fn transcript_config_path() -> PathBuf {
    std::env::var_os("CLAUDE_MEM_TRANSCRIPTS_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(claude_mem_core::shared::platform_paths::transcript_config_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unix_launcher_emits_posix_shebang() {
        let bin = PathBuf::from("/usr/local/bin/claude-mem");
        let contents = plugin_launcher_contents(&bin);
        assert!(
            contents.starts_with("#!/usr/bin/env sh\n"),
            "unix launcher must start with sh shebang, got: {contents}"
        );
        assert!(contents.contains("exec \"/usr/local/bin/claude-mem\" \"$@\""));
        assert_eq!(plugin_launcher_filename(), "claude-mem");
    }

    #[cfg(unix)]
    #[test]
    fn stable_cli_path_keeps_claude_mem_name_when_already_canonical() {
        let bin = PathBuf::from("/usr/local/bin/claude-mem");
        assert_eq!(stable_cli_path(&bin), bin);
    }

    #[cfg(unix)]
    #[test]
    fn stable_cli_path_wraps_target_debug_binary() {
        let bin = PathBuf::from("/repo/target/debug/claude-mem");
        let stable = stable_cli_path(&bin);
        assert!(
            stable.ends_with(Path::new(".local/bin/claude-mem")),
            "expected stable user launcher, got {}",
            stable.display()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_service_units_run_worker_and_transcript_watcher() {
        let bin = PathBuf::from("/usr/local/bin/claude-mem");
        let worker = systemd_worker_service(&bin);
        assert!(worker.contains("ExecStart=/usr/local/bin/claude-mem worker --daemon"));
        assert!(worker.contains("Environment=CLAUDE_MEM_WORKER_URL=http://127.0.0.1:37777"));

        let watcher = systemd_transcript_watch_service(&bin);
        assert!(watcher.contains("After=claude-mem-worker.service"));
        assert!(watcher.contains("ExecStart=/usr/local/bin/claude-mem transcript watch"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_launcher_emits_cmd_batch() {
        let bin = PathBuf::from(r"C:\Users\Me\.cargo\bin\claude-mem.exe");
        let contents = plugin_launcher_contents(&bin);
        // `cmd.exe` requires `@echo off` to suppress command echoing and
        // CRLF line endings for legacy compatibility.
        assert!(
            contents.starts_with("@echo off\r\n"),
            "windows launcher must start with @echo off + CRLF, got: {contents:?}"
        );
        // `%*` forwards every argument including quoted strings verbatim.
        assert!(
            contents.contains(r#""C:\Users\Me\.cargo\bin\claude-mem.exe" %*"#),
            "expected forwarded invocation, got: {contents:?}"
        );
        assert!(
            contents.ends_with("\r\n"),
            "windows launcher must end with CRLF, got: {contents:?}"
        );
        assert_eq!(plugin_launcher_filename(), "claude-mem.cmd");
    }

    #[cfg(windows)]
    #[test]
    fn windows_home_resolves_via_userprofile() {
        // platform_paths::home_dir() must round-trip USERPROFILE; if a
        // future refactor breaks that contract we want a Windows runner
        // to scream.
        let env_home =
            std::env::var_os("USERPROFILE").expect("USERPROFILE must be set on Windows test hosts");
        let resolved = home_dir();
        assert_eq!(resolved, PathBuf::from(env_home));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_use_backslash_separators() {
        // .join() on Windows produces backslashes; the explicit
        // multi-segment joins for cursor/gemini/codex/opencode must not
        // accidentally embed forward slashes that would confuse the
        // Windows shell when expanded.
        let cursor = cursor_mcp_path();
        let s = cursor.display().to_string();
        assert!(
            s.contains(r"\.cursor\mcp.json"),
            "expected backslash-separated .cursor path, got: {s}"
        );

        let gemini = gemini_settings_path();
        assert!(
            gemini
                .display()
                .to_string()
                .contains(r"\.gemini\settings.json"),
            "expected backslash-separated .gemini path"
        );

        let codex = codex_agents_path();
        assert!(
            codex.display().to_string().contains(r"\.codex\AGENTS.md"),
            "expected backslash-separated .codex path"
        );

        let opencode = opencode_config_path();
        assert!(
            opencode
                .display()
                .to_string()
                .contains(r"\.config\opencode\opencode.json"),
            "expected backslash-separated opencode config path"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_claude_mem_home_routes_through_platform_paths() {
        // The string emitted into Claude settings / worker env must
        // match what platform_paths resolves, so the worker and the
        // plugin agree on where state lives.
        let from_helper = claude_mem_home_string();
        let from_pp = claude_mem_core::shared::platform_paths::claude_mem_home()
            .display()
            .to_string();
        assert_eq!(from_helper, from_pp);
    }

    #[cfg(windows)]
    #[test]
    fn windows_bun_detection_accepts_cmd_shim() {
        // The installer doesn't currently shell out to bun, but any
        // future detection must agree with the supervisor's
        // process_manager helper. Pinning the contract here so a
        // regression on either side is caught on the Windows runner.
        use crate::infrastructure::process_manager::is_bun_executable_path;
        assert!(is_bun_executable_path(r"C:\Users\Me\.bun\bin\bun.exe"));
        assert!(is_bun_executable_path(r"C:\Users\Me\.bun\bin\bun.cmd"));
        assert!(is_bun_executable_path(r"C:\Users\Me\.bun\bin\BUN.CMD"));
        assert!(!is_bun_executable_path(r"C:\Users\Me\.bun\bin\node.exe"));
    }

    #[test]
    fn write_json_atomic_overwrites_existing_file() {
        // Regression: second-run installs must replace `settings.json` /
        // `mcp.json` etc. cleanly. `fs::rename` on Windows does not do
        // this for a pre-existing destination — see PR #9 Codex review.
        // The helper removes the destination on Windows before renaming.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        write_json_atomic(&path, &json!({"first": 1})).expect("first write");
        assert!(path.exists(), "first write should land");
        write_json_atomic(&path, &json!({"second": 2})).expect("overwrite");
        let body = fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("\"second\""),
            "expected overwrite content, got: {body}"
        );
        assert!(
            !body.contains("\"first\""),
            "old content should be replaced, got: {body}"
        );
    }
}
