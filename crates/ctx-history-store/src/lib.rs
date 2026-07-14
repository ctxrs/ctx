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
mod raw_sql;
mod records;
mod runs;
mod schema;
mod search;
mod sessions;
mod sources;
mod summaries;
mod sync;
mod vcs;
mod work_control;

pub use archive::{validate_archive_version, ArchiveImportOutcome};
pub use bulk_search::EventSearchBulkGuard;
pub use catalog::{
    CatalogCounts, CatalogIndexedStatus, CatalogSession, CatalogSourceIndexState,
    CatalogSourceIndexUpdate, IndexedHistoryCounts, SourceImportFile, SourceImportFileCounts,
    SourceImportFileIndexUpdate,
};
pub use error::{Result, StoreError};
pub use files::FileTouchScope;
pub use identity::{LocalDeviceIdentity, LocalWorkspaceIdentity};
pub use raw_sql::{
    RawSqlColumn, RawSqlLimits, RawSqlOptions, RawSqlResult, RawSqlTruncation, RawSqlValue,
    RAW_SQL_DEFAULT_MAX_COLUMNS, RAW_SQL_DEFAULT_MAX_ROWS, RAW_SQL_DEFAULT_MAX_SQL_BYTES,
    RAW_SQL_DEFAULT_MAX_VALUE_BYTES, RAW_SQL_DEFAULT_TIMEOUT, RAW_SQL_MAX_COLUMNS_CAP,
    RAW_SQL_MAX_RESULT_CELLS, RAW_SQL_MAX_RESULT_PREVIEW_BYTES, RAW_SQL_MAX_ROWS_CAP,
    RAW_SQL_MAX_SQL_BYTES_CAP, RAW_SQL_MAX_TIMEOUT, RAW_SQL_MAX_VALUE_BYTES_CAP,
};
pub use search::projections::{EventEmbeddingDocument, EventSearchHit};
pub use work_control::{
    ensure_indexing_disk_headroom, sqlite_amplifying_write_estimate, system_memory,
    ExternalIndexingCopyLease, ExternalIndexingWriterLease, IndexingAdmission,
    IndexingAdmissionStatus, IndexingIoPacer, IndexingPressure, IndexingResourceSnapshot,
    IndexingSlice, IndexingWorkClass, WalCheckpointStatus, INDEXING_TRANSACTION_MAX,
    INDEXING_WAL_DELTA_BYTES, WAL_PASSIVE_MIN_BYTES, WAL_RESTART_MIN_BYTES, WAL_TRUNCATE_MIN_BYTES,
};

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    sync::{atomic::AtomicUsize, Arc},
    time::Duration,
};

use rusqlite::Connection;

pub(crate) const SCHEMA_VERSION: i64 = 46;

pub struct Store {
    path: PathBuf,
    object_dir: PathBuf,
    conn: Connection,
    busy_timeout: Duration,
    event_search_bulk_depth: Arc<AtomicUsize>,
    event_search_transaction_lock: RefCell<Option<Connection>>,
    indexing_admission: Option<IndexingAdmission>,
    indexing_writer_lease: RefCell<Option<work_control::IndexingWriterLease>>,
    connection_quarantined: Cell<bool>,
}

#[cfg(test)]
mod connection_tests;
