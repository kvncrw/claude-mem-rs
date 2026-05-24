use anyhow::{anyhow, Result};
use claude_mem_supervisor::installer::{
    detect_ides, print_install_report, run_install, run_uninstall, InstallOptions, UninstallOptions,
};
use claude_mem_supervisor::transcripts::cli::run_transcript_command;

#[tokio::main]
async fn main() {
    let code = match run().await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    };
    std::process::exit(code);
}

async fn run() -> Result<i32> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let command = args.first().map(String::as_str).unwrap_or("help");
    match command {
        "install" => {
            let report = run_install(InstallOptions {
                ide: arg_value(&args, "--ide"),
                yes: has_flag(&args, "--yes") || has_flag(&args, "-y"),
                dry_run: has_flag(&args, "--dry-run"),
                bin_path: arg_value(&args, "--bin").map(Into::into),
            })?;
            print_install_report(&report);
            Ok(if report.failed.is_empty() { 0 } else { 1 })
        }
        "uninstall" | "remove" => {
            let report = run_uninstall(UninstallOptions {
                yes: has_flag(&args, "--yes") || has_flag(&args, "-y"),
                dry_run: has_flag(&args, "--dry-run"),
            })?;
            print_install_report(&report);
            Ok(0)
        }
        "detect" => {
            println!("{}", serde_json::to_string_pretty(&detect_ides())?);
            Ok(0)
        }
        "transcript" => {
            let subcommand = args.get(1).map(String::as_str);
            run_transcript_command(subcommand, &args[2..]).await
        }
        "version" | "--version" | "-v" => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        "help" | "--help" | "-h" => {
            print_help();
            Ok(0)
        }
        other => Err(anyhow!("unknown command: {other}")),
    }
}

fn print_help() {
    println!(
        "claude-mem-rs {}\n\nCommands:\n  install [--ide <ids>] [--yes] [--dry-run] [--bin <path>]\n  uninstall [--yes] [--dry-run]\n  detect\n  transcript <init|validate|process|watch> [--config <path>] [--once]\n  version\n",
        env!("CARGO_PKG_VERSION")
    );
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
