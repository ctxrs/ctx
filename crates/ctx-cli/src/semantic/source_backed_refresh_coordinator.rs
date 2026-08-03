use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant as StdInstant},
};

#[cfg(test)]
use std::fs;

use anyhow::{anyhow, bail, Context, Result};
#[cfg(test)]
use ctx_history_capture::SourceBackedRefreshProgress as CaptureSourceBackedRefreshProgress;
use ctx_history_capture::{
    build_automatic_source_backed_registry_from_report,
    discover_provider_sources_with_context_and_work_budget, source_backed_refresh_work_budget,
    source_backed_route_inventory, validate_provider_source_roots_outside_data_root, CaptureError,
    DiscoveryContext, DiscoveryReport, ProviderSourceStatus, RouteObservation,
    SourceBackedAutomaticRegistryIssue, SourceBackedAutomaticUnavailableReason,
    SourceBackedCoordinatorError,
    SourceBackedCurrentSourceProgress as CaptureSourceBackedCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage as CaptureSourceBackedCurrentSourceProgressStage,
    SourceBackedDetailedRefreshProgress as CaptureSourceBackedDetailedRefreshProgress,
    SourceBackedFailedRoute, SourceBackedFailedRouteOutcome, SourceBackedLogicalSourceFailures,
    SourceBackedProviderRegistry, SourceBackedRecordRejections, SourceBackedRefreshScope,
    SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
    SourceBackedSelectorAuthority, SourceBackedSourceFailureClass, SourceBackedSourceFailures,
    SourceBackedSuccessfulRouteOutcome, SourceBackedWatchCatalog,
};
#[cfg(test)]
use ctx_history_core::CaptureProvider;
use ctx_history_core::{utc_now, CertifiedSource, ScannedSourceCounts};
use ctx_history_index::{
    generation_incompatibility_requires_rebuild, GenerationManifest, IndexError,
    PublicationDisposition, SourceRouteIdentity, VerifiedIndex, WriterOptions,
};
use serde_json::{json, Value};
use uuid::Uuid;

