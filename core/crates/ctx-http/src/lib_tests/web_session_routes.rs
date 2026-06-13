use super::*;

mod fixtures;

use fixtures::WebSessionRouteFixture;

#[tokio::test]
async fn web_session_routes_are_registered() {
    let fixture = WebSessionRouteFixture::new(None).await;
    let app = fixture.app();

    let req = Request::builder()
        .method("POST")
        .uri("/api/sessions/web")
        .header("content-type", "application/json")
        .body(Body::from(json!({"url": ""}).to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        json!({"error":"url is required"})
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/sessions/web")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"url": "", "session_id": "not-a-uuid"}).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        json!({"error":"url is required"})
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/sessions/web")
        .header("content-type", "application/json")
        .body(Body::from(json!({"url": "file:///etc/passwd"}).to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        json!({"error":"url must use http:// or https://"})
    );

    let req = Request::builder()
        .method("POST")
        .uri("/api/sessions/web/does-not-exist/stream_token")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let req = Request::builder()
        .method("GET")
        .uri("/sessions/web/does-not-exist/view")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("GET")
        .uri("/sessions/web/does-not-exist/view?token=stream-token")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();

    let res = client
        .get(format!("http://{addr}/sessions/web/does-not-exist/signal"))
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let res = client
        .get(format!(
            "http://{addr}/sessions/web/does-not-exist/signal?token=stream-token"
        ))
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    server.abort();
}

#[tokio::test]
async fn missing_web_session_api_routes_return_not_found() {
    let fixture = WebSessionRouteFixture::new(Some("daemon-secret")).await;
    let app = fixture.app();

    let req = Request::builder()
        .method("POST")
        .uri("/api/sessions/web/does-not-exist/stream_token")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("GET")
        .uri("/api/sessions/web/does-not-exist")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert!(body.is_empty());

    let req = Request::builder()
        .method("POST")
        .uri("/api/sessions/web/does-not-exist/run")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .header("content-type", "application/json")
        .body(Body::from(json!({"code":"1 + 1"}).to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert!(body.is_empty());

    let req = Request::builder()
        .method("POST")
        .uri("/api/sessions/web/does-not-exist/eval")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .header("content-type", "application/json")
        .body(Body::from(json!({"code":"1 + 1"}).to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert!(body.is_empty());

    let req = Request::builder()
        .method("POST")
        .uri("/api/sessions/web/does-not-exist/close")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert!(body.is_empty());

    let req = Request::builder()
        .method("POST")
        .uri("/api/sessions/web/does-not-exist/stream_token")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn web_session_list_rejects_invalid_session_filter() {
    let fixture = WebSessionRouteFixture::new(Some("daemon-secret")).await;
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/sessions/web?session_id=not-a-uuid")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert!(body.is_empty());
}
