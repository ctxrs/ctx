use super::*;

#[tokio::test]
async fn mobile_api_tokens_still_authorize_mobile_registration() {
    let fixture = AuthBoundaryFixture::new().await;

    let state = fixture.daemon();

    let token = "ctxm_test_mobile_token";
    state
        .mobile_access_for_test()
        .seed_mobile_api_profile_for_test(token, &["device_registration"])
        .await
        .unwrap();

    let app = fixture.app();
    let req = Request::builder()
        .method("POST")
        .uri("/api/mobile/register")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "device_id": "11111111-1111-1111-1111-111111111111",
                "device_label": "test phone"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn mobile_api_tokens_without_device_registration_scope_reject_registration() {
    let fixture = AuthBoundaryFixture::new().await;

    let state = fixture.daemon();

    let token = "ctxm_test_mobile_token";
    state
        .mobile_access_for_test()
        .seed_mobile_api_profile_for_test(token, &["workspace_read"])
        .await
        .unwrap();

    let app = fixture.app();
    let req = Request::builder()
        .method("POST")
        .uri("/api/mobile/register")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "device_id": "11111111-1111-1111-1111-111111111111",
                "device_label": "test phone"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"],
        "mobile profile lacks device_registration scope"
    );
}

#[tokio::test]
async fn legacy_empty_scope_mobile_tokens_migrate_to_default_scopes_on_registration() {
    let fixture = AuthBoundaryFixture::new().await;

    let state = fixture.daemon();

    let token = "ctxm_test_mobile_token";
    let profile = state
        .mobile_access_for_test()
        .seed_mobile_api_profile_for_test(token, &[])
        .await
        .unwrap();

    let app = fixture.app();
    let req = Request::builder()
        .method("POST")
        .uri("/api/mobile/register")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "device_id": "12121212-1212-1212-1212-121212121212",
                "device_label": "test phone"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let profile = state
        .mobile_access_for_test()
        .mobile_profile_for_test(profile.id)
        .await
        .unwrap()
        .expect("profile should still exist");
    assert_eq!(
        profile.scopes,
        vec![
            "device_registration".to_string(),
            "workspace_read".to_string(),
            "workspace_stream".to_string(),
        ]
    );
}
