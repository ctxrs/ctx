#[cfg(test)]
use std::fs;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::{Duration as StdDuration, Instant as StdInstant},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_capture::{
    build_automatic_source_backed_registry_from_report,
    discover_provider_sources_with_context_and_work_budget, source_backed_refresh_work_budget,
    validate_provider_source_roots_outside_data_root, CaptureError, DiscoveryContext,
    DiscoveryReport, ProviderSourceStatus, SourceBackedAutomaticRegistryIssue,
    SourceBackedAutomaticUnavailableReason, SourceBackedProviderRegistry,
    SourceBackedRefreshProgress as CaptureSourceBackedRefreshProgress, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResult,
};
#[cfg(test)]
use ctx_history_core::CaptureProvider;
use ctx_history_core::{utc_now, CertifiedSource, ScannedSourceCounts};
use ctx_history_index::{IndexError, VerifiedIndex, WriterOptions};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    commands::import::{
        load_explicit_source_catalog_authority, validate_explicit_source_catalog_roots,
        ExplicitSourceCatalogAuthority,
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
mod current_state;

use capture_refresh::{execute_capture_owned_refresh, execute_source_backed_refresh};
#[cfg(test)]
use capture_refresh::{
    execute_capture_owned_refresh_with, refresh_all_provider_sources,
    reject_blocking_automatic_registry_issues,
};
#[cfg(test)]
use coordinator_state::CaptureOwnedSourceBackedRefreshExecutor;
pub(in crate::semantic) use coordinator_state::CoreRefreshEngine;
use coordinator_state::SourceBackedRefreshProgressUpdate;
pub(crate) use coordinator_state::{
    PinnedCorePublication, SourceBackedRefreshExecution, SourceBackedRefreshExecutor,
    SourceBackedRefreshReceipt, SourceBackedRefreshTimings,
};
pub(crate) use current_state::SourceBackedRefreshCurrent;

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";
const SOURCE_REFRESH_REQUEST_OP: &str = "source_refresh_request";
const SOURCE_REFRESH_STATUS_OP: &str = "source_refresh_status";
const SOURCE_REFRESH_ATTEMPT_HISTORY: usize = 64;
const SOURCE_REFRESH_ACTIVE_PENDING_LIMIT: usize = 8;
const SOURCE_REFRESH_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
const SOURCE_REFRESH_IPC_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const SOURCE_REFRESH_RESPONSE_MAX_BYTES: u64 = 64 * 1024;
const SOURCE_REFRESH_BUILD_ISSUE_LIMIT: usize = 8;
const TERMINAL_COVERAGE_ERROR_CODE: &str = "all_provider_terminal_coverage_unavailable";
thread_local! {
    /// Weak, exact-generation handoff for the synchronous daemon publication
    /// cycle. The coordinator's generation authority owns the strong pin; this
    /// slot neither prolongs its lifetime nor serves another generation.
    static DAEMON_CYCLE_VERIFIED_INDEX: RefCell<
        Option<(PathBuf, String, Weak<VerifiedIndex>)>,
    > = const { RefCell::new(None) };

    #[cfg(test)]
    static VERIFIED_INDEX_OPEN_COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

pub(super) fn open_verified_index(
    index_root: &Path,
) -> std::result::Result<VerifiedIndex, IndexError> {
    #[cfg(test)]
    VERIFIED_INDEX_OPEN_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
    VerifiedIndex::open_pinned(index_root)
}

#[cfg(test)]
pub(super) fn count_verified_index_opens<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    VERIFIED_INDEX_OPEN_COUNT.with(|count| {
        let previous = count.replace(Some(0));
        assert!(
            previous.is_none(),
            "verified-index open counters must not be nested"
        );
        let output = operation();
        let observed = count.replace(None).unwrap_or(0);
        (output, observed)
    })
}

fn retain_daemon_cycle_verified_index(index_root: &Path, index: &Arc<VerifiedIndex>) {
    let generation_id = index.generation_id().to_owned();
    let index = Arc::downgrade(index);
    DAEMON_CYCLE_VERIFIED_INDEX.with(|retained| {
        retained.replace(Some((index_root.to_path_buf(), generation_id, index)));
    });
}

pub(super) fn daemon_cycle_verified_index(
    data_root: &Path,
    generation_id: &str,
) -> Option<Arc<VerifiedIndex>> {
    let index_root = source_backed_index_root(data_root);
    DAEMON_CYCLE_VERIFIED_INDEX.with(|retained| {
        let retained = retained.borrow();
        let (retained_root, retained_generation, retained_index) = retained.as_ref()?;
        if retained_root != &index_root || retained_generation != generation_id {
            return None;
        }
        retained_index
            .upgrade()
            .filter(|index| index.generation_id() == generation_id)
    })
}

pub(super) fn nonzero_duration_micros(duration: StdDuration) -> u64 {
    u64::try_from(duration.as_micros())
        .unwrap_or(u64::MAX)
        .max(1)
}

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
        coordinate_source_backed_refresh_with_catalog(data_root, self, Some(authority), false)
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
    /// Exact explicit-source catalog snapshot registered into this publication.
    pub(crate) published_explicit_source_catalog: ExplicitSourceCatalogAuthority,
    pub(crate) scanned_routes: usize,
    pub(crate) unsupported_routes: usize,
    pub(crate) certified_source_count: usize,
    pub(crate) certified_source_bytes: u64,
    pub(crate) current: SourceBackedRefreshCurrent,
    pub(crate) timings: SourceBackedRefreshTimings,
}

