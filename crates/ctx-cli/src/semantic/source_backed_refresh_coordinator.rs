use std::{
    collections::VecDeque,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_capture::{
    build_automatic_source_backed_registry, DiscoveryContext, ProviderSourceStatus,
    SourceBackedAutomaticRegistryIssue, SourceBackedAutomaticUnavailableReason,
    SourceBackedRefreshProgress as CaptureSourceBackedRefreshProgress,
    SourceBackedResolverRegistry, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteResult,
};
#[cfg(test)]
use ctx_history_core::CaptureProvider;
use ctx_history_core::{
    utc_now, CertifiedSource, HydrationFailure, HydrationFailureKind, ScannedSourceCounts,
};
use ctx_history_index::{IndexError, VerifiedIndex, WriterOptions};
use ctx_pro_host_protocol::{SourceManifest, SourceRemoval};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    commands::import::{
        load_explicit_source_catalog_authority, register_explicit_source_catalog_routes,
        ExplicitSourceCatalogAuthority,
    },
    compact_json,
    config::{AppConfig, DaemonMode},
    identity,
    upgrade::data_migration::{self, MigrationPhase},
};

use super::{
    paths_status::{
        daemon_source_backed_refresh_job_path, read_daemon_status, write_daemon_job_status,
    },
    query_service::{daemon_source_refresh_request, DaemonSourceRefreshServiceUnavailable},
};

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";
const SOURCE_REFRESH_REQUEST_OP: &str = "source_refresh_request";
const SOURCE_REFRESH_STATUS_OP: &str = "source_refresh_status";
const SOURCE_REFRESH_ATTEMPT_HISTORY: usize = 64;
const SOURCE_REFRESH_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
const SOURCE_REFRESH_IPC_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const SOURCE_REFRESH_RESPONSE_MAX_BYTES: u64 = 64 * 1024;
const SOURCE_REFRESH_BUILD_ISSUE_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum SourceBackedRefreshMode {
    Off,
    Background,
    Wait,
}

impl SourceBackedRefreshMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Background => "background",
            Self::Wait => "wait",
        }
    }

    pub(crate) fn coordinate_explicit_source_catalog(
        self,
        data_root: &Path,
        authority: &ExplicitSourceCatalogAuthority,
    ) -> Result<SourceBackedRefreshObservation> {
        coordinate_source_backed_refresh_with_catalog(data_root, self, Some(authority))
    }
}

#[derive(Debug)]
pub(crate) struct SourceBackedRefreshDaemonUnavailable {
    detail: Option<String>,
}

impl SourceBackedRefreshDaemonUnavailable {
    fn new(detail: Option<String>) -> Self {
        Self { detail }
    }
}

impl fmt::Display for SourceBackedRefreshDaemonUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the ctx daemon is unavailable for source-backed refresh")?;
        if let Some(detail) = self.detail.as_deref() {
            write!(formatter, ": {detail}")?;
        }
        formatter.write_str("; no foreground writer was started")
    }
}

impl std::error::Error for SourceBackedRefreshDaemonUnavailable {}

/// Provider-neutral publication returned by the capture-owned refresh
/// executor after it atomically advances the source-backed generation.
#[derive(Debug, Clone)]
pub(crate) struct SourceBackedRefreshPublication {
    pub(crate) generation_id: String,
    /// Exact metadata-only Pro handoff for this Core generation. Test
    /// executors may omit it; the capture-owned production executor never does.
    pub(crate) source_manifest: Option<SourceManifest>,
    /// Resolver built from the exact automatic registry used for this
    /// publication. Production refreshes always supply it; injected test
    /// executors may omit it when resolver behavior is irrelevant.
    pub(crate) resolver: Option<Arc<SourceBackedResolverRegistry>>,
    pub(crate) scanned_routes: usize,
    pub(crate) unsupported_routes: usize,
    pub(crate) certified_source_count: usize,
    pub(crate) certified_source_bytes: u64,
    pub(crate) current: SourceBackedRefreshCurrent,
    pub(crate) timings: SourceBackedRefreshTimings,
}

/// Exact cardinalities of the generation that was verified after publication.
///
/// These are current-state facts, not deltas attributed to one refresh.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshCurrent {
    pub(crate) source_count: usize,
    pub(crate) indexed_documents: u64,
    pub(crate) complete_records: u64,
    pub(crate) retained_records: u64,
    pub(crate) rejected_records: u64,
    pub(crate) ignored_records: u64,
    pub(crate) certified_source_bytes: u64,
    pub(crate) sources_with_rejections: usize,
    pub(crate) removed_source_count: usize,
}

impl SourceBackedRefreshCurrent {
    fn from_sources(sources: &[CertifiedSource], removed_source_count: usize) -> Result<Self> {
        let mut current = Self {
            source_count: sources.len(),
            removed_source_count,
            ..Self::default()
        };
        for source in sources {
            let counts = source.counts();
            current.add_counts(counts)?;
            current.sources_with_rejections = current
                .sources_with_rejections
                .checked_add(usize::from(counts.rejected_records > 0))
                .ok_or_else(|| anyhow!("source-backed current rejection-source count overflow"))?;
        }
        Ok(current)
    }

    fn add_counts(&mut self, counts: ScannedSourceCounts) -> Result<()> {
        self.indexed_documents =
            checked_current_count(self.indexed_documents, counts.indexed_documents)?;
        self.complete_records =
            checked_current_count(self.complete_records, counts.complete_records)?;
        self.retained_records =
            checked_current_count(self.retained_records, counts.retained_records)?;
        self.rejected_records =
            checked_current_count(self.rejected_records, counts.rejected_records)?;
        self.ignored_records = checked_current_count(self.ignored_records, counts.ignored_records)?;
        self.certified_source_bytes =
            checked_current_count(self.certified_source_bytes, counts.certified_bytes)?;
        Ok(())
    }

    fn to_json(self) -> Value {
        json!({
            "current_source_count": self.source_count,
            "current_indexed_documents": self.indexed_documents,
            "current_complete_records": self.complete_records,
            "current_retained_records": self.retained_records,
            "current_rejected_records": self.rejected_records,
            "current_ignored_records": self.ignored_records,
            "current_certified_source_bytes": self.certified_source_bytes,
            "current_sources_with_rejections": self.sources_with_rejections,
            "removed_source_count": self.removed_source_count,
        })
    }
}

fn checked_current_count(current: u64, next: u64) -> Result<u64> {
    current
        .checked_add(next)
        .ok_or_else(|| anyhow!("source-backed current generation count overflow"))
}

/// Verified terminal receipt for one daemon-owned source refresh.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshReceipt {
    pub(crate) previous_generation: Option<String>,
    pub(crate) published_generation: String,
    pub(crate) generation_changed: bool,
    pub(crate) current: SourceBackedRefreshCurrent,
}