#[cfg(test)]
use crate::commands::import::load_explicit_source_catalog_authority;
use crate::{
    commands::import::{ExplicitSourceCatalogAuthority, ExplicitSourceCatalogRouteBinding},
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
mod publication_metadata;
mod publication_observation;
mod refresh_mode;
mod request;

use capture_refresh::{
    execute_capture_owned_refresh, execute_source_backed_refresh,
    source_backed_route_admission_fence, SourceBackedRefreshPlan,
};
#[cfg(test)]
use capture_refresh::{
    execute_capture_owned_refresh_with, refresh_all_provider_sources,
    refresh_all_provider_sources_route_local, reject_blocking_automatic_registry_issues,
};
use client::unknown_refresh_request_response;
pub(crate) use client::{
    coordinate_import_source_backed_refresh_with_progress, coordinate_source_backed_refresh,
    SourceBackedRefreshDaemonUnavailable, SourceBackedRefreshObservation,
};
#[cfg(test)]
use coordinator_state::CaptureOwnedSourceBackedRefreshExecutor;
#[allow(unused_imports)] // Makes the receipt method's projected result type crate-visible.
pub(crate) use coordinator_state::SourceBackedRefreshCatalogRouteOutcome;
use coordinator_state::SourceBackedRefreshProgressUpdate;
#[allow(unused_imports)] // Consumed by #282's watcher integration seam.
pub(in crate::semantic) use coordinator_state::{
    CoreRefreshEngine, SourceBackedRefreshCoverageCertificate, VerifiedSourceRefreshRouteBoundary,
};
pub(crate) use coordinator_state::{
    PinnedCorePublication, SourceBackedCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage, SourceBackedRefreshExecution,
    SourceBackedRefreshExecutor, SourceBackedRefreshProgress, SourceBackedRefreshReceipt,
    SourceBackedRefreshRecordRejection, SourceBackedRefreshRouteOutcome,
    SourceBackedRefreshRouteResult, SourceBackedRefreshSourceFailure, SourceBackedRefreshTimings,
};
pub(crate) use current_state::SourceBackedRefreshCurrent;
use publication_metadata::SourceBackedPublicationMetadata;
#[cfg(test)]
use publication_observation::install_after_capture_scan_before_metadata_hook_for_test;
pub(crate) use refresh_mode::SourceBackedRefreshMode;
use request::{SourceBackedRefreshOperation, SourceBackedRefreshRequest};

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
const SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT: usize = 256;
const SOURCE_REFRESH_RECOVERY_ROUTE_LIMIT: usize = SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT;
// Reserve half of Core's opaque metadata ceiling for the versioned envelope,
// exact request/scope, and bounded route-observation vector. Receipt
// diagnostics fill only the remaining half and are explicitly counted when
// omitted, so malformed peers cannot crowd out publication of valid routes.
const SOURCE_REFRESH_RECEIPT_JSON_BUDGET_BYTES: usize = 24 * 1024;
const SOURCE_REFRESH_STARTUP_OBSERVATION_BUDGET: StdDuration = StdDuration::from_millis(250);
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

/// Provider-neutral publication returned by the capture-owned refresh
/// executor after it atomically advances the source-backed generation.
#[derive(Clone)]
pub(crate) struct SourceBackedRefreshPublication {
    pub(crate) generation_id: String,
    /// Exact request-scoped explicit-source overlay incorporated into this publication.
    pub(crate) published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub(crate) unsupported_routes: usize,
    pub(crate) certified_source_count: usize,
    pub(crate) certified_source_bytes: u64,
    pub(crate) current: SourceBackedRefreshCurrent,
    pub(crate) timings: SourceBackedRefreshTimings,
    pub(crate) route_results: Vec<SourceBackedRefreshRouteResult>,
    pub(crate) catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    /// Exact Core pin returned by the metadata-aware publication primitive.
    /// Synthetic executor tests may leave this absent.
    pub(crate) verified_index: Option<Arc<VerifiedIndex>>,
}

/// Immutable request facts already certified by the exact predecessor of a
/// manual all-route continuation. These facts must join the request receipt
/// before Core advances its pointer so crash recovery sees the same result as
/// the live coordinator.
#[derive(Debug, Clone, Default)]
pub(crate) struct SourceBackedRefreshCoveredPublication {
    pub(crate) route_results: Vec<SourceBackedRefreshRouteResult>,
    pub(crate) removed_source_count: usize,
    pub(crate) timings: SourceBackedRefreshTimings,
}

impl SourceBackedRefreshCoveredPublication {
    fn apply_receipt(&self, publication: &mut SourceBackedRefreshPublication) {
        publication
            .route_results
            .extend(self.route_results.iter().cloned());
        publication
            .route_results
            .sort_by(|left, right| left.route_identity.cmp(&right.route_identity));
        publication.current.removed_source_count = publication
            .current
            .removed_source_count
            .saturating_add(self.removed_source_count);
    }

    fn apply_timings(&self, publication: &mut SourceBackedRefreshPublication) {
        publication.timings.discovery_us = publication
            .timings
            .discovery_us
            .saturating_add(self.timings.discovery_us);
        publication.timings.scan_stage_us = publication
            .timings
            .scan_stage_us
            .saturating_add(self.timings.scan_stage_us);
        publication.timings.commit_us = publication
            .timings
            .commit_us
            .saturating_add(self.timings.commit_us);
    }

    pub(crate) fn apply(&self, publication: &mut SourceBackedRefreshPublication) {
        self.apply_receipt(publication);
        self.apply_timings(publication);
    }
}

impl fmt::Debug for SourceBackedRefreshPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceBackedRefreshPublication")
            .field("generation_id", &self.generation_id)
            .field("route_results", &self.route_results)
            .field("has_verified_index", &self.verified_index.is_some())
            .finish_non_exhaustive()
    }
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
    let route_rejected_record_total =
        publication
            .route_results
            .iter()
            .try_fold(0_u64, |total, result| {
                total
                    .checked_add(result.rejected_record_total)
                    .ok_or_else(|| {
                        anyhow!("Core refresh publication route rejected-record total overflow")
                    })
            })?;
    if route_rejected_record_total > verified_current.rejected_records {
        bail!("Core refresh publication route rejections exceed its exact verified generation");
    }
    let witness_lineages = publication
        .published_explicit_source_catalog
        .as_ref()
        .map(ExplicitSourceCatalogAuthority::route_lineages)
        .unwrap_or_default();
    if publication.catalog_route_bindings.iter().any(|binding| {
        if witness_lineages.contains(&binding.catalog_lineage) {
            return SourceRouteIdentity::from_sha256(binding.route_identity.clone())
                .ok()
                .is_none_or(|route| manifest.source_route(&route).is_none());
        }
        !publication.route_results.iter().any(|result| {
            result.route_identity == binding.route_identity
                && matches!(
                    result.outcome,
                    SourceBackedRefreshRouteOutcome::Failed {
                        carried_forward: false,
                        ..
                    }
                )
        })
    }) {
        bail!("Core refresh publication catalog binding has no generation-bound authority or cold request failure");
    }
    Ok(())
}

