//! Worker health, readiness, port, shutdown, and version probes.
//!
//! This is the Rust port of `src/services/infrastructure/HealthMonitor.ts`.
//! Linux/Unix uses an atomic bind probe for port checks, matching the TS
//! fix that avoids racing two daemon spawns against an HTTP-only check.

use serde::Deserialize;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::time::{Duration, Instant};

const DEFAULT_HEALTH_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_PORT_FREE_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionCheckResult {
    pub matches: bool,
    pub plugin_version: String,
    pub worker_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkerVersionResponse {
    version: String,
}

pub fn is_port_in_use(port: u16) -> bool {
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            drop(listener);
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => true,
        Err(_) => false,
    }
}

pub async fn wait_for_health(port: u16, timeout: Option<Duration>) -> bool {
    poll_endpoint_until_ok(
        port,
        "/api/health",
        timeout.unwrap_or(DEFAULT_HEALTH_TIMEOUT),
    )
    .await
}

pub async fn wait_for_readiness(port: u16, timeout: Option<Duration>) -> bool {
    poll_endpoint_until_ok(
        port,
        "/api/readiness",
        timeout.unwrap_or(DEFAULT_HEALTH_TIMEOUT),
    )
    .await
}

pub async fn wait_for_port_free(port: u16, timeout: Option<Duration>) -> bool {
    let deadline = Instant::now() + timeout.unwrap_or(DEFAULT_PORT_FREE_TIMEOUT);

    while Instant::now() < deadline {
        if !is_port_in_use(port) {
            return true;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    false
}

pub async fn http_shutdown(port: u16) -> bool {
    let url = worker_url(port, "/api/admin/shutdown");
    reqwest::Client::new()
        .post(url)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

pub fn get_installed_plugin_version() -> String {
    if let Ok(root) = std::env::var("CLAUDE_MEM_MARKETPLACE_ROOT") {
        return get_installed_plugin_version_from(Path::new(&root));
    }

    env!("CARGO_PKG_VERSION").to_owned()
}

pub fn get_installed_plugin_version_from(root: &Path) -> String {
    let path = root.join("package.json");
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|json| {
            json.get("version")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        }) {
        Some(version) => version,
        None => "unknown".to_owned(),
    }
}

pub async fn get_running_worker_version(port: u16) -> Option<String> {
    let url = worker_url(port, "/api/version");
    let response = reqwest::get(url).await.ok()?;
    if !response.status().is_success() {
        return None;
    }

    response
        .json::<WorkerVersionResponse>()
        .await
        .ok()
        .map(|body| body.version)
}

pub async fn check_version_match(port: u16) -> VersionCheckResult {
    check_version_match_with_plugin_version(port, get_installed_plugin_version()).await
}

pub async fn check_version_match_with_plugin_version(
    port: u16,
    plugin_version: impl Into<String>,
) -> VersionCheckResult {
    let plugin_version = plugin_version.into();
    let worker_version = get_running_worker_version(port).await;

    let matches = worker_version.as_ref().is_none_or(|worker_version| {
        plugin_version == "unknown" || worker_version == &plugin_version
    });

    VersionCheckResult {
        matches,
        plugin_version,
        worker_version,
    }
}

pub fn worker_addr(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

fn worker_url(port: u16, path: &str) -> String {
    format!("http://{}{}", worker_addr(port), path)
}

async fn poll_endpoint_until_ok(port: u16, path: &str, timeout: Duration) -> bool {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + timeout;
    let url = worker_url(port, path);

    while Instant::now() < deadline {
        match client.get(&url).send().await {
            Ok(response) if response.status().is_success() => return true,
            _ => tokio::time::sleep(POLL_INTERVAL).await,
        }
    }

    false
}
