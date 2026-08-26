mod catalog_witness;
mod current_state;
mod execution;
mod explicit_source_catalog;
mod explicit_source_path;
mod metadata;
mod observation;
mod receipt;
mod receipt_parse;
mod registry_issues;
mod route_result;
#[cfg(test)]
mod tests;
mod types;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant as StdInstant},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_capture::{
    automatic_provider_root_coexistence_route_identity,
    automatic_provider_root_coexistence_source_lineage, automatic_source_backed_route_identity,
    build_automatic_source_backed_registry_from_report_with_retained_roots,
    discover_provider_sources_with_context_and_work_budget, source_backed_refresh_work_budget,
    source_backed_refresh_writer_options, validate_provider_source_roots_outside_data_root,
    DiscoveryContext, SourceBackedAutomaticRegistryIssue, SourceBackedAutomaticUnavailableReason,
    SourceBackedCoordinatorError,
    SourceBackedDetailedRefreshProgress as CaptureSourceBackedDetailedRefreshProgress,
    SourceBackedFailedRoute, SourceBackedFailedRouteOutcome, SourceBackedLogicalSourceFailures,
    SourceBackedProviderRegistry, SourceBackedRecordRejections, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResult, SourceBackedRouteSelection,
    SourceBackedSelectorAuthority, SourceBackedSourceFailureClass, SourceBackedSourceFailures,
    SourceBackedSuccessfulRouteOutcome, SourceBackedWatchCatalog,
    MAX_SOURCE_BACKED_ROUTE_CONTROL_BYTES,
};
#[cfg(test)]
use ctx_history_capture_model::DiscoveryIssue;
use ctx_history_capture_model::{
    DiscoveryIssueKind, DiscoveryReport, ProviderRootSourceIdentity, ProviderSource,
    ProviderSourceStatus, RetainedProviderRootAuthority,
};
use ctx_history_capture_runtime::{CapturePublicationDisposition, ImmutableCaptureSnapshot};
use ctx_history_core::{CaptureProvider, CertifiedSource, ScannedSourceCounts};
use ctx_history_index::{
    GenerationManifest, GenerationWriter, IndexError, SourceRouteIdentity, VerifiedIndex,
    WriterOptions,
};
use serde_json::{json, Value};

use catalog_witness::reconcile_published_catalog_witness;
use observation::{admitted_route_observations, run_after_capture_scan_before_metadata_hook};
use registry_issues::{
    automatic_registry_admission_failures, automatic_registry_route_less_blockers,
    selected_registry_route_count, terminal_registry_route_failures,
    AutomaticRegistryAdmissionFailurePolicy, RouteLessRegistryBlockers,
};
type SourceBackedRefreshOperation = RefreshOperation;

