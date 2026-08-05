#[cfg(test)]
use std::sync::Arc;
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

use crate::{commands::import::ExplicitSourceCatalogAuthority, compact_json, config::AppConfig};

use super::{
    query_service::{daemon_source_refresh_request, DaemonSourceRefreshServiceUnavailable},
    source_backed_refresh_adapter::{journal::DaemonRefreshJournal, runtime::DaemonRefreshRuntime},
};

mod client;
mod refresh_mode;
mod request;

#[cfg(not(test))]
pub(in crate::semantic) use ctx_history_refresh::RefreshEngine as CoreRefreshEngine;
pub(crate) use ctx_history_refresh::{
    explicit_catalog_request_is_accounted_for, nonzero_duration_micros, open_verified_index,
    optional_generation, published_refresh_receipt_for_index, source_backed_index_root,
    PinnedCorePublication, RefreshOutcomeClass, RefreshRequestState, RefreshStatus,
    RefreshStatusKind, RefreshTerminalOutcome, SourceBackedCurrentSourceProgress,
    SourceBackedPublicationMetadata, SourceBackedRefreshReceipt,
};

#[cfg(test)]
pub(crate) use ctx_history_refresh::{
    EventWatermark, RefreshLogicalPhase, SourceBackedRefreshCurrent, SourceBackedRefreshExecution,
    SourceBackedRefreshExecutor, SourceBackedRefreshPublication, SourceBackedRefreshRouteResult,
    SourceBackedRefreshSourceFailure, SourceBackedRefreshTimings,
};

#[cfg(test)]
pub(in crate::semantic) struct CoreRefreshEngine(ctx_history_refresh::RefreshEngine);

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
#[derive(Debug, Default)]
struct CliTestRefreshRuntime;

#[cfg(test)]
impl ctx_history_refresh::RefreshRuntime for CliTestRefreshRuntime {
    fn metadata(
        &self,
        data_root: &Path,
        operation: ctx_history_refresh::RefreshOperation,
    ) -> ctx_history_refresh::RefreshRuntimeMetadata {
        DaemonRefreshRuntime.metadata(data_root, operation)
    }

    fn discovery_context(&self, data_root: &Path) -> Result<ctx_history_capture::DiscoveryContext> {
        DaemonRefreshRuntime
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
    pub(in crate::semantic) fn new() -> Self {
        Self(ctx_history_refresh::RefreshEngine::new(
            Arc::new(DaemonRefreshJournal),
            Arc::new(CliTestRefreshRuntime),
        ))
    }

    pub(in crate::semantic) fn with_executor(
        executor: Arc<dyn SourceBackedRefreshExecutor>,
    ) -> Self {
        Self(ctx_history_refresh::RefreshEngine::with_executor(
            Arc::new(DaemonRefreshJournal),
            Arc::new(CliTestRefreshRuntime),
            executor,
        ))
    }

    pub(in crate::semantic) fn with_status_writer_for_test(
        executor: Arc<dyn SourceBackedRefreshExecutor>,
        writer: StatusWriter,
    ) -> Self {
        Self(ctx_history_refresh::RefreshEngine::with_executor(
            Arc::new(StatusWriterRefreshJournal { writer }),
            Arc::new(CliTestRefreshRuntime),
            executor,
        ))
    }

    pub(in crate::semantic) fn with_runtime_for_test(
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
            Arc::new(CliTestRefreshRuntime),
            executor,
            adapted,
        ))
    }

    pub(in crate::semantic) fn with_admission_fence_for_test(
        admission_fence: AdmissionFence,
    ) -> Self {
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
                Arc::new(CliTestRefreshRuntime),
                adapted,
            ),
        )
    }

    pub(in crate::semantic) fn status(&self, request_id: &str) -> Option<Value> {
        self.0
            .status(request_id)
            .map(|status| status.schema_v1_fields().clone())
    }

    pub(in crate::semantic) fn status_for_test(&self, request_id: &str) -> Option<Value> {
        self.status(request_id)
    }

    pub(in crate::semantic) fn has_pending_request(&self) -> bool {
        self.0.has_pending_request()
    }

    pub(in crate::semantic) fn pinned_core_publication(
        &self,
    ) -> Option<Arc<PinnedCorePublication>> {
        self.0.pinned_core_publication()
    }

    pub(in crate::semantic) fn handle_ipc_request(
        &self,
        data_root: &Path,
        request: &Value,
    ) -> Result<Option<Value>> {
        super::source_backed_refresh_adapter::wire::handle_ipc_request_for_test(
            &self.0, data_root, request,
        )
    }

    pub(in crate::semantic) fn handle_ipc_request_with_admission_fence_for_test(
        &self,
        data_root: &Path,
        request: &Value,
        observations: std::collections::BTreeMap<
            ctx_history_index::SourceRouteIdentity,
            Option<String>,
        >,
    ) -> Result<Option<Value>> {
        let response = self.handle_ipc_request(data_root, request)?;
        let Some(request_id) = response
            .as_ref()
            .and_then(|response| response.get("request_id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Ok(response);
        };
        if response
            .as_ref()
            .is_some_and(|response| response["request_state"] == "admission_pending")
        {
            self.0
                .complete_pending_admission_for_test(data_root, &request_id, observations)?;
            return Ok(self.status(&request_id));
        }
        Ok(response)
    }
}

