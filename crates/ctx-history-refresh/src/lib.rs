mod current_state;
mod engine;
mod explicit_source_catalog;
mod journal;
mod orchestration;
mod publication;
mod request;
mod route_ledger;

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
use ctx_history_capture::{
    automatic_source_backed_route_identity, build_automatic_source_backed_registry_from_report,
    discover_provider_sources_with_context_and_work_budget, source_backed_refresh_work_budget,
    source_backed_route_inventory, validate_provider_source_roots_outside_data_root,
    DiscoveryContext, DiscoveryReport, ProviderSourceStatus, RouteObservation,
    SourceBackedAutomaticRegistryIssue, SourceBackedAutomaticUnavailableReason,
    SourceBackedCoordinatorError,
    SourceBackedCurrentSourceProgress as CaptureSourceBackedCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage as CaptureSourceBackedCurrentSourceProgressStage,
    SourceBackedDetailedRefreshProgress as CaptureSourceBackedDetailedRefreshProgress,
    SourceBackedFailedRoute, SourceBackedFailedRouteOutcome, SourceBackedLogicalSourceFailures,
    SourceBackedProviderRegistry, SourceBackedRecordRejections, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResult, SourceBackedSourceFailureClass,
    SourceBackedSourceFailures, SourceBackedSuccessfulRouteOutcome, SourceBackedWatchCatalog,
};
#[cfg(test)]
use ctx_history_capture::{
    SourceBackedRefreshProgress as CaptureSourceBackedRefreshProgress,
    SourceBackedSelectorAuthority,
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

use request::SourceBackedRefreshOperation;

pub use ctx_history_capture::SourceBackedRefreshScope;
pub use ctx_history_capture::SourceBackedRefreshScope as RefreshScope;
pub use current_state::SourceBackedRefreshCurrent;
pub use engine::{
    CoreRefreshEngine as RefreshEngine, PinnedCorePublication, RefreshRuntime,
    RefreshRuntimeMetadata, SourceBackedCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage, SourceBackedRefreshCatalogRouteOutcome,
    SourceBackedRefreshCoverageCertificate, SourceBackedRefreshExecution,
    SourceBackedRefreshExecutor, SourceBackedRefreshProgress, SourceBackedRefreshReceipt,
    SourceBackedRefreshRecordRejection, SourceBackedRefreshRouteOutcome,
    SourceBackedRefreshRouteResult, SourceBackedRefreshRun as RefreshRun,
    SourceBackedRefreshSourceFailure, SourceBackedRefreshTimings,
    VerifiedSourceRefreshRouteBoundary,
};
#[cfg(any(test, feature = "test-support"))]
pub use explicit_source_catalog::explicit_source_catalog_authority_for_test;
pub use explicit_source_catalog::{
    explicit_source_for_path, relocate_explicit_source, upsert_explicit_source,
    validate_explicit_relocation_source, ExplicitSourceCatalogAuthority,
    ExplicitSourceCatalogRouteBinding, ExplicitSourceCatalogUpsert,
    ExplicitSourceRelocationAuthority,
};
pub use journal::{DurableAdmissionPersistence, RefreshJournal};
pub use orchestration::source_backed_watch_catalog;
#[cfg(any(test, feature = "test-support"))]
pub use publication::count_verified_index_opens;
pub use publication::metadata::SourceBackedPublicationMetadata;
pub use publication::{
    explicit_catalog_request_is_accounted_for, nonzero_duration_micros, open_verified_index,
    optional_generation, pin_active_verified_generation, pin_published_generation,
    pin_retained_generation, published_explicit_source_relocation_authority,
    published_refresh_receipt, published_refresh_receipt_for_index, source_backed_index_root,
    PinnedSourceBackedGeneration, SourceBackedRefreshPublication,
};
pub use request::{
    AdmissionResponseBarrier, RefreshAdmission, RefreshOperation, RefreshStatus, RefreshSubmission,
};
pub use route_ledger::EventWatermark;

#[cfg(test)]
use engine::TestRefreshJournal;
use engine::{CoreRefreshEngine, SourceBackedRefreshProgressUpdate};
use orchestration::{
    execute_capture_owned_refresh, execute_source_backed_refresh,
    source_backed_requested_route_observation_fence, source_backed_route_admission_fence,
    SourceBackedRefreshPlan,
};
use publication::{
    is_sha256_identity, open_published_generation, published_generation_id, required_generation,
    required_route_results, retained_generation_hint, verify_source_backed_publication,
    SourceBackedRefreshCoveredPublication,
};

const SEARCH_DIRECTORY: &str = "search";
const LEXICAL_DIRECTORY: &str = "lexical";
const SOURCE_REFRESH_ATTEMPT_HISTORY: usize = 64;
const SOURCE_REFRESH_ACTIVE_PENDING_LIMIT: usize = 8;
const SOURCE_REFRESH_BUILD_ISSUE_LIMIT: usize = 8;
const SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT: usize = 256;
const SOURCE_REFRESH_RECOVERY_ROUTE_LIMIT: usize = SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT;
const SOURCE_REFRESH_RECEIPT_JSON_BUDGET_BYTES: usize = 24 * 1024;
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
