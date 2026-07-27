pub mod archive;
mod artifacts;
mod bulk_search;
mod canonical_observations;
mod catalog;
mod connection;
mod error;
mod events;
mod files;
mod identity;
mod object_store;
mod projection_journal;
mod raw_sql;
mod records;
mod result_storage;
mod runs;
mod schema;
mod search;
mod sessions;
mod source_locators;
mod sources;
mod summaries;
mod sync;
mod vcs;

pub use archive::validate_archive_version;
pub use bulk_search::{EventSearchBulkGuard, SourceInventoryGuard};
pub use canonical_observations::{
    CanonicalActor, CanonicalByteRange, CanonicalCitation, CanonicalFileTouch,
    CanonicalObservation, CanonicalObservationKind, CanonicalResultEvidence,
    CanonicalResultEvidenceKind, CanonicalResultIdentifier, CanonicalResultOutcome, CanonicalRun,
    CanonicalSource, CanonicalSourceObservation, CanonicalTypedEventKind, CanonicalVcsChange,
};
pub use catalog::{
    CatalogCounts, CatalogIndexedStatus, CatalogSession, CatalogSourceIndexState,
    IndexedHistoryCounts, SourceImportFile, SourceImportFileCounts, SourceImportInventoryControl,
};
pub use error::{Result, StoreError};
pub use events::ProviderEventHashAuthority;
pub use files::FileTouchScope;
pub use identity::{LocalDeviceIdentity, LocalWorkspaceIdentity};
pub use projection_journal::{
    JournalCheckpoint, JournalEntityKind, JournalEvidenceIdentity, JournalOperation,
    JournalPosition, JournalProvenanceIdentity, ProjectionJournalRecord, ProjectionJournalSnapshot,
    PROJECTION_CONTRACT_VERSION, PROJECTION_JOURNAL_MAX_PAGE_BYTES, PROJECTION_JOURNAL_PAGE_SIZE,
    PROJECTION_JOURNAL_RECORD_MAX_BYTES,
};
pub use raw_sql::{
    RawSqlColumn, RawSqlLimits, RawSqlOptions, RawSqlResult, RawSqlTruncation, RawSqlValue,
    RAW_SQL_DEFAULT_MAX_COLUMNS, RAW_SQL_DEFAULT_MAX_ROWS, RAW_SQL_DEFAULT_MAX_SQL_BYTES,
    RAW_SQL_DEFAULT_MAX_VALUE_BYTES, RAW_SQL_DEFAULT_TIMEOUT, RAW_SQL_MAX_COLUMNS_CAP,
    RAW_SQL_MAX_RESULT_CELLS, RAW_SQL_MAX_RESULT_PREVIEW_BYTES, RAW_SQL_MAX_ROWS_CAP,
    RAW_SQL_MAX_SQL_BYTES_CAP, RAW_SQL_MAX_TIMEOUT, RAW_SQL_MAX_VALUE_BYTES_CAP,
};
pub use search::projections::{EventEmbeddingDocument, EventSearchHit};
pub use source_locators::{ProviderSourceLocatorObservation, ProviderSourceLocatorResolution};

use std::{
    cell::Cell,
    path::PathBuf,
    sync::{atomic::AtomicUsize, Arc},
    time::Duration,
};

use rusqlite::Connection;

pub const SCHEMA_VERSION: i64 = 47;
pub const FINAL_SCHEMA_IDENTITY: &str = "ctx-store-schema-47-final-v3";

pub struct Store {
    path: PathBuf,
    object_dir: PathBuf,
    conn: Connection,
    busy_timeout: Duration,
    event_search_bulk_depth: Arc<AtomicUsize>,
    batch_depth: Cell<usize>,
    import_batch_depth: Cell<usize>,
    event_search_projection_capabilities:
        Cell<Option<search::projections::EventSearchProjectionCapabilities>>,
    projection_journal_active_in_batch: Cell<Option<bool>>,
}

#[cfg(test)]
mod connection_tests;
#[cfg(test)]
mod records_tests;
