#![allow(unused_imports)]
use std::{
    collections::{BTreeSet, HashMap},
    ffi::CString,
    fs,
    os::raw::c_char,
    path::{Path, PathBuf},
    ptr,
    str::FromStr,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use chrono::{DateTime, Utc};

use ctx_history_core::{
    new_id, utc_now, AgentType, Artifact, ArtifactKind, CaptureProvider, CaptureSource,
    CaptureSourceDescriptor, EntityTimestamps, Event, EventRole, EventType, Fidelity, FileTouched,
    HistoryRecord, HistoryRecordLink, RedactionState, Run, RunStatus, RunType, Session,
    SessionEdge, SessionHistoryArchive, SessionStatus, Summary, SyncCursor, SyncMetadata,
    SyncState, VcsChange, VcsWorkspace, Visibility,
};

use rusqlite::{
    ffi, limits::Limit, params, types::ValueRef, Connection, ErrorCode, OpenFlags,
    OptionalExtension, Transaction,
};

use serde_json::Value;

use sha2::{Digest, Sha256};

use thiserror::Error;

use uuid::Uuid;

pub struct Store {
    path: PathBuf,
    object_dir: PathBuf,
    conn: Connection,
    busy_timeout: Duration,
}

#[path = "store/store_methods_01.rs"]
mod store_store_methods_01;
pub(crate) use store_store_methods_01::*;

#[path = "store/store_methods_02.rs"]
mod store_store_methods_02;
pub(crate) use store_store_methods_02::*;

#[path = "store/store_methods_03.rs"]
mod store_store_methods_03;
pub(crate) use store_store_methods_03::*;

#[path = "store/store_methods_04.rs"]
mod store_store_methods_04;
pub(crate) use store_store_methods_04::*;

#[path = "store/store_methods_05.rs"]
mod store_store_methods_05;
pub(crate) use store_store_methods_05::*;

#[path = "store/store_methods_06.rs"]
mod store_store_methods_06;
pub(crate) use store_store_methods_06::*;

#[path = "store/sqlite.rs"]
mod store_sqlite;
pub use store_sqlite::StoreError;
pub(crate) use store_sqlite::*;

#[path = "store/store.rs"]
mod store_store;
pub use store_store::Result;
pub(crate) use store_store::*;

#[path = "store/schema.rs"]
mod store_schema;
pub(crate) use store_schema::*;

#[path = "store/busy.rs"]
mod store_busy;
pub(crate) use store_busy::*;

#[path = "store/objects.rs"]
mod store_objects;
pub(crate) use store_objects::*;

#[path = "store/spool.rs"]
mod store_spool;
pub(crate) use store_spool::*;

#[path = "store/record.rs"]
mod store_record;
pub(crate) use store_record::*;

#[path = "store/legacy.rs"]
mod store_legacy;
pub(crate) use store_legacy::*;

#[path = "store/raw_sql.rs"]
mod store_raw_sql;
pub(crate) use store_raw_sql::*;
pub use store_raw_sql::{
    RawSqlColumn, RawSqlLimits, RawSqlOptions, RawSqlResult, RawSqlTruncation, RawSqlValue,
    RAW_SQL_DEFAULT_MAX_COLUMNS, RAW_SQL_DEFAULT_MAX_ROWS, RAW_SQL_DEFAULT_MAX_SQL_BYTES,
    RAW_SQL_DEFAULT_MAX_VALUE_BYTES, RAW_SQL_DEFAULT_TIMEOUT, RAW_SQL_MAX_COLUMNS_CAP,
    RAW_SQL_MAX_RESULT_CELLS, RAW_SQL_MAX_RESULT_PREVIEW_BYTES, RAW_SQL_MAX_ROWS_CAP,
    RAW_SQL_MAX_SQL_BYTES_CAP, RAW_SQL_MAX_TIMEOUT, RAW_SQL_MAX_VALUE_BYTES_CAP,
};

#[path = "store/identity.rs"]
mod store_identity;
pub(crate) use store_identity::*;
pub use store_identity::{LocalDeviceIdentity, LocalWorkspaceIdentity};

#[path = "store/catalog.rs"]
mod store_catalog;
pub(crate) use store_catalog::*;
pub use store_catalog::{
    CatalogCounts, CatalogIndexedStatus, CatalogSession, CatalogSourceIndexState,
    CatalogSourceIndexUpdate,
};

#[path = "store/source_import.rs"]
mod store_source_import;
pub(crate) use store_source_import::*;
pub use store_source_import::{SourceImportFile, SourceImportFileIndexUpdate};

#[path = "store/indexed.rs"]
mod store_indexed;
pub use store_indexed::IndexedHistoryCounts;
pub(crate) use store_indexed::*;

#[path = "store/search.rs"]
mod store_search;
pub use store_search::EventSearchHit;
pub(crate) use store_search::*;

#[path = "store/session.rs"]
mod store_session;
pub use store_session::FileTouchScope;
pub(crate) use store_session::*;

#[path = "store/claude.rs"]
mod store_claude;
pub(crate) use store_claude::*;

#[path = "store/sql_01.rs"]
mod store_sql_01;
pub(crate) use store_sql_01::*;

#[path = "store/sql_02.rs"]
mod store_sql_02;
pub(crate) use store_sql_02::*;

#[path = "store/hex.rs"]
mod store_hex;
pub(crate) use store_hex::*;

#[path = "store/duration.rs"]
mod store_duration;
pub(crate) use store_duration::*;

#[path = "store/path.rs"]
mod store_path;
pub(crate) use store_path::*;

#[path = "store/artifact.rs"]
mod store_artifact;
pub(crate) use store_artifact::*;

#[path = "store/local.rs"]
mod store_local;
pub(crate) use store_local::*;

#[path = "store/event.rs"]
mod store_event;
pub(crate) use store_event::*;

#[path = "store/non.rs"]
mod store_non;
pub(crate) use store_non::*;

#[path = "store/file.rs"]
mod store_file;
pub(crate) use store_file::*;

#[path = "store/escape.rs"]
mod store_escape;
pub(crate) use store_escape::*;

#[path = "store/migration.rs"]
mod store_migration;
pub(crate) use store_migration::*;

#[path = "store/rename.rs"]
mod store_rename;
pub(crate) use store_rename::*;

#[path = "store/rewrite.rs"]
mod store_rewrite;
pub(crate) use store_rewrite::*;

#[path = "store/drop.rs"]
mod store_drop;
pub(crate) use store_drop::*;

#[path = "store/column.rs"]
mod store_column;
pub(crate) use store_column::*;

#[path = "store/provider_source.rs"]
mod store_provider_source;
pub(crate) use store_provider_source::*;

#[path = "store/fts.rs"]
mod store_fts;
pub(crate) use store_fts::*;

#[path = "store/summary.rs"]
mod store_summary;
pub(crate) use store_summary::*;

#[path = "store/count.rs"]
mod store_count;
pub(crate) use store_count::*;

#[path = "store/time.rs"]
mod store_time;
pub(crate) use store_time::*;

#[path = "store/capped.rs"]
mod store_capped;
pub(crate) use store_capped::*;

#[path = "store/sha256.rs"]
mod store_sha256;
pub(crate) use store_sha256::*;

#[path = "store/archive.rs"]
mod store_archive;
pub use store_archive::validate_archive_version;
pub(crate) use store_archive::*;

#[path = "store/blob.rs"]
mod store_blob;
pub(crate) use store_blob::*;

#[path = "store/import.rs"]
mod store_import;
pub(crate) use store_import::*;

#[path = "store/capture.rs"]
mod store_capture;
pub(crate) use store_capture::*;

#[path = "store/vcs.rs"]
mod store_vcs;
pub(crate) use store_vcs::*;

#[path = "store/cursor.rs"]
mod store_cursor;
pub(crate) use store_cursor::*;

#[path = "store/sync.rs"]
mod store_sync;
pub(crate) use store_sync::*;

#[path = "store/optional.rs"]
mod store_optional;
pub(crate) use store_optional::*;

#[path = "store/parse_optional.rs"]
mod store_parse_optional;
pub(crate) use store_parse_optional::*;

#[path = "store/error.rs"]
mod store_error;
pub(crate) use store_error::*;

#[path = "store/source_identity.rs"]
mod store_source_identity;
pub(crate) use store_source_identity::*;

#[path = "store/collect.rs"]
mod store_collect;
pub(crate) use store_collect::*;

#[cfg(test)]
#[path = "store_tests/archive_validation_tests.rs"]
mod archive_validation_tests;

#[cfg(test)]
#[path = "store_tests/search_order_tests.rs"]
mod search_order_tests;

#[cfg(test)]
#[path = "store_tests/catalog_tests/mod.rs"]
mod catalog_tests;