impl SourceBackedRefreshReceipt {
    fn to_json(&self) -> Value {
        compact_json(json!({
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "generation_changed": self.generation_changed,
            "current": self.current.to_json(),
        }))
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(crate) struct SourceBackedRefreshTimings {
    pub(crate) discovery_us: u64,
    pub(crate) scan_stage_us: u64,
    pub(crate) commit_us: u64,
}

struct SourceBackedRefreshProgressUpdate {
    phase: String,
    completed_sources: usize,
    total_sources: usize,
    current_source: Option<String>,
}

/// Daemon-owned execution context passed to the capture refresh callback.
///
/// The callback owns source/provider discovery and publication. The daemon
/// owns request serialization, progress persistence, and publication
/// verification.
#[allow(dead_code)] // All fields are part of the capture-coordinator callback seam.
pub(crate) struct SourceBackedRefreshExecution<'a> {
    pub(crate) data_root: &'a Path,
    pub(crate) index_root: &'a Path,
    pub(crate) request_id: &'a str,
    report_progress: &'a dyn Fn(SourceBackedRefreshProgressUpdate) -> Result<()>,
}

impl SourceBackedRefreshExecution<'_> {
    pub(crate) fn report_progress(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        current_source: Option<String>,
    ) -> Result<()> {
        (self.report_progress)(SourceBackedRefreshProgressUpdate {
            phase: phase.to_owned(),
            completed_sources,
            total_sources,
            current_source,
        })
    }
}

/// Provider-neutral callback boundary for one daemon-serialized refresh.
pub(crate) trait SourceBackedRefreshExecutor: Send + Sync {
    fn refresh(
        &self,
        execution: SourceBackedRefreshExecution<'_>,
    ) -> Result<SourceBackedRefreshPublication>;

    #[cfg(test)]
    fn implementation_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }
}

impl<F> SourceBackedRefreshExecutor for F
where
    F: for<'a> Fn(SourceBackedRefreshExecution<'a>) -> Result<SourceBackedRefreshPublication>
        + Send
        + Sync,
{
    fn refresh(
        &self,
        execution: SourceBackedRefreshExecution<'_>,
    ) -> Result<SourceBackedRefreshPublication> {
        self(execution)
    }
}

#[derive(Debug, Default)]
struct CaptureOwnedSourceBackedRefreshExecutor;

impl SourceBackedRefreshExecutor for CaptureOwnedSourceBackedRefreshExecutor {
    fn refresh(
        &self,
        execution: SourceBackedRefreshExecution<'_>,
    ) -> Result<SourceBackedRefreshPublication> {
        execute_capture_owned_refresh(execution)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum SourceBackedRefreshState {
    Queued,
    Running,
    Published,
    Failed,
}

impl SourceBackedRefreshState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Published => "published",
            Self::Failed => "failed",
        }
    }

    fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone)]
struct SourceBackedRefreshProgress {
    phase: String,
    completed_sources: usize,
    total_sources: usize,
    current_source: Option<String>,
}

impl Default for SourceBackedRefreshProgress {
    fn default() -> Self {
        Self {
            phase: "queued".to_owned(),
            completed_sources: 0,
            total_sources: 0,
            current_source: None,
        }
    }
}

impl SourceBackedRefreshProgress {
    fn to_json(&self) -> Value {
        compact_json(json!({
            "phase": self.phase,
            "completed_sources": self.completed_sources,
            "total_sources": self.total_sources,
            "current_source": self.current_source,
        }))
    }
}

#[derive(Debug, Clone)]
struct SourceBackedRefreshAttempt {
    request_id: String,
    state: SourceBackedRefreshState,
    requested_at_ms: i64,
    started_at_ms: Option<i64>,
    finished_at_ms: Option<i64>,
    previous_generation: Option<String>,
    published_generation: Option<String>,
    requested_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    coalesced_requests: u64,
    progress: SourceBackedRefreshProgress,
    scanned_routes: Option<usize>,
    unsupported_routes: Option<usize>,
    certified_source_count: Option<usize>,
    certified_source_bytes: Option<u64>,
    receipt: Option<SourceBackedRefreshReceipt>,
    timings: Option<SourceBackedRefreshTimings>,
    daemon_mode: DaemonMode,
    trigger: &'static str,
    trigger_provenance: &'static str,
    last_error: Option<String>,
}

impl SourceBackedRefreshAttempt {
    fn to_json(&self) -> Value {
        compact_json(json!({
            "ok": true,
            "schema_version": 1,
            "owner": "daemon",
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "requested_at_ms": self.requested_at_ms,
            "started_at_ms": self.started_at_ms,
            "finished_at_ms": self.finished_at_ms,
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "requested_explicit_source_catalog": self.requested_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "published_explicit_source_catalog": self.published_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "generation_changed": self.receipt.as_ref().map(|receipt| receipt.generation_changed),
            "receipt": self.receipt.as_ref().map(SourceBackedRefreshReceipt::to_json),
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress.to_json(),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings.map(SourceBackedRefreshTimings::to_json),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "last_error": self.last_error,
        }))
    }

    fn job_json(&self) -> Value {
        let status = match self.state {
            SourceBackedRefreshState::Published => "completed",
            SourceBackedRefreshState::Failed => "failed",
            SourceBackedRefreshState::Queued | SourceBackedRefreshState::Running => "running",
        };
        compact_json(json!({
            "mode": SourceBackedRefreshMode::Background.as_str(),
            "owner": "daemon",
            "kind": "source_backed",
            "status": status,
            "request_id": self.request_id,
            "request_state": self.state.as_str(),
            "source_count": self.progress.total_sources,
            "last_run_at_ms": self.started_at_ms.unwrap_or(self.requested_at_ms),
            "previous_generation": self.previous_generation,
            "published_generation": self.published_generation,
            "requested_explicit_source_catalog": self.requested_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "published_explicit_source_catalog": self.published_explicit_source_catalog
                .as_ref()
                .map(ExplicitSourceCatalogAuthority::to_json),
            "generation_changed": self.receipt.as_ref().map(|receipt| receipt.generation_changed),
            "receipt": self.receipt.as_ref().map(SourceBackedRefreshReceipt::to_json),
            "coalesced_requests": self.coalesced_requests,
            "progress": self.progress.to_json(),
            "scanned_routes": self.scanned_routes,
            "unsupported_routes": self.unsupported_routes,
            "certified_source_count": self.certified_source_count,
            "certified_source_bytes": self.certified_source_bytes,
            "timings_us": self.timings.map(SourceBackedRefreshTimings::to_json),
            "daemon_mode": self.daemon_mode.as_str(),
            "trigger": self.trigger,
            "trigger_provenance": self.trigger_provenance,
            "last_error": self.last_error,
        }))
    }
}

impl SourceBackedRefreshTimings {
    fn to_json(self) -> Value {
        json!({
            "discovery": self.discovery_us,
            "scan_stage": self.scan_stage_us,
            "commit": self.commit_us,
        })
    }
}

struct SourceBackedRefreshCoordinatorState {
    active_request_id: Option<String>,
    attempts: VecDeque<SourceBackedRefreshAttempt>,
    published_resolver: Option<Arc<GenerationBoundSourceBackedResolver>>,
}

pub(in crate::semantic) struct SourceBackedRefreshCoordinator {
    state: Mutex<SourceBackedRefreshCoordinatorState>,
    executor: Arc<dyn SourceBackedRefreshExecutor>,
}

pub(in crate::semantic) struct SourceBackedRefreshRun {
    pub(in crate::semantic) job: Value,
    pub(in crate::semantic) did_work: bool,
    pub(in crate::semantic) failed: bool,
}

/// One daemon-retained resolver whose identity is inseparable from the
/// verified lexical generation that installed it.
#[derive(Debug)]
#[allow(dead_code)] // Query IPC consumes this seam in the batch-hydration lane.
pub(crate) struct GenerationBoundSourceBackedResolver {
    generation_id: String,
    source_manifest: Option<SourceManifest>,
    resolver: Arc<SourceBackedResolverRegistry>,
}

