//! Provider-neutral capture contracts, state, and value objects.
//!
//! This crate owns no source access, discovery execution, provider implementation,
//! source interpretation, refresh publication, or runtime policy.

/// Upper bound for provider-generated diagnostic and metadata previews.
pub const PROVIDER_MAX_PREVIEW_CHARS: usize = 4_000;

pub mod ctx_retrieval;
mod exact_json;
pub mod file_references;
mod identity;
mod import;
pub mod normalization;
mod output;
mod progress;
mod provider_root;
mod record;
mod route;
mod source;
pub mod time;
pub mod tool_input;

pub use exact_json::{
    exact_bounded_string_alias, exact_json_value, raw_object_keys_are_unique, ExactJsonStringAlias,
};
pub use identity::{fnv1a64, stable_capture_uuid};
pub use import::{
    default_machine_id, push_provider_import_failure, CatalogSummary, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportSummary, ProviderImportWorkResult,
};
pub use output::{OutputObservationKind, OutputOutcome, OutputOutcomeMetadata};
pub use progress::{
    source_level_progress, AttemptHistoryProgress, AttemptHistoryProgressSnapshot,
    CoreRecordBatchProgress, CoreRecordProgress, SharedAttemptHistoryProgress,
    SourceBackedCurrentSourceProgress, SourceBackedCurrentSourceProgressStage,
    SourceBackedDetailedRefreshProgress, SourceBackedExactScanProgress,
    SourceBackedRecordProgressDelta, SourceBackedRefreshProgress, SourceRecordProgress,
    SourceRecordProgressSnapshot,
};
pub use provider_root::{
    provider_root_encoded_path_len, provider_root_path_within_limit, provider_source_config_digest,
    ProviderRootConnectorBinding, ProviderRootDefinition, ProviderRootKind, ProviderRootSet,
    ProviderRootSetError, ProviderRootSourceIdentity, ReleasedProviderRootAutomaticRole,
    RetainedProviderRootAuthority, MAX_CONFIGURED_PROVIDER_ROOTS,
    MAX_PROVIDER_ROOT_ENCODED_PATH_BYTES, MAX_PROVIDER_ROOT_SELECTOR_BYTES,
};
pub use record::RecordDigest;
pub use route::{
    ProviderRouteRole, ProviderRouteRoleError, SourceRouteIdentity, SourceRouteIdentityError,
    MAX_PROVIDER_ROUTE_ROLE_BYTES,
};
pub use source::{
    DiscoveryIssue, DiscoveryIssueKind, DiscoveryReport, ProviderCatalogSupport,
    ProviderDefaultLocation, ProviderImportSupport, ProviderSource, ProviderSourceFailureKind,
    ProviderSourceKind, ProviderSourceRouteProvenance, ProviderSourceSpec, ProviderSourceStatus,
    ProviderSourceStatusReason,
};
