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

pub use archive::validate_archive_version;
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

use std::{
    path::PathBuf,
    sync::{atomic::AtomicUsize, Arc},
    time::Duration,
};

use rusqlite::Connection;

pub(crate) const SCHEMA_VERSION: i64 = 47;

pub struct Store {
    path: PathBuf,
    object_dir: PathBuf,
    conn: Connection,
    busy_timeout: Duration,
    event_search_bulk_depth: Arc<AtomicUsize>,
}

#[cfg(test)]
mod connection_tests;