#[allow(dead_code)] // Query IPC consumes this seam in the batch-hydration lane.
impl GenerationBoundSourceBackedResolver {
    pub(crate) fn generation_id(&self) -> &str {
        &self.generation_id
    }

    pub(crate) fn resolver(&self) -> &SourceBackedResolverRegistry {
        self.resolver.as_ref()
    }

    pub(crate) fn source_manifest(&self) -> Option<&SourceManifest> {
        self.source_manifest.as_ref()
    }
}

#[derive(Debug, Clone, Eq, Error, PartialEq)]
#[allow(dead_code)] // Query IPC exposes this typed failure in the hydration lane.
pub(crate) enum SourceBackedResolverAccessError {
    #[error("daemon has no resolver retained for source-backed generation {requested_generation}")]
    Missing { requested_generation: String },
    #[error(
        "daemon resolver generation mismatch: requested {requested_generation}, retained {retained_generation}"
    )]
    GenerationMismatch {
        requested_generation: String,
        retained_generation: String,
    },
}

impl SourceBackedRefreshCoordinator {
    pub(in crate::semantic) fn new() -> Self {
        Self::with_executor(Arc::new(CaptureOwnedSourceBackedRefreshExecutor))
    }

    pub(in crate::semantic) fn with_executor(
        executor: Arc<dyn SourceBackedRefreshExecutor>,
    ) -> Self {
        Self {
            state: Mutex::new(SourceBackedRefreshCoordinatorState {
                active_request_id: None,
                attempts: VecDeque::new(),
                published_resolver: None,
            }),
            executor,
        }
    }

    pub(in crate::semantic) fn has_pending_request(&self) -> bool {
        let state = self.lock_state();
        state
            .active_request_id
            .as_deref()
            .and_then(|request_id| find_attempt(&state, request_id))
            .is_some_and(|attempt| attempt.state.is_active())
    }

    pub(in crate::semantic) fn retained_published_generation(
        &self,
    ) -> Option<Arc<GenerationBoundSourceBackedResolver>> {
        self.lock_state()
            .published_resolver
            .as_ref()
            .map(Arc::clone)
    }

    /// Returns the resolver only when it is bound to the caller's exact
    /// lexical generation. Missing or stale daemon state queues the same
    /// provider-wide refresh path and remains a typed failure.
    #[allow(dead_code)] // Query IPC consumes this seam in the batch-hydration lane.
    pub(crate) fn resolver_for_generation(
        &self,
        data_root: &Path,
        generation_id: &str,
    ) -> std::result::Result<
        Arc<GenerationBoundSourceBackedResolver>,
        SourceBackedResolverAccessError,
    > {
        let result = {
            let state = self.lock_state();
            match state.published_resolver.as_ref() {
                Some(retained) if retained.generation_id == generation_id => {
                    return Ok(Arc::clone(retained));
                }
                Some(retained) => SourceBackedResolverAccessError::GenerationMismatch {
                    requested_generation: generation_id.to_owned(),
                    retained_generation: retained.generation_id.clone(),
                },
                None => SourceBackedResolverAccessError::Missing {
                    requested_generation: generation_id.to_owned(),
                },
            }
        };
        self.enqueue_with_metadata(
            Some(generation_id.to_owned()),
            source_refresh_runtime_metadata(data_root),
        );
        Err(result)
    }

    /// Preserves the capture resolver's typed failure while arranging for
    /// source state invalidations to be repaired by the daemon refresh loop.
    /// A future batch hydration worker can call this once for a failed batch;
    /// this method performs no hydration and has no legacy fallback.
    #[allow(dead_code)] // Query IPC consumes this seam in the batch-hydration lane.
    pub(crate) fn handle_hydration_failure(
        &self,
        data_root: &Path,
        generation_id: &str,
        failure: HydrationFailure,
    ) -> HydrationFailure {
        let retained_generation_matches = {
            let state = self.lock_state();
            state
                .published_resolver
                .as_ref()
                .is_some_and(|retained| retained.generation_id == generation_id)
        };
        if hydration_failure_queues_refresh(failure.kind) && retained_generation_matches {
            self.enqueue_with_metadata(
                Some(generation_id.to_owned()),
                source_refresh_runtime_metadata(data_root),
            );
        }
        failure
    }

    pub(in crate::semantic) fn handle_ipc_request(
        &self,
        data_root: &Path,
        request: &Value,
    ) -> Result<Option<Value>> {
        match request.get("op").and_then(Value::as_str) {
            Some(SOURCE_REFRESH_REQUEST_OP) => {
                let mode = request.get("mode").and_then(Value::as_str).unwrap_or("");
                if !matches!(mode, "background" | "wait") {
                    return Err(anyhow!("invalid daemon source refresh mode `{mode}`"));
                }
                let requested_catalog = request
                    .get("explicit_source_catalog")
                    .map(ExplicitSourceCatalogAuthority::from_json)
                    .transpose()?;
                let previous_generation = published_generation_id(data_root)?;
                let metadata = if requested_catalog.is_some() {
                    source_catalog_refresh_runtime_metadata(data_root)
                } else {
                    source_refresh_runtime_metadata(data_root)
                };
                let response = self.enqueue_with_catalog_metadata(
                    previous_generation,
                    metadata,
                    requested_catalog,
                )?;
                let request_id = response
                    .get("request_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("queued source refresh has no request ID"))?;
                if let Some(job) = self.job_status(request_id) {
                    write_daemon_job_status(
                        &daemon_source_backed_refresh_job_path(data_root),
                        &job,
                    )?;
                }
                Ok(Some(response))
            }
            Some(SOURCE_REFRESH_STATUS_OP) => {
                let request_id = request
                    .get("request_id")
                    .and_then(Value::as_str)
                    .filter(|request_id| !request_id.is_empty())
                    .ok_or_else(|| anyhow!("daemon source refresh request ID is missing"))?;
                let status = self.status(request_id).ok_or_else(|| {
                    anyhow!("daemon source refresh request `{request_id}` is unknown")
                })?;
                Ok(Some(status))
            }
            _ => Ok(None),
        }
    }

    pub(in crate::semantic) fn run_next(&self, data_root: &Path) -> Option<SourceBackedRefreshRun> {
        let executor = Arc::clone(&self.executor);
        self.run_next_with(
            |request_id, coordinator| {
                prepare_source_rebuild_if_needed(data_root)?;
                let requested_catalog =
                    coordinator.requested_explicit_source_catalog(request_id);
                let publication = execute_source_backed_refresh(
                    executor.as_ref(),
                    data_root,
                    request_id,
                    coordinator,
                )?;
                if let Some(expected) = requested_catalog {
                    let published = load_explicit_source_catalog_authority(data_root)
                        .context("verify published explicit source catalog authority")?;
                    if published != expected {
                        bail!(
                            "source-backed refresh published for explicit source catalog {:?}, not the requested authority {:?}",
                            published,
                            expected
                        );
                    }
                }
                Ok(publication)
            },
            || published_generation_id(data_root),
            |generation_id| complete_pending_source_rebuild(data_root, generation_id),
            |error| record_pending_source_rebuild_failure(data_root, error),
        )
    }

    pub(in crate::semantic) fn enqueue_periodic(&self, data_root: &Path) -> Result<Value> {
        let observed_generation = published_generation_id(data_root)?;
        let catalog = load_explicit_source_catalog_authority(data_root)?;
        self.enqueue_with_catalog_metadata(
            observed_generation,
            SourceRefreshRuntimeMetadata::periodic(),
            Some(catalog),
        )
    }

    #[cfg(test)]
    fn enqueue(&self, observed_generation: Option<String>) -> Value {
        self.enqueue_with_metadata(observed_generation, SourceRefreshRuntimeMetadata::default())
    }

    #[cfg(test)]
    pub(in crate::semantic) fn enqueue_for_test(
        &self,
        observed_generation: Option<String>,
    ) -> Value {
        self.enqueue(observed_generation)
    }

    fn enqueue_with_metadata(
        &self,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
    ) -> Value {
        self.enqueue_with_catalog_metadata(observed_generation, metadata, None)
            .expect("requests without catalog authority always coalesce")
    }

    fn enqueue_with_catalog_metadata(
        &self,
        observed_generation: Option<String>,
        metadata: SourceRefreshRuntimeMetadata,
        requested_catalog: Option<ExplicitSourceCatalogAuthority>,
    ) -> Result<Value> {
        let mut state = self.lock_state();
        if let Some(active_request_id) = state.active_request_id.clone() {
            if let Some(active) = find_attempt_mut(&mut state, &active_request_id) {
                if active.state.is_active() {
                    if let Some(requested_catalog) = requested_catalog {
                        if active.requested_explicit_source_catalog.is_none()
                            && active.state == SourceBackedRefreshState::Queued
                        {
                            active.requested_explicit_source_catalog = Some(requested_catalog);
                        } else if active.requested_explicit_source_catalog.as_ref()
                            != Some(&requested_catalog)
                        {
                            bail!(
                                "daemon source refresh request {} is already active for a different explicit source catalog authority; retry after it publishes",
                                active.request_id
                            );
                        }
                        if metadata.trigger == "import" {
                            active.trigger = metadata.trigger;
                            active.trigger_provenance = metadata.trigger_provenance;
                        }
                    }
                    active.coalesced_requests = active.coalesced_requests.saturating_add(1);
                    return Ok(active.to_json());
                }
            }
        }

        let attempt = SourceBackedRefreshAttempt {
            request_id: Uuid::now_v7().to_string(),
            state: SourceBackedRefreshState::Queued,
            requested_at_ms: utc_now().timestamp_millis(),
            started_at_ms: None,
            finished_at_ms: None,
            previous_generation: observed_generation.clone(),
            published_generation: observed_generation,
            requested_explicit_source_catalog: requested_catalog,
            published_explicit_source_catalog: None,
            coalesced_requests: 0,
            progress: SourceBackedRefreshProgress::default(),
            scanned_routes: None,
            unsupported_routes: None,
            certified_source_count: None,
            certified_source_bytes: None,
            receipt: None,
            timings: None,
            daemon_mode: metadata.daemon_mode,
            trigger: metadata.trigger,
            trigger_provenance: metadata.trigger_provenance,
            last_error: None,
        };
        let response = attempt.to_json();
        state.active_request_id = Some(attempt.request_id.clone());
        state.attempts.push_back(attempt);
        trim_attempt_history(&mut state);
        Ok(response)
    }

    fn status(&self, request_id: &str) -> Option<Value> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(SourceBackedRefreshAttempt::to_json)
    }

