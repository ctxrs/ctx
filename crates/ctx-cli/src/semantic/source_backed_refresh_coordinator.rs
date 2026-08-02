use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant as StdInstant},
};

#[cfg(test)]
use std::fs;

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_capture::{
    build_automatic_source_backed_registry_from_report,
    discover_provider_sources_with_context_and_work_budget, source_backed_refresh_work_budget,
    validate_provider_source_roots_outside_data_root, CaptureError, DiscoveryContext,
    DiscoveryReport, ProviderSourceStatus, SourceBackedAutomaticRegistryIssue,
    SourceBackedAutomaticUnavailableReason, SourceBackedCoordinatorError,
    SourceBackedProviderRegistry,
    SourceBackedRefreshProgress as CaptureSourceBackedRefreshProgress, SourceBackedRefreshScope,
    SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
    SourceBackedSourceFailureClass, SourceBackedWatchCatalog,
};
#[cfg(test)]
use ctx_history_core::CaptureProvider;
use ctx_history_core::{utc_now, CertifiedSource, ScannedSourceCounts};
use ctx_history_index::{
    generation_incompatibility_requires_rebuild, IndexError, SourceRouteIdentity, VerifiedIndex,
    WriterOptions,
};
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
mod client;
mod coordinator_state;
mod current_state;

use capture_refresh::{
    execute_capture_owned_refresh, execute_source_backed_refresh, SourceBackedRefreshPlan,
};
#[cfg(test)]
use capture_refresh::{
    execute_capture_owned_refresh_with, refresh_all_provider_sources,
    reject_blocking_automatic_registry_issues,
};
use client::unknown_refresh_request_response;
pub(crate) use client::{
    coordinate_import_source_backed_refresh, coordinate_source_backed_refresh,
    SourceBackedRefreshDaemonUnavailable, SourceBackedRefreshObservation,
};
#[cfg(test)]
use coordinator_state::CaptureOwnedSourceBackedRefreshExecutor;
pub(in crate::semantic) use coordinator_state::CoreRefreshEngine;
use coordinator_state::SourceBackedRefreshProgressUpdate;
pub(crate) use coordinator_state::{
    PinnedCorePublication, SourceBackedRefreshExecution, SourceBackedRefreshExecutor,
    SourceBackedRefreshReceipt, SourceBackedRefreshSourceFailure, SourceBackedRefreshTimings,
};
pub(crate) use current_state::SourceBackedRefreshCurrent;

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";
const SOURCE_REFRESH_REQUEST_OP: &str = "source_refresh_request";
const SOURCE_REFRESH_STATUS_OP: &str = "source_refresh_status";
const SOURCE_REFRESH_UNKNOWN_REQUEST_STATE: &str = "request_unknown";
const SOURCE_REFRESH_UNKNOWN_REQUEST_ERROR_CODE: &str = "source_refresh_request_unknown";
const SOURCE_REFRESH_ATTEMPT_HISTORY: usize = 64;
const SOURCE_REFRESH_ACTIVE_PENDING_LIMIT: usize = 8;
const SOURCE_REFRESH_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
const SOURCE_REFRESH_IPC_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const SOURCE_REFRESH_RESPONSE_MAX_BYTES: u64 = 64 * 1024;
const SOURCE_REFRESH_BUILD_ISSUE_LIMIT: usize = 8;
const TERMINAL_COVERAGE_ERROR_CODE: &str = "all_provider_terminal_coverage_unavailable";
#[cfg(test)]
thread_local! {
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

fn open_retained_verified_index(
    index_root: &Path,
    generation_id: &str,
) -> std::result::Result<VerifiedIndex, IndexError> {
    #[cfg(test)]
    VERIFIED_INDEX_OPEN_COUNT.with(|count| {
        if let Some(current) = count.get() {
            count.set(Some(current.saturating_add(1)));
        }
    });
    VerifiedIndex::open_pinned_generation(index_root, generation_id)
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
}

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
    pub(crate) selected_route_ids: Vec<String>,
    pub(crate) successful_route_ids: Vec<String>,
    pub(crate) source_failures: Vec<SourceBackedRefreshSourceFailure>,
}

