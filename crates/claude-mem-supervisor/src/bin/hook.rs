//! hook subcommand — stdin → adapter → handler → worker HTTP → stdout JSON.

#[tokio::main]
async fn main() {
    let code = match claude_mem_supervisor::hooks::run_hook_from_env().await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            2
        }
    };
    std::process::exit(code);
}