    fn requested_explicit_source_catalog(
        &self,
        request_id: &str,
    ) -> Option<ExplicitSourceCatalogAuthority> {
        let state = self.lock_state();
        find_attempt(&state, request_id)
            .and_then(|attempt| attempt.requested_explicit_source_catalog.clone())
    }

    fn job_status(&self, request_id: &str) -> Option<Value> {
        let state = self.lock_state();
        find_attempt(&state, request_id).map(SourceBackedRefreshAttempt::job_json)
    }

    fn set_progress(
        &self,
        request_id: &str,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        current_source: Option<String>,
    ) -> Option<Value> {
        let mut state = self.lock_state();
        let Some(attempt) = find_attempt_mut(&mut state, request_id) else {
            return None;
        };
        if attempt.state != SourceBackedRefreshState::Running {
            return None;
        }
        attempt.progress = SourceBackedRefreshProgress {
            phase: phase.to_owned(),
            completed_sources,
            total_sources,
            current_source,
        };
        Some(attempt.job_json())
    }

    fn run_next_with<Execute, Probe, Published, Failed>(
        &self,
        execute: Execute,
        probe: Probe,
        published: Published,
        failed: Failed,
    ) -> Option<SourceBackedRefreshRun>
    where
        Execute: FnOnce(&str, &Self) -> Result<SourceBackedRefreshPublication>,
        Probe: FnOnce() -> Result<Option<String>>,
        Published: FnOnce(&str) -> Result<()>,
        Failed: FnOnce(&str) -> Result<()>,
    {
        let (request_id, previous_generation, requested_catalog) = {
            let mut state = self.lock_state();
            let request_id = state.active_request_id.clone()?;
            let attempt = find_attempt_mut(&mut state, &request_id)?;
            if attempt.state != SourceBackedRefreshState::Queued {
                return None;
            }
            attempt.state = SourceBackedRefreshState::Running;
            attempt.started_at_ms = Some(utc_now().timestamp_millis());
            attempt.progress.phase = "starting".to_owned();
            (
                request_id,
                attempt.previous_generation.clone(),
                attempt.requested_explicit_source_catalog.clone(),
            )
        };

        let execution = execute(&request_id, self);
        let observed_generation = probe();
        let (verified, observed_for_status) = match (execution, observed_generation) {
            (Ok(publication), Ok(Some(observed))) if publication.generation_id == observed => {
                let manifest_generation_matches = publication
                    .source_manifest
                    .as_ref()
                    .is_none_or(|manifest| manifest.core_generation_id == observed);
                let production_handoff_complete =
                    publication.resolver.is_some() && publication.source_manifest.is_some();
                let verified = if !manifest_generation_matches {
                    Err(format!(
                        "source-backed refresh published generation {observed} with a source manifest for another generation"
                    ))
                } else if production_handoff_complete || cfg!(test) {
                    Ok((observed.clone(), publication))
                } else {
                    Err(format!(
                        "source-backed refresh published generation {observed} without its resolver registry or source manifest"
                    ))
                };
                (verified, Some(observed))
            }
            (Ok(publication), Ok(observed)) => (Err(format!(
                "source-backed refresh returned generation {}, but the verified published generation is {observed:?}",
                publication.generation_id
            )), observed),
            (Ok(publication), Err(error)) => (
                Err(format!(
                    "source-backed refresh returned generation {}, but publication verification failed: {error:#}",
                    publication.generation_id
                )),
                None,
            ),
            (Err(error), Ok(observed)) => (Err(format!("{error:#}")), observed),
            (Err(error), Err(probe_error)) => (Err(format!(
                "{error:#}; verifying the retained generation also failed: {probe_error:#}"
            )), None),
        };
        let verified = match verified {
            Ok((observed, publication)) => match published(&observed) {
                Ok(()) => Ok((observed, publication)),
                Err(error) => {
                    let error = format!("record source-backed rebuild publication: {error:#}");
                    match failed(&error) {
                        Ok(()) => Err(error),
                        Err(record_error) => Err(format!(
                            "{error}; recording the resumable rebuild failure also failed: {record_error:#}"
                        )),
                    }
                }
            },
            Err(error) => match failed(&error) {
                Ok(()) => Err(error),
                Err(record_error) => Err(format!(
                    "{error}; recording the resumable rebuild failure also failed: {record_error:#}"
                )),
            },
        };
        let mut state = self.lock_state();
        let mut installed_resolver = None;
        let (failed, did_work, job) = {
            let attempt = find_attempt_mut(&mut state, &request_id)?;
            attempt.finished_at_ms = Some(utc_now().timestamp_millis());
            attempt.progress.current_source = None;
            if observed_for_status.is_some() {
                attempt.published_generation = observed_for_status;
            }

            match verified {
                Ok((observed, publication)) => {
                    let generation_changed =
                        previous_generation.as_deref() != Some(observed.as_str());
                    attempt.state = SourceBackedRefreshState::Published;
                    attempt.published_generation = Some(observed.clone());
                    attempt.progress.phase = "published".to_owned();
                    attempt.progress.completed_sources = attempt.progress.total_sources;
                    attempt.scanned_routes = Some(publication.scanned_routes);
                    attempt.unsupported_routes = Some(publication.unsupported_routes);
                    attempt.certified_source_count = Some(publication.certified_source_count);
                    attempt.certified_source_bytes = Some(publication.certified_source_bytes);
                    attempt.receipt = Some(SourceBackedRefreshReceipt {
                        previous_generation: previous_generation.clone(),
                        published_generation: observed.clone(),
                        generation_changed,
                        current: publication.current,
                    });
                    attempt.timings = Some(publication.timings);
                    let source_manifest = publication.source_manifest;
                    attempt.published_explicit_source_catalog = requested_catalog.clone();
                    installed_resolver = publication.resolver.map(|resolver| {
                        Arc::new(GenerationBoundSourceBackedResolver {
                            generation_id: observed,
                            source_manifest,
                            resolver,
                        })
                    });
                }
                Err(error) => {
                    attempt.state = SourceBackedRefreshState::Failed;
                    attempt.progress.phase = "failed".to_owned();
                    attempt.last_error = Some(error);
                }
            }

            let failed = attempt.state == SourceBackedRefreshState::Failed;
            let did_work = !failed && attempt.published_generation != previous_generation;
            (failed, did_work, attempt.job_json())
        };
        if let Some(resolver) = installed_resolver {
            state.published_resolver = Some(resolver);
        }
        if state.active_request_id.as_deref() == Some(request_id.as_str()) {
            state.active_request_id = None;
        }
        Some(SourceBackedRefreshRun {
            job,
            did_work,
            failed,
        })
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, SourceBackedRefreshCoordinatorState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[derive(Debug, Clone, Copy)]
struct SourceRefreshRuntimeMetadata {
    daemon_mode: DaemonMode,
    trigger: &'static str,
    trigger_provenance: &'static str,
}

impl Default for SourceRefreshRuntimeMetadata {
    fn default() -> Self {
        Self {
            daemon_mode: DaemonMode::Full,
            trigger: "search",
            trigger_provenance: "manual",
        }
    }
}

impl SourceRefreshRuntimeMetadata {
    fn periodic() -> Self {
        Self {
            daemon_mode: DaemonMode::Full,
            trigger: "periodic",
            trigger_provenance: "daemon_scheduler",
        }
    }
}

fn source_refresh_runtime_metadata(data_root: &Path) -> SourceRefreshRuntimeMetadata {
    let daemon_status = read_daemon_status(data_root);
    let daemon_mode = AppConfig::load(data_root)
        .map(|config| config.daemon.mode)
        .ok()
        .or_else(|| {
            daemon_status
                .as_ref()
                .and_then(|status| status.get("config_reload"))
                .and_then(|reload| reload.get("applied"))
                .and_then(|applied| applied.get("daemon_mode"))
                .and_then(Value::as_str)
                .and_then(DaemonMode::parse)
        })
        .unwrap_or_default();
    let trigger_provenance = if daemon_status
        .as_ref()
        .and_then(|status| status.get("start_mode"))
        .and_then(Value::as_str)
        == Some("auto")
    {
        "autostart"
    } else {
        "manual"
    };
    SourceRefreshRuntimeMetadata {
        daemon_mode,
        trigger: "search",
        trigger_provenance,
    }
}

fn source_catalog_refresh_runtime_metadata(data_root: &Path) -> SourceRefreshRuntimeMetadata {
    SourceRefreshRuntimeMetadata {
        trigger: "import",
        trigger_provenance: "explicit_source_catalog",
        ..source_refresh_runtime_metadata(data_root)
    }
}

fn find_attempt<'a>(
    state: &'a SourceBackedRefreshCoordinatorState,
    request_id: &str,
) -> Option<&'a SourceBackedRefreshAttempt> {
    state
        .attempts
        .iter()
        .find(|attempt| attempt.request_id == request_id)
}