pub(super) fn source_backed_index_root(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY)
}

fn published_generation_id(data_root: &Path) -> Result<Option<String>> {
    Ok(open_published_generation(data_root)?.map(|index| index.generation_id().to_owned()))
}

fn open_published_generation(data_root: &Path) -> Result<Option<VerifiedIndex>> {
    let index_root = source_backed_index_root(data_root);
    if !index_root.is_dir() {
        if let Some(generation_id) = published_generation_receipt(data_root)? {
            bail!(
                "verified Core generation {generation_id} is missing from {}",
                index_root.display()
            );
        }
        return Ok(None);
    }
    match open_verified_index(&index_root) {
        Ok(index) => Ok(Some(index)),
        Err(IndexError::MissingActiveGenerationPointer) => {
            if let Some(generation_id) = published_generation_receipt(data_root)? {
                bail!(
                    "verified Core generation {generation_id} is missing from {}",
                    index_root.display()
                );
            }
            Ok(None)
        }
        Err(error) => {
            Err(error).with_context(|| format!("open verified Core index {}", index_root.display()))
        }
    }
}

fn verify_source_backed_publication(
    publication: &SourceBackedRefreshPublication,
    verified: &VerifiedIndex,
) -> Result<()> {
    if verified.generation_id() != publication.generation_id {
        bail!(
            "source-backed refresh returned generation {}, but its verified pin carries {}",
            publication.generation_id,
            verified.generation_id()
        );
    }
    let manifest = verified.manifest();
    let verified_current =
        SourceBackedRefreshCurrent::from_sources(&manifest.sources, manifest.removals.len())?;
    if verified_current != publication.current
        || publication.certified_source_count != verified_current.source_count
        || publication.certified_source_bytes != verified_current.certified_source_bytes
        || manifest.indexed_documents != verified_current.indexed_documents
    {
        bail!("Core refresh publication facts do not match its exact verified generation");
    }
    Ok(())
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

fn retained_generation_hint(data_root: &Path) -> Result<Option<String>> {
    let receipt_generation =
        read_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root)).and_then(|job| {
            job.get("published_generation")
                .and_then(Value::as_str)
                .filter(|generation_id| !generation_id.is_empty())
                .map(str::to_owned)
        });
    let index_root = source_backed_index_root(data_root);
    if !index_root.is_dir() {
        if let Some(generation_id) = receipt_generation {
            bail!(
                "retained lexical generation hint {generation_id} has no active generation at {}",
                index_root.display()
            );
        }
        return Ok(None);
    }
    match VerifiedIndex::active_generation_id(&index_root)? {
        Some(generation_id) => Ok(Some(generation_id)),
        None => {
            let Some(generation_id) = receipt_generation else {
                return Ok(None);
            };
            bail!(
                "retained lexical generation hint {generation_id} has no active generation at {}",
                index_root.display()
            )
        }
    }
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
    coordinate_source_backed_refresh_with_catalog(data_root, mode, None, true)
}

