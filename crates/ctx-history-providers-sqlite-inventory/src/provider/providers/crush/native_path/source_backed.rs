//! Direct Core projection for Crush's finite project-database inventory.
//!
//! The selector owner supplies a re-observable inventory. This adapter owns
//! Crush discovery binding, native SQLite parsing, stable provider identities,
//! and bounded complete-record projection.

use std::{
    collections::HashSet,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

use ctx_history_core::{
    derive_event_id, derive_session_id, ActivityInvocation, ActivityJsonCapture, ActivityResult,
    ActivityTextCapture, AgentScope, CaptureProvider, CertifiedSource, CoreActivity, CoreRecord,
    CoreRecordError, EventIdentityInput, NativeItemKey, NativeSessionKey, ProjectionContractError,
    ProviderNativeSessionRelationship, ScannedSourceCounts, SessionIdentityInput, SourceAnchor,
    SourceAnchorScope, SourceInventoryObservation, SourceKey, StableEntityId, TypedKey,
    CORE_ACTIVITY_REVISION,
};
use rusqlite::{limits::Limit, Connection};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    query::{load_message_batch, next_candidate_batch, row_decode_error_is_local, CrushCandidate},
    read_native_schema, CrushLoadedRow, CrushNativeFrontier, CrushNativeSchema,
};
use crate::{
    common::io::{OpenedProviderSourceFile, ProviderSourceRoot},
    fnv1a64,
    provider::source_backed::{
        family::document::ChangedDocumentSink, sqlite_rejection_draft,
        SourceBackedRecordRejectionClass, SourceBackedRouteError,
    },
    provider_sources::{
        retain_sqlite_source_directory_authority, SqliteFailurePhase, SqliteLogicalSnapshot,
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot,
    },
    CaptureError, CRUSH_SQLITE_SOURCE_FORMAT, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::super::{
    capture::message_record_digest_bytes,
    projection::{project_message, CrushMessageProjection, CrushRecordProjection, CrushSessionRow},
};

#[path = "source_backed_identity.rs"]
mod identity;
pub use identity::crush_source_key;
use identity::{crush_session_id, crush_source_key_scoped, session_lineage};

#[cfg(test)]
mod tests;

#[cfg(test)]
use crate::provider_sources::open_root_handle_sqlite_source_snapshot;

const CRUSH_SOURCE_ANCHOR_NAMESPACE: &str = "crush.project-database";
const CRUSH_INVENTORY_AUTHORITY_NAMESPACE: &str = "crush.project-inventory";
const CRUSH_INVENTORY_REVISION_KIND: &str = "crush-selected-registered-projects-v0";
pub(crate) const CRUSH_SOURCE_SCHEMA_VARIANT: &str = "crush-project-sqlite-v0";
pub(crate) const CRUSH_PARSER_REVISION: &str = "crush-sqlite-source-backed-v5-record-rejections";
const CRUSH_NATIVE_SESSION_NAMESPACE: &str = "crush.session";
const CRUSH_NATIVE_MESSAGE_NAMESPACE: &str = "crush.message";
const CRUSH_LOGICAL_SESSION_KIND: &str = "crush-session";
const CRUSH_LOGICAL_EVENT_KIND: &str = "crush-message";
const CRUSH_MESSAGE_DIGEST_DOMAIN: &[u8] = b"ctx-crush-source-backed-message-set-v0\0";
const MAX_CRUSH_PROJECT_DATABASES: usize = 128;
const MAX_CRUSH_SESSION_LINEAGE_DEPTH: usize = 256;
const CRUSH_MESSAGE_QUERY_BATCH: usize = 256;
const CRUSH_MESSAGE_QUERY_TARGET_BYTES: u64 = 6 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum CrushSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Route(#[from] SourceBackedRouteError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    SqliteSource(#[from] CrushSqliteSourceErrorV0),
    #[error("{primary}; explicit Crush SQLite snapshot cleanup also failed: {cleanup}")]
    SnapshotCleanup {
        primary: Box<CrushSourceBackedErrorV0>,
        cleanup: CrushSqliteSourceErrorV0,
    },
    #[error(
        "Crush project inventory exceeds the finite {MAX_CRUSH_PROJECT_DATABASES}-database bound"
    )]
    InventoryTooLarge,
    #[error("Crush project inventory contains a relative database path: {0:?}")]
    RelativeDatabasePath(PathBuf),
    #[error("Crush project inventory contains the same source lineage more than once")]
    DuplicateProjectKey,
    #[error("Crush project inventory contains the same database path more than once")]
    DuplicateDatabasePath,
    #[error("Crush project inventory contains a null project key")]
    NullProjectKey,
    #[error("Crush source count overflow")]
    CountOverflow,
    #[error("Crush message scan produced an unexpected native row")]
    UnexpectedNativeRow,
    #[error("Crush session lineage contains a cycle at provider session {0}")]
    SessionLineageCycle(String),
    #[error(
        "Crush session lineage exceeds the finite {MAX_CRUSH_SESSION_LINEAGE_DEPTH}-session bound"
    )]
    SessionLineageTooDeep,
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct CrushSqliteSourceErrorV0 {
    source: SqliteSourceAccessError,
}