fn find_attempt_mut<'a>(
    state: &'a mut SourceBackedRefreshCoordinatorState,
    request_id: &str,
) -> Option<&'a mut SourceBackedRefreshAttempt> {
    state
        .attempts
        .iter_mut()
        .find(|attempt| attempt.request_id == request_id)
}

fn trim_attempt_history(state: &mut SourceBackedRefreshCoordinatorState) {
    while state.attempts.len() > SOURCE_REFRESH_ATTEMPT_HISTORY {
        if state
            .attempts
            .front()
            .is_some_and(|attempt| attempt.state.is_active())
        {
            break;
        }
        state.attempts.pop_front();
    }
}

pub(super) fn source_backed_index_root(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY)
}

fn published_generation_id(data_root: &Path) -> Result<Option<String>> {
    Ok(open_published_generation(data_root)?.map(|index| index.generation_id().to_owned()))
}

fn open_published_generation(data_root: &Path) -> Result<Option<VerifiedIndex>> {
    let index_root = source_backed_index_root(data_root);
    if !index_root.join("meta.json").is_file() {
        return Ok(None);
    }
    match VerifiedIndex::open(&index_root) {
        Ok(index) => Ok(Some(index)),
        // Tantivy creates schema-only meta.json before the first ctx commit.
        // It is a replaceable cold-build artifact only while the epoch proves
        // that no lexical generation has ever been activated. Once activation
        // succeeds, the same typed error is corruption and remains fail-closed.
        Err(IndexError::MissingCommitPayload) if pending_source_rebuild(data_root)? => Ok(None),
        Err(error) => Err(error).with_context(|| {
            format!(
                "open verified source-backed lexical index {}",
                index_root.display()
            )
        }),
    }
}

fn prepare_source_rebuild_if_needed(data_root: &Path) -> Result<()> {
    let Some(marker) = data_migration::inspect(data_root)? else {
        return Ok(());
    };
    if marker.source_rebuild_required
        && matches!(
            marker.phase,
            MigrationPhase::Detected | MigrationPhase::SourceRebuildFailed
        )
    {
        data_migration::prepare(data_root, &[])?;
    }
    Ok(())
}

fn pending_source_rebuild(data_root: &Path) -> Result<bool> {
    Ok(data_migration::inspect(data_root)?.is_some_and(|marker| {
        marker.source_rebuild_required
            && matches!(
                marker.phase,
                MigrationPhase::Detected
                    | MigrationPhase::RebuildPending
                    | MigrationPhase::SourceRebuildFailed
            )
    }))
}

fn complete_pending_source_rebuild(data_root: &Path, generation_id: &str) -> Result<()> {
    if pending_source_rebuild(data_root)? {
        prepare_source_rebuild_if_needed(data_root)?;
        data_migration::complete_source_rebuild(data_root, generation_id)?;
    }
    Ok(())
}