pub use ctx_history_capture::{SourceBackedReconciliationDemand, SourceBackedRefreshScope};
pub use current_state::SourceBackedRefreshCurrent;
#[doc(hidden)]
pub use execution::{exclusive_scan_stage_duration, execute_capture_owned_refresh_with};
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub use execution::{
    refresh_all_provider_sources_route_local,
    refresh_all_provider_sources_route_local_with_worksets,
};
#[cfg(any(test, feature = "test-support"))]
pub use explicit_source_catalog::explicit_source_catalog_authority_for_test;
pub use explicit_source_catalog::{
    explicit_source_for_path, relocate_explicit_source, upsert_explicit_source,
    validate_explicit_relocation_source, ExplicitSourceCatalogAuthority,
    ExplicitSourceCatalogRouteBinding, ExplicitSourceCatalogUpsert,
    ExplicitSourceRelocationAuthority,
};
use explicit_source_path::canonicalize_explicit_source_path;
pub use explicit_source_path::{
    explicit_source_path_is_symlink_or_reparse_point, explicit_source_path_metadata,
    explicit_source_path_symlink_metadata, ExplicitSourcePathMissing,
};
pub use metadata::{
    verify_generation_query_readiness, GenerationQueryReadiness, SourceBackedPublicationMetadata,
    SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
};
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub use observation::install_after_capture_scan_before_metadata_hook_for_test;
pub use receipt::SourceBackedRefreshReceipt;
pub use receipt_parse::{
    is_sha256_identity, optional_generation, parse_zero_source_authority,
    published_refresh_receipt_for_index, published_refresh_receipt_for_recovery,
    required_generation, required_route_results, validate_zero_source_authority,
    zero_source_authority_json,
};
#[doc(hidden)]
pub use registry_issues::{
    automatic_registry_route_failures, reject_blocking_automatic_registry_issues,
};
pub use route_result::{
    source_backed_route_retry_disposition, source_failure_class_is_typed,
    SourceBackedRefreshCatalogRouteOutcome, SourceBackedRefreshRecordRejection,
    SourceBackedRefreshRouteOutcome, SourceBackedRefreshRouteResult,
    SourceBackedRefreshSourceFailure,
};
pub use types::{
    nonzero_duration_micros, AdmittedRefresh, AdmittedRefreshCoverage, PublishedSourceBackedState,
    PublishedSourceBackedStatePort, RefreshOperation, SourceBackedAdmittedDiscovery,
    SourceBackedCurrentSourceProgress, SourceBackedCurrentSourceProgressStage,
    SourceBackedExactScanProgress, SourceBackedRefreshExecution, SourceBackedRefreshProgressUpdate,
    SourceBackedRefreshPublication, SourceBackedRefreshTimings, SourceBackedRefreshWorkset,
    SourceBackedZeroSourceAuthority, SourceBackedZeroSourceAuthorityKind,
};

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";
const SOURCE_REFRESH_BUILD_ISSUE_LIMIT: usize = 8;
const SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT: usize = 256;
const SOURCE_REFRESH_RECEIPT_JSON_BUDGET_BYTES: usize = 24 * 1024;
const TERMINAL_COVERAGE_ERROR_CODE: &str = "all_provider_terminal_coverage_unavailable";

#[derive(Debug)]
pub struct ZeroSourcePublicationBlocked {
    detail: String,
}

impl ZeroSourcePublicationBlocked {
    #[doc(hidden)]
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ZeroSourcePublicationBlocked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{TERMINAL_COVERAGE_ERROR_CODE}: {}", self.detail)
    }
}

impl std::error::Error for ZeroSourcePublicationBlocked {}

#[derive(Debug, Clone)]
pub struct SourceBackedAdmissionRouteFailure {
    route_identity: SourceRouteIdentity,
    kind: SourceBackedRouteErrorKind,
    detail: String,
}

impl SourceBackedAdmissionRouteFailure {
    #[doc(hidden)]
    pub fn new(
        route_identity: SourceRouteIdentity,
        kind: SourceBackedRouteErrorKind,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            route_identity,
            kind,
            detail: detail.into(),
        }
    }

    pub fn route_identity(&self) -> &SourceRouteIdentity {
        &self.route_identity
    }

    pub fn kind(&self) -> SourceBackedRouteErrorKind {
        self.kind
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }
}

#[derive(Debug)]
pub struct SourceBackedAdmissionRouteFailures {
    failures: Vec<SourceBackedAdmissionRouteFailure>,
}

impl SourceBackedAdmissionRouteFailures {
    #[doc(hidden)]
    pub fn try_from_failures(
        failures: impl IntoIterator<Item = SourceBackedAdmissionRouteFailure>,
    ) -> Result<Self> {
        let failures = failures
            .into_iter()
            .map(|failure| (failure.route_identity.clone(), failure))
            .collect::<BTreeMap<_, _>>()
            .into_values()
            .collect::<Vec<_>>();
        if failures.is_empty() {
            bail!("source-backed admission route failures cannot be empty");
        }
        if failures.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
            bail!(
                "source-backed admission route failures exceed the terminal route limit: {} > {SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT}",
                failures.len()
            );
        }
        Ok(Self { failures })
    }

    pub fn failures(&self) -> &[SourceBackedAdmissionRouteFailure] {
        &self.failures
    }
}

impl fmt::Display for SourceBackedAdmissionRouteFailures {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "source-backed admission has {} route registration failure(s): ",
            self.failures.len()
        )?;
        for (index, failure) in self
            .failures
            .iter()
            .take(SOURCE_REFRESH_BUILD_ISSUE_LIMIT)
            .enumerate()
        {
            if index != 0 {
                formatter.write_str("; ")?;
            }
            write!(
                formatter,
                "{}: {}",
                failure.route_identity.as_str(),
                failure.detail
            )?;
        }
        let omitted = self
            .failures
            .len()
            .saturating_sub(SOURCE_REFRESH_BUILD_ISSUE_LIMIT);
        if omitted != 0 {
            write!(formatter, "; {omitted} additional failure(s) omitted")?;
        }
        Ok(())
    }
}