fn explicit_catalog_request_is_accounted_for(
    requested: &ExplicitSourceCatalogAuthority,
    published: Option<&ExplicitSourceCatalogAuthority>,
    bindings: &[ExplicitSourceCatalogRouteBinding],
    route_results: &[SourceBackedRefreshRouteResult],
) -> bool {
    if published.is_some_and(|catalog| catalog.carries_request(requested)) {
        return true;
    }
    let lineages = requested.route_lineages();
    !lineages.is_empty()
        && lineages.iter().all(|lineage| {
            bindings
                .iter()
                .find(|binding| binding.catalog_lineage == *lineage)
                .is_some_and(|binding| {
                    route_results.iter().any(|result| {
                        result.route_identity == binding.route_identity
                            && matches!(
                                result.outcome,
                                SourceBackedRefreshRouteOutcome::Failed {
                                    carried_forward: false,
                                    ..
                                }
                            )
                    })
                })
        })
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
    published_refresh_receipt_for_index(response, &pin.index)
}

fn published_refresh_receipt_for_index(
    response: &Value,
    verified_index: &VerifiedIndex,
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
        .map(ExplicitSourceCatalogAuthority::from_json)
        .transpose()?;
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
    let selected_route_total = required_usize(value, "selected_route_total")?;
    let successful_route_total = required_usize(value, "successful_route_total")?;
    let route_results = required_route_results(value.get("route_results"))?;
    let expected_catalog_lineages = published_explicit_source_catalog
        .as_ref()
        .map(ExplicitSourceCatalogAuthority::route_lineages)
        .unwrap_or_default();
    let catalog_route_bindings = required_catalog_route_bindings(
        value.get("catalog_route_bindings"),
        verified_index.manifest(),
        &route_results,
        &expected_catalog_lineages,
    )?;
    let actual_catalog_lineages = catalog_route_bindings
        .iter()
        .map(|binding| binding.catalog_lineage.clone())
        .collect::<BTreeSet<_>>();
    let derived_successful_route_total = route_results
        .iter()
        .filter(|result| result.outcome.is_success())
        .count();
    let derived_source_failure_total =
        route_results.iter().try_fold(0_usize, |total, result| {
            total
                .checked_add(result.source_failure_total)
                .ok_or_else(|| anyhow!("published daemon source-failure total overflow"))
        })?;
    let source_failure_diagnostic_total =
        route_results.iter().try_fold(0_usize, |total, result| {
            total
                .checked_add(result.source_failures.len())
                .ok_or_else(|| anyhow!("published daemon source-failure diagnostic total overflow"))
        })?;
    let derived_rejected_record_total = route_results.iter().try_fold(0_u64, |total, result| {
        total
            .checked_add(result.rejected_record_total)
            .ok_or_else(|| anyhow!("published daemon rejected-record total overflow"))
    })?;
    let rejection_diagnostic_total = route_results.iter().try_fold(0_u64, |total, result| {
        total
            .checked_add(result.rejection_diagnostics.len() as u64)
            .ok_or_else(|| anyhow!("published daemon rejection diagnostic total overflow"))
    })?;
    let source_failure_total = required_usize(value, "source_failure_total")?;
    let source_failures_omitted = required_usize(value, "source_failures_omitted")?;
    let rejected_record_total = required_u64(value, "rejected_record_total")?;
    let rejection_diagnostics_omitted = required_u64(value, "rejection_diagnostics_omitted")?;
    if selected_route_total != route_results.len()
        || successful_route_total != derived_successful_route_total
        || source_failure_total != derived_source_failure_total
        || source_failures_omitted
            != source_failure_total.saturating_sub(source_failure_diagnostic_total)
        || rejected_record_total != derived_rejected_record_total
        || rejected_record_total > current.rejected_records
        || rejection_diagnostics_omitted
            != rejected_record_total.saturating_sub(rejection_diagnostic_total)
        || !expected_catalog_lineages.is_subset(&actual_catalog_lineages)
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
    let identity_changed = previous_generation.as_deref() != Some(published_generation.as_str());
    let request_identity_changed =
        top_previous_generation.as_deref() != Some(top_published_generation.as_str());
    if published_generation != top_published_generation
        || generation_changed != identity_changed
        || top_generation_changed != request_identity_changed
    {
        bail!(
            "published daemon source refresh receipt has inconsistent publication identity facts"
        );
    }

    let manifest = verified_index.manifest();
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
        route_results,
        catalog_route_bindings,
    })
}

