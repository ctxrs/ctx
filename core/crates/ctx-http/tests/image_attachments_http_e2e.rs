use std::path::PathBuf;

use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use base64::Engine;
use serde_json::json;
use tower::ServiceExt;

mod common;

fn multipart_body(
    boundary: &str,
    name: &str,
    filename: &str,
    content_type: Option<&str>,
    bytes: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    out.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    if let Some(content_type) = content_type {
        out.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(bytes);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

const MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;
const QUEUED_MESSAGES_ENABLED_ENV: &str = "CTX_QUEUED_MESSAGES_ENABLED";

fn enable_queued_messages_for_test_binary() {
    static ENABLE: std::sync::Once = std::sync::Once::new();
    ENABLE.call_once(|| std::env::set_var(QUEUED_MESSAGES_ENABLED_ENV, "1"));
}

#[tokio::test]
async fn image_attachments_use_blobs_and_never_persist_base64() {
    enable_queued_messages_for_test_binary();
    // Tiny 1x1 PNG.
    const PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMB/6Vn3b0AAAAASUVORK5CYII=";
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(PNG_BASE64.as_bytes())
        .unwrap();

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    // 1) Upload blob and fetch it back.
    let boundary = "ctx-test-boundary";
    let body = multipart_body(boundary, "file", "x.png", Some("image/png"), &png_bytes);
    let req = Request::builder()
        .method("POST")
        .uri("/api/blobs")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let uploaded: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let uploaded_id = uploaded.get("blob_id").and_then(|v| v.as_str()).unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/blobs/{uploaded_id}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "image/png"
    );
    let fetched = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(fetched.as_ref(), png_bytes.as_slice());

    // 2) Create workspace/task/session and post message with legacy base64 attachment;
    //    server should normalize it to image_ref before persisting.
    let repo = common::init_git_repo(&[("README.md", "hello\n")]).await;

    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model").await;

    let data_base64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
    let (status, msg): (StatusCode, ctx_core::models::Message) = common::json_request(
        &app,
        Method::POST,
        format!("/api/sessions/{}/messages", session.id.0),
        Some(json!({
            "content":"hi",
            "delivery":"queued",
            "attachments":[{"kind":"image","mime_type":"image/png","data_base64":data_base64,"name":"x.png"}]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(msg.attachments.len(), 1);
    let att_json = serde_json::to_value(&msg.attachments[0]).unwrap();
    assert_eq!(
        att_json.get("kind").and_then(|v| v.as_str()),
        Some("image_ref")
    );
    assert!(
        att_json.get("data_base64").is_none(),
        "expected no base64 persisted in message attachment: {att_json:?}"
    );

    let ref_blob_id = att_json.get("blob_id").and_then(|v| v.as_str()).unwrap();
    let blob_path: PathBuf = fixture.data_dir.path().join("blobs").join(ref_blob_id);
    assert!(blob_path.exists(), "expected blob file at {blob_path:?}");
}

#[tokio::test]
async fn image_ref_attachments_use_stored_blob_mime_type() {
    enable_queued_messages_for_test_binary();
    // Tiny 1x1 PNG.
    const PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMB/6Vn3b0AAAAASUVORK5CYII=";
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(PNG_BASE64.as_bytes())
        .unwrap();

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let boundary = "ctx-test-boundary";
    let body = multipart_body(boundary, "file", "x.png", Some("image/png"), &png_bytes);
    let req = Request::builder()
        .method("POST")
        .uri("/api/blobs")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let uploaded: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let uploaded_id = uploaded.get("blob_id").and_then(|v| v.as_str()).unwrap();

    let repo = common::init_git_repo(&[("README.md", "hello\n")]).await;
    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model").await;

    let (status, msg): (StatusCode, ctx_core::models::Message) = common::json_request(
        &app,
        Method::POST,
        format!("/api/sessions/{}/messages", session.id.0),
        Some(json!({
            "content":"hi",
            "delivery":"queued",
            "attachments":[{"kind":"image_ref","blob_id":uploaded_id,"mime_type":"text/plain","name":"x.png"}]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let att_json = serde_json::to_value(&msg.attachments[0]).unwrap();
    assert_eq!(
        att_json.get("mime_type").and_then(|v| v.as_str()),
        Some("image/png")
    );
}

#[tokio::test]
async fn blob_upload_infers_image_mime_type_from_filename_when_part_content_type_is_missing() {
    const PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMB/6Vn3b0AAAAASUVORK5CYII=";
    let png_bytes = base64::engine::general_purpose::STANDARD
        .decode(PNG_BASE64.as_bytes())
        .unwrap();

    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let boundary = "ctx-test-boundary";
    let body = multipart_body(boundary, "file", "x.png", None, &png_bytes);
    let req = Request::builder()
        .method("POST")
        .uri("/api/blobs")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let uploaded: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        uploaded.get("mime_type").and_then(|value| value.as_str()),
        Some("image/png")
    );
}

#[tokio::test]
async fn blob_upload_route_accepts_attachment_limit_and_rejects_one_byte_over() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();
    let boundary = "ctx-upload-limit-boundary";

    let bytes_at_limit = vec![0_u8; MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES];
    let body = multipart_body(
        boundary,
        "file",
        "at-limit.png",
        Some("image/png"),
        &bytes_at_limit,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/blobs")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let uploaded: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        uploaded.get("bytes").and_then(|value| value.as_i64()),
        Some(MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES as i64)
    );
    drop(bytes_at_limit);

    let bytes_one_over = vec![0_u8; MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES + 1];
    let body = multipart_body(
        boundary,
        "file",
        "too-large.png",
        Some("image/png"),
        &bytes_one_over,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/blobs")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        err.get("error").and_then(|value| value.as_str()),
        Some("Image attachments must be 25 MiB or smaller.")
    );
    drop(bytes_one_over);

    let bytes_over_multipart_limit = vec![0_u8; MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES + (128 * 1024)];
    let body = multipart_body(
        boundary,
        "file",
        "far-too-large.png",
        Some("image/png"),
        &bytes_over_multipart_limit,
    );
    let req = Request::builder()
        .method("POST")
        .uri("/api/blobs")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let err: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        err.get("error").and_then(|value| value.as_str()),
        Some("Image attachments must be 25 MiB or smaller.")
    );
}

#[tokio::test]
async fn non_image_blob_refs_are_rejected() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let blob_id = uuid::Uuid::new_v4().to_string();
    let bytes = b"not-an-image";
    fixture
        .daemon
        .seed_non_image_attachment_blob_for_test(&blob_id, bytes, "not-image.txt")
        .await
        .unwrap();

    let repo = common::init_git_repo(&[("README.md", "hello\n")]).await;
    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model").await;

    let (status, err): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/sessions/{}/messages", session.id.0),
        Some(json!({
            "content":"hi",
            "delivery":"immediate",
            "attachments":[{"kind":"image_ref","blob_id":blob_id,"mime_type":"image/png","name":"not-image.txt"}]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(
        err.get("error").and_then(|v| v.as_str()),
        Some("Only image attachments are supported.")
    );

    assert!(fixture
        .daemon
        .session_has_no_persisted_messages_for_test(session.id)
        .await
        .unwrap());
}

#[tokio::test]
async fn oversized_image_ref_attachments_are_rejected_before_turn_start() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let repo = common::init_git_repo(&[("README.md", "hello\n")]).await;
    let ws = common::create_workspace(&app, repo.path(), "ws").await;
    let (_task, session) =
        common::create_task_with_session(&app, ws.id.0, "t1", "fake", "fake-model").await;
    let blob_id = uuid::Uuid::new_v4().to_string();
    fixture
        .daemon
        .seed_oversized_image_attachment_blob_metadata_for_test(
            &blob_id,
            (MAX_MESSAGE_IMAGE_ATTACHMENT_BYTES + 1) as i64,
            "too-large.png",
        )
        .await
        .unwrap();

    let (status, err): (StatusCode, serde_json::Value) = common::json_request(
        &app,
        Method::POST,
        format!("/api/sessions/{}/messages", session.id.0),
        Some(json!({
            "content":"hi",
            "delivery":"immediate",
            "attachments":[{"kind":"image_ref","blob_id":blob_id,"mime_type":"image/png","name":"too-large.png"}]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        err.get("error").and_then(|v| v.as_str()),
        Some("Image attachments must be 25 MiB or smaller.")
    );

    assert!(fixture
        .daemon
        .session_has_no_persisted_messages_for_test(session.id)
        .await
        .unwrap());
}