fn record_pending_source_rebuild_failure(data_root: &Path, error: &str) -> Result<()> {
    if pending_source_rebuild(data_root)? {
        prepare_source_rebuild_if_needed(data_root)?;
        data_migration::record_source_rebuild_failure(data_root, error)?;
    }
    Ok(())
}

fn execute_source_backed_refresh(
    executor: &dyn SourceBackedRefreshExecutor,
    data_root: &Path,
    request_id: &str,
    coordinator: &SourceBackedRefreshCoordinator,
) -> Result<SourceBackedRefreshPublication> {
    let index_root = source_backed_index_root(data_root);
    let report_progress = |update: SourceBackedRefreshProgressUpdate| {
        record_source_backed_refresh_progress(
            data_root,
            coordinator,
            request_id,
            &update.phase,
            update.completed_sources,
            update.total_sources,
            update.current_source,
        )
    };
    executor.refresh(SourceBackedRefreshExecution {
        data_root,
        index_root: &index_root,
        request_id,
        report_progress: &report_progress,
    })
}

fn execute_capture_owned_refresh(
    execution: SourceBackedRefreshExecution<'_>,
) -> Result<SourceBackedRefreshPublication> {
    let discovery = source_backed_discovery_context()?;
    execute_capture_owned_refresh_with(execution, &discovery, refresh_all_provider_sources)
}

fn execute_capture_owned_refresh_with<Refresh>(
    execution: SourceBackedRefreshExecution<'_>,
    discovery: &DiscoveryContext,
    refresh_all: Refresh,
) -> Result<SourceBackedRefreshPublication>
where
    Refresh: FnOnce(
        &DiscoveryContext,
        &Path,
        &Path,
        &mut dyn FnMut(CaptureSourceBackedRefreshProgress) -> SourceBackedRouteResult<()>,
    ) -> Result<SourceBackedRefreshPublication>,
{
    execution.report_progress("discovering", 0, 0, None)?;
    let mut report_progress = |update: CaptureSourceBackedRefreshProgress| {
        execution
            .report_progress(
                update.phase,
                update.completed_sources,
                update.total_sources,
                update.current_source,
            )
            .map_err(|error| {
                SourceBackedRouteError::new(
                    SourceBackedRouteErrorKind::Internal,
                    format!("persist daemon source-backed refresh progress: {error:#}"),
                )
            })
    };
    refresh_all(
        discovery,
        execution.data_root,
        execution.index_root,
        &mut report_progress,
    )
}

fn refresh_all_provider_sources(
    discovery: &DiscoveryContext,
    data_root: &Path,
    index_root: &Path,
    report_progress: &mut dyn FnMut(
        CaptureSourceBackedRefreshProgress,
    ) -> SourceBackedRouteResult<()>,
) -> Result<SourceBackedRefreshPublication> {
    let mut build = build_automatic_source_backed_registry(discovery);
    register_explicit_source_catalog_routes(data_root, index_root, &mut build)?;
    let (executor, issues) = build.into_refresh_executor(WriterOptions::default());
    reject_blocking_automatic_registry_issues(&issues)?;
    let resolver = Arc::new(executor.registry().resolver_registry());
    let receipt = executor
        .refresh(index_root, report_progress)
        .context("run capture-owned all-provider source-backed refresh")?;
    let current =
        SourceBackedRefreshCurrent::from_sources(&receipt.sources, receipt.removals.len())?;
    if current.source_count != receipt.certified_source_count
        || current.certified_source_bytes != receipt.certified_source_bytes
        || current.indexed_documents != receipt.commit.indexed_documents
    {
        bail!(
            "capture-owned source refresh receipt does not match its retained generation cardinalities"
        );
    }
    let removals = receipt
        .removals
        .iter()
        .cloned()
        .map(|removal| SourceRemoval::new(removal.deletion, removal.inventory))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| anyhow!("build certified Pro source removal: {}", error.message))?;
    let source_manifest = SourceManifest::new(
        receipt.commit.generation_id.clone(),
        receipt.sources.clone(),
        removals,
    )
    .map_err(|error| anyhow!("build Pro source manifest: {}", error.message))?;
    Ok(SourceBackedRefreshPublication {
        generation_id: receipt.commit.generation_id,
        source_manifest: Some(source_manifest),
        resolver: Some(resolver),
        scanned_routes: receipt.scanned_routes,
        unsupported_routes: receipt.unsupported_routes.len(),
        certified_source_count: receipt.certified_source_count,
        certified_source_bytes: receipt.certified_source_bytes,
        current,
        timings: SourceBackedRefreshTimings {
            discovery_us: nonzero_duration_micros(receipt.discovery_duration),
            scan_stage_us: nonzero_duration_micros(receipt.scan_stage_duration),
            commit_us: nonzero_duration_micros(receipt.commit_duration),
        },
    })
}

fn nonzero_duration_micros(duration: StdDuration) -> u64 {
    u64::try_from(duration.as_micros())
        .unwrap_or(u64::MAX)
        .max(1)
}

fn source_backed_discovery_context() -> Result<DiscoveryContext> {
    let home = identity::home_dir()
        .context("resolve the user home for source-backed provider discovery")?;
    Ok(DiscoveryContext::from_process(home))
}

fn reject_blocking_automatic_registry_issues(
    issues: &[SourceBackedAutomaticRegistryIssue],
) -> Result<()> {
    let mut blocker_count = 0usize;
    let mut blocker_details = Vec::new();
    for issue in issues {
        let SourceBackedAutomaticRegistryIssue::Unavailable { source, reason } = issue else {
            continue;
        };
        let blocks_publication = match reason {
            SourceBackedAutomaticUnavailableReason::SourceStatus(
                ProviderSourceStatus::Missing | ProviderSourceStatus::Unknown,
            ) => false,
            SourceBackedAutomaticUnavailableReason::SourceStatus(_)
            | SourceBackedAutomaticUnavailableReason::UnsupportedFormat { .. }
            | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { .. }
            | SourceBackedAutomaticUnavailableReason::RegistrationRejected { .. } => source.exists,
        };
        if !blocks_publication {
            continue;
        }
        blocker_count = blocker_count.saturating_add(1);
        if blocker_details.len() < SOURCE_REFRESH_BUILD_ISSUE_LIMIT {
            blocker_details.push(format!(
                "{} {}: {}",
                source.provider.as_str(),
                source.path.display(),
                automatic_registry_issue_reason(reason),
            ));
        }
    }
    if blocker_count == 0 {
        return Ok(());
    }
    let omitted = blocker_count.saturating_sub(blocker_details.len());
    let omitted = if omitted == 0 {
        String::new()
    } else {
        format!("; {omitted} additional blocking issue(s) omitted")
    };
    Err(anyhow!(
        "capture automatic registry has {blocker_count} blocking detected-source issue(s): {}{omitted}",
        blocker_details.join("; ")
    ))
}

fn automatic_registry_issue_reason(reason: &SourceBackedAutomaticUnavailableReason) -> String {
    match reason {
        SourceBackedAutomaticUnavailableReason::SourceStatus(status) => {
            format!("source status is {}", status.as_str())
        }
        SourceBackedAutomaticUnavailableReason::UnsupportedFormat { detail }
        | SourceBackedAutomaticUnavailableReason::SelectorAuthorityUnavailable { detail } => {
            (*detail).to_owned()
        }
        SourceBackedAutomaticUnavailableReason::RegistrationRejected { detail } => detail.clone(),
    }
}

