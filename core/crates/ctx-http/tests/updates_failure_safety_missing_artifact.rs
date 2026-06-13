#![allow(clippy::await_holding_lock)]

use std::sync::Arc;

use axum::http::StatusCode;
use axum::routing::get;
use serde_json::{json, Value};

mod common;

use common::updates_failure_safety::{
    current_platform_key, lock_env, release_manifest_for, test_app_router, EnvGuard,
};

#[tokio::test]
async fn download_update_rejects_missing_artifact_and_api_stays_healthy() {
    let _env_lock = lock_env();

    let Some(platform) = current_platform_key() else {
        eprintln!("skipping: unsupported platform for updater checks");
        return;
    };

    let manifest = Arc::new(release_manifest_for(
        platform,
        "/download/stable/9.9.9/missing.AppImage",
        "1111111111111111111111111111111111111111111111111111111111111111",
    ));

    let fake_release_server = common::spawn_http_server(
        axum::Router::new()
            .route(
                "/releases/stable/latest.json",
                get({
                    let manifest = Arc::clone(&manifest);
                    move || {
                        let manifest = Arc::clone(&manifest);
                        async move {
                            (
                                StatusCode::OK,
                                [("content-type", "application/json")],
                                manifest.manifest_body.clone(),
                            )
                        }
                    }
                }),
            )
            .route(
                "/releases/stable/latest.json.sig",
                get({
                    let manifest = Arc::clone(&manifest);
                    move || {
                        let manifest = Arc::clone(&manifest);
                        async move {
                            (
                                StatusCode::OK,
                                [("content-type", "text/plain")],
                                manifest.signature_b64.clone(),
                            )
                        }
                    }
                }),
            ),
    )
    .await;
    let _download_base = EnvGuard::set("CTX_DOWNLOAD_BASE_URL", &fake_release_server.base_url);
    let _manifest_pubkey = EnvGuard::set("CTX_RELEASE_MANIFEST_PUBKEY", &manifest.pubkey_b64);

    let data_dir = tempfile::tempdir().unwrap();
    let target_path = data_dir.path().join("ctx.AppImage");
    tokio::fs::write(&target_path, b"old-appimage")
        .await
        .unwrap();
    let target_path_string = target_path.to_string_lossy().to_string();
    let _appimage = EnvGuard::set("CTX_APPIMAGE_PATH", &target_path_string);
    let app_harness = test_app_router(data_dir.path()).await;
    let app = app_harness.app();

    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/updates/appimage/download",
        Some(json!({ "channel": "stable" })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        error.contains("download http error"),
        "unexpected error payload: {body}"
    );

    let (health_status, _health): (StatusCode, Value) =
        common::json_request(&app, axum::http::Method::GET, "/api/health", None).await;
    assert_eq!(health_status, StatusCode::OK);
}