impl CrushSqliteSourceErrorV0 {
    pub(crate) fn source(&self) -> &SqliteSourceAccessError {
        &self.source
    }

    pub(crate) fn into_source(self) -> SqliteSourceAccessError {
        self.source
    }
}

impl From<SqliteSourceAccessError> for CrushSqliteSourceErrorV0 {
    fn from(source: SqliteSourceAccessError) -> Self {
        Self { source }
    }
}

impl From<SqliteSourceAccessError> for CrushSourceBackedErrorV0 {
    fn from(source: SqliteSourceAccessError) -> Self {
        Self::SqliteSource(source.into())
    }
}

pub type CrushSourceBackedResultV0<T> = Result<T, CrushSourceBackedErrorV0>;

/// One selected or registered Crush project database.
///
/// `project_key` is the stable catalog lineage. It must not be synthesized
/// from the current physical database path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrushProjectDatabaseV0 {
    project_key: TypedKey,
    path: PathBuf,
}

impl CrushProjectDatabaseV0 {
    pub fn new(project_key: TypedKey, path: impl Into<PathBuf>) -> CrushSourceBackedResultV0<Self> {
        if project_key == TypedKey::Null {
            return Err(CrushSourceBackedErrorV0::NullProjectKey);
        }
        let path = path.into();
        if !path.is_absolute() {
            return Err(CrushSourceBackedErrorV0::RelativeDatabasePath(path));
        }
        Ok(Self { project_key, path })
    }

    pub fn project_key(&self) -> &TypedKey {
        &self.project_key
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// One exact observation of the selector-owned project inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrushProjectInventoryObservationV0 {
    authority_key: TypedKey,
    revision: Vec<u8>,
    databases: Vec<CrushProjectDatabaseV0>,
}

impl CrushProjectInventoryObservationV0 {
    pub fn new(
        authority_key: TypedKey,
        revision: Vec<u8>,
        databases: Vec<CrushProjectDatabaseV0>,
    ) -> CrushSourceBackedResultV0<Self> {
        if databases.len() > MAX_CRUSH_PROJECT_DATABASES {
            return Err(CrushSourceBackedErrorV0::InventoryTooLarge);
        }
        SourceInventoryObservation::new(
            CaptureProvider::Crush.as_str(),
            CRUSH_INVENTORY_AUTHORITY_NAMESPACE,
            authority_key.clone(),
            CRUSH_INVENTORY_REVISION_KIND,
            revision.clone(),
        )?;
        Ok(Self {
            authority_key,
            revision,
            databases,
        })
    }

    pub fn databases(&self) -> &[CrushProjectDatabaseV0] {
        &self.databases
    }

    fn core_observation(&self) -> CrushSourceBackedResultV0<SourceInventoryObservation> {
        Ok(SourceInventoryObservation::new(
            CaptureProvider::Crush.as_str(),
            CRUSH_INVENTORY_AUTHORITY_NAMESPACE,
            self.authority_key.clone(),
            CRUSH_INVENTORY_REVISION_KIND,
            self.revision.clone(),
        )?)
    }
}

/// Authoritative selector seam used at opening, certification, and commit.
///
/// The implementation is expected to reread the same bounded official
/// configuration and project registry used by provider discovery.
pub trait CrushProjectInventorySourceV0 {
    fn observe(&self) -> CrushSourceBackedResultV0<CrushProjectInventoryObservationV0>;

    #[cfg(test)]
    fn record_projection_pass(&self) {}