pub(super) fn source_backed_watch_catalog(data_root: &Path) -> Result<SourceBackedWatchCatalog> {
    capture_refresh::source_backed_watch_catalog(data_root)
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
        Err(error) if generation_incompatibility_requires_rebuild(&error) => Ok(None),
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
    let verified_current = SourceBackedRefreshCurrent::from_sources(
        &manifest.sources,
        publication.current.removed_source_count,
    )?;
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
    match VerifiedIndex::active_generation_id(&index_root) {
        Ok(Some(generation_id)) => Ok(Some(generation_id)),
        Ok(None) => {
            let Some(generation_id) = receipt_generation else {
                return Ok(None);
            };
            bail!(
                "retained lexical generation hint {generation_id} has no active generation at {}",
                index_root.display()
            )
        }
        Err(error) if generation_incompatibility_requires_rebuild(&error) => Ok(receipt_generation),
        Err(error) => Err(error.into()),
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

    pub(super) fn verified_index(&self) -> &VerifiedIndex {
        &self.index
    }

    #[cfg(test)]
    pub(crate) fn from_index(index: VerifiedIndex) -> Self {
        Self { index }
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
    let selected_route_ids =
        required_route_identity_array(value.get("selected_route_ids"), "selected_route_ids")?;
    let successful_route_ids =
        required_route_identity_array(value.get("successful_route_ids"), "successful_route_ids")?;
    let source_failures = required_source_failures(value.get("source_failures"))?;
    let failed_route_ids = source_failures
        .iter()
        .map(|failure| failure.route_identity.clone())
        .collect::<BTreeSet<_>>();
    let successful_route_id_set = successful_route_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let selected_route_id_set = selected_route_ids.iter().cloned().collect::<BTreeSet<_>>();
    if selected_route_id_set.len() != selected_route_ids.len()
        || successful_route_id_set.len() != successful_route_ids.len()
        || failed_route_ids.len() != source_failures.len()
        || !successful_route_id_set.is_disjoint(&failed_route_ids)
        || selected_route_id_set
            != successful_route_id_set
                .union(&failed_route_ids)
                .cloned()
                .collect()
    {
        bail!("published daemon source refresh has an invalid route-result partition");
    }

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
        SourceBackedRefreshCurrent::from_sources(&manifest.sources, current.removed_source_count)?;
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
        selected_route_ids,
        successful_route_ids,
        source_failures,
    })
}

fn required_route_identity_array(
    value: Option<&Value>,
    field: &'static str,
) -> Result<Vec<String>> {
    let value = value.ok_or_else(|| {
        anyhow!("published daemon source refresh receipt has no required {field} array")
    })?;
    let values = value.as_array().ok_or_else(|| {
        anyhow!("published daemon source refresh receipt {field} must be an array")
    })?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| is_sha256_identity(value))
                .map(str::to_owned)
                .ok_or_else(|| {
                    anyhow!(
                        "published daemon source refresh receipt {field} contains a malformed route identity"
                    )
                })
        })
        .collect()
}

fn required_source_failures(
    value: Option<&Value>,
) -> Result<Vec<SourceBackedRefreshSourceFailure>> {
    let value = value.ok_or_else(|| {
        anyhow!("published daemon source refresh receipt has no required source_failures array")
    })?;
    value
        .as_array()
        .ok_or_else(|| {
            anyhow!("published daemon source refresh receipt source_failures must be an array")
        })?
        .iter()
        .map(|value| {
            let value = value
                .as_object()
                .ok_or_else(|| anyhow!("daemon source refresh source failure is malformed"))?;
            let required = |field: &'static str| {
                value
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow!("daemon source refresh source failure has no {field}"))
            };
            Ok(SourceBackedRefreshSourceFailure {
                route_identity: required("route_identity")?
                    .into_sha256_identity("route_identity")?,
                source_identity: required("source_identity")?
                    .into_sha256_identity("source_identity")?,
                provider: required("provider")?,
                class: match required("class")?.as_str() {
                    "unavailable" => "unavailable".to_owned(),
                    "source_changed" => "source_changed".to_owned(),
                    "unreadable" => "unreadable".to_owned(),
                    "incompatible" => "incompatible".to_owned(),
                    _ => bail!("daemon source refresh source failure class is malformed"),
                },
                carried_forward: value
                    .get("carried_forward")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| {
                        anyhow!("daemon source refresh source failure has no carried_forward fact")
                    })?,
            })
        })
        .collect()
}

trait Sha256IdentityString {
    fn into_sha256_identity(self, field: &'static str) -> Result<String>;
}

impl Sha256IdentityString for String {
    fn into_sha256_identity(self, field: &'static str) -> Result<String> {
        if is_sha256_identity(&self) {
            Ok(self)
        } else {
            bail!("daemon source refresh source failure {field} is malformed")
        }
    }
}

fn is_sha256_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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

pub(super) fn pin_published_generation(
    data_root: &Path,
) -> Result<Option<PinnedSourceBackedGeneration>> {
    Ok(open_published_generation(data_root)?.map(|index| PinnedSourceBackedGeneration { index }))
}

fn pin_retained_generation(
    data_root: &Path,
    generation_id: &str,
) -> Result<PinnedSourceBackedGeneration> {
    let index_root = source_backed_index_root(data_root);
    let index = open_retained_verified_index(&index_root, generation_id).with_context(|| {
        format!(
            "open retained Core generation {generation_id} from {}",
            index_root.display()
        )
    })?;
    Ok(PinnedSourceBackedGeneration { index })
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
#[path = "source_backed_refresh_coordinator/pending_missing_tests.rs"]
mod pending_missing_tests;

#[cfg(test)]
#[path = "source_backed_refresh_coordinator/restart_recovery_tests.rs"]
mod restart_recovery_tests;

#[cfg(test)]
#[path = "source_backed_refresh_coordinator/source_backed_refresh_coordinator_tests.rs"]
mod tests;