pub(crate) fn coordinate_core_refresh_without_autostart(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
) -> Result<SourceBackedRefreshObservation> {
    coordinate_source_backed_refresh_with_catalog(data_root, mode, None, false)
}

fn coordinate_source_backed_refresh_with_catalog(
    data_root: &Path,
    mode: SourceBackedRefreshMode,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    allow_daemon_autostart: bool,
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
    if allow_daemon_autostart && config.daemon.enabled {
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

    wait_for_published_generation(
        data_root,
        request_id,
        mode,
        explicit_source_catalog,
        allow_daemon_autostart,
    )
}

fn wait_for_published_generation(
    data_root: &Path,
    mut request_id: String,
    mode: SourceBackedRefreshMode,
    expected_catalog: Option<&ExplicitSourceCatalogAuthority>,
    allow_daemon_autostart: bool,
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
                request_id = recover_wait_refresh_request(
                    data_root,
                    expected_catalog,
                    allow_daemon_autostart,
                )
                .with_context(|| {
                    format!("recover daemon while waiting for source refresh request {request_id}")
                })?;
                continue;
            }
            Err(error)
                if error
                    .downcast_ref::<DaemonSourceRefreshServiceUnavailable>()
                    .is_some() =>
            {
                request_id = recover_wait_refresh_request(
                    data_root,
                    expected_catalog,
                    allow_daemon_autostart,
                )
                .with_context(|| {
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
                let detail =
                    format!("daemon-owned source-backed refresh failed: {error}{retained}");
                match response.get("failure_type").and_then(Value::as_str) {
                    Some("unsupported_schema") => {
                        return Err(CaptureError::UnsupportedSchema(detail).into())
                    }
                    Some("malformed_source") => {
                        return Err(CaptureError::InvalidPayload(detail).into())
                    }
                    _ => {}
                }
                return Err(anyhow!("{detail}"));
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
    allow_daemon_autostart: bool,
) -> Result<String> {
    if !allow_daemon_autostart {
        return Err(SourceBackedRefreshDaemonUnavailable::new(Some(
            "the explicit source import disabled daemon autostart".to_owned(),
        ))
        .into());
    }
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
    let published_explicit_source_catalog = value
        .get("published_explicit_source_catalog")
        .ok_or_else(|| {
            anyhow!(
                "published daemon source refresh receipt has no explicit source catalog authority"
            )
        })
        .and_then(ExplicitSourceCatalogAuthority::from_json)?;
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
    let top_published_explicit_source_catalog = response
        .get("published_explicit_source_catalog")
        .ok_or_else(|| {
            anyhow!("published daemon source refresh has no explicit source catalog authority")
        })
        .and_then(ExplicitSourceCatalogAuthority::from_json)?;
    let identity_changed = previous_generation.as_deref() != Some(published_generation.as_str());
    if previous_generation != top_previous_generation
        || published_generation != top_published_generation
        || generation_changed != top_generation_changed
        || generation_changed != identity_changed
        || published_explicit_source_catalog != top_published_explicit_source_catalog
    {
        bail!(
            "published daemon source refresh receipt has inconsistent publication identity facts"
        );
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
        published_explicit_source_catalog,
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
#[path = "source_backed_refresh_coordinator/source_backed_refresh_coordinator_tests_retained_generation_tests.rs"]
mod retained_generation_tests;

#[cfg(test)]
#[path = "source_backed_refresh_coordinator/source_backed_refresh_coordinator_tests.rs"]
mod tests;
