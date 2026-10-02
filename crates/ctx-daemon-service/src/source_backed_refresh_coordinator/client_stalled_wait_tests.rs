use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
struct AdvancingAvailability(AtomicU64);

impl crate::DaemonAvailabilityPort for AdvancingAvailability {
    fn ensure_available(
        &self,
        _: &Path,
        _: crate::DaemonTrigger,
        _: crate::DaemonAvailabilityDemand,
    ) -> Result<crate::DaemonAvailability> {
        bail!("a responsive daemon must not be restarted")
    }

    fn pause(&self, _: StdDuration) -> Result<()> {
        if self.0.load(Ordering::SeqCst) >= 600 {
            bail!("test cancelled refresh wait")
        }
        self.0.fetch_add(150, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn responsive_stalled_wait_is_cancellable_without_discarding_the_durable_request() -> Result<()> {
    let data_root = short_data_root()?;
    ctx_history_platform::platform_security::establish_private_data_root(data_root.path())?;
    let engine = Arc::new(CoreRefreshEngine::new());
    let server_engine = engine.clone();
    let server_root = data_root.path().to_owned();
    let request_id = Uuid::new_v4().to_string();
    let availability = AdvancingAvailability::default();
    let start = StdInstant::now();
    let intent = exhaustive_import_intent(RefreshSelection::All);
    let (result, exchanges) = foreground_transport_fixture(
        data_root.path(),
        move |request| {
            server_engine
                .handle_ipc_request(&server_root, request)?
                .context("wire response")
        },
        || -> Result<SourceBackedRefreshObservation> {
            enqueue_equivalent_wait_refresh_request(
                &availability,
                data_root.path(),
                &request_id,
                intent.clone(),
                RefreshRequestTrigger::Import,
            )?;
            let recovery = WaitRefreshRecovery::new(
                data_root.path(),
                &RefreshRequest::new(
                    request_id.clone(),
                    intent.clone(),
                    RefreshRequestTrigger::Import,
                ),
                true,
            )?;
            wait_for_published_generation_inner(
                &availability,
                data_root.path(),
                request_id.clone(),
                PublishedGenerationWait {
                    mode: SourceBackedRefreshMode::Wait,
                    intent,
                    retain_peer: false,
                    report_progress: None,
                },
                recovery,
                None,
                || start + StdDuration::from_secs(availability.0.load(Ordering::SeqCst)),
            )
        },
    )?;
    let error = result.err().context("cancelled observation must fail")?;
    assert_eq!(error.to_string(), "test cancelled refresh wait");
    assert_eq!(availability.0.load(Ordering::SeqCst), 600);
    assert_eq!(
        exchanges
            .iter()
            .filter(|(request, _)| request["op"] == SOURCE_REFRESH_REQUEST_OP)
            .count(),
        1,
        "stalled observation must not resubmit the request"
    );
    assert!(engine.has_pending_request());
    assert_eq!(
        engine.status(&request_id).unwrap()["request_state"],
        "admission_pending"
    );
    let recovered = CoreRefreshEngine::new();
    assert!(recovered.recover_interrupted_publication(data_root.path())?);
    assert_eq!(
        recovered.status(&request_id).unwrap()["request_state"],
        "admission_pending"
    );
    Ok(())
}

#[test]
fn quiet_long_phases_wait_for_the_authoritative_terminal_result() -> Result<()> {
    for (phase, publish) in [
        ("refreshing", true),
        ("refreshing", false),
        ("merging", true),
        ("merging", false),
    ] {
        let data_root = short_data_root()?;
        ctx_history_platform::platform_security::establish_private_data_root(data_root.path())?;
        let source = tempfile::tempdir()?;
        let path = source.path().join("source.jsonl");
        std::fs::write(
            &path,
            "{\"record_type\":\"manifest\",\"schema_version\":\"ctx-history-jsonl-v2\"}\n",
        )?;
        let source =
            ctx_history_refresh::explicit_source_for_path(data_root.path(), &path, None, true)?;
        let authority =
            ctx_history_refresh::upsert_explicit_source(data_root.path(), &source)?.authority;
        let intent = exhaustive_import_intent(RefreshSelection::ExactSource(authority));
        let request_id = Uuid::new_v4().to_string();
        let server_root = data_root.path().to_owned();
        let engine = CoreRefreshEngine::new();
        let mut polls = 0_u64;
        let availability = AdvancingAvailability::default();
        let start = StdInstant::now();
        let (result, exchanges) = foreground_transport_fixture(
            data_root.path(),
            move |request| {
                if request["op"] == SOURCE_REFRESH_STATUS_OP {
                    polls += 1;
                    if polls <= 4 {
                        return Ok(json!({
                            "ok": true, "schema_version": 1, "owner": "daemon",
                            "request_id": request["request_id"], "request_state": "running",
                            "progress": {
                                "phase": phase, "completed_sources": 0, "total_sources": 1,
                                "completed_records": 0, "completed_bytes": 0,
                                "elapsed_millis": 0,
                            },
                        }));
                    }
                    if !publish {
                        return Ok(json!({
                            "ok": true, "schema_version": 1, "owner": "daemon",
                            "request_id": request["request_id"], "request_state": "failed",
                            "last_error": "synthetic worker failure",
                            "progress": {"phase": "failed", "completed_sources": 0, "total_sources": 1},
                        }));
                    }
                    assert!(engine.prepare_next_pending_admission(&server_root)?);
                    let run = engine
                        .run_next(&server_root)
                        .context("terminal refresh run")?;
                    assert!(!run.failed, "{:#}", run.job);
                }
                engine
                    .handle_ipc_request(&server_root, request)?
                    .context("wire response")
            },
            || {
                enqueue_equivalent_wait_refresh_request(
                    &availability,
                    data_root.path(),
                    &request_id,
                    intent.clone(),
                    RefreshRequestTrigger::Import,
                )?;
                let recovery = WaitRefreshRecovery::new(
                    data_root.path(),
                    &RefreshRequest::new(
                        request_id.clone(),
                        intent.clone(),
                        RefreshRequestTrigger::Import,
                    ),
                    true,
                )?;
                wait_for_published_generation_inner(
                    &availability,
                    data_root.path(),
                    request_id.clone(),
                    PublishedGenerationWait {
                        mode: SourceBackedRefreshMode::Wait,
                        intent,
                        retain_peer: false,
                        report_progress: None,
                    },
                    recovery,
                    None,
                    || start + StdDuration::from_secs(availability.0.load(Ordering::SeqCst)),
                )
            },
        )?;
        assert_eq!(availability.0.load(Ordering::SeqCst), 600);
        assert_eq!(exchanges.len(), 6, "one admission and five status polls");
        if publish {
            let observed = result?;
            assert_eq!(observed.status, "published");
            assert_eq!(observed.request_id.as_deref(), Some(request_id.as_str()));
            assert_eq!(
                observed.pin.generation_id(),
                exchanges.last().unwrap().1["published_generation"]
                    .as_str()
                    .unwrap()
            );
        } else {
            let error = result
                .err()
                .context("terminal failure must fail the client")?;
            assert!(
                error.to_string().contains("synthetic worker failure"),
                "{error:#}"
            );
        }
    }
    Ok(())
}

#[test]
fn foreground_unknown_recovery_is_bounded_and_rejects_mismatched_identity() -> Result<()> {
    for mutation in [
        None,
        Some(("request_id", json!("different-request"))),
        Some(("owner", json!("different-owner"))),
        Some(("schema_version", json!(2))),
        Some(("ok", json!(true))),
        Some(("request_state", json!("failed"))),
        Some(("reason", json!("unrecognized-reason"))),
        Some(("retryable", json!(true))),
        Some(("retryable", Value::Null)),
        Some(("error_code", Value::Null)),
        Some((
            "admission_durability",
            json!("replacement_visible_or_indeterminate"),
        )),
        Some((
            "admission_acknowledgement",
            json!("retained_after_durability_error"),
        )),
        Some(("receipt", json!({}))),
        Some(("receipt", Value::Null)),
        Some(("published_generation", json!("generation"))),
        Some(("previous_generation", json!("previous"))),
        Some(("generation_changed", json!(false))),
        Some(("outcome", json!("published"))),
        Some(("structured_outcome", json!({}))),
        Some(("finished_at_ms", json!(123))),
    ] {
        let malformed = mutation.is_some();
        let data_root = short_data_root()?;
        let (error, exchanges) = foreground_transport_fixture(
            data_root.path(),
            move |request| {
                let id = request["request_id"].as_str().context("client ID")?;
                Ok(if request["op"] == SOURCE_REFRESH_REQUEST_OP {
                    json!({"ok":true,"owner":"daemon","request_id":id,"request_state":"admission_pending","schema_version":1,
                        "progress":{"phase":"admission_pending","completed_sources":0,"total_sources":0}})
                } else {
                    let mut response = json!({"ok":false,"owner":"daemon","request_id":id,
                        "request_state":"request_unknown","error_code":"source_refresh_request_unknown",
                        "reason":"request_not_retained_after_restart","retryable":false,"schema_version":1,
                        "error":"arbitrary human text cannot authorize recovery",
                        "diagnostic_extension":"informational"});
                    if let Some((field, value)) = &mutation {
                        response[*field] = value.clone();
                    }
                    response
                })
            },
            || {
                coordinate_source_backed_refresh(
                    &RecordingAvailability::default(),
                    data_root.path(),
                    SourceBackedRefreshMode::Wait,
                )
                .err()
                .expect("unobservable or mismatched request must fail")
            },
        )?;
        assert_eq!(exchanges.len(), if malformed { 2 } else { 4 });
        if !malformed {
            assert_eq!(exchanges[0].0, exchanges[2].0);
            let typed = error
                .downcast_ref::<SourceRefreshObservationRecoveryFailed>()
                .expect("bounded unknown recovery");
            assert_eq!(
                typed.request_id,
                exchanges[0].0["request_id"].as_str().unwrap()
            );
            assert_eq!(typed.recovery_attempts, 1);
        }
    }
    Ok(())
}
