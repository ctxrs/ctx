use super::*;
use ctx_http_auth::{
    derive_browser_capability_token, derive_browser_query_secret, BrowserCapabilityAuthScope,
};

#[tokio::test]
async fn blob_download_requires_browser_capability_query_token() {
    let fixture = AuthBoundaryFixture::new().await;
    let app = fixture.app();
    let blob_id = "blob-auth-boundary";

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/blobs/{blob_id}?token=daemon-secret"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/blobs/{blob_id}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let expires_at = chrono::Utc::now().timestamp() + 60 * 60;
    let scoped_token = derive_browser_capability_token(
        "daemon-secret",
        &BrowserCapabilityAuthScope::Blob {
            blob_id: blob_id.to_string(),
        },
        expires_at,
    );
    let browser_query_secret = derive_browser_query_secret("daemon-secret");
    let scoped_browser_secret_token = derive_browser_capability_token(
        &browser_query_secret,
        &BrowserCapabilityAuthScope::Blob {
            blob_id: blob_id.to_string(),
        },
        expires_at,
    );
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/blobs/{blob_id}?expires_at={expires_at}&token={scoped_token}"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/blobs/{blob_id}?expires_at={expires_at}&token={scoped_browser_secret_token}"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/blobs/{blob_id}?expires_at={expires_at}&token={scoped_token}"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let expired_at = chrono::Utc::now().timestamp() - 1;
    let expired_token = derive_browser_capability_token(
        "daemon-secret",
        &BrowserCapabilityAuthScope::Blob {
            blob_id: blob_id.to_string(),
        },
        expired_at,
    );
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/blobs/{blob_id}?expires_at={expired_at}&token={expired_token}"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let too_far_expires_at = chrono::Utc::now().timestamp() + 2 * 60 * 60;
    let too_far_token = derive_browser_capability_token(
        "daemon-secret",
        &BrowserCapabilityAuthScope::Blob {
            blob_id: blob_id.to_string(),
        },
        too_far_expires_at,
    );
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/blobs/{blob_id}?expires_at={too_far_expires_at}&token={too_far_token}"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn session_artifact_download_requires_browser_capability_query_token() {
    let fixture = AuthBoundaryFixture::new().await;
    let app = fixture.app();
    let session_id = "11111111-1111-1111-1111-111111111111";
    let artifact_id = "22222222-2222-2222-2222-222222222222";

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/sessions/{session_id}/artifacts/{artifact_id}?token=daemon-secret"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/sessions/{session_id}/artifacts/{artifact_id}"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let expires_at = chrono::Utc::now().timestamp() + 60 * 60;
    let scoped_token = derive_browser_capability_token(
        "daemon-secret",
        &BrowserCapabilityAuthScope::SessionArtifact {
            session_id: session_id.to_string(),
            artifact_id: artifact_id.to_string(),
        },
        expires_at,
    );
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/sessions/{session_id}/artifacts/{artifact_id}?expires_at={expires_at}&token={scoped_token}"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/sessions/{session_id}/artifacts/{artifact_id}?expires_at={expires_at}&token={scoped_token}"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
