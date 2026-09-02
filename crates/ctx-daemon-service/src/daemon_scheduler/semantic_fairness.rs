#[derive(Default)]
pub(crate) struct DaemonSidecarDrain {
    pub(crate) generation: Option<String>,
    pub(crate) semantic_attempted_generation: Option<String>,
    pub(crate) semantic_turn_pending: bool,
}

impl DaemonSidecarDrain {
    pub(super) fn record_core_publication(&mut self, generation: String) {
        self.generation = Some(generation);
        self.semantic_attempted_generation = None;
        self.semantic_turn_pending = true;
    }
}

pub(super) fn semantic_turn_continues(job: &serde_json::Value) -> bool {
    job.get("status").and_then(serde_json::Value::as_str) == Some("budget_exhausted")
        && job
            .get("semantic_progress_sequence")
            .and_then(serde_json::Value::as_u64)
            .is_some()
        && job
            .get("source_generation_ready")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        && job
            .get("source_work_remaining")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

pub(crate) fn restore_daemon_consumer_retries(
    runtime: &mut crate::daemon::DaemonRuntime,
    data_root: &std::path::Path,
) {
    let semantic = crate::paths_status::read_daemon_job_status(
        &crate::paths_status::daemon_semantic_job_path(data_root),
    );
    restore_consumer_retry(&mut runtime.semantic_retry, semantic.as_ref());
    let core = crate::paths_status::read_daemon_job_status(
        &crate::paths_status::daemon_core_refresh_job_path(data_root),
    );
    let core_generation = core
        .as_ref()
        .and_then(|job| job.get("published_generation"))
        .and_then(serde_json::Value::as_str);
    let semantic_generation = semantic
        .as_ref()
        .and_then(|job| job.get("core_generation_id"))
        .and_then(serde_json::Value::as_str);
    let generation = core_generation.or(semantic_generation);
    if semantic.as_ref().is_some_and(semantic_turn_continues)
        || core_generation.is_some_and(|generation| semantic_generation != Some(generation))
    {
        runtime.sidecar_drain.generation = generation.map(str::to_owned);
        runtime.sidecar_drain.semantic_turn_pending = true;
    }
}

fn restore_consumer_retry(
    backoff: &mut crate::daemon_retry::DaemonRetryBackoff,
    status: Option<&serde_json::Value>,
) {
    backoff.restore(status);
    let persisted_failures = status
        .and_then(|status| status.get("consecutive_failures"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32;
    if backoff.consecutive_failures == 0
        && persisted_failures > 0
        && status
            .and_then(|status| status.get("retryable"))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    {
        backoff.consecutive_failures = persisted_failures;
    }
}

#[derive(Clone, Copy)]
pub(super) enum DaemonSemanticCatchUpBudget {
    Drain,
    OneDurableBoundary,
}

#[derive(Clone, Copy)]
pub(super) struct DaemonSemanticGeneration<'a> {
    pub(super) source_generation:
        &'a crate::source_backed_refresh_coordinator::PinnedSourceBackedGeneration,
    pub(super) contract: &'a ctx_semantic_index::SemanticModelContract,
}

#[derive(Clone, Copy)]
pub(crate) struct DaemonSemanticJobPorts<'a> {
    pub(crate) artifact_fetcher: &'a dyn ctx_semantic_model::ArtifactFetcher,
    pub(crate) config: &'a dyn crate::DaemonConfigPort,
}

pub(crate) struct DaemonSchedulerPorts<'a, N: ?Sized> {
    pub(crate) generation_published: &'a N,
    pub(crate) semantic: DaemonSemanticJobPorts<'a>,
    pub(crate) observation: &'a dyn crate::DaemonObservationPort,
}
