use super::*;

fn admitted_response(request_id: &str, request_state: &str) -> Value {
    compact_json(json!({
        "ok": true,
        "schema_version": 1,
        "owner": "daemon",
        "request_id": request_id,
        "request_state": request_state,
    }))
}

#[test]
fn definite_pre_submission_error_is_preserved_without_retry() {
    let request_id = "019fcaaa-0000-7000-8000-000000000294";
    let mut observed_backoffs = Vec::new();
    let mut observed_request_ids = Vec::new();

    let error = request_admission_with_recovery(
        request_id,
        |backoff| observed_backoffs.push(backoff),
        || {
            observed_request_ids.push(request_id);
            Err(anyhow!("parse daemon source refresh endpoint"))
        },
    )
    .unwrap_err();

    assert!(observed_backoffs.is_empty());
    assert_eq!(observed_request_ids, [request_id]);
    assert_eq!(error.to_string(), "parse daemon source refresh endpoint");
    assert!(error
        .downcast_ref::<SourceRefreshAdmissionRecoveryFailed>()
        .is_none());
}

#[test]
fn admission_pending_is_a_non_terminal_protocol_state() {
    let request_id = "019fcaaa-0000-7000-8000-000000000295";
    let response = admitted_response(request_id, "admission_pending");

    validate_source_refresh_status_response_authority(&response, request_id).unwrap();
    assert_eq!(
        source_refresh_protocol_state(&response).unwrap(),
        RefreshRequestState::AdmissionPending
    );
}

#[test]
fn missing_endpoint_before_any_roundtrip_remains_genuinely_unavailable() {
    let request_id = "019fcaaa-0000-7000-8000-000000000297";
    let mut roundtrips = 0;
    let response = request_admission_with_recovery(
        request_id,
        |_| panic!("genuine unavailability must not enter ambiguous recovery"),
        || {
            roundtrips += 1;
            Ok(None)
        },
    )
    .unwrap();

    assert!(response.is_none());
    assert_eq!(roundtrips, 1);
    let Err(error) = daemon_unavailable_fallback(
        Path::new("unused-for-wait-mode"),
        SourceBackedRefreshMode::Wait,
        None,
    ) else {
        panic!("wait mode must reject a genuinely missing daemon endpoint");
    };
    assert!(error
        .downcast_ref::<SourceBackedRefreshDaemonUnavailable>()
        .is_some());
}

#[test]
fn no_daemon_post_ack_recovery_is_typed_retained_and_unobservable() {
    let request_id = "019fcaaa-0000-7000-8000-000000000296";
    let error = recover_wait_refresh_request(
        Path::new("unused-after-durable-ack"),
        request_id,
        SourceBackedRefreshOperation::Refresh,
        None,
        false,
        false,
    )
    .unwrap_err();

    let retained = error
        .downcast_ref::<observation_recovery::SourceRefreshObservationRecoveryFailed>()
        .expect("post-ack endpoint loss must retain request identity");
    assert_eq!(retained.request_id, request_id);
    assert_eq!(retained.recovery_attempts, 0);
    assert_eq!(
        retained.disconnect_policy,
        observation_recovery::DISCONNECT_POLICY
    );
    assert!(error
        .downcast_ref::<SourceBackedRefreshDaemonUnavailable>()
        .is_none());
    assert!(!error
        .to_string()
        .contains("no foreground writer was started"));
}

#[test]
fn disabled_daemon_post_ack_recovery_preserves_stable_request_identity() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    std::fs::write(
        data_root.join(crate::config::CONFIG_FILE),
        "[daemon]\nenabled = false\n",
    )
    .unwrap();
    let request_id = "019fcaaa-0000-7000-8000-0000000002b1";

    let error = recover_wait_refresh_request(
        &data_root,
        request_id,
        SourceBackedRefreshOperation::Refresh,
        None,
        false,
        true,
    )
    .unwrap_err();

    let retained = error
        .downcast_ref::<observation_recovery::SourceRefreshObservationRecoveryFailed>()
        .expect("disabled post-ack recovery remains request-bound");
    assert_eq!(retained.request_id, request_id);
    assert_eq!(
        retained.disconnect_policy,
        observation_recovery::DISCONNECT_POLICY
    );
    assert!(format!("{error:#}").contains("daemon was disabled"));
    assert!(error
        .downcast_ref::<SourceBackedRefreshDaemonUnavailable>()
        .is_none());
}

#[test]
fn malformed_config_post_ack_recovery_preserves_stable_request_identity() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    std::fs::write(data_root.join(crate::config::CONFIG_FILE), "[daemon\n").unwrap();
    let request_id = "019fcaaa-0000-7000-8000-0000000002b2";

    let error = recover_wait_refresh_request(
        &data_root,
        request_id,
        SourceBackedRefreshOperation::Refresh,
        None,
        false,
        true,
    )
    .unwrap_err();

    let retained = error
        .downcast_ref::<observation_recovery::SourceRefreshObservationRecoveryFailed>()
        .expect("configuration failure remains request-bound after acknowledgement");
    assert_eq!(retained.request_id, request_id);
    assert_eq!(
        retained.disconnect_policy,
        observation_recovery::DISCONNECT_POLICY
    );
    assert!(format!("{error:#}").contains("load daemon configuration"));
}

#[test]
fn typed_initial_service_unavailability_preserves_existing_fallback() {
    let request_id = "019fcaaa-0000-7000-8000-000000000298";
    let mut roundtrips = 0;
    let error = request_admission_with_recovery(
        request_id,
        |_| panic!("typed initial unavailability must not enter ambiguous recovery"),
        || {
            roundtrips += 1;
            Err(DaemonSourceRefreshServiceUnavailable.into())
        },
    )
    .unwrap_err();

    assert_eq!(roundtrips, 1);
    assert!(error
        .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
        .is_some());
}
