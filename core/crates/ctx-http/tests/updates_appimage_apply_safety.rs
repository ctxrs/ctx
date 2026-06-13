#![allow(clippy::await_holding_lock)]

use std::sync::Arc;

use axum::http::StatusCode;
use axum::routing::get;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

mod common;

use common::updates_failure_safety::{
    current_platform_key, lock_env, release_manifest_for, test_app_router, EnvGuard,
};

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

async fn write_verified_candidate_meta(
    data_root: &std::path::Path,
    target_path: &std::path::Path,
    candidate_path: &std::path::Path,
    target_version: &str,
    payload: &[u8],
) {
    let base_url = ctx_update_service::default_download_base_url();
    let artifact_url_path = "/download/stable/9.9.9/ctx.AppImage";
    let meta = ctx_update_service::VerifiedAppImageCandidateMeta {
        schema_version: ctx_update_service::VerifiedAppImageCandidateMeta::SCHEMA_VERSION,
        candidate_path: candidate_path.to_path_buf(),
        target_path: target_path.to_path_buf(),
        channel: "stable".to_string(),
        platform: current_platform_key().unwrap_or("linux-x64").to_string(),
        target_version: target_version.to_string(),
        current_version: "0.59.0".to_string(),
        artifact_url: format!("{base_url}{artifact_url_path}"),
        artifact_url_path: artifact_url_path.to_string(),
        manifest_url: format!("{base_url}/releases/stable/latest.json"),
        base_url,
        sha256: sha256_hex(payload),
        size_bytes: payload.len() as u64,
        verified_at_ms: 1,
    };
    let meta_path = ctx_update_service::appimage_candidate_meta_path(data_root);
    tokio::fs::create_dir_all(meta_path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&meta_path, serde_json::to_vec_pretty(&meta).unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn appimage_apply_requires_verified_candidate_metadata() {
    let _env_lock = lock_env();
    let data_dir = tempfile::tempdir().unwrap();
    let target_path = data_dir.path().join("ctx.AppImage");
    tokio::fs::write(&target_path, b"old-appimage")
        .await
        .unwrap();
    let target_path_string = target_path.to_string_lossy().to_string();
    let _appimage = EnvGuard::set("CTX_APPIMAGE_PATH", &target_path_string);
    let candidate = ctx_update_service::appimage_candidate_path(data_dir.path());
    tokio::fs::create_dir_all(candidate.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&candidate, b"legacy-candidate")
        .await
        .unwrap();

    let app_harness = test_app_router(data_dir.path()).await;
    let app = app_harness.app();
    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/updates/appimage/apply",
        Some(json!({ "confirm": true })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        error.contains("reading") || error.contains("metadata"),
        "unexpected error payload: {body}"
    );
    assert_eq!(
        tokio::fs::read(&target_path).await.unwrap(),
        b"old-appimage",
        "unverified candidate must not replace target"
    );
}

#[tokio::test]
async fn appimage_apply_rejects_candidate_path_outside_updates_dir() {
    let _env_lock = lock_env();
    let data_dir = tempfile::tempdir().unwrap();
    let target_path = data_dir.path().join("ctx.AppImage");
    tokio::fs::write(&target_path, b"old-appimage")
        .await
        .unwrap();
    let target_path_string = target_path.to_string_lossy().to_string();
    let _appimage = EnvGuard::set("CTX_APPIMAGE_PATH", &target_path_string);
    let payload = b"verified-appimage";
    let candidate = data_dir.path().join("outside.AppImage");
    tokio::fs::write(&candidate, payload).await.unwrap();
    write_verified_candidate_meta(data_dir.path(), &target_path, &candidate, "9.9.9", payload)
        .await;

    let app_harness = test_app_router(data_dir.path()).await;
    let app = app_harness.app();
    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/updates/appimage/apply",
        Some(json!({ "confirm": true })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        error.contains("verified candidate path"),
        "unexpected error payload: {body}"
    );
    assert_eq!(
        tokio::fs::read(&target_path).await.unwrap(),
        b"old-appimage"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn appimage_apply_rejects_symlink_candidate_inside_updates_dir() {
    let _env_lock = lock_env();
    let data_dir = tempfile::tempdir().unwrap();
    let target_path = data_dir.path().join("ctx.AppImage");
    tokio::fs::write(&target_path, b"old-appimage")
        .await
        .unwrap();
    let target_path_string = target_path.to_string_lossy().to_string();
    let _appimage = EnvGuard::set("CTX_APPIMAGE_PATH", &target_path_string);
    let payload = b"verified-appimage";
    let outside = data_dir.path().join("outside-real-file");
    tokio::fs::write(&outside, payload).await.unwrap();
    let candidate = ctx_update_service::appimage_candidate_path(data_dir.path());
    tokio::fs::create_dir_all(candidate.parent().unwrap())
        .await
        .unwrap();
    std::os::unix::fs::symlink(&outside, &candidate).unwrap();
    write_verified_candidate_meta(data_dir.path(), &target_path, &candidate, "9.9.9", payload)
        .await;

    let app_harness = test_app_router(data_dir.path()).await;
    let app = app_harness.app();
    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/updates/appimage/apply",
        Some(json!({ "confirm": true })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        error.contains("regular file"),
        "unexpected error payload: {body}"
    );
    assert_eq!(
        tokio::fs::read(&target_path).await.unwrap(),
        b"old-appimage"
    );
}

#[tokio::test]
async fn appimage_apply_rejects_stale_or_downgrade_candidate() {
    let _env_lock = lock_env();
    let data_dir = tempfile::tempdir().unwrap();
    let target_path = data_dir.path().join("ctx.AppImage");
    tokio::fs::write(&target_path, b"old-appimage")
        .await
        .unwrap();
    let target_path_string = target_path.to_string_lossy().to_string();
    let _appimage = EnvGuard::set("CTX_APPIMAGE_PATH", &target_path_string);
    let payload = b"verified-appimage";
    let candidate = ctx_update_service::appimage_candidate_path(data_dir.path());
    tokio::fs::create_dir_all(candidate.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&candidate, payload).await.unwrap();
    write_verified_candidate_meta(data_dir.path(), &target_path, &candidate, "0.0.1", payload)
        .await;

    let app_harness = test_app_router(data_dir.path()).await;
    let app = app_harness.app();
    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/updates/appimage/apply",
        Some(json!({ "confirm": true })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        error.contains("not newer"),
        "unexpected error payload: {body}"
    );
    assert_eq!(
        tokio::fs::read(&target_path).await.unwrap(),
        b"old-appimage"
    );
}

#[tokio::test]
async fn appimage_apply_rejects_malformed_candidate_version() {
    let _env_lock = lock_env();
    let data_dir = tempfile::tempdir().unwrap();
    let target_path = data_dir.path().join("ctx.AppImage");
    tokio::fs::write(&target_path, b"old-appimage")
        .await
        .unwrap();
    let target_path_string = target_path.to_string_lossy().to_string();
    let _appimage = EnvGuard::set("CTX_APPIMAGE_PATH", &target_path_string);
    let payload = b"verified-appimage";
    let candidate = ctx_update_service::appimage_candidate_path(data_dir.path());
    tokio::fs::create_dir_all(candidate.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&candidate, payload).await.unwrap();
    write_verified_candidate_meta(
        data_dir.path(),
        &target_path,
        &candidate,
        "not-a-version",
        payload,
    )
    .await;

    let app_harness = test_app_router(data_dir.path()).await;
    let app = app_harness.app();
    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/updates/appimage/apply",
        Some(json!({ "confirm": true })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        error.contains("semver-compatible"),
        "unexpected error payload: {body}"
    );
    assert_eq!(
        tokio::fs::read(&target_path).await.unwrap(),
        b"old-appimage"
    );
}

#[tokio::test]
async fn appimage_apply_rejects_tampered_verified_candidate() {
    let _env_lock = lock_env();
    let Some(platform) = current_platform_key() else {
        eprintln!("skipping: unsupported platform for updater checks");
        return;
    };
    let payload = b"verified-appimage";
    let manifest = Arc::new(release_manifest_for(
        platform,
        "/download/stable/9.9.9/ctx.AppImage",
        &sha256_hex(payload),
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
            )
            .route(
                "/download/stable/9.9.9/ctx.AppImage",
                get(|| async {
                    (
                        StatusCode::OK,
                        [("content-type", "application/octet-stream")],
                        "verified-appimage",
                    )
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

    let (download_status, _download_body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/updates/appimage/download",
        Some(json!({ "channel": "stable" })),
    )
    .await;
    assert_eq!(download_status, StatusCode::OK);

    let candidate = ctx_update_service::appimage_candidate_path(data_dir.path());
    tokio::fs::write(&candidate, b"tampered-appimage")
        .await
        .unwrap();
    let (apply_status, body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/updates/appimage/apply",
        Some(json!({ "confirm": true })),
    )
    .await;

    assert_eq!(apply_status, StatusCode::BAD_REQUEST);
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        error.contains("checksum") || error.contains("size"),
        "unexpected error payload: {body}"
    );
    assert_eq!(
        tokio::fs::read(&target_path).await.unwrap(),
        b"old-appimage"
    );
}

#[tokio::test]
async fn appimage_failed_redownload_invalidates_existing_verified_candidate() {
    let _env_lock = lock_env();
    let Some(platform) = current_platform_key() else {
        eprintln!("skipping: unsupported platform for updater checks");
        return;
    };
    let new_payload = b"new-verified-appimage";
    let manifest = Arc::new(release_manifest_for(
        platform,
        "/download/stable/10.0.0/ctx.AppImage",
        &sha256_hex(new_payload),
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
            )
            .route(
                "/download/stable/10.0.0/ctx.AppImage",
                get(|| async {
                    (
                        StatusCode::OK,
                        [("content-type", "application/octet-stream")],
                        "wrong-appimage",
                    )
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
    let stale_payload = b"previous-verified-appimage";
    let candidate = ctx_update_service::appimage_candidate_path(data_dir.path());
    tokio::fs::create_dir_all(candidate.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(&candidate, stale_payload).await.unwrap();
    write_verified_candidate_meta(
        data_dir.path(),
        &target_path,
        &candidate,
        "9.9.8",
        stale_payload,
    )
    .await;
    let app_harness = test_app_router(data_dir.path()).await;
    let app = app_harness.app();

    let (download_status, _download_body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/updates/appimage/download",
        Some(json!({ "channel": "stable" })),
    )
    .await;
    assert_eq!(download_status, StatusCode::BAD_GATEWAY);

    let (apply_status, body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/updates/appimage/apply",
        Some(json!({ "confirm": true, "channel": "stable" })),
    )
    .await;

    assert_eq!(apply_status, StatusCode::BAD_REQUEST);
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        error.contains("reading") || error.contains("metadata"),
        "unexpected error payload: {body}"
    );
    assert_eq!(
        tokio::fs::read(&target_path).await.unwrap(),
        b"old-appimage"
    );
}

#[tokio::test]
async fn appimage_apply_replaces_target_and_clears_candidate() {
    let _env_lock = lock_env();
    let Some(platform) = current_platform_key() else {
        eprintln!("skipping: unsupported platform for updater checks");
        return;
    };
    let payload = b"verified-appimage";
    let manifest = Arc::new(release_manifest_for(
        platform,
        "/download/stable/9.9.9/ctx.AppImage",
        &sha256_hex(payload),
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
            )
            .route(
                "/download/stable/9.9.9/ctx.AppImage",
                get(|| async {
                    (
                        StatusCode::OK,
                        [("content-type", "application/octet-stream")],
                        "verified-appimage",
                    )
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

    let (download_status, _download_body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/updates/appimage/download",
        Some(json!({ "channel": "stable" })),
    )
    .await;
    assert_eq!(download_status, StatusCode::OK);
    let meta = ctx_update_service::read_verified_appimage_candidate_meta(data_dir.path())
        .await
        .unwrap();
    assert_eq!(
        meta.current_version,
        env!("CARGO_PKG_VERSION"),
        "downloaded AppImage metadata must use the ctx-http package version"
    );

    let (apply_status, _body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::POST,
        "/api/updates/appimage/apply",
        Some(json!({ "confirm": true })),
    )
    .await;
    assert_eq!(apply_status, StatusCode::OK);
    assert_eq!(tokio::fs::read(&target_path).await.unwrap(), payload);
    assert!(!ctx_update_service::appimage_candidate_path(data_dir.path()).exists());
    assert!(!ctx_update_service::appimage_candidate_meta_path(data_dir.path()).exists());
}