#[allow(dead_code)] // Used by the retained resolver's future batch consumer.
fn hydration_failure_queues_refresh(kind: HydrationFailureKind) -> bool {
    matches!(
        kind,
        HydrationFailureKind::TemporarilyUnavailable
            | HydrationFailureKind::ConfirmedDeleted
            | HydrationFailureKind::StaleSourceEvidence
            | HydrationFailureKind::StaleRecordEvidence
            | HydrationFailureKind::MissingRecord
    )
}

fn record_source_backed_refresh_progress(
    data_root: &Path,
    coordinator: &SourceBackedRefreshCoordinator,
    request_id: &str,
    phase: &str,
    completed_sources: usize,
    total_sources: usize,
    current_source: Option<String>,
) -> Result<()> {
    if let Some(job) = coordinator.set_progress(
        request_id,
        phase,
        completed_sources,
        total_sources,
        current_source,
    ) {
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), &job)?;
    }
    Ok(())
}

pub(crate) struct PinnedSourceBackedGeneration {
    index: VerifiedIndex,
}

impl PinnedSourceBackedGeneration {
    #[allow(dead_code)] // Available to callers that report the selected pin.
    pub(crate) fn generation_id(&self) -> &str {
        self.index.generation_id()
    }

    pub(super) fn semantic_eligible_event_count(&self) -> Result<u64> {
        self.index
            .semantic_eligible_event_count()
            .map_err(anyhow::Error::new)
    }

    pub(crate) fn into_index(self) -> VerifiedIndex {
        self.index
    }

    #[cfg(test)]
    pub(crate) fn from_index(index: VerifiedIndex) -> Self {
        Self { index }
    }
}

#[allow(dead_code)] // Request metadata is retained for CLI/status integrations.
pub(crate) struct SourceBackedRefreshObservation {
    pub(crate) mode: SourceBackedRefreshMode,
    pub(crate) status: String,
    pub(crate) request_id: Option<String>,
    pub(crate) daemon_available: bool,
    pub(crate) source_count: usize,
    pub(crate) receipt: Option<SourceBackedRefreshReceipt>,
    pub(crate) pin: PinnedSourceBackedGeneration,
}

/// Coordinates source-backed refresh without ever falling back to a foreground
/// writer. The returned reader is already pinned to one verified generation.
pub(crate) fn coordinate_source_backed_refresh(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
) -> Result<SourceBackedRefreshObservation> {
    coordinate_source_backed_refresh_with_catalog(data_root, mode, None)
}

fn coordinate_source_backed_refresh_with_catalog(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
) -> Result<SourceBackedRefreshObservation> {
    if mode == SourceBackedRefreshMode::Off {
        if explicit_source_catalog.is_some() {
            bail!("explicit source catalog imports require daemon refresh mode `wait`");
        }
        let pin = pin_published_generation(data_root)?.ok_or_else(|| {
            anyhow!("the source-backed index does not exist; retry with daemon refresh enabled")
        })?;
        return Ok(SourceBackedRefreshObservation {
            mode,
            status: "off".to_owned(),
            request_id: None,
            daemon_available: false,
            source_count: 0,
            receipt: None,
            pin,
        });
    }

    let request = compact_json(json!({
        "schema_version": 1,
        "op": SOURCE_REFRESH_REQUEST_OP,
        "mode": mode.as_str(),
        "explicit_source_catalog": explicit_source_catalog
            .map(ExplicitSourceCatalogAuthority::to_json),
    }));
    let response = match daemon_source_refresh_request(
        data_root,
        request,
        SOURCE_REFRESH_IPC_TIMEOUT,
        SOURCE_REFRESH_RESPONSE_MAX_BYTES,
    ) {
        Ok(Some(response)) => response,
        Ok(None) => return daemon_unavailable_fallback(data_root, mode, None),
        Err(error)
            if error
                .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                .is_some() =>
        {
            return daemon_unavailable_fallback(data_root, mode, Some(error))
        }
        Err(error) => return Err(error.context("request daemon-owned source-backed refresh")),
    };
    validate_daemon_refresh_response(&response)?;
    let request_id = response
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|request_id| !request_id.is_empty())
        .ok_or_else(|| anyhow!("daemon source refresh response has no request ID"))?
        .to_owned();

    if mode == SourceBackedRefreshMode::Background {
        let pin = pin_published_generation(data_root)?.ok_or_else(|| {
            anyhow!(
                "daemon source refresh was queued but no published generation exists; retry with --refresh wait"
            )
        })?;
        return Ok(SourceBackedRefreshObservation {
            mode,
            status: response
                .get("request_state")
                .and_then(Value::as_str)
                .unwrap_or("queued")
                .to_owned(),
            request_id: Some(request_id),
            daemon_available: true,
            source_count: response_source_count(&response),
            receipt: None,
            pin,
        });
    }

    wait_for_published_generation(data_root, request_id, mode, explicit_source_catalog)
}

fn wait_for_published_generation(
    data_root: &Path,
    request_id: String,
    mode: SourceBackedRefreshMode,
    expected_catalog: Option<&ExplicitSourceCatalogAuthority>,
) -> Result<SourceBackedRefreshObservation> {
    loop {
        let response = match daemon_source_refresh_request(
            data_root,
            compact_json(json!({
                "schema_version": 1,
                "op": SOURCE_REFRESH_STATUS_OP,
                "request_id": request_id,
            })),
            SOURCE_REFRESH_IPC_TIMEOUT,
            SOURCE_REFRESH_RESPONSE_MAX_BYTES,
        ) {
            Ok(Some(response)) => response,
            Ok(None) => {
                return Err(SourceBackedRefreshDaemonUnavailable::new(Some(format!(
                    "daemon became unavailable while waiting for request {request_id}"
                )))
                .into())
            }
            Err(error)
                if error
                    .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                    .is_some() =>
            {
                return Err(
                    SourceBackedRefreshDaemonUnavailable::new(Some(format!("{error:#}"))).into(),
                )
            }
            Err(error) => {
                return Err(error.context("wait for daemon-owned source-backed refresh publication"))
            }
        };
        validate_daemon_refresh_response(&response)?;
        match response.get("request_state").and_then(Value::as_str) {
            Some("published") => {
                if let Some(expected_catalog) = expected_catalog {
                    let published_catalog = response
                        .get("published_explicit_source_catalog")
                        .ok_or_else(|| {
                            anyhow!(
                                "published daemon source refresh has no explicit source catalog authority"
                            )
                        })
                        .and_then(ExplicitSourceCatalogAuthority::from_json)?;
                    if &published_catalog != expected_catalog {
                        bail!(
                            "daemon published an unexpected explicit source catalog authority: expected {:?}, published {:?}",
                            expected_catalog,
                            published_catalog
                        );
                    }
                }
                let expected = response
                    .get("published_generation")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow!("published daemon source refresh has no generation ID")
                    })?;
                let pin = pin_published_generation(data_root)?.ok_or_else(|| {
                    anyhow!(
                        "daemon published source-backed generation {expected}, but no verified generation can be opened"
                    )
                })?;
                if pin.generation_id() != expected {
                    bail!(
                        "daemon reported source-backed generation {expected}, but the verified published generation is {}",
                        pin.generation_id()
                    );
                }
                let receipt = published_refresh_receipt(&response, &pin)?;
                return Ok(SourceBackedRefreshObservation {
                    mode,
                    status: "published".to_owned(),
                    request_id: Some(request_id),
                    daemon_available: true,
                    source_count: response_source_count(&response),
                    receipt: Some(receipt),
                    pin,
                });
            }
            Some("failed") => {
                let error = response
                    .get("last_error")
                    .and_then(Value::as_str)
                    .unwrap_or("source-backed refresh failed");
                let retained = response
                    .get("published_generation")
                    .and_then(Value::as_str)
                    .or_else(|| response.get("previous_generation").and_then(Value::as_str))
                    .map(|generation| format!("; retained generation {generation}"))
                    .unwrap_or_default();
                return Err(anyhow!(
                    "daemon-owned source-backed refresh failed: {error}{retained}"
                ));
            }
            Some("queued" | "running") => {
                std::thread::sleep(SOURCE_REFRESH_POLL_INTERVAL);
            }
            Some(state) => {
                return Err(anyhow!(
                    "daemon source refresh request {request_id} has unknown state `{state}`"
                ));
            }
            None => {
                return Err(anyhow!(
                    "daemon source refresh response has no request state"
                ))
            }
        }
    }
}

