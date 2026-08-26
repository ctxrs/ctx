#[cfg(test)]
use std::{collections::BTreeSet, sync::Arc};
use std::{
    fmt,
    path::Path,
    time::{Duration as StdDuration, Instant as StdInstant},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_capture::CaptureError;
use ctx_history_refresh::RefreshRuntime;
use serde_json::{json, Value};
use uuid::Uuid;

use ctx_history_refresh::{ExplicitSourceCatalogAuthority, ExplicitSourceRelocationAuthority};

use crate::compact_json;

use super::{
    query_service::{daemon_source_refresh_request, DaemonSourceRefreshServiceUnavailable},
    source_backed_refresh_adapter::{journal::DaemonRefreshJournal, runtime::DaemonRefreshRuntime},
};

mod client;
mod refresh_mode;
mod request;

#[cfg(feature = "test-support")]
pub use client::SourceRefreshObservationRecoveryFailed;

#[cfg(feature = "test-support")]
pub fn recover_wait_refresh_request_for_test(
    availability: &dyn crate::DaemonAvailabilityPort,
    data_root: &Path,
    request_id: &str,
) -> Result<String> {
    client::recover_wait_refresh_request(
        availability,
        data_root,
        request_id,
        ctx_history_refresh::RefreshRequestTrigger::Search,
        true,
    )
}

#[cfg(not(test))]
pub(crate) use ctx_history_refresh::RefreshEngine as CoreRefreshEngine;
pub use ctx_history_refresh::{
    explicit_catalog_request_is_accounted_for, optional_generation,
    published_refresh_receipt_for_index, RefreshIntent, RefreshOutcomeClass, RefreshRequest,
    RefreshRequestState, RefreshRequestTrigger, RefreshSelection, RefreshStatus, RefreshStatusKind,
    RefreshTerminalOutcome, SourceBackedCurrentSourceProgress, SourceBackedPublicationMetadata,
    SourceBackedRefreshReceipt,
};

#[cfg(test)]
pub(crate) use ctx_history_refresh::{
    open_verified_index, source_backed_index_root, EventWatermark, RefreshLogicalPhase,
    SourceBackedRefreshCurrent, SourceBackedRefreshExecution, SourceBackedRefreshExecutor,
    SourceBackedRefreshPublication, SourceBackedRefreshRouteResult,
    SourceBackedRefreshSourceFailure, SourceBackedRefreshTimings,
};

#[cfg(test)]
pub(crate) fn publish_authoritative_empty_generation_for_test(
    index_root: &Path,
    request_id: &str,
    operation: ctx_history_refresh::RefreshOperation,
    scope: ctx_history_capture::SourceBackedRefreshScope,
    published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
) -> Result<SourceBackedRefreshPublication> {
    publish_authoritative_empty_generation_with_route_results_for_test(
        index_root,
        request_id,
        operation,
        scope,
        published_explicit_source_catalog,
        None,
    )
}

#[cfg(test)]
pub(crate) fn publish_authoritative_empty_generation_with_route_results_for_test(
    index_root: &Path,
    request_id: &str,
    operation: ctx_history_refresh::RefreshOperation,
    scope: ctx_history_capture::SourceBackedRefreshScope,
    published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    route_results: Option<Vec<SourceBackedRefreshRouteResult>>,
) -> Result<SourceBackedRefreshPublication> {
    use ctx_history_index::{GenerationWriter, IndexError, SourceRouteIdentity, WriterOptions};
    use ctx_history_refresh::{
        SourceBackedZeroSourceAuthority, SourceBackedZeroSourceAuthorityKind,
    };

    let previous_generation = open_verified_index(index_root)
        .ok()
        .map(|index| index.generation_id().to_owned());
    let selected_routes = match &scope {
        ctx_history_capture::SourceBackedRefreshScope::All => {
            vec![SourceRouteIdentity::from_sha256("ab".repeat(32))?]
        }
        ctx_history_capture::SourceBackedRefreshScope::Exact(routes) => {
            if routes.is_empty() {
                return Err(anyhow!("authoritative-empty test scope has no route"));
            }
            routes.iter().cloned().collect()
        }
    };
    let route_results = route_results.unwrap_or_else(|| {
        selected_routes
            .iter()
            .map(|route| SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), true))
            .collect()
    });
    let authority_routes = route_results
        .iter()
        .map(|result| {
            if !result.outcome.is_success() {
                return Err(anyhow!(
                    "authoritative-empty test route did not succeed: {}",
                    result.route_identity
                ));
            }
            SourceRouteIdentity::from_sha256(result.route_identity.clone()).map_err(Into::into)
        })
        .collect::<Result<Vec<_>>>()?;
    if authority_routes.iter().collect::<BTreeSet<_>>().len() != authority_routes.len() {
        return Err(anyhow!(
            "authoritative-empty test fixture contains a duplicate route"
        ));
    }
    let refresh_scope = match &scope {
        ctx_history_capture::SourceBackedRefreshScope::All => json!({"kind": "all"}),
        ctx_history_capture::SourceBackedRefreshScope::Exact(routes) => json!({
            "kind": "exact",
            "routes": routes.iter().map(SourceRouteIdentity::as_str).collect::<Vec<_>>(),
        }),
    };
    let metadata_previous_generation = previous_generation.clone();
    let metadata_route_results = route_results.clone();
    let metadata_authority_routes = authority_routes.clone();
    let metadata_catalog = published_explicit_source_catalog.clone();
    let request_id = request_id.to_owned();
    let published = GenerationWriter::open(index_root, WriterOptions::default())?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?
        .commit_with_publication_metadata(
            |_| true,
            move |context| {
                let generation_id = context.generation_id().to_owned();
                let generation_changed =
                    metadata_previous_generation.as_deref() != Some(generation_id.as_str());
                let receipt = SourceBackedRefreshReceipt {
                    previous_generation: metadata_previous_generation.clone(),
                    published_generation: generation_id.clone(),
                    generation_changed,
                    published_explicit_source_catalog: metadata_catalog.clone(),
                    current: SourceBackedRefreshCurrent::default(),
                    route_results: metadata_route_results.clone(),
                    zero_source_authority: metadata_authority_routes
                        .iter()
                        .cloned()
                        .map(|route_identity| SourceBackedZeroSourceAuthority {
                            generation_id: generation_id.clone(),
                            route_identity,
                            kind: SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory,
                        })
                        .collect(),
                    catalog_route_bindings: Vec::new(),
                };
                serde_json::to_vec(&json!({
                    "version": ctx_history_refresh::SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
                    "request_id": request_id,
                    "operation": operation.as_str(),
                    "refresh_scope": refresh_scope,
                    "receipt": receipt.to_json(),
                    "route_observations": vec![Value::Null; metadata_authority_routes.len()],
                    "route_controls": {},
                    "committed_rejection_diagnostics": {},
                }))
                .map_err(|error| IndexError::PublicationMetadata(error.to_string()))
            },
        )?;
    let generation_id = published.receipt().generation_id.clone();
    let (_, _, verified_index) = published.into_parts();
    Ok(SourceBackedRefreshPublication {
        generation_id: generation_id.clone(),
        published_explicit_source_catalog,
        unsupported_routes: 0,
        certified_source_count: 0,
        certified_source_bytes: 0,
        current: SourceBackedRefreshCurrent::default(),
        timings: SourceBackedRefreshTimings::default(),
        route_results,
        zero_source_authority: authority_routes
            .into_iter()
            .map(|route_identity| SourceBackedZeroSourceAuthority {
                generation_id: generation_id.clone(),
                route_identity,
                kind: SourceBackedZeroSourceAuthorityKind::CompleteEmptyInventory,
            })
            .collect(),
        catalog_route_bindings: Vec::new(),
        verified_index: Some(Arc::new(verified_index)),
    })
}

