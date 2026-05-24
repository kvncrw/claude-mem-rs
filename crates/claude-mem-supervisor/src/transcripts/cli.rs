use super::config::{
    default_config_path, default_state_path, expand_home_path, load_config, write_sample_config,
};
use super::watcher::TranscriptWatcher;
use crate::hooks::WorkerClient;
use anyhow::Result;

pub async fn run_transcript_command(subcommand: Option<&str>, args: &[String]) -> Result<i32> {
    let config_path = arg_value(args, "--config")
        .map(expand_home_path)
        .unwrap_or_else(default_config_path);
    match subcommand.unwrap_or("help") {
        "init" => {
            write_sample_config(&config_path)?;
            println!("created {}", config_path.display());
            Ok(0)
        }
        "validate" => {
            let config = ensure_config(&config_path)?;
            println!(
                "config OK: {} ({} watches)",
                config_path.display(),
                config.watches.len()
            );
            Ok(0)
        }
        "process" | "watch" => {
            let config = ensure_config(&config_path)?;
            let state_path = config
                .state_file
                .as_deref()
                .map(expand_home_path)
                .unwrap_or_else(default_state_path);
            let mut watcher = TranscriptWatcher::new(config, state_path, WorkerClient::from_env());
            if subcommand == Some("process") || has_flag(args, "--once") {
                let stats = watcher.process_once().await?;
                println!("{}", serde_json::to_string_pretty(&stats_json(&stats))?);
                return Ok(0);
            }
            println!("transcript watcher running; press Ctrl+C to stop");
            watcher.watch_forever().await?;
            Ok(0)
        }
        _ => {
            println!("Usage: claude-mem transcript <init|validate|process|watch> [--config <path>] [--once]");
            Ok(1)
        }
    }
}

fn ensure_config(path: &std::path::Path) -> Result<super::config::TranscriptWatchConfig> {
    if !path.exists() {
        write_sample_config(path)?;
    }
    load_config(path)
}

fn stats_json(stats: &super::watcher::WatchRunStats) -> serde_json::Value {
    serde_json::json!({
        "filesSeen": stats.files_seen,
        "linesProcessed": stats.lines_processed,
        "matchedEvents": stats.matched_events,
        "sessionInits": stats.session_inits,
        "observations": stats.observations,
        "summaries": stats.summaries,
        "completions": stats.completions
    })
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|idx| args.get(idx + 1))
        .cloned()
}
