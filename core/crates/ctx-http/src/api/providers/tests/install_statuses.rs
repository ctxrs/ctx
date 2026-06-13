use super::*;

#[tokio::test]
async fn get_install_statuses_returns_known_and_missing_installs_in_request_order() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;
    let daemon = fixture.daemon();

    let (install_id, started_new) = daemon
        .start_install("codex".to_string(), Some(InstallTarget::Container))
        .await;
    assert!(started_new);
    daemon
        .emit_install_event(
            install_id,
            InstallProgressEvent {
                install_id,
                provider_id: "codex".to_string(),
                target: Some(InstallTarget::Container),
                at: Utc::now(),
                stage: "download".to_string(),
                message: "Downloading runtime".to_string(),
                level: InstallEventLevel::Info,
                bytes: Some(64),
                total_bytes: Some(128),
                attempt: Some(1),
                error_code: None,
            },
        )
        .await;
    let missing_install_id = InstallId::new_v4();

    let Json(resp) = get_install_statuses(
        State(fixture.provider_install()),
        Json(ProviderInstallStatusesRouteRequest::new(vec![
            install_id.to_string(),
            missing_install_id.to_string(),
        ])),
    )
    .await
    .expect("get install statuses should succeed");

    assert_eq!(resp.installs().len(), 2);
    assert_eq!(resp.installs()[0].install_id(), install_id.to_string());
    let info = resp.installs()[0]
        .info()
        .expect("known install should return status");
    assert_eq!(info.provider_id, "codex");
    assert_eq!(info.target, Some(InstallTarget::Container));
    assert!(matches!(
        info.state,
        ctx_provider_install::install_state::InstallStateKind::Running
    ));
    assert_eq!(
        info.last_event.as_ref().map(|event| event.stage.as_str()),
        Some("download")
    );
    assert_eq!(
        resp.installs()[1].install_id(),
        missing_install_id.to_string()
    );
    assert!(resp.installs()[1].info().is_none());
}

#[tokio::test]
async fn get_install_statuses_rejects_invalid_install_ids() {
    let fixture = crate::test_support::TestDaemonFixture::new("http://127.0.0.1:4310").await;

    let err = get_install_statuses(
        State(fixture.provider_install()),
        Json(ProviderInstallStatusesRouteRequest::new(vec![
            "not-a-uuid".to_string()
        ])),
    )
    .await
    .expect_err("invalid install id should fail");
    let (status, Json(body)) = err;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid install id: not-a-uuid");
}
