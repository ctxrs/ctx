use std::{
    collections::{HashMap, VecDeque},
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant as StdInstant},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_capture::{
    build_automatic_source_backed_registry_from_report, discover_provider_sources_with_context,
    validate_provider_source_roots_outside_data_root, DiscoveryContext, DiscoveryReport,
    ProviderSourceStatus, SourceBackedAutomaticRegistryIssue,
    SourceBackedAutomaticUnavailableReason, SourceBackedProviderRegistry,
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
        validate_explicit_source_catalog_roots, ExplicitSourceCatalogAuthority,
    },
    compact_json,
    config::{AppConfig, DaemonMode},
    identity,
};

use super::{
    paths_status::{
        daemon_source_backed_refresh_job_path, read_daemon_job_status, read_daemon_status,
        write_daemon_job_status,
    },
    query_service::{daemon_source_refresh_request, DaemonSourceRefreshServiceUnavailable},
};

mod capture_refresh;
mod coordinator_state;
mod old_store_retirement;

use capture_refresh::{
    execute_capture_owned_refresh, execute_source_backed_refresh, hydration_failure_queues_refresh,
};
#[cfg(test)]
use capture_refresh::{
    execute_capture_owned_refresh_with, reject_blocking_automatic_registry_issues,
};
#[cfg(test)]
use coordinator_state::CaptureOwnedSourceBackedRefreshExecutor;
pub(in crate::semantic) use coordinator_state::SourceBackedRefreshCoordinator;
use coordinator_state::SourceBackedRefreshProgressUpdate;
pub(crate) use coordinator_state::{
    GenerationBoundSourceBackedResolver, SourceBackedRefreshExecution, SourceBackedRefreshExecutor,
    SourceBackedRefreshReceipt, SourceBackedRefreshTimings, SourceBackedResolverAccessError,
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
const TERMINAL_COVERAGE_ERROR_CODE: &str = "all_provider_terminal_coverage_unavailable";
// Covers a search/show generation pin crossing the daemon IPC boundary; an
// acquired Arc lease keeps its exact resolver alive beyond this grace.
const SOURCE_RESOLVER_RETIREMENT_GRACE: StdDuration = StdDuration::from_secs(5 * 60);

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

pub(super) fn source_backed_index_root(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY)
}

fn published_generation_id(data_root: &Path) -> Result<Option<String>> {
    Ok(open_published_generation(data_root)?.map(|index| index.generation_id().to_owned()))
}

fn open_published_generation(data_root: &Path) -> Result<Option<VerifiedIndex>> {
    let index_root = source_backed_index_root(data_root);
    if !index_root.join("meta.json").is_file() {
        if let Some(generation_id) = published_generation_receipt(data_root)? {
            bail!(
                "verified source-backed lexical generation {generation_id} is missing from {}",
                index_root.display()
            );
        }
        return Ok(None);
    }
    match VerifiedIndex::open(&index_root) {
        Ok(index) => Ok(Some(index)),
        // Tantivy creates schema-only meta.json before the first ctx commit.
        // It is replaceable only while no durable publication receipt proves
        // that a real generation was activated. Once publication succeeds,
        // the same typed error is corruption and remains fail-closed.
        Err(IndexError::MissingCommitPayload)
            if published_generation_receipt(data_root)?.is_none()
                && source_backed_lexical_artifact_is_uncommitted_schema_only(&index_root)? =>
        {
            Ok(None)
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "open verified source-backed lexical index {}",
                index_root.display()
            )
        }),
    }
}

pub(in crate::semantic) fn source_backed_lexical_artifact_is_uncommitted_schema_only(
    index_root: &Path,
) -> Result<bool> {
    let meta_path = index_root.join("meta.json");
    let meta: Value = serde_json::from_slice(
        &fs::read(&meta_path)
            .with_context(|| format!("read lexical metadata {}", meta_path.display()))?,
    )
    .with_context(|| format!("parse lexical metadata {}", meta_path.display()))?;
    let schema_only = meta.get("payload").is_none_or(Value::is_null)
        && meta.get("opstamp").and_then(Value::as_u64) == Some(0)
        && meta
            .get("segments")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
    Ok(schema_only)
}

