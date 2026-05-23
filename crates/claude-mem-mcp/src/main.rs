//! claude-mem-mcp stdio entry.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    claude_mem_mcp::server::run_stdio().await
}
