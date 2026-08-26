mod engine;
mod journal;
mod orchestration;
mod publication;
mod request;
mod route_ledger;

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, Instant as StdInstant},
};

#[cfg(test)]
use std::{collections::HashMap, fs};

use anyhow::{anyhow, bail, Context, Result};
#[cfg(test)]
use ctx_history_capture::{
    build_automatic_source_backed_registry_from_report,
    SourceBackedDetailedRefreshProgress as CaptureSourceBackedDetailedRefreshProgress,
    SourceBackedFailedRoute, SourceBackedRouteResult, SourceBackedSelectorAuthority,
    SourceBackedSourceFailures,
};
use ctx_history_capture::{
    discover_provider_sources_with_context_and_work_budget, source_backed_refresh_work_budget,
    source_backed_refresh_writer_options, validate_provider_source_roots_outside_data_root,
    DiscoveryContext, RouteObservation, SourceBackedCoordinatorError,
    SourceBackedRouteControlExpectation, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedSourceFailureClass, SourceBackedWatchCatalog,
};
#[cfg(test)]
use ctx_history_capture_model::{DiscoveryReport, ProviderSourceStatus};
use ctx_history_core::{utc_now, CaptureProvider};
#[cfg(test)]
use ctx_history_index::WriterOptions;
use ctx_history_index::{
    generation_incompatibility_requires_rebuild,
    generation_incompatibility_requires_recovery_rebuild, IndexError, SourceRouteIdentity,
    VerifiedIndex,
};
use ctx_history_refresh_execution::{
    is_sha256_identity, refresh_scope_from_json, refresh_scope_json, required_generation,
    source_backed_requested_route_observations, source_backed_route_retry_disposition,
    verify_generation_query_readiness, GenerationQueryReadiness, PublishedSourceBackedState,
    PublishedSourceBackedStatePort, SourceBackedAdmissionRouteFailures,
    SourceBackedExactScanProgress,
    SourceBackedRefreshProgressUpdate as PhysicalRefreshProgressUpdate,
};
use serde_json::{json, Value};
use uuid::Uuid;

use request::SourceBackedRefreshOperation;

pub use ctx_history_capture::SourceBackedReconciliationDemand;
pub use ctx_history_capture::SourceBackedRefreshScope;
pub use ctx_history_capture::SourceBackedRefreshScope as RefreshScope;
#[cfg(any(test, feature = "test-support"))]
pub use ctx_history_refresh_execution::explicit_source_catalog_authority_for_test;
pub use ctx_history_refresh_execution::source_backed_watch_catalog;
pub use ctx_history_refresh_execution::{
    explicit_source_for_path, explicit_source_path_is_symlink_or_reparse_point,
    explicit_source_path_metadata, explicit_source_path_symlink_metadata, nonzero_duration_micros,
    optional_generation, published_refresh_receipt_for_index,
    published_refresh_receipt_for_recovery, relocate_explicit_source, source_backed_index_root,
    upsert_explicit_source, validate_explicit_relocation_source, ExplicitSourceCatalogAuthority,
    ExplicitSourceCatalogRouteBinding, ExplicitSourceCatalogUpsert, ExplicitSourcePathMissing,
    ExplicitSourceRelocationAuthority, SourceBackedCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage, SourceBackedPublicationMetadata,
    SourceBackedReconciliationDemand as RefreshReconciliationDemand,
    SourceBackedRefreshCatalogRouteOutcome, SourceBackedRefreshCurrent,
    SourceBackedRefreshExecution, SourceBackedRefreshPublication, SourceBackedRefreshReceipt,
    SourceBackedRefreshRecordRejection, SourceBackedRefreshRouteOutcome,
    SourceBackedRefreshRouteResult, SourceBackedRefreshSourceFailure, SourceBackedRefreshTimings,
    SourceBackedRefreshWorkset, SourceBackedZeroSourceAuthority,
    SourceBackedZeroSourceAuthorityKind, ZeroSourcePublicationBlocked,
    SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
};
pub use engine::{
    CoreRefreshEngine as RefreshEngine, PinnedCorePublication, RefreshRuntime,
    RefreshRuntimeMetadata, SourceBackedRefreshCoverageCertificate, SourceBackedRefreshExecutor,
    SourceBackedRefreshProgress, SourceBackedRefreshRun as RefreshRun, SourceBackedRefreshStage,
    VerifiedSourceRefreshRouteBoundary,
};
pub use journal::{DurableAdmissionPersistence, RefreshJournal};
#[cfg(any(test, feature = "test-support"))]
pub use publication::count_verified_index_opens;
pub use publication::{
    explicit_catalog_request_is_accounted_for, open_verified_index, pin_active_verified_generation,
    pin_published_generation, pin_retained_generation,
    published_explicit_source_relocation_authority, published_refresh_receipt,
    verified_generation_is_query_ready, verify_generation_query_authority,
    GenerationQueryAuthorityError, MissingActiveGeneration, PinnedSourceBackedGeneration,
};
pub use request::{
    AdmissionResponseBarrier, RefreshAdmission, RefreshIntent, RefreshLogicalPhase,
    RefreshLogicalStatus, RefreshMaintenanceWakeStatus, RefreshOperation, RefreshOutcomeClass,
    RefreshOutcomeCode, RefreshRequest, RefreshRequestState, RefreshRequestTrigger,
    RefreshRetryAdvice, RefreshSelection, RefreshStatus, RefreshStatusKind,
    RefreshTerminalFailureScope, RefreshTerminalFailureType, RefreshTerminalOutcome,
};
pub use route_ledger::EventWatermark;

#[cfg(test)]
use engine::TestRefreshJournal;
use engine::{CoreRefreshEngine, SourceBackedRefreshProgressUpdate};
#[cfg(any(test, feature = "test-support"))]
use orchestration::admitted_refresh_for_test;
use orchestration::{execute_source_backed_refresh, source_backed_route_admission_fence};
use publication::{
    open_published_generation, open_published_generation_for_recovery,
    prepare_generation_control_state, published_generation_id, retained_generation_hint,
    verify_source_backed_publication, PublishedGenerationOpen,
};

const SOURCE_REFRESH_ATTEMPT_HISTORY: usize = 64;
const SOURCE_REFRESH_ACTIVE_PENDING_LIMIT: usize = 8;
const SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT: usize = 256;
const SOURCE_REFRESH_STARTUP_OBSERVATION_BUDGET: StdDuration = StdDuration::from_millis(250);
const TERMINAL_COVERAGE_ERROR_CODE: &str = "all_provider_terminal_coverage_unavailable";

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

#[cfg(test)]
fn committed_generation_recovery_error(
    recovery: ctx_history_index::CommittedPredecessorMigrationRecovery,
) -> ctx_history_index::IndexError {
    ctx_history_index::IndexError::CommittedGenerationNeedsRecovery {
        generation_id: recovery.generation_id().to_owned(),
        stage: "predecessor migration recovery",
        detail: recovery.detail().to_owned(),
    }
}
