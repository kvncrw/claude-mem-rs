//! Worker HTTP helpers (port of `shared/worker-utils.ts`).

const DEFAULT_PORT: u16 = 37777;

pub fn worker_port_from_env() -> u16 {
    std::env::var("CLAUDE_MEM_WORKER_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

pub fn worker_base_url() -> String {
    format!("http://127.0.0.1:{}", worker_port_from_env())
}

pub fn build_worker_url(path: &str) -> String {
    let base = worker_base_url();
    if path.starts_with('/') {
        format!("{}{}", base, path)
    } else {
        format!("{}/{}", base, path)
    }
}
