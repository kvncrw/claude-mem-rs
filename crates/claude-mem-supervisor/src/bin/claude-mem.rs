use anyhow::{anyhow, Result};
use claude_mem_supervisor::claude_md::{clean, generate, print_report, ClaudeMdOptions};
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
        "generate" | "generate-claude-md" => {
            let report = generate(claude_md_options(&args)?)?;
            print_report("generate", &report);
            Ok(if report.errors.is_empty() { 0 } else { 1 })
        }
        "clean" | "clean-claude-md" => {
            let report = clean(claude_md_options(&args)?)?;
            print_report("clean", &report);
            Ok(if report.errors.is_empty() { 0 } else { 1 })
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
        "claude-mem-rs {}\n\nCommands:\n  install [--ide <ids>] [--yes] [--dry-run] [--bin <path>]\n  uninstall [--yes] [--dry-run]\n  detect\n  transcript <init|validate|process|watch> [--config <path>] [--once]\n  generate [--dry-run] [--root <path>] [--db <path>] [--project <name>] [--target <CLAUDE.md>] [--limit <n>]\n  clean [--dry-run] [--root <path>] [--target <CLAUDE.md>]\n  version\n",
        env!("CARGO_PKG_VERSION")
    );
}

fn claude_md_options(args: &[String]) -> Result<ClaudeMdOptions> {
    Ok(ClaudeMdOptions {
        dry_run: has_flag(args, "--dry-run"),
        project_root: arg_value(args, "--root")
            .map(Into::into)
            .unwrap_or(std::env::current_dir()?),
        db_path: arg_value(args, "--db").map(Into::into),
        project: arg_value(args, "--project"),
        target_file: arg_value(args, "--target"),
        limit: arg_value(args, "--limit")
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(50),
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