fn published_refresh_receipt(
    response: &Value,
    pin: &PinnedSourceBackedGeneration,
) -> Result<SourceBackedRefreshReceipt> {
    let value = response
        .get("receipt")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("published daemon source refresh has no terminal receipt"))?;
    let previous_generation = optional_generation(value.get("previous_generation"))?;
    let published_generation = required_generation(
        value.get("published_generation"),
        "terminal receipt published generation",
    )?;
    let generation_changed = value
        .get("generation_changed")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            anyhow!("published daemon source refresh receipt has no generation_changed fact")
        })?;
    let current_value = value
        .get("current")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            anyhow!("published daemon source refresh receipt has no current generation facts")
        })?;
    let current = SourceBackedRefreshCurrent {
        source_count: required_usize(current_value, "current_source_count")?,
        indexed_documents: required_u64(current_value, "current_indexed_documents")?,
        complete_records: required_u64(current_value, "current_complete_records")?,
        retained_records: required_u64(current_value, "current_retained_records")?,
        rejected_records: required_u64(current_value, "current_rejected_records")?,
        ignored_records: required_u64(current_value, "current_ignored_records")?,
        certified_source_bytes: required_u64(current_value, "current_certified_source_bytes")?,
        sources_with_rejections: required_usize(current_value, "current_sources_with_rejections")?,
        removed_source_count: required_usize(current_value, "removed_source_count")?,
    };

    let top_previous_generation = optional_generation(response.get("previous_generation"))?;
    let top_published_generation = required_generation(
        response.get("published_generation"),
        "published daemon source refresh generation",
    )?;
    let top_generation_changed = response
        .get("generation_changed")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("published daemon source refresh has no generation_changed fact"))?;
    let identity_changed = previous_generation.as_deref() != Some(published_generation.as_str());
    if previous_generation != top_previous_generation
        || published_generation != top_published_generation
        || generation_changed != top_generation_changed
        || generation_changed != identity_changed
    {
        bail!("published daemon source refresh receipt has inconsistent generation identity facts");
    }

    let manifest = pin.index.manifest();
    let verified_current =
        SourceBackedRefreshCurrent::from_sources(&manifest.sources, manifest.removals.len())?;
    if current != verified_current
        || current.source_count
            != required_usize_from_value(
                response.get("certified_source_count"),
                "certified_source_count",
            )?
        || current.certified_source_bytes
            != required_u64_from_value(
                response.get("certified_source_bytes"),
                "certified_source_bytes",
            )?
    {
        bail!(
            "published daemon source refresh receipt does not match the verified current generation"
        );
    }

    Ok(SourceBackedRefreshReceipt {
        previous_generation,
        published_generation,
        generation_changed,
        current,
    })
}

fn optional_generation(value: Option<&Value>) -> Result<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        _ => bail!("daemon source refresh generation identity is malformed"),
    }
}

fn required_generation(value: Option<&Value>, label: &str) -> Result<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{label} is missing"))
}

fn required_usize(value: &serde_json::Map<String, Value>, field: &str) -> Result<usize> {
    required_usize_from_value(value.get(field), field)
}

fn required_usize_from_value(value: Option<&Value>, field: &str) -> Result<usize> {
    value
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| anyhow!("published daemon source refresh receipt has invalid {field}"))
}

fn required_u64(value: &serde_json::Map<String, Value>, field: &str) -> Result<u64> {
    required_u64_from_value(value.get(field), field)
}

fn required_u64_from_value(value: Option<&Value>, field: &str) -> Result<u64> {
    value
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("published daemon source refresh receipt has invalid {field}"))
}

fn daemon_unavailable_fallback(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    error: Option<anyhow::Error>,
) -> Result<SourceBackedRefreshObservation> {
    if mode == SourceBackedRefreshMode::Background {
        if let Some(pin) = pin_published_generation(data_root)? {
            return Ok(SourceBackedRefreshObservation {
                mode,
                status: "daemon_unavailable".to_owned(),
                request_id: None,
                daemon_available: false,
                source_count: 0,
                receipt: None,
                pin,
            });
        }
    }
    Err(SourceBackedRefreshDaemonUnavailable::new(error.map(|error| format!("{error:#}"))).into())
}

fn validate_daemon_refresh_response(response: &Value) -> Result<()> {
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    Err(anyhow!(
        "{}",
        response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("daemon source refresh request failed")
    ))
}

fn response_source_count(response: &Value) -> usize {
    response
        .get("progress")
        .and_then(|progress| progress.get("total_sources"))
        .or_else(|| response.get("source_count"))
        .and_then(Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or(0)
}

pub(super) fn pin_published_generation(
    data_root: &Path,
) -> Result<Option<PinnedSourceBackedGeneration>> {
    Ok(open_published_generation(data_root)?.map(|index| PinnedSourceBackedGeneration { index }))
}

#[cfg(test)]
mod tests {
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
            ExplicitSourceCatalogAuthority::from_json(
                &response["requested_explicit_source_catalog"]
            )
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
            ExplicitSourceCatalogAuthority::from_json(
                &published["published_explicit_source_catalog"]
            )
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
            reason: SourceBackedAutomaticUnavailableReason::SourceStatus(
                ProviderSourceStatus::Missing,
            ),
        };
        assert!(reject_blocking_automatic_registry_issues(&[missing]).is_ok());

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
        let error = reject_blocking_automatic_registry_issues(&[selector_gap]).unwrap_err();
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
    fn scheduled_refresh_activates_fresh_v026_source_state() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
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
        assert!(!ctx_history_core::database_path(data_root.clone()).exists());
        let marker = data_migration::inspect(&data_root)
            .unwrap()
            .expect("v0.26 epoch marker");
        assert_eq!(marker.phase, MigrationPhase::Ready);
        assert!(!marker.source_rebuild_required);
        assert_eq!(
            marker.lexical_generation_id.as_deref(),
            run.job["published_generation"].as_str()
        );
    }

    #[test]
    fn scheduled_refresh_failure_leaves_v026_source_state_resumable() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
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
        assert!(!ctx_history_core::database_path(data_root.clone()).exists());
        let marker = data_migration::inspect(&data_root)
            .unwrap()
            .expect("v0.26 epoch marker");
        assert_eq!(marker.phase, MigrationPhase::SourceRebuildFailed);
        assert!(marker.source_rebuild_required);
        assert!(marker.resumable);
        assert!(marker
            .error
            .as_deref()
            .is_some_and(|error| error.contains("provider-neutral executor failed")));
    }

    #[test]
    fn cold_failed_writer_artifacts_are_retried_as_no_prior_generation() {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
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
    }
}