pub(crate) fn published_explicit_source_relocation_authority(
    data_root: &Path,
    old_path: &Path,
) -> Result<Option<crate::commands::import::ExplicitSourceRelocationAuthority>> {
    let verified = open_published_generation(data_root)?
        .ok_or_else(|| anyhow!("explicit relocation requires an active Core publication"))?;
    let metadata = SourceBackedPublicationMetadata::decode(&verified)
        .context("load exact explicit relocation authority from Core publication metadata")?;
    let receipt = published_refresh_receipt_for_index(&metadata.response_value(), &verified)?;
    receipt
        .published_explicit_source_catalog
        .as_ref()
        .map(|catalog| catalog.relocation_authority(old_path, &receipt.catalog_route_bindings))
        .transpose()
        .map(Option::flatten)
}

fn required_route_results(value: Option<&Value>) -> Result<Vec<SourceBackedRefreshRouteResult>> {
    let value = value
        .ok_or_else(|| anyhow!("published daemon source refresh receipt has no route_results"))?;
    let values = value.as_object().ok_or_else(|| {
        anyhow!("published daemon source refresh receipt route_results must be an object")
    })?;
    if values.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
        bail!(
            "published daemon source refresh exceeds the bounded route-result limit of {SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT}"
        );
    }
    values
        .iter()
        .map(|(route_identity, value)| {
            if !is_sha256_identity(route_identity) {
                bail!("published daemon source refresh route identity is invalid");
            }
            let fields = value.as_array().ok_or_else(|| {
                anyhow!("published daemon source refresh compact route result must be an array")
            })?;
            let (
                outcome,
                source_failure_total,
                source_failures,
                rejected_record_total,
                rejection_diagnostics,
            ) = match fields.first().and_then(Value::as_str) {
                Some("s") if fields.len() == 2 => {
                    let changed = fields[1].as_bool().ok_or_else(|| {
                        anyhow!("published daemon successful route result has no changed fact")
                    })?;
                    (
                        SourceBackedRefreshRouteOutcome::Succeeded { changed },
                        0,
                        Vec::new(),
                        0,
                        Vec::new(),
                    )
                }
                Some("s") if fields.len() == 6 => {
                    let changed = fields[1].as_bool().ok_or_else(|| {
                        anyhow!("published daemon successful route result has no changed fact")
                    })?;
                    let total =
                        required_usize_from_value(fields.get(2), "route source_failure_total")?;
                    let failures = required_route_source_failures(route_identity, fields.get(3))?;
                    let rejected_record_total =
                        required_u64_from_value(fields.get(4), "route rejected_record_total")?;
                    let rejection_diagnostics =
                        required_route_rejection_diagnostics(route_identity, fields.get(5))?;
                    (
                        SourceBackedRefreshRouteOutcome::Succeeded { changed },
                        total,
                        failures,
                        rejected_record_total,
                        rejection_diagnostics,
                    )
                }
                Some("f") if fields.len() == 5 => {
                    let class = compact_source_failure_class(fields[1].as_str())?;
                    let carried_forward = fields[2].as_bool().ok_or_else(|| {
                        anyhow!("published daemon failed route result has no carried-forward fact")
                    })?;
                    let total =
                        required_usize_from_value(fields.get(3), "route source_failure_total")?;
                    let failures = required_route_source_failures(route_identity, fields.get(4))?;
                    (
                        SourceBackedRefreshRouteOutcome::Failed {
                            class,
                            carried_forward,
                        },
                        total,
                        failures,
                        0,
                        Vec::new(),
                    )
                }
                _ => bail!("published daemon source refresh route result has inconsistent fields"),
            };
            let result = SourceBackedRefreshRouteResult {
                route_identity: route_identity.clone(),
                outcome,
                source_failure_total,
                source_failures,
                rejected_record_total,
                rejection_diagnostics,
            };
            result.validate_source_failures()?;
            Ok(result)
        })
        .collect()
}