impl std::error::Error for SourceBackedAdmissionRouteFailures {}

pub fn source_backed_index_root(data_root: &Path) -> PathBuf {
    data_root.join(SEARCH_DIRECTORY).join(LEXICAL_DIRECTORY)
}

pub fn source_backed_watch_catalog(
    data_root: &Path,
    discovery: &DiscoveryContext,
) -> Result<ctx_history_capture::SourceBackedWatchCatalog> {
    let discovery = discovery.clone().with_data_root(data_root);
    let work_budget =
        source_backed_refresh_work_budget(source_backed_refresh_writer_options().indexer_threads);
    let discovery_started = StdInstant::now();
    let report = discover_provider_sources_with_context_and_work_budget(&discovery, work_budget);
    let discovery_duration = discovery_started.elapsed();
    validate_provider_source_roots_outside_data_root(data_root, report.sources.iter())
        .context("validate provider roots before deriving source watch catalog")?;
    let index_root = source_backed_index_root(data_root);
    let retained_generation = match VerifiedIndex::open(&index_root) {
        Ok(index) => Some(index),
        Err(IndexError::MissingActiveGenerationPointer) => None,
        Err(error) => return Err(error.into()),
    };
    let retained_provider_roots =
        configured_retained_provider_roots(&discovery, retained_generation.as_ref())?;
    let mut build = build_automatic_source_backed_registry_from_report_with_retained_roots(
        &discovery,
        data_root,
        report,
        &retained_provider_roots,
    );
    build.discovery_duration = discovery_duration;
    Ok(build.registry.watch_catalog())
}

fn configured_retained_provider_roots(
    discovery: &DiscoveryContext,
    retained_generation: Option<&VerifiedIndex>,
) -> Result<BTreeMap<String, RetainedProviderRootAuthority>> {
    let roots = discovery.configured_provider_roots();
    let mut root_ids = BTreeSet::new();
    if let Some(duplicate) = roots.iter().find(|root| !root_ids.insert(root.id.as_str())) {
        bail!(
            "configured provider root id {:?} is not unique",
            duplicate.id
        );
    }
    roots
        .iter()
        .filter_map(|root| {
            let retained = retained_generation.and_then(|generation| {
                generation
                    .manifest()
                    .provider_roots()
                    .iter()
                    .find(|applied| provider_root_retention_compatible(applied.definition(), root))
            });
            retained
                .map(|applied| applied.retained_authority())
                .or_else(|| {
                    retained_generation.and_then(|generation| {
                        generation
                            .manifest()
                            .detached_released_provider_roots()
                            .iter()
                            .find(|authority| authority.matches_definition(root))
                            .map(|authority| Ok(authority.retained_authority()))
                    })
                })
                .map(|authority| authority.map(|authority| (root.id.clone(), authority)))
        })
        .collect::<ctx_history_index::Result<BTreeMap<_, _>>>()
        .map_err(Into::into)
}

fn provider_root_retention_compatible(
    retained: &ctx_history_capture::ProviderRootDefinition,
    current: &ctx_history_capture::ProviderRootDefinition,
) -> bool {
    retained.id == current.id
        && retained.provider == current.provider
        && retained.kind == current.kind
}

#[cfg(test)]
fn incompatible_configured_provider_root_routes(
    retained: &[ctx_history_index::AppliedProviderRoot],
    desired: &[ctx_history_capture::ProviderRootDefinition],
) -> BTreeSet<SourceRouteIdentity> {
    retained
        .iter()
        .filter(|root| {
            !desired
                .iter()
                .any(|current| provider_root_retention_compatible(root.definition(), current))
        })
        .flat_map(|root| {
            root.routes()
                .iter()
                .filter(|route| root.exact_source_tokens_for_route(route).is_none())
                .cloned()
        })
        .collect()
}

