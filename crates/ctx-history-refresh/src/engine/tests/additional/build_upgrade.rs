use super::*;

#[test]
fn automatic_retry_records_the_application_version() {
    let workspace = include_str!("../../../../../../Cargo.toml");
    let package = workspace.split_once("[workspace.package]").unwrap().1;
    let version = package
        .lines()
        .find_map(|line| line.strip_prefix("version = "))
        .unwrap()
        .trim_matches('"');
    assert_eq!(SOURCE_REFRESH_BUILD_VERSION, version);
}

// Driven by automatic_retry_upgrade_test with two independently compiled
// binaries. No checkpoint field or source observation is edited between runs.
#[test]
#[ignore = "requires the two-build Bazel harness"]
fn automatic_retry_across_builds() {
    let root = PathBuf::from(std::env::var_os("CTX_TEST_RETRY_UPGRADE_ROOT").unwrap());
    let phase = std::env::var("CTX_TEST_RETRY_UPGRADE_PHASE").unwrap();
    let expected_version = std::env::var("CTX_TEST_RETRY_UPGRADE_VERSION").unwrap();
    assert_eq!(SOURCE_REFRESH_BUILD_VERSION, expected_version);
    let data_root = root.join("data");
    let source_path = root.join("history.jsonl");
    if phase == "pause" {
        ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
        fs::write(&source_path, b"stable\n").unwrap();
        publish_pin_source(
            &source_backed_index_root(&data_root),
            publication_pin_source(),
        );
    }
    let (catalog, route) = automatic_retry_catalog(source_path);
    let calls = Arc::new(AtomicUsize::new(0));
    let coordinator = CoreRefreshEngine::with_executor_and_admitted_routes(
        failing_executor(Arc::clone(&calls)),
        [route.clone()],
    );
    if phase != "pause" {
        assert!(!coordinator
            .recover_interrupted_publication(&data_root)
            .unwrap());
    }
    coordinator.install_watch_catalog(catalog);
    match phase.as_str() {
        "pause" => {
            run_due_failure(&coordinator, &data_root, &route);
            let paused = run_due_failure(&coordinator, &data_root, &route);
            assert_eq!(paused.job["automatic_retry"]["state"], "paused");
            assert_eq!(
                paused.job["automatic_retry"]["routes"][route.as_str()]["build_version"],
                expected_version
            );
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        }
        "same-build" => {
            coordinator.schedule_startup_route_reconciliation(
                [route],
                automatic_retry_test_watermark(),
                ledger_now_ms().saturating_sub(1_000),
            );
            assert!(!coordinator
                .enqueue_next_dirty_route(&data_root, u64::MAX)
                .unwrap());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
        "upgrade" => {
            assert!(coordinator.has_scheduled_route_work());
            let retried = run_due_failure(&coordinator, &data_root, &route);
            assert_eq!(retried.job["automatic_retry"]["state"], "confirming");
            let checkpoint = &retried.job["automatic_retry"]["routes"][route.as_str()];
            assert_eq!(checkpoint["build_version"], expected_version);
            assert_eq!(checkpoint["matching_failures"], 1);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
        _ => panic!("unexpected upgrade test phase: {phase}"),
    }
}
