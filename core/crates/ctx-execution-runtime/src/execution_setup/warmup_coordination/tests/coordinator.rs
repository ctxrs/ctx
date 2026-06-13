use super::*;

#[tokio::test]
async fn shared_runtime_warmup_is_deduplicated_for_same_image() {
    let ops = Arc::new(FakeWarmupOperations::blocking_runtime());
    let coordinator = LaunchPrewarmCoordinator::new(ops.clone());
    let settings = container_settings("ghcr.io/ctxrs/ctx-harness:test");

    let coord_a = coordinator.clone();
    let settings_a = settings.clone();
    let first = tokio::spawn(async move {
        coord_a
            .ensure_scope(&settings_a, RuntimePrewarmScope::Runtime, None)
            .await
    });

    ops.wait_for_runtime_runs(1).await;

    let coord_b = coordinator.clone();
    let settings_b = settings.clone();
    let second = tokio::spawn(async move {
        coord_b
            .ensure_scope(&settings_b, RuntimePrewarmScope::Runtime, None)
            .await
    });

    ops.expect_runtime_runs_below(2).await;
    ops.release_runtime();

    first
        .await
        .expect("first runtime wait failed")
        .expect("first runtime wait errored");
    second
        .await
        .expect("second runtime wait failed")
        .expect("second runtime wait errored");
}

