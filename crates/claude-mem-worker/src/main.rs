#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "claude_mem_worker=info,tower_http=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    claude_mem_worker::run_from_env().await
}
