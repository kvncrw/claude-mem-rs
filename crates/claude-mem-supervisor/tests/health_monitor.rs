use axum::routing::{get, post};
use axum::{Json, Router};
use claude_mem_supervisor::infrastructure::health_monitor::{
    check_version_match_with_plugin_version, get_installed_plugin_version_from,
    get_running_worker_version, http_shutdown, is_port_in_use, wait_for_health, wait_for_port_free,
    wait_for_readiness,
};
use serde_json::json;
use std::net::TcpListener as StdTcpListener;
use std::time::Duration;
use tempfile::tempdir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

async fn spawn_health_server() -> (u16, oneshot::Sender<()>) {
    let app = Router::new()
        .route(
            "/api/health",
            get(|| async { Json(json!({ "status": "ok" })) }),
        )
        .route(
            "/api/readiness",
            get(|| async { Json(json!({ "ready": true })) }),
        )
        .route(
            "/api/version",
            get(|| async { Json(json!({ "version": "9.8.7" })) }),
        )
        .route(
            "/api/admin/shutdown",
            post(|| async { Json(json!({ "success": true })) }),
        );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    (port, tx)
}

#[test]
fn detects_occupied_and_free_ports_with_socket_bind() {
    let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    assert!(is_port_in_use(port));
    drop(listener);
    assert!(!is_port_in_use(port));
}

#[tokio::test]
async fn waits_for_health_and_readiness_endpoints() {
    let (port, shutdown) = spawn_health_server().await;

    assert!(wait_for_health(port, Some(Duration::from_secs(2))).await);
    assert!(wait_for_readiness(port, Some(Duration::from_secs(2))).await);

    let _ = shutdown.send(());
}

#[tokio::test]
async fn health_wait_times_out_when_no_worker_answers() {
    let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    assert!(!wait_for_health(port, Some(Duration::from_millis(100))).await);
}

#[tokio::test]
async fn waits_for_port_to_become_free() {
    let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(650)).await;
        drop(listener);
    });

    assert!(wait_for_port_free(port, Some(Duration::from_secs(3))).await);
}

#[tokio::test]
async fn shutdown_and_version_probe_use_worker_http_api() {
    let (port, shutdown) = spawn_health_server().await;

    assert_eq!(
        get_running_worker_version(port).await.as_deref(),
        Some("9.8.7")
    );
    assert!(http_shutdown(port).await);

    let matching = check_version_match_with_plugin_version(port, "9.8.7").await;
    assert!(matching.matches);
    assert_eq!(matching.worker_version.as_deref(), Some("9.8.7"));

    let mismatched = check_version_match_with_plugin_version(port, "0.0.0").await;
    assert!(!mismatched.matches);
    assert_eq!(mismatched.plugin_version, "0.0.0");

    let _ = shutdown.send(());
}

#[tokio::test]
async fn version_check_assumes_match_when_worker_unavailable() {
    let listener = StdTcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let result = check_version_match_with_plugin_version(port, "1.2.3").await;
    assert!(result.matches);
    assert_eq!(result.worker_version, None);
}

#[test]
fn installed_plugin_version_reads_package_json_or_unknown() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), r#"{ "version": "1.2.3" }"#).unwrap();
    assert_eq!(get_installed_plugin_version_from(dir.path()), "1.2.3");

    let missing = tempdir().unwrap();
    assert_eq!(get_installed_plugin_version_from(missing.path()), "unknown");
}