fn removed_configured_provider_root_routes(
    retained: &[ctx_history_index::AppliedProviderRoot],
    desired: &[ctx_history_capture::ProviderRootDefinition],
) -> BTreeSet<SourceRouteIdentity> {
    retained
        .iter()
        .filter(|root| {
            !desired
                .iter()
                .any(|current| root.definition().id == current.id)
        })
        .flat_map(|root| {
            root.routes()
                .iter()
                .filter(|route| root.exact_source_tokens_for_route(route).is_none())
                .cloned()
        })
        .collect()
}

#[doc(hidden)]
pub fn source_backed_requested_route_observations(
    catalog: &ctx_history_capture::SourceBackedWatchCatalog,
    requested_routes: &BTreeSet<SourceRouteIdentity>,
) -> BTreeMap<SourceRouteIdentity, Option<String>> {
    requested_routes
        .iter()
        .cloned()
        .map(|route| {
            let observation = catalog.certify_route_observation(&route);
            (route, observation)
        })
        .collect()
}

fn committed_generation_recovery_error(
    recovery: ctx_history_index::CommittedPredecessorMigrationRecovery,
) -> ctx_history_index::IndexError {
    ctx_history_index::IndexError::CommittedGenerationNeedsRecovery {
        generation_id: recovery.generation_id().to_owned(),
        stage: "predecessor migration recovery",
        detail: recovery.detail().to_owned(),
    }
}

fn compact_json(mut value: Value) -> Value {
    prune_null_json(&mut value);
    value
}

fn prune_null_json(value: &mut Value) {
    match value {
        Value::Object(map) => map.retain(|_, nested| {
            prune_null_json(nested);
            !nested.is_null()
        }),
        Value::Array(items) => items.iter_mut().for_each(prune_null_json),
        _ => {}
    }
}

#[doc(hidden)]
pub fn refresh_scope_json(scope: &SourceBackedRefreshScope) -> Value {
    match scope {
        SourceBackedRefreshScope::All => json!({ "kind": "all" }),
        SourceBackedRefreshScope::Exact(routes) => json!({
            "kind": "exact",
            "routes": routes.iter().map(SourceRouteIdentity::as_str).collect::<Vec<_>>(),
        }),
    }
}

#[doc(hidden)]
pub fn refresh_scope_from_json(value: Option<&Value>) -> Result<SourceBackedRefreshScope> {
    let value = value.ok_or_else(|| anyhow!("source refresh recovery scope is missing"))?;
    match value.get("kind").and_then(Value::as_str) {
        Some("all") => Ok(SourceBackedRefreshScope::All),
        Some("exact") => {
            let routes = value
                .get("routes")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("exact source refresh recovery scope has no route list"))?;
            if routes.is_empty() || routes.len() > SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT {
                bail!(
                    "exact source refresh recovery scope must contain 1..={SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT} routes"
                );
            }
            routes
                .iter()
                .map(|route| {
                    let route = route.as_str().ok_or_else(|| {
                        anyhow!("exact source refresh recovery route is not a string")
                    })?;
                    SourceRouteIdentity::from_sha256(route.to_owned()).map_err(Into::into)
                })
                .collect::<Result<BTreeSet<_>>>()
                .map(SourceBackedRefreshScope::Exact)
        }
        Some(kind) => bail!("unknown source refresh recovery scope kind `{kind}`"),
        None => bail!("source refresh recovery scope kind is missing"),
    }
}

pub fn execute_refresh(
    execution: SourceBackedRefreshExecution<'_>,
) -> Result<SourceBackedRefreshPublication> {
    execution::execute_capture_owned_refresh(execution)
}

#[doc(hidden)]
pub fn source_backed_watch_catalog_from_report(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    published_state: &dyn PublishedSourceBackedStatePort,
) -> Result<ctx_history_capture::SourceBackedWatchCatalog> {
    let merged = execution::build_merged_source_backed_registry(
        discovery,
        report,
        discovery_duration,
        data_root,
        explicit_source_catalog,
        published_state,
    )?;
    Ok(merged.build.registry.watch_catalog())
}