    #[cfg(test)]
    fn record_snapshot_work(&self, _work: CrushSnapshotWorkV0) {}
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrushSnapshotWorkV0 {
    pub(crate) immutable_snapshot_opens: u64,
    pub(crate) copied_snapshot_opens: u64,
    pub(crate) source_bytes_copied: u64,
    pub(crate) terminal_fences: u64,
    pub(crate) terminal_revalidations: u64,
    pub(crate) max_active_snapshots: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct BoundDatabase {
    pub(crate) source_key: SourceKey,
    pub(crate) canonical_path: PathBuf,
    source_root: ProviderSourceRoot,
    database_file: Arc<OpenedProviderSourceFile>,
    #[cfg(test)]
    sqlite_authority: Arc<SqliteSourceDirectoryAuthority>,
    #[cfg(test)]
    database_name: OsString,
}

#[derive(Debug)]
pub(crate) struct FrozenInventory {
    pub(crate) observation: SourceInventoryObservation,
    pub(crate) databases: Vec<BoundDatabase>,
}

pub(crate) struct OpenedSource {
    pub(crate) database: BoundDatabase,
    read_snapshot: SqliteSourceReadSnapshot,
    schema: CrushNativeSchema,
}

impl OpenedSource {
    fn connection(&self) -> CrushSourceBackedResultV0<&Connection> {
        self.read_snapshot.connection().map_err(Into::into)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceScan {
    pub(crate) content_digest: [u8; 32],
    pub(crate) counts: ScannedSourceCounts,
}

#[cfg(test)]
thread_local! {
    static BEFORE_SOURCE_PUBLICATION_REVALIDATION:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn set_before_source_publication_revalidation(hook: Option<Box<dyn FnOnce()>>) {
    BEFORE_SOURCE_PUBLICATION_REVALIDATION.with(|slot| {
        *slot.borrow_mut() = hook;
    });
}

#[cfg(test)]
fn before_source_publication_revalidation() {
    BEFORE_SOURCE_PUBLICATION_REVALIDATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_source_publication_revalidation() {}

#[cfg(test)]
pub(crate) fn bind_inventory(
    data_root: &Path,
    observation: CrushProjectInventoryObservationV0,
) -> CrushSourceBackedResultV0<FrozenInventory> {
    bind_inventory_scoped(data_root, observation, SourceAnchorScope::Unqualified)
}

pub(crate) fn bind_inventory_scoped(
    data_root: &Path,
    observation: CrushProjectInventoryObservationV0,
    source_scope: SourceAnchorScope,
) -> CrushSourceBackedResultV0<FrozenInventory> {
    if observation.databases.len() > MAX_CRUSH_PROJECT_DATABASES {
        return Err(CrushSourceBackedErrorV0::InventoryTooLarge);
    }
    let core_observation = observation.core_observation()?;
    let mut source_ids = HashSet::new();
    let mut paths = HashSet::new();
    let mut databases = Vec::with_capacity(observation.databases.len());
    for database in observation.databases {
        if database.project_key == TypedKey::Null {
            return Err(CrushSourceBackedErrorV0::NullProjectKey);
        }
        if !database.path.is_absolute() {
            return Err(CrushSourceBackedErrorV0::RelativeDatabasePath(
                database.path,
            ));
        }
        let canonical_path = std::fs::canonicalize(&database.path)?;
        let (source_root, _sqlite_authority, _database_name) =
            retain_crush_sqlite_authority(data_root, &canonical_path)?;
        let database_file = Arc::new(source_root.open_file(Path::new(&_database_name))?);
        let source_key = crush_source_key_scoped(database.project_key, source_scope)?;
        if !source_ids.insert(source_key.identity().digest()) {
            return Err(CrushSourceBackedErrorV0::DuplicateProjectKey);
        }
        if !paths.insert(canonical_path.clone()) {
            return Err(CrushSourceBackedErrorV0::DuplicateDatabasePath);
        }
        databases.push(BoundDatabase {
            source_key,
            canonical_path,
            source_root,
            database_file,
            #[cfg(test)]
            sqlite_authority: Arc::new(_sqlite_authority),
            #[cfg(test)]
            database_name: _database_name,
        });
    }
    databases.sort_by_key(|database| database.source_key.identity().digest());
    Ok(FrozenInventory {
        observation: core_observation,
        databases,
    })
}

#[cfg(test)]
pub(crate) fn open_source(database: BoundDatabase) -> CrushSourceBackedResultV0<OpenedSource> {
    let read_snapshot = open_root_handle_sqlite_source_snapshot(
        &database.sqlite_authority,
        &database.database_name,
    )?;
    open_source_snapshot(database, read_snapshot)
}

pub(crate) fn open_source_snapshot(
    database: BoundDatabase,
    read_snapshot: SqliteSourceReadSnapshot,
) -> CrushSourceBackedResultV0<OpenedSource> {
    let configure = (|| {
        let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
            .map_err(|_| CrushSourceBackedErrorV0::CountOverflow)?;
        read_snapshot
            .connection()?
            .set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
        read_snapshot
            .connection()?
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|source| {
                read_snapshot.diagnose_provider_query_error(
                    "setting the private Crush SQLite busy timeout",
                    source,
                    SqliteFailurePhase::SourceValidation,
                )
            })?;
        let schema = read_native_schema(read_snapshot.connection()?).map_err(|error| {
            diagnose_crush_provider_query_error(
                &read_snapshot,
                error.into(),
                SqliteFailurePhase::Schema,
            )
        })?;
        database.source_root.revalidate()?;
        Ok(schema)
    })();
    let schema = match configure {
        Ok(schema) => schema,
        Err(primary) => return Err(abort_crush_snapshot(read_snapshot, primary)),
    };
    Ok(OpenedSource {
        database,
        read_snapshot,
        schema,
    })
}

fn abort_crush_snapshot(
    snapshot: SqliteSourceReadSnapshot,
    primary: CrushSourceBackedErrorV0,
) -> CrushSourceBackedErrorV0 {
    match snapshot.abort() {
        Ok(()) => primary,
        Err(cleanup) => CrushSourceBackedErrorV0::SnapshotCleanup {
            primary: Box::new(primary),
            cleanup: cleanup.into(),
        },
    }
}

fn diagnose_crush_provider_query_error(
    snapshot: &SqliteSourceReadSnapshot,
    error: CrushSourceBackedErrorV0,
    phase: SqliteFailurePhase,
) -> CrushSourceBackedErrorV0 {
    let source = match error {
        CrushSourceBackedErrorV0::Sqlite(source)
        | CrushSourceBackedErrorV0::Capture(CaptureError::Sqlite(source)) => source,
        error => return error,
    };
    snapshot
        .diagnose_provider_query_error("querying the root-authorized Crush snapshot", source, phase)
        .into()
}

pub(crate) fn abort_opened_source(
    source: OpenedSource,
    primary: CrushSourceBackedErrorV0,
) -> CrushSourceBackedErrorV0 {
    abort_crush_snapshot(source.read_snapshot, primary)
}

fn finish_source(source: OpenedSource) -> CrushSourceBackedResultV0<()> {
    let OpenedSource {
        database,
        read_snapshot,
        ..
    } = source;
    read_snapshot.finish()?;
    before_source_publication_revalidation();
    database.database_file.revalidate_same_object_leaf()?;
    database.source_root.revalidate()?;
    Ok(())
}

/// Closes the guarded SQLite read transaction, validates its retained
/// DB/WAL/SHM evidence, and then revalidates the admitted family snapshot.
///
/// The shared coordinator consumes this boundary before it certifies a Crush
/// source so central publication cannot accidentally bypass the provider's
/// root-bound closing proof.
pub(crate) fn finish_opened_source(source: OpenedSource) -> CrushSourceBackedResultV0<bool> {
    match finish_source(source) {
        Ok(()) => Ok(true),
        Err(CrushSourceBackedErrorV0::Capture(
            CaptureError::SourceChangedDuringCapture
            | CaptureError::InvalidProviderTranscriptPath { .. },
        )) => Ok(false),
        Err(error) => Err(error),
    }
}

fn retain_crush_sqlite_authority(
    data_root: &Path,
    canonical_path: &Path,
) -> CrushSourceBackedResultV0<(ProviderSourceRoot, SqliteSourceDirectoryAuthority, OsString)> {
    let parent = canonical_path.parent().ok_or_else(|| {
        CaptureError::InvalidPayload("Crush SQLite source has no parent directory".to_owned())
    })?;
    let database_name = canonical_path
        .file_name()
        .map(OsString::from)
        .ok_or_else(|| {
            CaptureError::InvalidPayload("Crush SQLite source has no leaf name".to_owned())
        })?;
    let source_root = ProviderSourceRoot::open(parent)?;
    let directory = source_root.directory()?;
    let authority_handle = directory.try_clone_authority_handle()?;
    let sqlite_authority =
        retain_sqlite_source_directory_authority(data_root, &authority_handle, parent)?;
    source_root.revalidate()?;
    Ok((source_root, sqlite_authority, database_name))
}

pub(crate) fn scan_source<L, S>(
    source: &OpenedSource,
    sink: &mut ChangedDocumentSink<'_, '_, L, S>,
) -> CrushSourceBackedResultV0<CertifiedSource>
where
    L: crate::lifecycle::CaptureLifecycleSink,
    S: crate::lifecycle::DocumentRecordSpool,
{
    let scan = scan_source_in_snapshot(source, sink).map_err(|error| {
        diagnose_crush_provider_query_error(
            &source.read_snapshot,
            error,
            SqliteFailurePhase::Projection,
        )
    })?;
    Ok(SqliteLogicalSnapshot::new(
        CRUSH_PARSER_REVISION,
        source.schema.schema_fingerprint.as_bytes(),
        scan.content_digest,
        scan.counts,
    )
    .certify(source.database.source_key.clone())?)
}

fn scan_source_in_snapshot<L, S>(
    source: &OpenedSource,
    sink: &mut ChangedDocumentSink<'_, '_, L, S>,
) -> CrushSourceBackedResultV0<SourceScan>
where
    L: crate::lifecycle::CaptureLifecycleSink,
    S: crate::lifecycle::DocumentRecordSpool,
{
    let mut frontier = CrushNativeFrontier { after_rowid: None };
    let mut digest = Sha256::new();
    digest.update(CRUSH_MESSAGE_DIGEST_DOMAIN);
    let mut counts = ScannedSourceCounts::default();
    loop {
        let observed = next_candidate_batch(
            source.connection()?,
            &source.schema,
            &frontier,
            CRUSH_MESSAGE_QUERY_BATCH,
        )?;
        if observed.is_empty() {
            break;
        }
        let mut batch_len = 0;
        let mut batch_bytes = 0_u64;
        for candidate in &observed {
            if batch_len > 0
                && batch_bytes.saturating_add(candidate.observed_bytes)
                    > CRUSH_MESSAGE_QUERY_TARGET_BYTES
            {
                break;
            }
            batch_len += 1;
            batch_bytes = batch_bytes.saturating_add(candidate.observed_bytes);
        }
        let candidates = &observed[..batch_len];
        let mut loaded = load_message_batch(source.connection()?, &source.schema, candidates)?;

        for candidate in candidates {
            frontier.after_rowid = Some(candidate.rowid);
            counts.complete_records = checked_add(counts.complete_records, 1)?;
            counts.certified_bytes = checked_add(counts.certified_bytes, candidate.observed_bytes)?;
            let row = loaded
                .remove(&candidate.rowid)
                .ok_or(CaptureError::SourceChangedDuringCapture)?;
            let (row, session, digest_values) = match row {
                Ok(CrushLoadedRow {
                    row,
                    session,
                    digest_values,
                }) => (row, session, digest_values),
                Err(error) if row_decode_error_is_local(&error) => {
                    counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                    hash_rejected_candidate(&mut digest, candidate, error.to_string().as_bytes());
                    sink.record_rejection(sqlite_rejection_draft(
                        &source.database.source_key,
                        CaptureProvider::Crush,
                        &source.database.canonical_path,
                        u64::try_from(candidate.rowid).unwrap_or_default(),
                        SourceBackedRecordRejectionClass::MalformedRecord,
                        error.to_string(),
                    ));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let record_digest = message_record_digest_bytes(&digest_values);
            super::hash_field(&mut digest, &candidate.rowid.to_be_bytes());
            super::hash_field(&mut digest, &record_digest);

            match project_message(&row, session.as_ref())? {
                CrushRecordProjection::Rejection => {
                    counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                    sink.record_rejection(sqlite_rejection_draft(
                        &source.database.source_key,
                        CaptureProvider::Crush,
                        &source.database.canonical_path,
                        u64::try_from(candidate.rowid).unwrap_or_default(),
                        SourceBackedRecordRejectionClass::UnsupportedRecord,
                        "Crush SQLite row has an unsupported message shape",
                    ));
                }
                CrushRecordProjection::Message(projection) if projection.event.is_some() => {
                    let session = session
                        .as_ref()
                        .ok_or(CrushSourceBackedErrorV0::UnexpectedNativeRow)?;
                    match core_record(source, &row, session, &projection) {
                        Ok(record) => {
                            sink.emit_core_record(record)?;
                            counts.retained_records = checked_add(counts.retained_records, 1)?;
                            counts.indexed_documents = checked_add(counts.indexed_documents, 1)?;
                        }
                        Err(error) if crush_row_projection_error(&error) => {
                            counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                            sink.record_rejection(sqlite_rejection_draft(
                                &source.database.source_key,
                                CaptureProvider::Crush,
                                &source.database.canonical_path,
                                u64::try_from(candidate.rowid).unwrap_or_default(),
                                SourceBackedRecordRejectionClass::UnsupportedRecord,
                                error.to_string(),
                            ));
                        }
                        Err(error) => return Err(error),
                    }
                }
                CrushRecordProjection::Message(_) => {
                    counts.ignored_records = checked_add(counts.ignored_records, 1)?;
                }
            }
        }
    }
    Ok(SourceScan {
        content_digest: digest.finalize().into(),
        counts,
    })
}

fn crush_row_projection_error(error: &CrushSourceBackedErrorV0) -> bool {
    matches!(
        error,
        CrushSourceBackedErrorV0::Projection(ProjectionContractError::EmptyField {
            field: "typed_key_utf8",
        }) | CrushSourceBackedErrorV0::Projection(ProjectionContractError::FieldTooLarge {
            field: "typed_key_utf8",
            ..
        }) | CrushSourceBackedErrorV0::CoreRecord(CoreRecordError::EmptyField {
            field: "activity.invocation.tool" | "activity.result.status",
        }) | CrushSourceBackedErrorV0::CoreRecord(CoreRecordError::FieldTooLarge {
            field: "normalized_body"
                | "structured_content"
                | "selected_content"
                | "activity.invocation.tool"
                | "activity.result.status",
            ..
        })
    )
}

fn core_record(
    source: &OpenedSource,
    row: &super::super::projection::CrushMessageRow,
    session: &CrushSessionRow,
    projection: &CrushMessageProjection,
) -> CrushSourceBackedResultV0<CoreRecord> {
    let session_id = crush_session_id(&source.database.source_key, &row.session_id)?;
    let lineage = session_lineage(source, session)?;
    let item_key = NativeItemKey::native_id(
        CRUSH_NATIVE_MESSAGE_NAMESPACE,
        TypedKey::utf8(row.id.clone())?,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source: &source.database.source_key,
        session_id,
        logical_item_kind: CRUSH_LOGICAL_EVENT_KIND,
        native_item_key: &item_key,
        subrecord_selector: None,
    })?;
    let event = projection
        .event
        .as_ref()
        .ok_or(CrushSourceBackedErrorV0::UnexpectedNativeRow)?;
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        source.database.source_key.clone(),
        fnv1a64(row.id.as_bytes()),
        event.event_type.as_str(),
        CRUSH_PARSER_REVISION,
        policy_selected_body(row, projection),
    )?;
    record.agent_scope = Some(if lineage.parent_session_id.is_some() {
        AgentScope::Subagent
    } else {
        AgentScope::Primary
    });
    if let Some(parent_session_id) = lineage.parent_session_id {
        record.parent_session_id = Some(parent_session_id);
        record.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    }
    record.provider_session_id = Some(row.session_id.clone());
    record.native_event_id = Some(TypedKey::utf8(row.id.clone())?);
    record.occurred_at_unix_ms = event.occurred_at_unix_ms;
    record.role = event.role.map(|role| role.as_str().to_owned());
    record.content.structured_content = Some(json!({
            "native_message": {
                "rowid": row.rowid,
                "id": row.id,
                "session_id": row.session_id,
                "role": row.role,
                "parts": projection.raw_parts,
                "created_at_unix_ms": row.created_at,
                "updated_at_unix_ms": row.updated_at,
                "provider": row.provider,
                "model": row.model,
                "is_summary_message": row.is_summary_message,
            },
            "native_session": {
                "id": session.id,
                "parent_session_id": session.parent_session_id,
                "title": session.title,
                "created_at_unix_ms": session.created_at,
                "updated_at_unix_ms": session.updated_at,
                "prompt_tokens": session.prompt_tokens,
                "completion_tokens": session.completion_tokens,
                "cost": session.cost,
                "summary_message_id": session.summary_message_id,
            }
    }));
    record.content.activity = crush_activity(projection, event.occurred_at_unix_ms)?;
    if record.content.encoded_content_bytes()? > ctx_history_core::MAX_CORE_CONTENT_BYTES {
        let capture = record.content.activity.as_mut().and_then(|activity| {
            activity
                .invocation
                .as_mut()
                .map(|invocation| &mut invocation.arguments)
                .or_else(|| {
                    activity
                        .result
                        .as_mut()
                        .map(|result| &mut result.structured_content)
                })
        });
        if let Some(capture @ ActivityJsonCapture::Present { .. }) = capture {
            let observed_encoded_bytes = match capture {
                ActivityJsonCapture::Present { value } => serde_json::to_vec(value)
                    .ok()
                    .and_then(|encoded| u64::try_from(encoded.len()).ok()),
                _ => None,
            };
            *capture = ActivityJsonCapture::Omitted {
                reason: "size_limit".to_owned(),
                observed_encoded_bytes,
            };
        }
    }
    record
        .content
        .omit_structured_content_if_aggregate_exceeds_limit()?;
    record.validate_contract()?;
    Ok(record)
}

fn policy_selected_body(
    row: &super::super::projection::CrushMessageRow,
    projection: &CrushMessageProjection,
) -> String {
    projection
        .complete_text
        .clone()
        .unwrap_or_else(|| row.parts.clone())
}

fn crush_activity(
    projection: &CrushMessageProjection,
    occurred_at_unix_ms: Option<i64>,
) -> CrushSourceBackedResultV0<Option<CoreActivity>> {
    if let Some(output) = projection.output.as_ref() {
        let Some(call_id) = output.call_id.as_deref() else {
            return Ok(None);
        };
        return Ok(Some(CoreActivity {
            revision: CORE_ACTIVITY_REVISION,
            provider_call_id: Some(TypedKey::utf8(call_id)?),
            invocation: None,
            result: Some(ActivityResult {
                status: output.status.clone(),
                completed_at_unix_ms: occurred_at_unix_ms,
                duration_ns: output
                    .duration_ms
                    .and_then(|value| value.checked_mul(1_000_000)),
                text: ActivityTextCapture::NormalizedBody,
                structured_content: ActivityJsonCapture::Present {
                    value: projection.raw_parts.clone(),
                },
            }),
            facts: Vec::new(),
        }));
    }
    if projection.event_type != ctx_history_core::EventType::ToolCall {
        return Ok(None);
    }
    let Some(call) = projection.raw_parts.as_array().and_then(|parts| {
        parts
            .iter()
            .find(|part| part.get("type").and_then(serde_json::Value::as_str) == Some("tool_call"))
    }) else {
        return Ok(None);
    };
    let data = call.get("data").unwrap_or(call);
    let call_id = ["tool_call_id", "toolCallId", "call_id", "callId"]
        .into_iter()
        .find_map(|key| data.get(key).and_then(serde_json::Value::as_str));
    let tool = ["name", "tool_name", "toolName"]
        .into_iter()
        .find_map(|key| data.get(key).and_then(serde_json::Value::as_str));
    let (Some(call_id), Some(tool)) = (call_id, tool) else {
        return Ok(None);
    };
    let arguments = data
        .get("arguments")
        .or_else(|| data.get("input"))
        .cloned()
        .map(|value| ActivityJsonCapture::Present { value })
        .unwrap_or(ActivityJsonCapture::Absent);
    Ok(Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8(call_id)?),
        invocation: Some(ActivityInvocation {
            protocol: None,
            server: None,
            tool: tool.to_owned(),
            arguments,
            started_at_unix_ms: occurred_at_unix_ms,
        }),
        result: None,
        facts: Vec::new(),
    }))
}

fn hash_rejected_candidate(digest: &mut Sha256, candidate: &CrushCandidate, reason: &[u8]) {
    super::hash_field(digest, &candidate.rowid.to_be_bytes());
    super::hash_field(digest, &candidate.observed_bytes.to_be_bytes());
    super::hash_field(digest, reason);
}

fn checked_add(left: u64, right: u64) -> CrushSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(CrushSourceBackedErrorV0::CountOverflow)
}
