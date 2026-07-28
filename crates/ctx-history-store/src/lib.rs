pub mod archive;
mod artifacts;
mod bulk_search;
mod canonical_observations;
mod catalog;
mod cold_store;
mod connection;
mod error;
mod events;
mod files;
mod identity;
mod native_path_group;
mod object_store;
mod projection_journal;
mod raw_sql;
mod records;
mod result_storage;
mod runs;
mod schema;
mod search;
mod semantic_projection_epoch;
mod sessions;
pub mod source_backed_relational;
mod source_generations;
mod source_locators;
mod sources;
mod summaries;
mod sync;
mod vcs;

pub use archive::validate_archive_version;
pub use bulk_search::{EventSearchBulkGroupAdmission, EventSearchBulkGuard, SourceInventoryGuard};
pub use canonical_observations::{
    CanonicalActor, CanonicalByteRange, CanonicalCitation, CanonicalFileTouch,
    CanonicalObservation, CanonicalObservationKind, CanonicalResultEvidence,
    CanonicalResultEvidenceKind, CanonicalResultIdentifier, CanonicalResultOutcome, CanonicalRun,
    CanonicalSource, CanonicalSourceObservation, CanonicalTypedEventKind, CanonicalVcsChange,
};
pub use catalog::{
    CatalogCounts, CatalogIndexedStatus, CatalogSession, CatalogSourceIndexState,
    IndexedHistoryCounts, InventorySourceByteProgress, SourceImportFile, SourceImportFileCounts,
    SourceImportInventoryControl,
};
#[doc(hidden)]
pub use cold_store::{
    ColdStoreBuild, ColdStoreBuildCounts, ColdStoreBuildReceipt, ColdStoreBuildTimings,
};
pub use error::{Result, StoreError};
pub use events::ProviderEventHashAuthority;
pub use files::FileTouchScope;
pub use identity::{LocalDeviceIdentity, LocalWorkspaceIdentity};
#[doc(hidden)]
pub use native_path_group::{
    decode_native_path_committed_cursor, NativePathCommittedCursor, NativePathCursorKey,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    NativePathGroupReceipt, NativePathPublicationGroup, NATIVE_PATH_MAX_CORE_BOUND_BYTES,
    NATIVE_PATH_MAX_GROUP_PAGES, NATIVE_PATH_MAX_GROUP_SOURCES, NATIVE_PATH_MAX_JOURNAL_BYTES,
    NATIVE_PATH_MAX_JOURNAL_RECORDS, NATIVE_PATH_MAX_MUTATION_UNITS,
    NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
};
pub use projection_journal::{
    JournalCheckpoint, JournalEntityKind, JournalEvidenceIdentity, JournalOperation,
    JournalPosition, JournalProvenanceIdentity, ProjectionJournalContextWindow,
    ProjectionJournalRecord, ProjectionJournalSnapshot, PROJECTION_CONTRACT_VERSION,
    PROJECTION_JOURNAL_CONTEXT_MAX_BYTES, PROJECTION_JOURNAL_CONTEXT_RECORDS,
    PROJECTION_JOURNAL_MAX_PAGE_BYTES, PROJECTION_JOURNAL_PAGE_SIZE,
    PROJECTION_JOURNAL_RECORD_MAX_BYTES,
};
pub use raw_sql::{
    RawSqlColumn, RawSqlLimits, RawSqlOptions, RawSqlResult, RawSqlTruncation, RawSqlValue,
    RAW_SQL_DEFAULT_MAX_COLUMNS, RAW_SQL_DEFAULT_MAX_ROWS, RAW_SQL_DEFAULT_MAX_SQL_BYTES,
    RAW_SQL_DEFAULT_MAX_VALUE_BYTES, RAW_SQL_DEFAULT_TIMEOUT, RAW_SQL_MAX_COLUMNS_CAP,
    RAW_SQL_MAX_RESULT_CELLS, RAW_SQL_MAX_RESULT_PREVIEW_BYTES, RAW_SQL_MAX_ROWS_CAP,
    RAW_SQL_MAX_SQL_BYTES_CAP, RAW_SQL_MAX_TIMEOUT, RAW_SQL_MAX_VALUE_BYTES_CAP,
};
pub use search::projections::{EventEmbeddingDocument, EventSearchHit, SemanticProjectionSnapshot};
pub use semantic_projection_epoch::CanonicalSemanticProjectionVersion;
pub use source_backed_relational::{
    CommittedCoreGeneration, RelationalEventMetadata, RelationalFileTouchMetadata,
    RelationalProjectionError, RelationalProjectionMetadata, RelationalProjectionReceipt,
    RelationalProjectionRecord, RelationalProjectionStatus, RelationalSessionMetadata,
    RelationalSourceMetadata, SourceBackedRelationalProjection, RELATIONAL_EVENT_PREVIEW_MAX_CHARS,
    RELATIONAL_PROJECTION_CONTRACT_VERSION, RELATIONAL_PROJECTION_SCHEMA_VERSION,
};
pub use source_generations::{
    NativePathRetainedSourceEntities, NativePathSourceEntityFrontier, NativePathSourceEntityKind,
    NativePathSourceGenerationKey, NativePathSourceRetirementPage,
};
pub use source_locators::{
    AuthorizedSourceRoute, ProviderSourceLocatorObservation, ProviderSourceLocatorResolution,
    ProviderSourceRouteBinding, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason,
};

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize},
        Arc,
    },
    time::Duration,
};

use rusqlite::Connection;

pub const SCHEMA_VERSION: i64 = 47;
pub const FINAL_SCHEMA_IDENTITY: &str = "ctx-store-schema-47-final-v7";
/// Final-v7 adds local NativePath generation-retirement state, but does not
/// change projected bytes, canonical rows, or journal framing. Mutation
/// observation, verified locator cleanup, and local provider-route binding
/// likewise do not change canonical helper input, so the frozen projection
/// contract remains v3.
pub const CANONICAL_PROJECTION_SCHEMA_IDENTITY: &str = "ctx-store-schema-47-final-v3";

pub struct Store {
    path: PathBuf,
    object_dir: PathBuf,
    conn: Connection,
    busy_timeout: Duration,
    event_search_bulk_depth: Arc<AtomicUsize>,
    // Even values are inactive; each acquired root owns the following odd
    // epoch. Nested guards and one-use admissions are valid only in that epoch.
    event_search_bulk_epoch: Arc<AtomicU64>,
    batch_depth: Cell<usize>,
    connection_quarantined: Cell<bool>,
    event_search_projection_capabilities:
        Cell<Option<search::projections::EventSearchProjectionCapabilities>>,
    projection_journal_active_in_batch: Cell<Option<bool>>,
    projection_journal_group_collector: RefCell<Option<projection_journal::GroupJournalCollector>>,
    native_path_group_token: Cell<Option<uuid::Uuid>>,
    native_cold_load_active: Cell<bool>,
    native_path_mutation_scope: Arc<AtomicBool>,
    native_path_group_poisoned: Arc<AtomicBool>,
    native_path_transaction_control_scope: Cell<bool>,
    event_search_bulk_group_admission_outstanding: Arc<AtomicBool>,
}

#[cfg(test)]
mod connection_tests;
#[cfg(test)]
mod records_tests;
