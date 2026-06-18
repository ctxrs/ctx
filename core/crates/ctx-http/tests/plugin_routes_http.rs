use axum::http::{Method, StatusCode};
use serde_json::Value;

mod common;

#[tokio::test]
async fn plugin_routes_are_registered_through_http_router() {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let app = fixture.router();

    let (status, inventory): (StatusCode, Value) =
        common::json_request(&app, Method::GET, "/api/plugins", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(inventory.get("plugins").and_then(Value::as_array).is_some());

    let (status, extensions): (StatusCode, Value) =
        common::json_request(&app, Method::GET, "/api/plugins/extensions", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(extensions
        .get("registry")
        .and_then(Value::as_object)
        .is_some());

    let (status, reloaded): (StatusCode, Value) =
        common::json_request(&app, Method::POST, "/api/plugins/reload", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(reloaded.get("plugins").and_then(Value::as_array).is_some());
}