#[allow(unused_imports)] // Stable typed terminal outcome for command/API integrations.
pub(crate) use client::{
    coordinate_import_source_backed_refresh_with_progress, coordinate_source_backed_refresh,
    coordinate_source_backed_refresh_with_progress, SourceBackedRefreshDaemonUnavailable,
    SourceBackedRefreshObservation, SourceBackedRefreshTerminalError,
};
#[cfg(test)]
pub(crate) use ctx_history_refresh::count_verified_index_opens;
pub(crate) use refresh_mode::SourceBackedRefreshMode;
use request::{SourceBackedRefreshOperation, SourceBackedRefreshRequest};

const SOURCE_REFRESH_REQUEST_OP: &str = "source_refresh_request";
const SOURCE_REFRESH_STATUS_OP: &str = "source_refresh_status";
const SOURCE_REFRESH_UNKNOWN_REQUEST_STATE: &str = "request_unknown";
const SOURCE_REFRESH_UNKNOWN_REQUEST_ERROR_CODE: &str = "source_refresh_request_unknown";
const SOURCE_REFRESH_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
const SOURCE_REFRESH_IPC_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const SOURCE_REFRESH_RESPONSE_MAX_BYTES: u64 = 64 * 1024;

pub(crate) struct PinnedSourceBackedGeneration(ctx_history_refresh::PinnedSourceBackedGeneration);

impl PinnedSourceBackedGeneration {
    pub(crate) fn generation_id(&self) -> &str {
        self.0.generation_id()
    }

    pub(crate) fn semantic_eligible_event_count(&self) -> Result<u64> {
        self.0.semantic_eligible_event_count()
    }

    pub(crate) fn verified_index(&self) -> &ctx_history_index::VerifiedIndex {
        self.0.verified_index()
    }

    pub(crate) fn into_index(self) -> ctx_history_index::VerifiedIndex {
        self.0.into_index()
    }

    #[cfg(test)]
    pub(crate) fn from_index(index: ctx_history_index::VerifiedIndex) -> Self {
        Self(ctx_history_refresh::PinnedSourceBackedGeneration::from_index(index))
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

pub(crate) fn pin_active_verified_generation(
    data_root: &Path,
) -> Result<PinnedSourceBackedGeneration> {
    ctx_history_refresh::pin_active_verified_generation(data_root, &DaemonRefreshJournal)
        .map(PinnedSourceBackedGeneration)
}

fn pin_retained_generation(
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
) -> Result<ctx_history_capture::SourceBackedWatchCatalog> {
    let discovery_context = DaemonRefreshRuntime.discovery_context(data_root)?;
    ctx_history_refresh::source_backed_watch_catalog(data_root, &discovery_context)
}

pub(crate) fn published_explicit_source_relocation_authority(
    data_root: &std::path::Path,
    old_path: &std::path::Path,
) -> anyhow::Result<Option<crate::commands::import::ExplicitSourceRelocationAuthority>> {
    ctx_history_refresh::published_explicit_source_relocation_authority(
        data_root,
        old_path,
        &DaemonRefreshJournal,
    )
}

#[cfg(test)]
#[path = "source_backed_refresh_coordinator/restart_recovery_tests.rs"]
mod restart_recovery_tests;
