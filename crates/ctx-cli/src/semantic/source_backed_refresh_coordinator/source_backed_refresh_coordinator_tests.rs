use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Barrier,
};

use ctx_history_capture::{
    DiscoveryPlatform, DiscoveryPlatformDirs, ProviderCatalogSupport, ProviderImportSupport,
    ProviderSource, ProviderSourceKind,
};

use super::*;

struct TestExecutor {
    calls: Arc<AtomicUsize>,
    generation_id: String,
    failure: Option<String>,
}

impl SourceBackedRefreshExecutor for TestExecutor {
    fn refresh(
        &self,
        execution: SourceBackedRefreshExecution<'_>,
    ) -> Result<SourceBackedRefreshPublication> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            execution.index_root,
            source_backed_index_root(execution.data_root)
        );
        assert!(!execution.request_id.is_empty());
        if let Some(error) = self.failure.as_deref() {
            return Err(anyhow!("{error}"));
        }
        execution.report_progress("refreshing", 0, 1, Some("provider-neutral".to_owned()))?;
        execution.report_progress("verifying", 1, 1, None)?;
        Ok(test_publication(self.generation_id.clone()))
    }
}

fn test_publication(generation_id: impl Into<String>) -> SourceBackedRefreshPublication {
    SourceBackedRefreshPublication {
        generation_id: generation_id.into(),
        source_manifest: None,
        resolver: None,
        scanned_routes: 1,
        unsupported_routes: 0,
        certified_source_count: 1,
        certified_source_bytes: 128,
        current: SourceBackedRefreshCurrent {
            source_count: 1,
            indexed_documents: 2,
            complete_records: 3,
            retained_records: 2,
            rejected_records: 1,
            certified_source_bytes: 128,
            sources_with_rejections: 1,
            ..SourceBackedRefreshCurrent::default()
        },
        timings: SourceBackedRefreshTimings {
            discovery_us: 11,
            scan_stage_us: 22,
            commit_us: 33,
        },
    }
}

fn test_resolver() -> Arc<SourceBackedResolverRegistry> {
    Arc::new(ctx_history_capture::SourceBackedProviderRegistry::new().resolver_registry())
}

fn request_id(response: &Value) -> String {
    response
        .get("request_id")
        .and_then(Value::as_str)
        .expect("request ID")
        .to_owned()
}

