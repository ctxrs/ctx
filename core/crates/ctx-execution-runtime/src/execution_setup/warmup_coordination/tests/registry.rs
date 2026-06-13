use super::*;

#[test]
fn prewarm_job_registry_matches_compatible_jobs_by_scope_and_image() {
    let settings = container_settings("ghcr.io/ctxrs/ctx-harness:test");
    let other_settings = container_settings("ghcr.io/ctxrs/ctx-harness:other");

    let all_job = Arc::new(SharedPrewarmLaunchJob::new(
        "job-all".to_string(),
        &settings,
        RuntimePrewarmScope::All,
    ));
    let builder_job = Arc::new(SharedPrewarmLaunchJob::new(
        "job-builder".to_string(),
        &settings,
        RuntimePrewarmScope::Builder,
    ));
    let other_runtime_job = Arc::new(SharedPrewarmLaunchJob::new(
        "job-other-runtime".to_string(),
        &other_settings,
        RuntimePrewarmScope::Runtime,
    ));

    let mut registry = PrewarmJobRegistry::default();
    registry.insert(Arc::clone(&all_job));
    registry.insert(Arc::clone(&builder_job));
    registry.insert(Arc::clone(&other_runtime_job));

    let runtime_match = registry
        .find_compatible(&settings, RuntimePrewarmScope::Runtime)
        .expect("runtime request should reuse matching all-scope job");
    assert!(Arc::ptr_eq(&runtime_match, &all_job));

    let builder_match = registry
        .find_compatible(&settings, RuntimePrewarmScope::Builder)
        .expect("builder request should prefer exact builder job");
    assert!(Arc::ptr_eq(&builder_match, &builder_job));

    let launch_ready_match = registry
        .find_compatible(&settings, RuntimePrewarmScope::LaunchReady)
        .expect("launch-ready request should reuse matching all-scope job");
    assert!(Arc::ptr_eq(&launch_ready_match, &all_job));

    let all_match = registry
        .find_compatible(&settings, RuntimePrewarmScope::All)
        .expect("all-scope request should match all-scope job");
    assert!(Arc::ptr_eq(&all_match, &all_job));

    let promoted_other_match = registry
        .find_compatible(&other_settings, RuntimePrewarmScope::All)
        .expect("all-scope request should reuse matching runtime job for the same image");
    assert!(Arc::ptr_eq(&promoted_other_match, &other_runtime_job));
}

#[test]
fn prewarm_job_registry_promotes_runtime_jobs_for_launch_ready_and_builder_requests() {
    let settings = container_settings("ghcr.io/ctxrs/ctx-harness:test");
    let runtime_job = Arc::new(SharedPrewarmLaunchJob::new(
        "job-runtime".to_string(),
        &settings,
        RuntimePrewarmScope::Runtime,
    ));

    let mut registry = PrewarmJobRegistry::default();
    registry.insert(Arc::clone(&runtime_job));

    let launch_ready_match = registry
        .find_compatible(&settings, RuntimePrewarmScope::LaunchReady)
        .expect("launch-ready request should reuse matching runtime job");
    assert!(Arc::ptr_eq(&launch_ready_match, &runtime_job));

    let builder_match = registry
        .find_compatible(&settings, RuntimePrewarmScope::Builder)
        .expect("builder request should reuse matching runtime job");
    assert!(Arc::ptr_eq(&builder_match, &runtime_job));

    let all_match = registry
        .find_compatible(&settings, RuntimePrewarmScope::All)
        .expect("all-scope request should reuse matching runtime job");
    assert!(Arc::ptr_eq(&all_match, &runtime_job));
}

#[test]
fn prewarm_job_registry_remove_if_current_only_removes_exact_pointer() {
    let settings = container_settings("ghcr.io/ctxrs/ctx-harness:test");
    let stale_job = Arc::new(SharedPrewarmLaunchJob::new(
        "job-stale".to_string(),
        &settings,
        RuntimePrewarmScope::Runtime,
    ));
    let current_job = Arc::new(SharedPrewarmLaunchJob::new(
        "job-current".to_string(),
        &settings,
        RuntimePrewarmScope::Runtime,
    ));
    let key = PrewarmLaunchJobKey::for_request(&settings, RuntimePrewarmScope::Runtime);

    let mut registry = PrewarmJobRegistry::default();
    registry.insert(Arc::clone(&stale_job));
    registry.insert(Arc::clone(&current_job));

    registry.remove_if_current(&key, &stale_job);
    let still_present = registry
        .find_compatible(&settings, RuntimePrewarmScope::Runtime)
        .expect("current job should remain after stale-pointer removal");
    assert!(Arc::ptr_eq(&still_present, &current_job));

    registry.remove_if_current(&key, &current_job);
    assert!(
        registry
            .find_compatible(&settings, RuntimePrewarmScope::Runtime)
            .is_none(),
        "current pointer removal should clear the registry entry"
    );
}

#[test]
fn shared_prewarm_launch_job_terminal_completion_is_idempotent() {
    let settings = container_settings("ghcr.io/ctxrs/ctx-harness:test");
    let job = SharedPrewarmLaunchJob::new(
        "job-terminal".to_string(),
        &settings,
        RuntimePrewarmScope::Runtime,
    );

    assert!(job.runtime_requested());
    assert!(!job.requires_launch_ready_runtime());
    assert!(!job.builder_requested());
    assert!(job.complete_ready().is_some());

    let snapshot = job.snapshot();
    assert_eq!(snapshot.state, ExecutionLaunchState::Ready);
    assert!(snapshot.error.is_none());

    assert!(
        job.complete_error("should not apply".to_string()).is_none(),
        "second terminal completion should be ignored"
    );

    let snapshot_after = job.snapshot();
    assert_eq!(snapshot_after.state, ExecutionLaunchState::Ready);
    assert!(snapshot_after.error.is_none());
}

#[test]
fn shared_prewarm_launch_job_scope_requests_merge_monotonically() {
    let settings = container_settings("ghcr.io/ctxrs/ctx-harness:test");
    let job = SharedPrewarmLaunchJob::new(
        "job-scope".to_string(),
        &settings,
        RuntimePrewarmScope::Runtime,
    );

    assert!(job.request_scope(RuntimePrewarmScope::Builder));
    assert!(job.runtime_requested());
    assert!(job.builder_requested());
    assert!(!job.requires_launch_ready_runtime());

    assert!(job.request_scope(RuntimePrewarmScope::LaunchReady));
    assert!(job.runtime_requested());
    assert!(job.builder_requested());
    assert!(job.requires_launch_ready_runtime());
}
