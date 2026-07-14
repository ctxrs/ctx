use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use ctx_history_core::{
    CaptureProvider, CaptureSource, CaptureSourceDescriptor, Event, FileTouched, HistoryRecordLink,
    Run, Session, SessionEdge, SessionHistoryArchive, Summary, VcsChange,
};
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::catalog::{CatalogSourceIndexUpdate, SourceImportFileIndexUpdate};
use crate::connection::{capped_i64, nonnegative_i64_to_u32, nonnegative_i64_to_u64};
use crate::events::provider_event_hash_conflict_rows;
use crate::events::{event_from_row, event_select_sql};
use crate::schema::ddl::table_exists;
use crate::search::projections::{
    decrement_semantic_searchable_item_stats_if_cached, invalidate_semantic_searchable_item_stats,
    semantic_searchable_document_count_from_stored_event,
};
use crate::sessions::{session_from_row, session_select_sql};
use crate::sources::capture_source_from_row;
use crate::{CatalogIndexedStatus, Result, Store, StoreError};

include!("provider_files/types.rs");
include!("provider_files/checkpoints.rs");
include!("provider_files/lifecycle_begin.rs");
include!("provider_files/lifecycle_progress.rs");
include!("provider_files/lifecycle_finalize.rs");
include!("provider_files/write_scope.rs");
include!("provider_files/observation_validation.rs");
include!("provider_files/publication_marker.rs");
include!("provider_files/staging_recovery.rs");
include!("provider_files/fencing_entity_writes.rs");
include!("provider_files/fencing_reference_sources.rs");
include!("provider_files/reconciliation_scan.rs");
include!("provider_files/reconciliation_delete.rs");
include!("provider_files/archive.rs");
include!("provider_files/reconciliation_phases.rs");
include!("provider_files/visibility.rs");
include!("provider_files/validation.rs");
include!("provider_files/locks.rs");

#[cfg(test)]
mod tests;
