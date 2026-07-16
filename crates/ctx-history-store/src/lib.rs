pub mod archive;
mod artifacts;
mod bulk_search;
mod catalog;
mod connection;
mod error;
mod events;
mod files;
mod identity;
mod object_store;
mod provider_files;
mod raw_sql;
mod records;
mod runs;
mod schema;
mod search;
mod sessions;
mod sources;
mod store_identity;
mod summaries;
mod sync;
mod vcs;

pub use archive::validate_archive_version;
#[doc(hidden)]
pub use bulk_search::{install_event_search_maintenance_pacer, EventSearchMaintenancePacingGuard};
pub use bulk_search::{EventSearchBulkGuard, EventSearchBulkMaintenanceOutcome};
pub use catalog::{
    CatalogCounts, CatalogImportWork, CatalogIndexedStatus, CatalogSession,
    CatalogSourceIndexState, CatalogSourceIndexUpdate, ImportPendingReason,
    ImportPendingReasonRepairProgress, ImportWorkClass, IndexedHistoryCounts, SourceImportFile,
    SourceImportFileCounts, SourceImportFileIndexUpdate, SourceImportFileWork,
};
pub use error::{Result, StoreError};
pub use files::FileTouchScope;
pub use identity::{LocalDeviceIdentity, LocalWorkspaceIdentity};
pub use provider_files::{
    ProviderFileCheckpoint, ProviderFileCheckpointKey, ProviderFileFinalizeOutcome,
    ProviderFileImportOutcome, ProviderFileInventoryFamily, ProviderFileInventoryObservation,
    ProviderFileMaintenanceWarning, ProviderFilePreparationProgress, ProviderFilePublicationCommit,
    ProviderFilePublicationCompletion, ProviderFilePublicationInventoryOwner,
    ProviderFilePublicationKind, ProviderFilePublicationPhase,
    ProviderFilePublicationRetirementWork, ProviderFilePublicationScope,
    ProviderFileReconciliationCounts, ProviderFileReconciliationProgress,
    PROVIDER_FILE_CHECKPOINT_RESUME_STATE_MAX_BYTES, PROVIDER_FILE_PREPARATION_MAX_ROWS,
    PROVIDER_FILE_PUBLICATION_COMPLETION_MAX_BYTES, PROVIDER_FILE_RECONCILIATION_MAX_ROWS,
};
pub use raw_sql::{
    RawSqlColumn, RawSqlLimits, RawSqlOptions, RawSqlResult, RawSqlTruncation, RawSqlValue,
    RAW_SQL_DEFAULT_MAX_COLUMNS, RAW_SQL_DEFAULT_MAX_ROWS, RAW_SQL_DEFAULT_MAX_SQL_BYTES,
    RAW_SQL_DEFAULT_MAX_VALUE_BYTES, RAW_SQL_DEFAULT_TIMEOUT, RAW_SQL_MAX_COLUMNS_CAP,
    RAW_SQL_MAX_RESULT_CELLS, RAW_SQL_MAX_RESULT_PREVIEW_BYTES, RAW_SQL_MAX_ROWS_CAP,
    RAW_SQL_MAX_SQL_BYTES_CAP, RAW_SQL_MAX_TIMEOUT, RAW_SQL_MAX_VALUE_BYTES_CAP,
};
pub use search::projections::{EventEmbeddingDocument, EventSearchHit};

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    sync::{atomic::AtomicUsize, Arc},
    time::Duration,
};

use rusqlite::Connection;

pub(crate) const SCHEMA_VERSION: i64 = 55;

pub struct Store {
    path: PathBuf,
    object_dir: PathBuf,
    conn: Connection,
    busy_timeout: Duration,
    event_search_bulk_depth: Arc<AtomicUsize>,
    store_identity: store_identity::CanonicalStoreIdentity,
    provider_file_publication: RefCell<Option<provider_files::ActiveProviderFilePublication>>,
    provider_file_write_scope: Cell<Option<uuid::Uuid>>,
    #[cfg(test)]
    provider_file_fault: std::cell::Cell<Option<provider_files::ProviderFileFaultPoint>>,
    #[cfg(test)]
    provider_file_reconciliation_queries: Cell<usize>,
    #[cfg(test)]
    provider_file_reconciliation_candidates: Cell<usize>,
}

impl Drop for Store {
    fn drop(&mut self) {
        self.cleanup_provider_file_publication_on_drop();
    }
}

#[cfg(test)]
mod connection_tests;
