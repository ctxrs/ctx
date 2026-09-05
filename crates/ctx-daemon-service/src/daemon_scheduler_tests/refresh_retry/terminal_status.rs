use super::*;

struct TerminalPersistenceFixture {
    temp: tempfile::TempDir,
    coordinator: CoreRefreshEngine,
    request_id: String,
    executions: Arc<AtomicUsize>,
}

impl TerminalPersistenceFixture {
    fn new(provider_fails: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
        publish_empty_core_generation(&data_root);
        let status_path = daemon_core_refresh_job_path(&data_root);
        let saved_status = temp.path().join("running.json");
        let executions = Arc::new(AtomicUsize::new(0));
        let execution_count = Arc::clone(&executions);
        let coordinator = CoreRefreshEngine::with_executor(Arc::new(
            move |execution: SourceBackedRefreshExecution<'_>| {
                execution_count.fetch_add(1, Ordering::SeqCst);
                let result = if provider_fails {
                    Err(anyhow::anyhow!("provider capture failed"))
                } else {
                    Ok(publish_empty_authoritative_generation(&execution))
                };
                // Preserve the real running journal and obstruct only its replacement.
                // The production journal, not a callback returning a synthetic error,
                // must reject the directory at its regular status-file destination.
                std::fs::rename(&status_path, &saved_status)?;
                std::fs::create_dir(&status_path)?;
                result
            },
        ));
        let queued = coordinator.enqueue_periodic(&data_root).unwrap();
        let request_id = queued["request_id"].as_str().unwrap().to_owned();
        coordinator
            .complete_pending_admission_for_test(&data_root, &request_id, BTreeMap::new())
            .unwrap();
        Self {
            temp,
            coordinator,
            request_id,
            executions,
        }
    }

    fn data_root(&self) -> std::path::PathBuf {
        self.temp.path().join("data")
    }

    fn restore_journal(&self) {
        let path = daemon_core_refresh_job_path(&self.data_root());
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(self.temp.path().join("running.json"), path).unwrap();
    }

    fn status(&self) -> Value {
        self.coordinator
            .handle_ipc_request(
                &self.data_root(),
                &json!({"op": "source_refresh_status", "request_id": self.request_id}),
            )
            .unwrap()
            .unwrap()
    }
}

fn assert_waiting_for_terminal(status: Value, request_id: &str) {
    assert_eq!(status["request_id"], request_id);
    assert_eq!(status["request_state"], "running");
    assert_eq!(status["logical_phase"], "direct");
    assert_eq!(status["physical_attempt_state"], "running");
    assert_eq!(status["progress_owner_attempt_state"], "running");
    assert_eq!(status["progress"]["phase"], "persisting_terminal");
    assert_eq!(status["progress"]["whole_run_stage"], "activation");
    for field in [
        "receipt",
        "outcome",
        "structured_outcome",
        "finished_at_ms",
        "last_error",
    ] {
        assert!(
            status.get(field).is_none(),
            "premature terminal field {field}"
        );
    }
    let parsed = ctx_history_refresh::RefreshStatus::parse_schema_v1(status).unwrap();
    assert!(!parsed.kind().unwrap().request_state().is_terminal());
    assert!(parsed.kind().unwrap().terminal_outcome().is_none());
}

#[test]
fn terminal_status_waits_for_real_journal_replacement_and_scheduler_retry() {
    for provider_fails in [false, true] {
        let fixture = TerminalPersistenceFixture::new(provider_fails);
        let data_root = fixture.data_root();
        let mut runtime = source_refresh_only_runtime();
        let notification = FailingGenerationPublished::default();
        let first = run_pending_core_refresh(
            &data_root,
            &mut runtime,
            Some(&fixture.coordinator),
            true,
            &notification,
            &crate::test_support::OBSERVATION,
        )
        .unwrap()
        .unwrap();
        assert!(first.failed);
        assert!(!first.did_work);
        assert!(first.provider_refresh_events.is_empty());
        assert_eq!(notification.calls.load(Ordering::SeqCst), 0);
        assert!(fixture.coordinator.has_pending_request());
        assert_waiting_for_terminal(fixture.status(), &fixture.request_id);

        let generation = ctx_history_refresh::pin_published_generation(&data_root)
            .unwrap()
            .unwrap();
        let published_id = generation.verified_index().generation_id().to_owned();
        // This exercises the verified lexical reader while completion is pending.
        assert!(complete_lexical_candidates(generation.verified_index(), "missing", 1).is_empty());
        assert_eq!(fixture.status()["published_generation"], published_id);
        assert_eq!(fixture.executions.load(Ordering::SeqCst), 1);

        fixture.restore_journal();
        let retry = run_pending_core_refresh(
            &data_root,
            &mut runtime,
            Some(&fixture.coordinator),
            true,
            &notification,
            &crate::test_support::OBSERVATION,
        )
        .unwrap()
        .unwrap();
        assert_eq!(retry.failed, provider_fails);
        assert_eq!(fixture.executions.load(Ordering::SeqCst), 1);
        assert_eq!(
            notification.calls.load(Ordering::SeqCst),
            usize::from(!provider_fails)
        );
        assert!(!fixture.coordinator.has_pending_request());
        let status = fixture.status();
        let durable = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
        assert_eq!(
            status["request_state"],
            if provider_fails {
                "failed"
            } else {
                "published"
            }
        );
        for field in [
            "request_id",
            "request_state",
            "published_generation",
            "receipt",
            "structured_outcome",
        ] {
            assert_eq!(status[field], durable[field], "terminal {field}");
        }
        assert_eq!(status["published_generation"], published_id);
    }
}

#[test]
fn terminal_status_persistence_failure_replays_same_request_after_process_loss() {
    let fixture = TerminalPersistenceFixture::new(false);
    let data_root = fixture.data_root();
    let mut runtime = source_refresh_only_runtime();
    let iteration = run_source_refresh_cycle(&data_root, &mut runtime, &fixture.coordinator);
    assert!(iteration.failed);
    assert_waiting_for_terminal(fixture.status(), &fixture.request_id);
    let generation_id = fixture.status()["published_generation"]
        .as_str()
        .unwrap()
        .to_owned();
    fixture.restore_journal();
    let request_id = fixture.request_id.clone();
    // Retain the filesystem, but discard all in-memory terminal retry state.
    let TerminalPersistenceFixture {
        temp: _temp,
        coordinator,
        ..
    } = fixture;
    drop(coordinator);
    let restarted = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            assert_eq!(execution.request_id, request_id);
            assert_eq!(
                ctx_history_refresh::pin_published_generation(&data_root)?
                    .unwrap()
                    .generation_id(),
                generation_id,
            );
            Ok(publish_empty_authoritative_generation(&execution))
        },
    ));
    let data_root = _temp.path().join("data");
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    let replay = restarted.run_next(&data_root).unwrap();
    assert!(!replay.failed, "{:#}", replay.job);
    assert!(!replay.terminal_persistence_pending);
    assert_eq!(replay.job["request_state"], "published");
    assert_eq!(
        read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap()["request_id"],
        replay.job["request_id"],
    );
}