#[test]
fn explicit_catalog_request_retains_daemon_metadata_and_authority() {
    let temp = tempfile::tempdir().unwrap();
    let authority = load_explicit_source_catalog_authority(temp.path()).unwrap();
    let coordinator = SourceBackedRefreshCoordinator::new();
    let periodic = coordinator.enqueue_periodic(temp.path()).unwrap();
    let response = coordinator
        .handle_ipc_request(
            temp.path(),
            &json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "explicit_source_catalog": authority.to_json(),
            }),
        )
        .unwrap()
        .expect("source refresh response");

    assert_eq!(request_id(&response), request_id(&periodic));
    assert_eq!(response["coalesced_requests"], 1);
    assert_eq!(response["owner"], "daemon");
    assert_eq!(response["trigger"], "import");
    assert_eq!(response["trigger_provenance"], "explicit_source_catalog");
    assert_eq!(
        ExplicitSourceCatalogAuthority::from_json(&response["requested_explicit_source_catalog"])
            .unwrap(),
        authority
    );

    let request_id = request_id(&response);
    let run = coordinator
        .run_next_with(
            |_, _| Ok(test_publication("catalog-generation")),
            || Ok(Some("catalog-generation".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert!(!run.failed);
    let published = coordinator.status(&request_id).unwrap();
    assert_eq!(published["request_state"], "published");
    assert_eq!(
        ExplicitSourceCatalogAuthority::from_json(&published["published_explicit_source_catalog"])
            .unwrap(),
        authority
    );
}

#[test]
fn default_executor_invokes_one_all_provider_callback_and_maps_progress() {
    let coordinator = SourceBackedRefreshCoordinator::new();
    assert_eq!(
        coordinator.executor.implementation_name(),
        std::any::type_name::<CaptureOwnedSourceBackedRefreshExecutor>()
    );

    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let discovery = DiscoveryContext::new(
        temp.path(),
        temp.path().join("cwd"),
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    );
    let updates = Mutex::new(Vec::new());
    let report_progress = |update: SourceBackedRefreshProgressUpdate| {
        updates.lock().unwrap().push((
            update.phase,
            update.completed_sources,
            update.total_sources,
            update.current_source,
        ));
        Ok(())
    };
    let execution = SourceBackedRefreshExecution {
        data_root: &data_root,
        index_root: &index_root,
        request_id: "all-provider-request",
        report_progress: &report_progress,
    };
    let mut provider_wide_calls = 0;

    let publication = execute_capture_owned_refresh_with(
        execution,
        &discovery,
        |observed_discovery, observed_data_root, observed_index_root, progress| {
            provider_wide_calls += 1;
            assert_eq!(observed_discovery, &discovery);
            assert_eq!(observed_data_root, data_root);
            assert_eq!(observed_index_root, index_root);
            progress(CaptureSourceBackedRefreshProgress {
                phase: "discovering",
                completed_sources: 0,
                total_sources: 2,
                current_source: None,
                stage_duration: StdDuration::ZERO,
                elapsed: StdDuration::ZERO,
                certified_source_count: None,
                certified_source_bytes: None,
            })?;
            progress(CaptureSourceBackedRefreshProgress {
                phase: "refreshing",
                completed_sources: 1,
                total_sources: 2,
                current_source: Some("provider-wide-route".to_owned()),
                stage_duration: StdDuration::ZERO,
                elapsed: StdDuration::ZERO,
                certified_source_count: None,
                certified_source_bytes: None,
            })?;
            progress(CaptureSourceBackedRefreshProgress {
                phase: "verifying",
                completed_sources: 2,
                total_sources: 2,
                current_source: None,
                stage_duration: StdDuration::ZERO,
                elapsed: StdDuration::ZERO,
                certified_source_count: None,
                certified_source_bytes: None,
            })?;
            Ok(test_publication("all-provider-generation"))
        },
    )
    .unwrap();

    assert_eq!(provider_wide_calls, 1);
    assert_eq!(publication.generation_id, "all-provider-generation");
    assert_eq!(
        updates.into_inner().unwrap(),
        vec![
            ("discovering".to_owned(), 0, 0, None),
            ("discovering".to_owned(), 0, 2, None),
            (
                "refreshing".to_owned(),
                1,
                2,
                Some("provider-wide-route".to_owned()),
            ),
            ("verifying".to_owned(), 2, 2, None),
        ]
    );
}

#[test]
fn missing_roots_are_nonblocking_but_detected_selector_gaps_block_publication() {
    let source = |path: &'static str, exists, status| ProviderSource {
        provider: CaptureProvider::Warp,
        path: PathBuf::from(path),
        exists,
        source_format: "warp_sqlite",
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::Native,
        status,
        unsupported_reason: None,
    };
    let missing = SourceBackedAutomaticRegistryIssue::Unavailable {
        source: source(
            "/unavailable/warp.sqlite",
            false,
            ProviderSourceStatus::Missing,
        ),
        reason: SourceBackedAutomaticUnavailableReason::SourceStatus(ProviderSourceStatus::Missing),
    };
    assert!(reject_blocking_automatic_registry_issues(&[missing], &[]).is_ok());

    let selector_gap = SourceBackedAutomaticRegistryIssue::Unavailable {
        source: source(
            "/detected/warp.sqlite",
            true,
            ProviderSourceStatus::Available,
        ),
        reason: SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable {
            detail: "injected selector gap",
        },
    };
    let error = reject_blocking_automatic_registry_issues(&[selector_gap], &[]).unwrap_err();
    assert!(format!("{error:#}").contains("injected selector gap"));
}

#[test]
fn duplicate_concurrent_requests_launch_one_writer() {
    const REQUESTS: usize = 16;

    let coordinator = Arc::new(SourceBackedRefreshCoordinator::new());
    let barrier = Arc::new(Barrier::new(REQUESTS));
    let mut threads = Vec::new();
    for _ in 0..REQUESTS {
        let coordinator = coordinator.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            coordinator.enqueue(Some("generation-1".to_owned()))
        }));
    }
    let responses = threads
        .into_iter()
        .map(|thread| thread.join().expect("request thread"))
        .collect::<Vec<_>>();
    let expected_request_id = request_id(&responses[0]);
    assert!(responses
        .iter()
        .all(|response| request_id(response) == expected_request_id));

    let writer_launches = AtomicUsize::new(0);
    let run = coordinator
        .run_next_with(
            |request_id, coordinator| {
                writer_launches.fetch_add(1, Ordering::SeqCst);
                let _ = coordinator.set_progress(
                    request_id,
                    "refreshing",
                    0,
                    1,
                    Some("source-a".to_owned()),
                );
                Ok(test_publication("generation-2"))
            },
            || Ok(Some("generation-2".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("queued refresh");

    assert_eq!(writer_launches.load(Ordering::SeqCst), 1);
    assert!(run.did_work);
    assert!(!run.failed);
    let status = coordinator
        .status(&expected_request_id)
        .expect("published request status");
    assert_eq!(status["request_state"], "published");
    assert_eq!(status["published_generation"], "generation-2");
    assert_eq!(status["generation_changed"], true);
    assert_eq!(status["receipt"]["previous_generation"], "generation-1");
    assert_eq!(status["receipt"]["published_generation"], "generation-2");
    assert_eq!(status["receipt"]["generation_changed"], true);
    assert_eq!(status["receipt"]["current"]["current_source_count"], 1);
    assert_eq!(status["receipt"]["current"]["current_indexed_documents"], 2);
    assert_eq!(status["receipt"]["current"]["current_rejected_records"], 1);
    assert_eq!(
        status["coalesced_requests"].as_u64(),
        Some((REQUESTS - 1) as u64)
    );
    assert_eq!(status["certified_source_count"], 1);
    assert_eq!(status["certified_source_bytes"], 128);
    assert_eq!(status["timings_us"]["discovery"], 11);
    assert_eq!(status["timings_us"]["scan_stage"], 22);
    assert_eq!(status["timings_us"]["commit"], 33);
    assert!(coordinator
        .run_next_with(
            |_, _| panic!("duplicate writer launched"),
            || Ok(Some("generation-2".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .is_none());
}

#[test]
fn unchanged_nonempty_publication_is_no_op_by_generation_identity() {
    let coordinator = SourceBackedRefreshCoordinator::new();
    let request = coordinator.enqueue(Some("generation-1".to_owned()));
    let request_id = request_id(&request);
    let run = coordinator
        .run_next_with(
            |_, _| Ok(test_publication("generation-1")),
            || Ok(Some("generation-1".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("queued refresh");

    assert!(!run.failed);
    assert!(!run.did_work);
    let status = coordinator.status(&request_id).expect("published request");
    assert_eq!(status["generation_changed"], false);
    assert_eq!(status["receipt"]["generation_changed"], false);
    assert_eq!(status["receipt"]["current"]["current_source_count"], 1);
    assert_eq!(status["receipt"]["current"]["current_indexed_documents"], 2);
}

#[test]
fn ipc_job_records_source_refresh_only_search_autostart_provenance() {
    let _env_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join(crate::config::CONFIG_FILE),
        "[daemon]\nmode = \"source-refresh-only\"\n",
    )
    .unwrap();
    crate::semantic::paths_status::write_daemon_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "status": "running",
            "start_mode": "auto",
            "trigger_command": "search",
        }),
    )
    .unwrap();
    let coordinator = SourceBackedRefreshCoordinator::new();

    let response = coordinator
        .handle_ipc_request(
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "background",
            }),
        )
        .unwrap()
        .expect("source refresh response");
    let job = crate::semantic::paths_status::read_daemon_job_status(
        &daemon_source_backed_refresh_job_path(temp.path()),
    )
    .expect("persisted source refresh job");

    assert_eq!(response["daemon_mode"], "source-refresh-only");
    assert_eq!(response["trigger"], "search");
    assert_eq!(response["trigger_provenance"], "autostart");
    assert_eq!(job["daemon_mode"], "source-refresh-only");
    assert_eq!(job["trigger"], "search");
    assert_eq!(job["trigger_provenance"], "autostart");
}

#[test]
fn failed_refresh_retains_the_previous_published_generation() {
    let coordinator = SourceBackedRefreshCoordinator::new();
    let request = coordinator.enqueue(Some("generation-1".to_owned()));
    let request_id = request_id(&request);
    let run = coordinator
        .run_next_with(
            |request_id, coordinator| {
                let _ = coordinator.set_progress(
                    request_id,
                    "refreshing",
                    0,
                    1,
                    Some("source-a".to_owned()),
                );
                Err(anyhow!("injected writer failure before publication"))
            },
            || Ok(Some("generation-1".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("queued refresh");

    assert!(run.failed);
    assert!(!run.did_work);
    let status = coordinator
        .status(&request_id)
        .expect("failed request status");
    assert_eq!(status["request_state"], "failed");
    assert_eq!(status["previous_generation"], "generation-1");
    assert_eq!(status["published_generation"], "generation-1");
    assert!(status.get("generation_changed").is_none());
    assert!(status.get("receipt").is_none());
    assert!(status["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("injected writer failure")));
    assert_eq!(run.job["status"], "failed");
    assert_eq!(run.job["published_generation"], "generation-1");
    assert_eq!(run.job["progress"]["phase"], "failed");
}

#[test]
fn unverified_returned_generation_is_never_recorded_as_published() {
    let coordinator = SourceBackedRefreshCoordinator::new();
    let request = coordinator.enqueue(Some("generation-1".to_owned()));
    let request_id = request_id(&request);
    let resolver = test_resolver();
    let run = coordinator
        .run_next_with(
            |_, _| {
                let mut publication = test_publication("generation-2");
                publication.resolver = Some(resolver);
                Ok(publication)
            },
            || Ok(Some("generation-1".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("queued refresh");

    assert!(run.failed);
    assert!(!run.did_work);
    let status = coordinator
        .status(&request_id)
        .expect("failed request status");
    assert_eq!(status["request_state"], "failed");
    assert_eq!(status["previous_generation"], "generation-1");
    assert_eq!(status["published_generation"], "generation-1");
    assert!(status["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("returned generation generation-2")));
    assert!(coordinator.lock_state().published_resolver.is_none());
}

#[test]
fn verified_publication_atomically_installs_generation_bound_resolver() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let missing_coordinator = SourceBackedRefreshCoordinator::new();
    let missing = missing_coordinator
        .resolver_for_generation(&data_root, "missing-generation")
        .expect_err("missing daemon resolver must fail typed");
    assert_eq!(
        missing,
        SourceBackedResolverAccessError::Missing {
            requested_generation: "missing-generation".to_owned(),
        }
    );
    assert!(missing_coordinator.has_pending_request());

    let resolver = test_resolver();
    let executor_resolver = Arc::clone(&resolver);
    let coordinator = SourceBackedRefreshCoordinator::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?;
            let receipt = writer.commit(|_| true)?;
            let mut publication = test_publication(receipt.generation_id.clone());
            publication.source_manifest = Some(
                SourceManifest::new(receipt.generation_id, Vec::new(), Vec::new())
                    .map_err(|error| anyhow!(error.message))?,
            );
            publication.resolver = Some(Arc::clone(&executor_resolver));
            Ok(publication)
        },
    ));
    coordinator.enqueue_periodic(&data_root).unwrap();

    let run = coordinator.run_next(&data_root).expect("queued refresh");
    let generation_id = run.job["published_generation"]
        .as_str()
        .expect("published generation");
    let retained = coordinator
        .resolver_for_generation(&data_root, generation_id)
        .expect("exact generation resolver");

    assert_eq!(retained.generation_id(), generation_id);
    assert!(std::ptr::eq(retained.resolver(), resolver.as_ref()));
    assert_eq!(
        retained
            .source_manifest()
            .expect("retained source manifest")
            .core_generation_id,
        generation_id
    );
    assert!(!coordinator.has_pending_request());

    let error = coordinator
        .resolver_for_generation(&data_root, "stale-query-generation")
        .expect_err("generation mismatch must not return a resolver");
    assert_eq!(
        error,
        SourceBackedResolverAccessError::GenerationMismatch {
            requested_generation: "stale-query-generation".to_owned(),
            retained_generation: generation_id.to_owned(),
        }
    );
    assert!(coordinator.has_pending_request());
}

#[test]
fn typed_source_hydration_failure_queues_refresh_without_fallback() {
    let coordinator = SourceBackedRefreshCoordinator::new();
    let resolver = test_resolver();
    coordinator.enqueue(Some("generation-1".to_owned()));
    let run = coordinator
        .run_next_with(
            |_, _| {
                let mut publication = test_publication("generation-2");
                publication.resolver = Some(resolver);
                Ok(publication)
            },
            || Ok(Some("generation-2".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("queued refresh");
    assert!(!run.failed);
    assert!(!coordinator.has_pending_request());

    let failure = HydrationFailure {
        kind: HydrationFailureKind::StaleSourceEvidence,
        detail: "source changed after publication".to_owned(),
    };
    let returned = coordinator.handle_hydration_failure(
        Path::new("/typed-source-failure"),
        "generation-2",
        failure.clone(),
    );

    assert_eq!(returned, failure);
    assert!(coordinator.has_pending_request());
    for kind in [
        HydrationFailureKind::TemporarilyUnavailable,
        HydrationFailureKind::ConfirmedDeleted,
        HydrationFailureKind::StaleSourceEvidence,
        HydrationFailureKind::StaleRecordEvidence,
        HydrationFailureKind::MissingRecord,
    ] {
        assert!(hydration_failure_queues_refresh(kind));
    }
    assert!(!hydration_failure_queues_refresh(
        HydrationFailureKind::UnsupportedParserRevision
    ));
    assert!(!hydration_failure_queues_refresh(
        HydrationFailureKind::InvalidLocator
    ));
}

#[test]
fn verified_activation_retires_old_store_family_idempotently() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    fs::create_dir_all(&data_root).unwrap();
    let old_artifacts = [
        "work.sqlite",
        "work.sqlite-wal",
        "work.sqlite-shm",
        "work.sqlite-journal",
        "work.sqlite.event-search-bulk.lock.sqlite",
        "work.sqlite.event-search-bulk.lock.sqlite-wal",
        "vectors.sqlite",
        "vectors.sqlite-wal",
        "semantic-worker.lock",
        "semantic-worker.json.7.tmp",
        "work.sqlite.ctx-native-cold-00000000-0000-4000-8000-000000000000.sqlite",
    ]
    .map(|name| data_root.join(name));
    for path in &old_artifacts {
        fs::write(path, b"old-store-sentinel").unwrap();
    }
    for directory in ["objects", "spool", "semantic-vectors"] {
        let nested = data_root.join(directory).join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("old-history-artifact"), b"old").unwrap();
    }
    let preserved = [
        data_root.join("config.toml"),
        data_root.join("install.json"),
        data_root.join("usage.sqlite"),
        data_root.join("logs/daemon.log"),
        data_root.join("search/semantic/current"),
        data_root.join("relational/current"),
    ];
    for path in &preserved {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"current-state").unwrap();
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let executor_calls = calls.clone();
    let coordinator = SourceBackedRefreshCoordinator::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            executor_calls.fetch_add(1, Ordering::SeqCst);
            let writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?;
            let receipt = writer.commit(|_| true)?;
            Ok(test_publication(receipt.generation_id))
        },
    ));
    let queued = coordinator.enqueue_periodic(&data_root).unwrap();

    let run = coordinator.run_next(&data_root).expect("queued refresh");

    assert!(!run.failed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(queued["daemon_mode"], "full");
    assert_eq!(queued["trigger"], "periodic");
    assert_eq!(queued["trigger_provenance"], "daemon_scheduler");
    assert!(source_backed_index_root(&data_root)
        .join("meta.json")
        .is_file());
    for path in &old_artifacts {
        assert!(
            !path.exists(),
            "verified activation retained {}",
            path.display()
        );
    }
    for directory in ["objects", "spool", "semantic-vectors"] {
        assert!(data_root
            .join(directory)
            .join("nested/old-history-artifact")
            .is_file());
    }
    for path in preserved {
        assert_eq!(fs::read(path).unwrap(), b"current-state");
    }
    remove_old_store_family(&data_root).expect("cleanup is idempotent");
    assert!(run.job["published_generation"].is_string());
}

#[cfg(unix)]
#[test]
fn old_store_cleanup_preserves_all_directory_targets() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&data_root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"outside").unwrap();
    let database = ctx_history_core::database_path(data_root.clone());
    fs::write(&database, b"old-store").unwrap();
    symlink(&outside, data_root.join("objects")).unwrap();

    remove_old_store_family(&data_root).unwrap();

    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"outside");
    assert!(!database.exists());
    assert!(data_root.join("objects").is_symlink());
}

#[test]
fn verified_generation_mismatch_never_retires_old_store() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let writer = ctx_history_index::GenerationWriter::open(
        source_backed_index_root(&data_root),
        WriterOptions::default(),
    )
    .unwrap();
    let receipt = writer.commit(|_| true).unwrap();
    let database = ctx_history_core::database_path(data_root.clone());
    fs::write(&database, b"old-store").unwrap();

    let error = complete_verified_source_epoch(&data_root, "different-generation")
        .expect_err("generation mismatch must block retirement");

    assert!(format!("{error:#}").contains(&receipt.generation_id));
    assert_eq!(fs::read(database).unwrap(), b"old-store");
}

#[test]
fn failed_pre_activation_refresh_leaves_old_store_bytes_for_forward_retry() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    fs::create_dir_all(&data_root).unwrap();
    let database = ctx_history_core::database_path(data_root.clone());
    fs::write(&database, b"old-store-before-failed-activation").unwrap();
    let coordinator = SourceBackedRefreshCoordinator::with_executor(Arc::new(TestExecutor {
        calls: Arc::new(AtomicUsize::new(0)),
        generation_id: "unused-generation".to_owned(),
        failure: Some("provider-neutral executor failed".to_owned()),
    }));
    coordinator.enqueue_periodic(&data_root).unwrap();

    let run = coordinator.run_next(&data_root).expect("queued refresh");

    assert!(run.failed);
    assert!(run.job["published_generation"].is_null());
    assert!(run.job["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("provider-neutral executor failed")));
    assert_eq!(
        fs::read(&database).unwrap(),
        b"old-store-before-failed-activation"
    );
    assert!(open_published_generation(&data_root).unwrap().is_none());
}

#[test]
fn cold_failed_writer_artifacts_are_retried_as_no_prior_generation() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    fs::create_dir_all(&data_root).unwrap();
    let database = ctx_history_core::database_path(data_root.clone());
    let wal = PathBuf::from(format!("{}-wal", database.display()));
    fs::write(&database, b"old-store").unwrap();
    fs::write(&wal, b"old-wal").unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let executor_attempts = attempts.clone();
    let coordinator = SourceBackedRefreshCoordinator::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?;
            if executor_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(anyhow!("injected cold writer failure before commit"));
            }
            let receipt = writer.commit(|_| true)?;
            Ok(test_publication(receipt.generation_id))
        },
    ));

    let first_request = coordinator.enqueue_periodic(&data_root).unwrap();
    let first_run = coordinator.run_next(&data_root).expect("first refresh");
    assert!(first_run.failed);
    assert!(first_run.job["published_generation"].is_null());
    assert!(source_backed_index_root(&data_root)
        .join("meta.json")
        .is_file());
    assert!(matches!(
        VerifiedIndex::open(source_backed_index_root(&data_root)),
        Err(IndexError::MissingCommitPayload)
    ));
    assert!(pin_published_generation(&data_root).unwrap().is_none());
    assert_eq!(fs::read(&database).unwrap(), b"old-store");
    assert_eq!(fs::read(&wal).unwrap(), b"old-wal");

    let retry_request = coordinator
        .enqueue_periodic(&data_root)
        .expect("incomplete cold generation must enqueue for retry");
    let retry_run = coordinator.run_next(&data_root).expect("retry refresh");

    assert_ne!(request_id(&first_request), request_id(&retry_request));
    assert!(!retry_run.failed);
    assert!(retry_run.did_work);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let published = retry_run.job["published_generation"]
        .as_str()
        .expect("retry publication");
    let pinned = pin_published_generation(&data_root)
        .unwrap()
        .expect("verified retry generation");
    assert_eq!(pinned.generation_id(), published);
    assert!(!database.exists());
    assert!(!wal.exists());
}

