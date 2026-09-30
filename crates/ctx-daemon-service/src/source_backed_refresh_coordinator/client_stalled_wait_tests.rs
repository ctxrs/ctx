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
    let intent = RefreshIntent::SelectedImport(RefreshSelection::All);
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
            wait_for_published_generation_inner(
                &availability,
                data_root.path(),
                request_id.clone(),
                PublishedGenerationWait {
                    mode: SourceBackedRefreshMode::Wait,
                    intent,
                    trigger: RefreshRequestTrigger::Import,
                    allow_daemon_autostart: true,
                    retain_peer: false,
                    report_progress: None,
                },
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
        let intent = RefreshIntent::SelectedImport(RefreshSelection::ExactSource(authority));
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
                wait_for_published_generation_inner(
                    &availability,
                    data_root.path(),
                    request_id.clone(),
                    PublishedGenerationWait {
                        mode: SourceBackedRefreshMode::Wait,
                        intent,
                        trigger: RefreshRequestTrigger::Import,
                        allow_daemon_autostart: true,
                        retain_peer: false,
                        report_progress: None,
                    },
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
