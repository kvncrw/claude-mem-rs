//! POSIX installer UX for the Rust runtime.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const PLUGIN_ID: &str = "claude-mem@kvncrw";
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
    reject_windows()?;
    let version = env!("CARGO_PKG_VERSION").to_owned();
    let selected_ides = select_ides(options.ide.as_deref(), options.yes)?;
    let bin_path = options.bin_path.unwrap_or_else(current_binary_path);
    let mut actions = Vec::new();
    let mut failed = Vec::new();

    register_claude_plugin(&version, &bin_path, options.dry_run, &mut actions)?;
    for ide in &selected_ides {
        if let Err(error) = install_ide(ide, &bin_path, options.dry_run, &mut actions) {
            failed.push(format!("{ide}: {error}"));
        }
    }
    write_transcript_sample_if_needed(options.dry_run, &mut actions)?;

    Ok(InstallReport {
        version,
        selected_ides,
        dry_run: options.dry_run,
        actions,
        failed,
    })
}

pub fn run_uninstall(options: UninstallOptions) -> Result<InstallReport> {
    reject_windows()?;
    if !options.yes && stdin_is_tty() && !confirm("Uninstall claude-mem runtime integrations?")? {
        return Err(anyhow!("uninstall cancelled"));
    }
    let mut actions = Vec::new();
    remove_claude_plugin(options.dry_run, &mut actions)?;
    remove_file_if_exists(gemini_settings_path(), options.dry_run, &mut actions)?;
    remove_file_if_exists(cursor_mcp_path(), options.dry_run, &mut actions)?;
    remove_file_if_exists(codex_agents_path(), options.dry_run, &mut actions)?;
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
        other => return Err(anyhow!("unsupported IDE: {other}")),
    }
    Ok(())
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
    let plugin_json = plugin_dir.join(".claude-plugin/plugin.json");
    let scripts_dir = plugin_dir.join("scripts");
    let launcher = scripts_dir.join("claude-mem");
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
        fs::write(
            &launcher,
            format!(
                "#!/usr/bin/env sh\nexec \"{}\" \"$@\"\n",
                bin_path.display()
            ),
        )?;
        set_executable(&launcher)?;
        register_marketplace(version)?;
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
    let path = claude_marketplace_dir().join("plugin/hooks/hooks.json");
    let command = format!("\"{}\" hook claude-code", bin_path.display());
    let manifest = json!({
        "description": "claude-mem-rs memory system hooks",
        "hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": format!("{command} context")}]}],
            "UserPromptSubmit": [{"hooks": [{"type": "command", "command": format!("{command} session-init")}]}],
            "PostToolUse": [{"hooks": [{"type": "command", "command": format!("{command} observation")}]}],
            "Stop": [{"hooks": [{"type": "command", "command": format!("{command} summarize")}]}],
            "SessionEnd": [{"hooks": [{"type": "command", "command": format!("{command} session-complete")}]}]
        }
    });
    if !dry_run {
        write_json_atomic(&path, &manifest)?;
    }
    actions.push(format!("wrote Claude hook manifest {}", path.display()));
    Ok(())
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
    config["hooks"]["AfterAgent"] = json!([{
        "command": bin_path.display().to_string(),
        "args": ["hook", "gemini-cli", "observation"]
    }]);
    config["hooks"]["Stop"] = json!([{
        "command": bin_path.display().to_string(),
        "args": ["hook", "gemini-cli", "summarize"]
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
    fs::rename(tmp, path)?;
    Ok(())
}

fn worker_env_json() -> Value {
    let mut env = BTreeMap::new();
    if let Ok(value) = std::env::var("CLAUDE_MEM_WORKER_URL") {
        env.insert("CLAUDE_MEM_WORKER_URL", value);
    }
    json!(env)
}

fn reject_windows() -> Result<()> {
    if cfg!(windows) {
        Err(anyhow!("Windows is not supported by claude-mem-rs"))
    } else {
        Ok(())
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
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {command} >/dev/null 2>&1"))
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn current_binary_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("claude-mem"))
}

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
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn claude_config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".claude"))
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
        .unwrap_or_else(|| home_dir().join(".cursor/mcp.json"))
}

fn gemini_settings_path() -> PathBuf {
    std::env::var_os("GEMINI_SETTINGS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".gemini/settings.json"))
}

fn codex_agents_path() -> PathBuf {
    std::env::var_os("CODEX_AGENTS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".codex/AGENTS.md"))
}

fn transcript_config_path() -> PathBuf {
    std::env::var_os("CLAUDE_MEM_TRANSCRIPTS_CONFIG_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".claude-mem/transcript-watch.json"))
}