/// Builds exact, provider-neutral execution authority from one already-bounded
/// discovery report. The returned routes are derived from that same report;
/// this helper performs no discovery of its own.
#[doc(hidden)]
pub fn source_backed_admitted_discovery_from_report(
    discovery: &DiscoveryContext,
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    data_root: &Path,
    coverage: AdmittedRefreshCoverage,
    explicit_source_catalog: Option<&ExplicitSourceCatalogAuthority>,
    published_state: &dyn PublishedSourceBackedStatePort,
) -> Result<AdmittedRefresh> {
    // Explicit-catalog discovery is already reconstructed from the catalog
    // authority itself. Unlike automatic routes, explicit routes deliberately
    // do not expose replayable registration sources through the watch catalog.
    let request_admitted_report = (explicit_source_catalog.is_some()
        || coverage == AdmittedRefreshCoverage::CompleteCatalog)
        .then(|| report.clone());
    let merged = execution::build_merged_source_backed_registry(
        discovery,
        report,
        discovery_duration,
        data_root,
        explicit_source_catalog,
        published_state,
    )?;
    reject_blocking_automatic_registry_issues(&merged.build.issues)?;
    let watch_catalog = merged.build.registry.watch_catalog();
    let selected_routes = if explicit_source_catalog.is_some() {
        merged
            .requested_catalog_route_bindings
            .iter()
            .map(|binding| {
                SourceRouteIdentity::from_sha256(binding.route_identity.clone()).map_err(Into::into)
            })
            .collect::<Result<BTreeSet<_>>>()?
    } else if coverage == AdmittedRefreshCoverage::CompleteCatalog {
        // A newly admitted automatic-maintenance request owns the complete
        // catalog snapshot, including retained missing routes whose lifecycle
        // state must advance. This is the admission boundary; exact retries
        // reconstruct only their persisted route-local catalog authority.
        watch_catalog.route_ids().cloned().collect()
    } else {
        watch_catalog
            .route_ids()
            .filter(|route| {
                watch_catalog
                    .route_discovery_report(&BTreeSet::from([(*route).clone()]))
                    .is_some()
            })
            .cloned()
            .collect()
    };
    let route_worksets = explicit_source_catalog
        .map(|catalog| {
            catalog.automatic_route_worksets(
                &merged.build.registry,
                &merged.requested_catalog_route_bindings,
            )
        })
        .transpose()?
        .unwrap_or_default();
    let admission_failure_policy = match coverage {
        AdmittedRefreshCoverage::CompleteCatalog => {
            AutomaticRegistryAdmissionFailurePolicy::SystemicOnly
        }
        AdmittedRefreshCoverage::SelectedRoutes => {
            AutomaticRegistryAdmissionFailurePolicy::ScopedSelection
        }
    };
    if let Some(registration_failures) =
        automatic_registry_admission_failures(&merged.build.issues, admission_failure_policy)?
    {
        return Err(registration_failures.into());
    }
    let registry_failures = automatic_registry_route_failures(
        &merged.build.issues,
        merged.retained_generation.as_ref(),
    )?;
    if selected_routes.is_empty() {
        if coverage == AdmittedRefreshCoverage::SelectedRoutes && !registry_failures.is_empty() {
            return Err(SourceBackedCoordinatorError::NoUsableSourceRoutes {
                failed_routes: SourceBackedSourceFailures::from_failures(registry_failures),
            }
            .into());
        }
        let route_less_blockers =
            automatic_registry_route_less_blockers(&merged.build.issues, &registry_failures);
        if route_less_blockers.total != 0 {
            return Err(route_less_blockers.publication_error().into());
        }
    }
    let admitted_report = match request_admitted_report {
        Some(report) => report,
        None if selected_routes.is_empty() => DiscoveryReport {
            sources: Vec::new(),
            issues: Vec::new(),
        },
        None => watch_catalog
            .route_discovery_report(&selected_routes)
            .ok_or_else(|| {
                anyhow!("selected source routes have no provider-neutral discovery report")
            })?,
    };
    if coverage == AdmittedRefreshCoverage::SelectedRoutes && selected_routes.is_empty() {
        bail!("selected source discovery produced no executable source routes");
    }
    AdmittedRefresh::new(
        coverage,
        selected_routes,
        SourceBackedAdmittedDiscovery::new(admitted_report, discovery_duration, watch_catalog)
            .with_automatic_provider_discovery(discovery.automatic_provider_discovery_enabled())
            .with_configured_provider_roots(discovery.configured_provider_roots().to_vec()),
    )?
    .with_execution_facts(route_worksets)
}