fn required_route_rejection_diagnostics(
    route_identity: &str,
    value: Option<&Value>,
) -> Result<Vec<SourceBackedRefreshRecordRejection>> {
    let value =
        value.ok_or_else(|| anyhow!("terminal route result has no rejection diagnostics"))?;
    value
        .as_array()
        .ok_or_else(|| anyhow!("terminal route result rejection diagnostics must be an array"))?
        .iter()
        .map(|value| {
            let fields = value
                .as_array()
                .filter(|fields| fields.len() == 7)
                .ok_or_else(|| {
                    anyhow!("daemon source refresh compact rejection diagnostic is malformed")
                })?;
            let required = |index: usize, field: &'static str| {
                fields[index]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        anyhow!("daemon source refresh rejection diagnostic has no {field}")
                    })
            };
            Ok(SourceBackedRefreshRecordRejection {
                route_identity: route_identity.to_owned(),
                source_identity: required(0, "source_identity")?
                    .into_sha256_identity("source_identity")?,
                provider: required(1, "provider")?,
                source_selector: required(2, "source_selector")?,
                line: required_u64_from_value(fields.get(3), "rejection line")?,
                payload_type: required(4, "payload_type")?,
                class: compact_record_rejection_class(fields[5].as_str())?,
                detail: required(6, "detail")?,
            })
        })
        .collect()
}

fn compact_record_rejection_class(value: Option<&str>) -> Result<String> {
    Ok(match value {
        Some("m") => "malformed_record",
        Some("u") => "unsupported_record",
        _ => bail!("published daemon source refresh record rejection class is invalid"),
    }
    .to_owned())
}

fn required_catalog_route_bindings(
    value: Option<&Value>,
    manifest: &GenerationManifest,
    route_results: &[SourceBackedRefreshRouteResult],
    expected_catalog_lineages: &BTreeSet<String>,
) -> Result<Vec<ExplicitSourceCatalogRouteBinding>> {
    let values = value
        .ok_or_else(|| {
            anyhow!("published daemon source refresh receipt has no catalog_route_bindings")
        })?
        .as_object()
        .ok_or_else(|| {
            anyhow!(
                "published daemon source refresh receipt catalog_route_bindings must be an object"
            )
        })?;
    let retained = manifest
        .source_routes()
        .iter()
        .map(|route| route.route_identity().as_str())
        .collect::<BTreeSet<_>>();
    values
        .iter()
        .map(|(catalog_lineage, route_identity)| {
            if !is_sha256_identity(catalog_lineage) {
                bail!("published daemon source refresh catalog lineage is invalid");
            }
            let route_identity = route_identity.as_str().ok_or_else(|| {
                anyhow!("published daemon source refresh catalog binding route is invalid")
            })?;
            let retained_witness = expected_catalog_lineages.contains(catalog_lineage)
                && retained.contains(route_identity);
            let cold_request_failure = !expected_catalog_lineages.contains(catalog_lineage)
                && route_results.iter().any(|result| {
                    result.route_identity == route_identity
                        && matches!(
                            result.outcome,
                            SourceBackedRefreshRouteOutcome::Failed {
                                carried_forward: false,
                                ..
                            }
                        )
                });
            if !retained_witness && !cold_request_failure {
                bail!("published daemon source refresh catalog binding is neither a retained witness nor a cold request failure");
            }
            Ok(ExplicitSourceCatalogRouteBinding {
                catalog_lineage: catalog_lineage.clone(),
                route_identity: route_identity.to_owned(),
            })
        })
        .collect()
}

fn required_route_source_failures(
    route_identity: &str,
    value: Option<&Value>,
) -> Result<Vec<SourceBackedRefreshSourceFailure>> {
    let value = value.ok_or_else(|| anyhow!("terminal route result has no source diagnostics"))?;
    value
        .as_array()
        .ok_or_else(|| anyhow!("terminal route result source diagnostics must be an array"))?
        .iter()
        .map(|value| {
            let fields = value
                .as_array()
                .filter(|fields| fields.len() == 6)
                .ok_or_else(|| {
                    anyhow!("daemon source refresh compact source diagnostic is malformed")
                })?;
            let required = |index: usize, field: &'static str| {
                fields[index]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        anyhow!("daemon source refresh source diagnostic has no {field}")
                    })
            };
            Ok(SourceBackedRefreshSourceFailure {
                route_identity: route_identity.to_owned(),
                source_identity: required(0, "source_identity")?
                    .into_sha256_identity("source_identity")?,
                provider: required(1, "provider")?,
                class: compact_source_failure_class(fields[2].as_str())?,
                carried_forward: fields[3].as_bool().ok_or_else(|| {
                    anyhow!("daemon source refresh source diagnostic has no carried_forward fact")
                })?,
                source_selector: required(4, "source_selector")?,
                detail: required(5, "detail")?,
            })
        })
        .collect()
}

fn compact_source_failure_class(value: Option<&str>) -> Result<String> {
    Ok(match value {
        Some("u") => "unavailable",
        Some("c") => "source_changed",
        Some("r") => "unreadable",
        Some("i") => "incompatible",
        _ => bail!("published daemon source refresh source failure class is invalid"),
    }
    .to_owned())
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
