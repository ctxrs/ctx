#![allow(clippy::await_holding_lock)]

use axum::http::StatusCode;
use axum::routing::get;
use serde_json::Value;
use std::sync::Arc;

mod common;

use common::updates_failure_safety::{
    lock_env, sign_release_manifest_body, test_app_router, EnvGuard,
};

#[tokio::test]
async fn updates_check_rejects_unsigned_manifest_metadata() {
    let _env_lock = lock_env();

    let unsigned_manifest_server = common::spawn_http_server(axum::Router::new().route(
        "/releases/stable/latest.json",
        get(|| async {
            (
                StatusCode::OK,
                [("content-type", "application/json")],
                r#"{"channel":"stable","latest_version":"9.9.9","published_at":"2026-02-19T00:00:00Z","platforms":{"linux-x64":{"appimage":{"url_path":"/download/stable/9.9.9/ctx.AppImage","sha256":"1111111111111111111111111111111111111111111111111111111111111111"},"daemon":{"url_path":"/download/stable/9.9.9/ctx-daemon","sha256":"2222222222222222222222222222222222222222222222222222222222222222"}}}}"#,
            )
        }),
    ))
    .await;
    let _download_base = EnvGuard::set("CTX_DOWNLOAD_BASE_URL", &unsigned_manifest_server.base_url);

    let data_dir = tempfile::tempdir().unwrap();
    let app_harness = test_app_router(data_dir.path()).await;
    let app = app_harness.app();
    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/updates/check?channel=stable",
        None,
    )
    .await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        error.contains("signature"),
        "unexpected error payload: {body}"
    );
}

#[tokio::test]
async fn updates_check_rejects_tampered_manifest_metadata() {
    let _env_lock = lock_env();

    let signed_body = r#"{"channel":"stable","latest_version":"9.9.9","published_at":"2026-02-19T00:00:00Z","platforms":{"linux-x64":{"appimage":{"url_path":"/download/stable/9.9.9/ctx.AppImage","sha256":"1111111111111111111111111111111111111111111111111111111111111111"},"daemon":{"url_path":"/download/stable/9.9.9/ctx-daemon","sha256":"2222222222222222222222222222222222222222222222222222222222222222"}}}}"#;
    let (signature_b64, pubkey_b64) = sign_release_manifest_body(signed_body);
    let tampered_body = signed_body.replace("9.9.9", "9.9.10");
    let signature_b64 = Arc::new(signature_b64);
    let tampered_body = Arc::new(tampered_body);
    let tampered_manifest_server = common::spawn_http_server(
        axum::Router::new()
            .route(
                "/releases/stable/latest.json",
                get({
                    let tampered_body = Arc::clone(&tampered_body);
                    move || {
                        let tampered_body = Arc::clone(&tampered_body);
                        async move {
                            (
                                StatusCode::OK,
                                [("content-type", "application/json")],
                                tampered_body.to_string(),
                            )
                        }
                    }
                }),
            )
            .route(
                "/releases/stable/latest.json.sig",
                get({
                    let signature_b64 = Arc::clone(&signature_b64);
                    move || {
                        let signature_b64 = Arc::clone(&signature_b64);
                        async move {
                            (
                                StatusCode::OK,
                                [("content-type", "text/plain")],
                                signature_b64.to_string(),
                            )
                        }
                    }
                }),
            ),
    )
    .await;
    let _download_base = EnvGuard::set("CTX_DOWNLOAD_BASE_URL", &tampered_manifest_server.base_url);
    let _manifest_pubkey = EnvGuard::set("CTX_RELEASE_MANIFEST_PUBKEY", &pubkey_b64);

    let data_dir = tempfile::tempdir().unwrap();
    let app_harness = test_app_router(data_dir.path()).await;
    let app = app_harness.app();
    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/updates/check?channel=stable",
        None,
    )
    .await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        error.contains("signature") || error.contains("verifying"),
        "unexpected error payload: {body}"
    );
}

#[tokio::test]
async fn updates_check_rejects_third_party_artifact_url_refs() {
    let _env_lock = lock_env();

    let manifest_body = r#"{"channel":"stable","latest_version":"9.9.9","published_at":"2026-02-19T00:00:00Z","platforms":{"linux-x64":{"appimage":{"url_path":"https://downloads.example.test/ctx.AppImage","sha256":"1111111111111111111111111111111111111111111111111111111111111111"},"daemon":{"url_path":"/download/stable/9.9.9/ctx-daemon","sha256":"2222222222222222222222222222222222222222222222222222222222222222"}}}}"#;
    let (signature_b64, pubkey_b64) = sign_release_manifest_body(manifest_body);
    let manifest_body = Arc::new(manifest_body.to_string());
    let signature_b64 = Arc::new(signature_b64);
    let manifest_server = common::spawn_http_server(
        axum::Router::new()
            .route(
                "/releases/stable/latest.json",
                get({
                    let manifest_body = Arc::clone(&manifest_body);
                    move || {
                        let manifest_body = Arc::clone(&manifest_body);
                        async move {
                            (
                                StatusCode::OK,
                                [("content-type", "application/json")],
                                manifest_body.to_string(),
                            )
                        }
                    }
                }),
            )
            .route(
                "/releases/stable/latest.json.sig",
                get({
                    let signature_b64 = Arc::clone(&signature_b64);
                    move || {
                        let signature_b64 = Arc::clone(&signature_b64);
                        async move {
                            (
                                StatusCode::OK,
                                [("content-type", "text/plain")],
                                signature_b64.to_string(),
                            )
                        }
                    }
                }),
            ),
    )
    .await;
    let _download_base = EnvGuard::set("CTX_DOWNLOAD_BASE_URL", &manifest_server.base_url);
    let _manifest_pubkey = EnvGuard::set("CTX_RELEASE_MANIFEST_PUBKEY", &pubkey_b64);

    let data_dir = tempfile::tempdir().unwrap();
    let app_harness = test_app_router(data_dir.path()).await;
    let app = app_harness.app();
    let (status, body): (StatusCode, Value) = common::json_request(
        &app,
        axum::http::Method::GET,
        "/api/updates/check?channel=stable",
        None,
    )
    .await;

    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        error.contains("origin") || error.contains("artifact"),
        "unexpected error payload: {body}"
    );
}