#[tokio::test]
async fn runtime_warmup_does_not_deduplicate_different_images() {
    let ops = Arc::new(FakeWarmupOperations::blocking_runtime());
    let coordinator = LaunchPrewarmCoordinator::new(ops.clone());
    let settings_a = container_settings("ghcr.io/ctxrs/ctx-harness:test-a");
    let settings_b = container_settings("ghcr.io/ctxrs/ctx-harness:test-b");

    let coord_a = coordinator.clone();
    let first = tokio::spawn(async move {
        coord_a
            .ensure_scope(&settings_a, RuntimePrewarmScope::Runtime, None)
            .await
    });

    ops.wait_for_runtime_runs(1).await;

    let coord_b = coordinator.clone();
    let second = tokio::spawn(async move {
        coord_b
            .ensure_scope(&settings_b, RuntimePrewarmScope::Runtime, None)
            .await
    });

    ops.wait_for_runtime_runs(2).await;
    ops.release_runtime();

    first
        .await
        .expect("first runtime wait failed")
        .expect("first runtime wait errored");
    second
        .await
        .expect("second runtime wait failed")
        .expect("second runtime wait errored");

    assert_eq!(ops.runtime_runs.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn runtime_scope_does_not_invoke_builder_warmup() {
    let ops = Arc::new(FakeWarmupOperations::default());
    let coordinator = LaunchPrewarmCoordinator::new(ops.clone());
    let settings = container_settings("ghcr.io/ctxrs/ctx-harness:test");

    coordinator
        .ensure_scope(&settings, RuntimePrewarmScope::Runtime, None)
        .await
        .expect("runtime scope should succeed");

    assert_eq!(ops.runtime_runs.load(Ordering::SeqCst), 1);
    assert_eq!(ops.launch_ready_runs.load(Ordering::SeqCst), 0);
    assert_eq!(ops.builder_runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn late_runtime_joiner_replays_progress_updates() {
    let ops = Arc::new(FakeWarmupOperations::blocking_runtime());
    let coordinator = LaunchPrewarmCoordinator::new(ops.clone());
    let settings = container_settings("ghcr.io/ctxrs/ctx-harness:test");
    let first_observer = Arc::new(RecordingObserver::default());

    let background_coordinator = coordinator.clone();
    let background_settings = settings.clone();
    let background_observer = first_observer.clone();
    let first = tokio::spawn(async move {
        background_coordinator
            .ensure_scope(
                &background_settings,
                RuntimePrewarmScope::Runtime,
                Some(background_observer.as_ref()),
            )
            .await
    });

    ops.wait_for_runtime_runs(1).await;

    let second_observer = Arc::new(RecordingObserver::default());
    let join_coordinator = coordinator.clone();
    let join_settings = settings.clone();
    let join_observer = second_observer.clone();
    let second = tokio::spawn(async move {
        join_coordinator
            .ensure_scope(
                &join_settings,
                RuntimePrewarmScope::Runtime,
                Some(join_observer.as_ref()),
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if !second_observer
                .progress
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("timed out waiting for replayed progress");

    let replayed = second_observer
        .progress
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert!(replayed.iter().any(|update| {
        update.phase == HarnessSetupPhase::ArtifactDownload
            && update
                .active_download
                .as_ref()
                .map(|download| download.downloaded_bytes == 512)
                .unwrap_or(false)
    }));

    ops.release_runtime();

    first
        .await
        .expect("first runtime wait failed")
        .expect("first runtime wait errored");
    second
        .await
        .expect("second runtime wait failed")
        .expect("second runtime wait errored");
}

#[tokio::test]
async fn foreground_runtime_request_finishes_before_background_all_builder_work() {
    let ops = Arc::new(FakeWarmupOperations::blocking_runtime_and_builder());
    let coordinator = LaunchPrewarmCoordinator::new(ops.clone());
    let settings = container_settings("ghcr.io/ctxrs/ctx-harness:test");

    let background_coordinator = coordinator.clone();
    let background_settings = settings.clone();
    let background = tokio::spawn(async move {
        background_coordinator
            .ensure_scope(&background_settings, RuntimePrewarmScope::All, None)
            .await
    });

    ops.wait_for_launch_ready_runs(1).await;

    let foreground_coordinator = coordinator.clone();
    let foreground_settings = settings.clone();
    let foreground = tokio::spawn(async move {
        foreground_coordinator
            .ensure_scope(&foreground_settings, RuntimePrewarmScope::Runtime, None)
            .await
    });

    ops.expect_runtime_runs_below(2).await;
    assert_eq!(ops.runtime_runs.load(Ordering::SeqCst), 0);

    ops.release_launch_ready();
    ops.wait_for_builder_runs(1).await;

    tokio::time::timeout(Duration::from_secs(1), foreground)
        .await
        .expect("foreground runtime wait timed out")
        .expect("foreground runtime task failed")
        .expect("foreground runtime wait errored");

    assert!(!background.is_finished());

    ops.release_builder();

    background
        .await
        .expect("background all wait failed")
        .expect("background all wait errored");
}

#[tokio::test]
async fn runtime_scope_reuses_running_launch_ready_task_for_same_target() {
    let ops = Arc::new(FakeWarmupOperations::blocking_launch_ready());
    let coordinator = LaunchPrewarmCoordinator::new(ops.clone());
    let settings = container_settings("ghcr.io/ctxrs/ctx-harness:test");

    let background_coordinator = coordinator.clone();
    let background_settings = settings.clone();
    let background = tokio::spawn(async move {
        background_coordinator
            .ensure_scope(&background_settings, RuntimePrewarmScope::LaunchReady, None)
            .await
    });

    ops.wait_for_launch_ready_runs(1).await;

    let foreground_coordinator = coordinator.clone();
    let foreground_settings = settings.clone();
    let foreground = tokio::spawn(async move {
        foreground_coordinator
            .ensure_scope(&foreground_settings, RuntimePrewarmScope::Runtime, None)
            .await
    });

    ops.expect_runtime_runs_below(1).await;
    ops.release_launch_ready();

    foreground
        .await
        .expect("foreground runtime wait failed")
        .expect("foreground runtime wait errored");
    background
        .await
        .expect("background launch-ready wait failed")
        .expect("background launch-ready wait errored");

    assert_eq!(ops.launch_ready_runs.load(Ordering::SeqCst), 1);
    assert_eq!(ops.runtime_runs.load(Ordering::SeqCst), 0);
}