#[cfg(test)]
pub(crate) struct CoreRefreshEngine(ctx_history_refresh::RefreshEngine);

#[cfg(test)]
impl std::ops::Deref for CoreRefreshEngine {
    type Target = ctx_history_refresh::RefreshEngine;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
type StatusWriter = Arc<dyn Fn(&Path, &Value) -> Result<()> + Send + Sync>;

#[cfg(test)]
type AdmissionFence = Arc<
    dyn for<'data, 'catalog> Fn(
            &'data Path,
            Option<&'catalog ExplicitSourceCatalogAuthority>,
        ) -> Result<
            std::collections::BTreeMap<ctx_history_index::SourceRouteIdentity, Option<String>>,
        > + Send
        + Sync,
>;

#[cfg(test)]
struct StatusWriterRefreshJournal {
    writer: StatusWriter,
}

#[cfg(test)]
struct CliTestRefreshRuntime {
    config: &'static dyn crate::DaemonConfigPort,
}

#[cfg(test)]
impl ctx_history_refresh::RefreshRuntime for CliTestRefreshRuntime {
    fn metadata(
        &self,
        data_root: &Path,
        operation: ctx_history_refresh::RefreshOperation,
    ) -> ctx_history_refresh::RefreshRuntimeMetadata {
        DaemonRefreshRuntime::new(self.config).metadata(data_root, operation)
    }