fn published_generation_receipt(data_root: &Path) -> Result<Option<String>> {
    let Some(job) = read_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root))
    else {
        return Ok(None);
    };
    if job.get("request_state").and_then(Value::as_str) != Some("published") {
        return Ok(None);
    }
    Ok(job
        .get("published_generation")
        .and_then(Value::as_str)
        .filter(|generation_id| !generation_id.is_empty())
        .map(str::to_owned))
}

fn complete_verified_source_epoch(data_root: &Path, generation_id: &str) -> Result<()> {
    let verified = VerifiedIndex::open(source_backed_index_root(data_root))
        .context("reopen source-backed generation before retiring the old Store family")?;
    if verified.generation_id() != generation_id {
        bail!(
            "active source-backed generation {} changed before retiring old Store state for {generation_id}",
            verified.generation_id()
        );
    }
    remove_old_store_family(data_root)?;
    Ok(())
}

pub(in crate::semantic) fn reconcile_verified_source_epoch(data_root: &Path) -> Result<()> {
    let Some(verified) = open_published_generation(data_root)? else {
        return Ok(());
    };
    complete_verified_source_epoch(data_root, verified.generation_id())
}

fn remove_old_store_family(data_root: &Path) -> Result<()> {
    old_store_retirement::retire(data_root)
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

    let config = AppConfig::load(data_root)
        .context("load daemon configuration before source-backed refresh")?;
    if config.daemon.enabled {
        super::daemon_autostart::autostart_daemon_and_wait(
            data_root,
            &config,
            crate::DaemonTriggerCommandArg::Search,
        )
        .context("start or recover enabled daemon before source-backed refresh")?;
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
    mut request_id: String,
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
                request_id = recover_wait_refresh_request(data_root, expected_catalog)
                    .with_context(|| {
                        format!(
                            "recover daemon while waiting for source refresh request {request_id}"
                        )
                    })?;
                continue;
            }
            Err(error)
                if error
                    .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                    .is_some() =>
            {
                request_id =
                    recover_wait_refresh_request(data_root, expected_catalog).with_context(|| {
                        format!(
                            "recover unavailable daemon while waiting for source refresh request {request_id}: {error:#}"
                        )
                    })?;
                continue;
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

fn recover_wait_refresh_request(
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
) -> Result<String> {
    let config =
        AppConfig::load(data_root).context("load daemon configuration for refresh recovery")?;
    if !config.daemon.enabled {
        return Err(SourceBackedRefreshDaemonUnavailable::new(Some(
            "daemon was disabled while waiting for source refresh".to_owned(),
        ))
        .into());
    }
    super::daemon_autostart::autostart_daemon_and_wait(
        data_root,
        &config,
        crate::DaemonTriggerCommandArg::Search,
    )
    .context("restart daemon-owned source refresh service")?;
    let response = daemon_source_refresh_request(
        data_root,
        compact_json(json!({
            "schema_version": 1,
            "op": SOURCE_REFRESH_REQUEST_OP,
            "mode": SourceBackedRefreshMode::Wait.as_str(),
            "explicit_source_catalog": explicit_source_catalog
                .map(ExplicitSourceCatalogAuthority::to_json),
        })),
        SOURCE_REFRESH_IPC_TIMEOUT,
        SOURCE_REFRESH_RESPONSE_MAX_BYTES,
    )?
    .ok_or_else(|| {
        SourceBackedRefreshDaemonUnavailable::new(Some(
            "restarted daemon did not publish a source refresh endpoint".to_owned(),
        ))
    })?;
    validate_daemon_refresh_response(&response)?;
    response
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|request_id| !request_id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("recovered daemon source refresh response has no request ID"))
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

pub(crate) fn pin_active_verified_generation(
    data_root: &Path,
) -> Result<PinnedSourceBackedGeneration> {
    pin_published_generation(data_root)
        .context("source_unavailable: verify active Core generation")?
        .ok_or_else(|| anyhow!("source_unavailable: active verified Core generation is missing"))
}

#[cfg(test)]
#[path = "source_backed_refresh_coordinator/source_backed_refresh_coordinator_tests.rs"]
mod tests;
