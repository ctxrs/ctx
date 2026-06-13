use super::fixtures::test_state;
use super::*;

#[tokio::test]
async fn mirrored_prerequisite_progress_does_not_inherit_source_byte_completion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let state = test_state(&temp).await;

    let (source_install_id, _) = state
        .start_install("acp-crp-bridge".to_string(), Some(InstallTarget::Container))
        .await;
    let (mirror_install_id, _) = state
        .start_install("cursor".to_string(), Some(InstallTarget::Container))
        .await;

    assert!(
        state
            .register_install_progress_mirror(source_install_id, mirror_install_id)
            .await
    );

    state
        .emit_install_event(
            source_install_id,
            InstallProgressEvent {
                install_id: source_install_id,
                provider_id: "acp-crp-bridge".to_string(),
                target: Some(InstallTarget::Container),
                at: chrono::Utc::now(),
                stage: "download".to_string(),
                message: "downloading…".to_string(),
                level: InstallEventLevel::Info,
                bytes: Some(10),
                total_bytes: Some(10),
                attempt: Some(1),
                error_code: None,
            },
        )
        .await;

    let mirror_info = state
        .get_install_polling_info(mirror_install_id)
        .await
        .expect("mirror install info");

    assert!(matches!(mirror_info.state, InstallStateKind::Running));
    assert_eq!(mirror_info.progress_pct, Some(2));
    assert_eq!(
        mirror_info
            .last_event
            .as_ref()
            .map(|event| event.stage.as_str()),
        Some("start")
    );
    assert_eq!(
        mirror_info
            .last_event
            .as_ref()
            .and_then(|event| event.bytes),
        None
    );
    assert_eq!(
        mirror_info
            .last_event
            .as_ref()
            .map(|event| event.message.contains("Prerequisite acp-crp-bridge")),
        Some(true)
    );
}