    fn discovery_context(&self, data_root: &Path) -> Result<ctx_history_capture::DiscoveryContext> {
        DaemonRefreshRuntime::new(self.config)
            .discovery_context(data_root)
            .or_else(|_| {
                Ok(ctx_history_capture::DiscoveryContext::from_process(
                    data_root.join("test-home"),
                ))
            })
    }
}

#[cfg(test)]
impl ctx_history_refresh::RefreshJournal for StatusWriterRefreshJournal {
    fn load(&self, data_root: &Path) -> Result<Option<Value>> {
        Ok(super::paths_status::read_daemon_job_status(
            &super::paths_status::daemon_source_backed_refresh_job_path(data_root),
        ))
    }

    fn store(&self, data_root: &Path, value: &Value) -> Result<()> {
        (self.writer)(
            &super::paths_status::daemon_source_backed_refresh_job_path(data_root),
            value,
        )
    }

    fn store_before_ack(
        &self,
        data_root: &Path,
        value: &Value,
    ) -> ctx_history_refresh::DurableAdmissionPersistence {
        match self.store(data_root, value) {
            Ok(()) => ctx_history_refresh::DurableAdmissionPersistence::Confirmed,
            Err(error) if self.load(data_root).ok().flatten().as_ref() == Some(value) => {
                ctx_history_refresh::DurableAdmissionPersistence::Retained(error)
            }
            Err(error) => ctx_history_refresh::DurableAdmissionPersistence::Failed(error),
        }
    }
}

#[cfg(test)]
impl CoreRefreshEngine {
    pub(crate) fn new() -> Self {
        Self::with_config(&crate::test_support::CONFIG)
    }

    pub(crate) fn with_config(config: &'static dyn crate::DaemonConfigPort) -> Self {
        Self(ctx_history_refresh::RefreshEngine::new(
            Arc::new(DaemonRefreshJournal),
            Arc::new(CliTestRefreshRuntime { config }),
        ))
    }

    pub(crate) fn with_executor(executor: Arc<dyn SourceBackedRefreshExecutor>) -> Self {
        Self(ctx_history_refresh::RefreshEngine::with_executor(
            Arc::new(DaemonRefreshJournal),
            Arc::new(CliTestRefreshRuntime {
                config: &crate::test_support::CONFIG,
            }),
            executor,
        ))
    }

    pub(crate) fn with_status_writer_for_test(
        executor: Arc<dyn SourceBackedRefreshExecutor>,
        writer: StatusWriter,
    ) -> Self {
        Self(ctx_history_refresh::RefreshEngine::with_executor(
            Arc::new(StatusWriterRefreshJournal { writer }),
            Arc::new(CliTestRefreshRuntime {
                config: &crate::test_support::CONFIG,
            }),
            executor,
        ))
    }

    pub(crate) fn with_runtime_for_test(
        executor: Arc<dyn SourceBackedRefreshExecutor>,
        admission_fence: AdmissionFence,
        writer: StatusWriter,
    ) -> Self {
        let adapted = Arc::new(
            move |_discovery: &ctx_history_capture::DiscoveryContext,
                  _journal: &dyn ctx_history_refresh::RefreshJournal,
                  data_root: &Path,
                  catalog: Option<&ExplicitSourceCatalogAuthority>| {
                admission_fence(data_root, catalog)
            },
        );
        Self(ctx_history_refresh::RefreshEngine::with_runtime_for_test(
            Arc::new(StatusWriterRefreshJournal { writer }),
            Arc::new(CliTestRefreshRuntime {
                config: &crate::test_support::CONFIG,
            }),
            executor,
            adapted,
        ))
    }

    pub(crate) fn with_admission_fence_for_test(admission_fence: AdmissionFence) -> Self {
        let adapted = Arc::new(
            move |_discovery: &ctx_history_capture::DiscoveryContext,
                  _journal: &dyn ctx_history_refresh::RefreshJournal,
                  data_root: &Path,
                  catalog: Option<&ExplicitSourceCatalogAuthority>| {
                admission_fence(data_root, catalog)
            },
        );
        Self(
            ctx_history_refresh::RefreshEngine::with_admission_fence_for_test(
                Arc::new(DaemonRefreshJournal),
                Arc::new(CliTestRefreshRuntime {
                    config: &crate::test_support::CONFIG,
                }),
                adapted,
            ),
        )
    }

    pub(crate) fn status(&self, request_id: &str) -> Option<Value> {
        self.0
            .status(request_id)
            .map(|status| status.schema_v1_fields().clone())
    }