#[test]
fn restart_reconciliation_finishes_commit_before_cleanup_without_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let index_root = source_backed_index_root(&data_root);
    let writer =
        ctx_history_index::GenerationWriter::open(&index_root, WriterOptions::default()).unwrap();
    let receipt = writer.commit(|_| true).unwrap();
    let database = ctx_history_core::database_path(data_root.clone());
    let wal = PathBuf::from(format!("{}-wal", database.display()));
    fs::write(&database, b"old-store-after-commit").unwrap();
    fs::write(&wal, b"old-wal-after-commit").unwrap();

    reconcile_verified_source_epoch(&data_root).unwrap();
    reconcile_verified_source_epoch(&data_root).unwrap();

    assert_eq!(
        VerifiedIndex::open(&index_root).unwrap().generation_id(),
        receipt.generation_id
    );
    assert!(!database.exists());
    assert!(!wal.exists());
}

#[test]
fn active_generation_pin_fails_closed_when_core_state_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let error = match pin_active_verified_generation(temp.path()) {
        Ok(_) => panic!("missing Core state must not fall back to a helper receipt"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").starts_with("source_unavailable:"));
}

#[test]
fn activated_generation_missing_commit_payload_remains_typed_corruption() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let coordinator = SourceBackedRefreshCoordinator::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let writer = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?;
            let receipt = writer.commit(|_| true)?;
            Ok(test_publication(receipt.generation_id))
        },
    ));
    coordinator.enqueue_periodic(&data_root).unwrap();
    let run = coordinator.run_next(&data_root).expect("initial refresh");
    assert!(!run.failed);
    write_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root), &run.job).unwrap();

    let meta_path = source_backed_index_root(&data_root).join("meta.json");
    let mut meta: Value = serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    assert!(meta.as_object_mut().unwrap().remove("payload").is_some());
    std::fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();

    let error = coordinator
        .enqueue_periodic(&data_root)
        .expect_err("activated generation corruption must fail closed");
    assert!(matches!(
        error.downcast_ref::<IndexError>(),
        Some(IndexError::MissingCommitPayload)
    ));
    assert!(!coordinator.has_pending_request());

    let error = match pin_active_verified_generation(&data_root) {
        Ok(_) => panic!("corrupt active Core state must fail closed before blame"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").starts_with("source_unavailable:"));
}
