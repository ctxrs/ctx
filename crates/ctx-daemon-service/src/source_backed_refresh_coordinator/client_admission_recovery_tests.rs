use super::*;

#[derive(Default)]
struct RecordingAvailability(
    std::sync::Mutex<Vec<(crate::DaemonTrigger, crate::DaemonAvailabilityDemand)>>,
);

impl crate::DaemonAvailabilityPort for RecordingAvailability {
    fn ensure_available(
        &self,
        _data_root: &Path,
        trigger: crate::DaemonTrigger,
        demand: crate::DaemonAvailabilityDemand,
    ) -> Result<crate::DaemonAvailability> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((trigger, demand));
        Ok(crate::DaemonAvailability::Available)
    }
}

#[derive(Default)]
struct CancelledRecoveryAvailability {
    ensures: std::sync::atomic::AtomicUsize,
}

impl crate::DaemonAvailabilityPort for CancelledRecoveryAvailability {
    fn ensure_available(
        &self,
        _data_root: &Path,
        _trigger: crate::DaemonTrigger,
        _demand: crate::DaemonAvailabilityDemand,
    ) -> Result<crate::DaemonAvailability> {
        self.ensures
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(crate::DaemonAvailability::Available)
    }

    fn checkpoint(&self) -> Result<()> {
        Err(anyhow!("cancelled before recovery ensure"))
    }

    fn interrupted(&self, error: &anyhow::Error) -> bool {
        error.to_string() == "cancelled before recovery ensure"
    }
}

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
fn cancellation_before_admission_performs_no_roundtrip() {
    let mut roundtrips = 0;
    let error = request_admission_with_recovery_cancellable(
        "cancel-before-admission",
        |_| panic!("pre-admission cancellation must not sleep"),
        || Err(anyhow!("cancelled before admission")),
        || {
            roundtrips += 1;
            Ok(None)
        },
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "cancelled before admission");
    assert_eq!(roundtrips, 0);
}

#[test]
fn cancellation_during_ambiguous_admission_backoff_prevents_replay() {
    let mut roundtrips = 0;
    let mut checkpoints = 0;
    let error = recover_ambiguous_admission(
        "cancel-ambiguous-admission",
        |backoff| {
            assert_eq!(backoff, StdDuration::from_millis(25));
            Err(anyhow!("cancelled during admission backoff"))
        },
        || {
            checkpoints += 1;
            Ok(())
        },
        || {
            roundtrips += 1;
            Ok(None)
        },
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "cancelled during admission backoff");
    assert_eq!(checkpoints, 1);
    assert_eq!(roundtrips, 0);
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
        &crate::test_support::AVAILABILITY,
        Path::new("unused-after-durable-ack"),
        request_id,
        RefreshRequestTrigger::Search,
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
fn cancellation_before_recovery_ensure_never_starts_a_replacement() {
    let availability = CancelledRecoveryAvailability::default();
    let error = recover_wait_refresh_request(
        &availability,
        Path::new("unused-before-recovery"),
        "cancelled-recovery-request",
        RefreshRequestTrigger::Search,
        true,
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "cancelled before recovery ensure");
    assert_eq!(
        availability
            .ensures
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[test]
fn post_ack_daemon_recovery_reobserves_coalesced_id_before_readmission() {
    let request_id = "periodic-physical-request";
    let availability = RecordingAvailability::default();

    let recovered = recover_wait_refresh_request(
        &availability,
        Path::new("no-endpoint-needed-before-reobservation"),
        request_id,
        RefreshRequestTrigger::Search,
        true,
    )
    .unwrap();

    assert_eq!(recovered, request_id);
    assert_eq!(
        *availability
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        [(
            crate::DaemonTrigger::Search,
            crate::DaemonAvailabilityDemand::ExplicitWait,
        )]
    );
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