    pub(crate) fn status_for_test(&self, request_id: &str) -> Option<Value> {
        self.status(request_id)
    }

    pub(crate) fn has_pending_request(&self) -> bool {
        self.0.has_pending_request()
    }

    pub(crate) fn request_activity_generation(&self) -> u64 {
        self.0.request_activity_generation()
    }

    pub(crate) fn handle_ipc_request(
        &self,
        data_root: &Path,
        request: &Value,
    ) -> Result<Option<Value>> {
        super::source_backed_refresh_adapter::wire::handle_ipc_request_for_test(
            &self.0, data_root, request,
        )
    }
}

#[allow(unused_imports)] // Stable typed terminal outcome for command/API integrations.
pub use client::{
    coordinate_import_source_backed_refresh_with_progress,
    coordinate_setup_source_backed_refresh_with_progress, coordinate_source_backed_refresh,
    coordinate_source_backed_refresh_with_progress, SourceBackedRefreshDaemonUnavailable,
    SourceBackedRefreshObservation, SourceBackedRefreshPendingPublication,
    SourceBackedRefreshTerminalError,
};
pub use refresh_mode::SourceBackedRefreshMode;
use request::SourceBackedRefreshRequest;

const SOURCE_REFRESH_REQUEST_OP: &str = "source_refresh_request";
const SOURCE_REFRESH_STATUS_OP: &str = "source_refresh_status";
const SOURCE_REFRESH_UNKNOWN_REQUEST_STATE: &str = "request_unknown";
const SOURCE_REFRESH_UNKNOWN_REQUEST_ERROR_CODE: &str = "source_refresh_request_unknown";
const SOURCE_REFRESH_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
const SOURCE_REFRESH_IPC_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const SOURCE_REFRESH_RESPONSE_MAX_BYTES: u64 = 64 * 1024;

pub struct PinnedSourceBackedGeneration(ctx_history_refresh::PinnedSourceBackedGeneration);

impl PinnedSourceBackedGeneration {
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_index(index: ctx_history_index::VerifiedIndex) -> Self {
        Self(ctx_history_refresh::PinnedSourceBackedGeneration::from_index(index))
    }

    pub fn generation_id(&self) -> &str {
        self.0.generation_id()
    }

    pub fn semantic_eligible_event_count(&self) -> Result<u64> {
        self.0.semantic_eligible_event_count()
    }

    pub fn verified_index(&self) -> &ctx_history_index::VerifiedIndex {
        self.0.verified_index()
    }

    pub fn into_index(self) -> ctx_history_index::VerifiedIndex {
        self.0.into_index()
    }
}

pub(crate) fn pin_published_generation(
    data_root: &Path,
) -> Result<Option<PinnedSourceBackedGeneration>> {
    Ok(
        ctx_history_refresh::pin_published_generation(data_root, &DaemonRefreshJournal)?
            .map(PinnedSourceBackedGeneration),
    )
}

pub fn pin_active_verified_generation(data_root: &Path) -> Result<PinnedSourceBackedGeneration> {
    ctx_history_refresh::pin_active_verified_generation(data_root, &DaemonRefreshJournal)
        .map(PinnedSourceBackedGeneration)
}

pub(crate) fn pin_retained_generation(
    data_root: &Path,
    generation_id: &str,
) -> Result<PinnedSourceBackedGeneration> {
    ctx_history_refresh::pin_retained_generation(data_root, generation_id)
        .map(PinnedSourceBackedGeneration)
}

fn published_refresh_receipt(
    response: &Value,
    pin: &PinnedSourceBackedGeneration,
) -> Result<SourceBackedRefreshReceipt> {
    ctx_history_refresh::published_refresh_receipt(response, &pin.0)
}

pub(super) fn source_backed_watch_catalog(
    data_root: &Path,
    config: &'static dyn crate::DaemonConfigPort,
) -> Result<ctx_history_capture::SourceBackedWatchCatalog> {
    let discovery_context = DaemonRefreshRuntime::new(config).discovery_context(data_root)?;
    ctx_history_refresh::source_backed_watch_catalog(data_root, &discovery_context)
}

pub fn published_explicit_source_relocation_authority(
    data_root: &std::path::Path,
    old_path: &std::path::Path,
) -> anyhow::Result<Option<ExplicitSourceRelocationAuthority>> {
    ctx_history_refresh::published_explicit_source_relocation_authority(
        data_root,
        old_path,
        &DaemonRefreshJournal,
    )
}

#[cfg(test)]
#[path = "source_backed_refresh_coordinator/restart_recovery_tests.rs"]
mod restart_recovery_tests;
