//! Source-backed lexical projection for Crush's finite project-database inventory.
//!
//! The selector owner supplies a re-observable inventory. This adapter owns
//! Crush discovery binding, native SQLite parsing, stable provider coordinates,
//! and bounded lexical projection. Source lifecycle,

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(test)]
use std::cell::Cell;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceInventoryObservation, SourceKey, SourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::{IndexError, LexicalDocument};
use rusqlite::{limits::Limit, Connection};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    query::{
        hydrate_message_batch, load_session_parents, next_candidate_batch,
        row_decode_error_is_local, CrushCandidate,
    },
    read_native_schema, CrushHydratedRow, CrushNativeFrontier, CrushNativePhase, CrushNativeSchema,
    CRUSH_NATIVE_MAX_EVENT_TOUCHES, CRUSH_NATIVE_MAX_ROW_BYTES,
};
use crate::{
    common::io::ProviderSourceRoot,
    native_source::NativeSqliteValue,
    provider::file_touches::{
        event_type_supports_structured_file_touches, visit_provider_file_touch_drafts_with_limit,
    },
    provider::source_backed::{family::document::ChangedDocumentSink, SourceBackedRouteError},
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteLogicalSnapshot, SqliteSourceAccessError, SqliteSourceDirectoryAuthority,
        SqliteSourceEvidence, SqliteSourceReadSnapshot,
    },
    CaptureError, OutputOutcome, ProviderAdapterContext, CRUSH_SQLITE_SOURCE_FORMAT,
    MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::super::{
    capture::message_record_digest_bytes,
    projection::{
        crush_complete_message, project_message, CrushMessageProjection, CrushRecordProjection,
        CrushSessionRow,
    },
    CRUSH_CAPTURE_REVISION, CRUSH_POLICY_REVISION,
};

#[path = "source_backed_identity.rs"]
mod identity;
use identity::{
    crush_session_id, crush_source_key, crush_source_revision, session_lineage,
    validate_message_locator, MessageAddress,
};

const CRUSH_SOURCE_ANCHOR_NAMESPACE: &str = "crush.project-database";
const CRUSH_INVENTORY_AUTHORITY_NAMESPACE: &str = "crush.project-inventory";
const CRUSH_INVENTORY_REVISION_KIND: &str = "crush-selected-registered-projects-v0";
pub(crate) const CRUSH_DISCOVERY_REVISION: &str = "crush-project-inventory-source-backed-v0";
pub(crate) const CRUSH_SOURCE_SCHEMA_VARIANT: &str = "crush-project-sqlite-v0";
const CRUSH_SOURCE_REVISION_KIND: &str = "crush-sqlite-snapshot-v1";
pub(crate) const CRUSH_FRONTIER_KIND: &str = "crush-sqlite-exact-snapshot-v0";
pub(crate) const CRUSH_PARSER_REVISION: &str = "crush-sqlite-source-backed-v0";
const CRUSH_NATIVE_SESSION_NAMESPACE: &str = "crush.session";
const CRUSH_NATIVE_MESSAGE_NAMESPACE: &str = "crush.message";
const CRUSH_LOGICAL_SESSION_KIND: &str = "crush-session";
const CRUSH_LOGICAL_EVENT_KIND: &str = "crush-message";
const CRUSH_MESSAGE_RELATION: &str = "crush.messages-with-parent-session";
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
    Index(#[from] IndexError),
    #[error(transparent)]
    Route(#[from] SourceBackedRouteError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("Crush root-bound SQLite source access failed: {0}")]
    SqliteSourceAccess(String),
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
    #[error("Crush project database path is not valid UTF-8: {0:?}")]
    NonUtf8DatabasePath(PathBuf),
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
    #[error("Crush locator does not address a parented typed message row")]
    InvalidLocator,
    #[error("Crush locator source is not present in the selected/registered project inventory")]
    LocatorSourceNotFound,
    #[error("Crush locator source revision no longer matches the provider database")]
    StaleSourceEvidence,
    #[error("Crush locator row evidence no longer matches the provider database")]
    StaleRecordEvidence,
    #[error("Crush locator row is missing from the provider database")]
    MissingRecord,
}

pub type CrushSourceBackedResultV0<T> = Result<T, CrushSourceBackedErrorV0>;

impl From<SqliteSourceAccessError> for CrushSourceBackedErrorV0 {
    fn from(error: SqliteSourceAccessError) -> Self {
        Self::SqliteSourceAccess(error.to_string())
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrushHydratedRecordV0 {
    pub provider_session_id: String,
    pub native_record_id: String,
    pub normalized_payload_hash: Option<String>,
    pub decoded_display_text: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct BoundDatabase {
    pub(crate) source_key: SourceKey,
    pub(crate) canonical_path: PathBuf,
    source_root: ProviderSourceRoot,
    sqlite_authority: Arc<SqliteSourceDirectoryAuthority>,
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
    pub(crate) observation: SourceObservation,
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

/// One-invocation exact resolver for source-backed Crush locators.
pub struct CrushLocatorResolverV0 {
    inventory: FrozenInventory,
    #[cfg(test)]
    snapshot_opens: Cell<u64>,
    #[cfg(test)]
    native_key_batches: Cell<u64>,
}

impl CrushLocatorResolverV0 {
    pub fn discover(
        data_root: &Path,
        inventory_source: &dyn CrushProjectInventorySourceV0,
    ) -> CrushSourceBackedResultV0<Self> {
        Ok(Self {
            inventory: bind_inventory(data_root, inventory_source.observe()?)?,
            #[cfg(test)]
            snapshot_opens: Cell::new(0),
            #[cfg(test)]
            native_key_batches: Cell::new(0),
        })
    }

    #[cfg(test)]
    fn hydration_counters(&self) -> (u64, u64) {
        (self.snapshot_opens.get(), self.native_key_batches.get())
    }

    pub(crate) fn hydrate_locators(
        &self,
        locators: &[&SourceRecordLocator],
    ) -> CrushSourceBackedResultV0<Vec<CrushHydratedRecordV0>> {
        if locators.is_empty() {
            return Ok(Vec::new());
        }
        let mut groups =
            HashMap::<[u8; 32], (BoundDatabase, Vec<(usize, MessageAddress, [u8; 32])>)>::new();
        for (index, locator) in locators.iter().enumerate() {
            locator.validate_contract()?;
            let address = validate_message_locator(locator)?;
            let database = self
                .inventory
                .databases
                .iter()
                .find(|database| database.source_key.exact_descriptor_eq(locator.source()))
                .cloned()
                .ok_or(CrushSourceBackedErrorV0::LocatorSourceNotFound)?;
            groups
                .entry(database.source_key.identity().digest())
                .or_insert_with(|| (database, Vec::new()))
                .1
                .push((index, address, *locator.record_digest()));
        }

        let mut output = vec![None; locators.len()];
        for (_, (database, requests)) in groups {
            let source = open_source(database)?;
            #[cfg(test)]
            self.snapshot_opens
                .set(self.snapshot_opens.get().saturating_add(1));
            let resolved = (|| {
                let mut rows = HashMap::new();
                let mut unique = requests
                    .iter()
                    .map(|(_, address, _)| address.rowid)
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>();
                unique.sort_unstable();
                for rowids in unique.chunks(CRUSH_MESSAGE_QUERY_BATCH) {
                    let candidates = rowids
                        .iter()
                        .map(|rowid| CrushCandidate {
                            rowid: *rowid,
                            observed_bytes: 0,
                        })
                        .collect::<Vec<_>>();
                    #[cfg(test)]
                    self.native_key_batches
                        .set(self.native_key_batches.get().saturating_add(1));
                    for (rowid, hydrated) in
                        hydrate_message_batch(source.connection()?, &source.schema, &candidates)?
                    {
                        rows.insert(
                            rowid,
                            resolve_hydrated_message(&source.database.canonical_path, hydrated?)?,
                        );
                    }
                }
                for (index, address, expected_digest) in &requests {
                    let row = rows
                        .get(&address.rowid)
                        .ok_or(CrushSourceBackedErrorV0::MissingRecord)?;
                    if row.native_record_id != address.native_record_id
                        || row.provider_session_id != address.provider_session_id
                        || row.parent_rowid != address.parent_rowid
                        || row.record_digest != *expected_digest
                    {
                        return Err(CrushSourceBackedErrorV0::StaleRecordEvidence);
                    }
                    output[*index] = Some(CrushHydratedRecordV0 {
                        provider_session_id: row.provider_session_id.clone(),
                        native_record_id: row.native_record_id.clone(),
                        normalized_payload_hash: Some(row.normalized_payload_hash.clone()),
                        decoded_display_text: Some(row.decoded_display_text.clone()),
                    });
                }
                Ok(())
            })();
            finish_source(source)?;
            resolved?;
        }
        output
            .into_iter()
            .map(|record| record.ok_or(CrushSourceBackedErrorV0::MissingRecord))
            .collect()
    }
}

struct ResolvedCrushMessage {
    provider_session_id: String,
    native_record_id: String,
    parent_rowid: i64,
    record_digest: [u8; 32],
    normalized_payload_hash: String,
    decoded_display_text: String,
}

fn resolve_hydrated_message(
    source_path: &Path,
    hydrated: CrushHydratedRow,
) -> CrushSourceBackedResultV0<ResolvedCrushMessage> {
    let CrushHydratedRow::Message {
        row,
        session,
        digest_values,
        ..
    } = hydrated
    else {
        return Err(CrushSourceBackedErrorV0::UnexpectedNativeRow);
    };
    let parent_rowid = parent_rowid(&digest_values)?;
    let record_digest = message_record_digest_bytes(&digest_values);
    let projection =
        match project_message(&row, session.as_ref(), &deterministic_context(source_path))? {
            CrushRecordProjection::Message(projection) if projection.event.is_some() => projection,
            CrushRecordProjection::Message(_) | CrushRecordProjection::Rejection { .. } => {
                return Err(CrushSourceBackedErrorV0::StaleRecordEvidence);
            }
        };
    let (normalized_payload_hash, decoded_display_text) = if projection.output.is_none() {
        let (provider_session_id, native_record_id, normalized_hash, text) =
            crush_complete_message(&digest_values)?;
        if provider_session_id != row.session_id || native_record_id != row.id {
            return Err(CrushSourceBackedErrorV0::StaleRecordEvidence);
        }
        (normalized_hash, text)
    } else {
        let event = projection
            .event
            .as_ref()
            .ok_or(CrushSourceBackedErrorV0::StaleRecordEvidence)?;
        (event.provider_event_hash.clone(), lexical_body(&projection))
    };
    Ok(ResolvedCrushMessage {
        provider_session_id: row.session_id,
        native_record_id: row.id,
        parent_rowid,
        record_digest,
        normalized_payload_hash,
        decoded_display_text,
    })
}

pub(crate) fn bind_inventory(
    data_root: &Path,
    observation: CrushProjectInventoryObservationV0,
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
        let (source_root, sqlite_authority, database_name) =
            retain_crush_sqlite_authority(data_root, &canonical_path)?;
        let source_key = crush_source_key(database.project_key)?;
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
            sqlite_authority: Arc::new(sqlite_authority),
            database_name,
        });
    }
    databases.sort_by_key(|database| database.source_key.identity().digest());
    Ok(FrozenInventory {
        observation: core_observation,
        databases,
    })
}

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
    let family_snapshot = read_snapshot.evidence().clone();
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| CrushSourceBackedErrorV0::CountOverflow)?;
    read_snapshot
        .connection()?
        .set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
    read_snapshot
        .connection()?
        .busy_timeout(std::time::Duration::from_secs(5))?;
    let schema = read_native_schema(read_snapshot.connection()?)?;
    database.source_root.revalidate()?;
    let revision = crush_source_revision(&family_snapshot, &schema.schema_fingerprint).into_bytes();
    let observation = SourceObservation::new(
        database.source_key.clone(),
        CRUSH_SOURCE_REVISION_KIND,
        revision,
    )?;
    Ok(OpenedSource {
        database,
        read_snapshot,
        schema,
        observation,
    })
}

fn finish_source(source: OpenedSource) -> CrushSourceBackedResultV0<()> {
    let OpenedSource {
        database,
        read_snapshot,
        ..
    } = source;
    read_snapshot.finish()?;
    before_source_publication_revalidation();
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

pub(crate) fn exact_replay_matches(base: &CertifiedSource, source: &OpenedSource) -> bool {
    base.parser_revision() == CRUSH_PARSER_REVISION
        && base.observation() == &source.observation
        && base.frontier().is_some_and(|frontier| {
            frontier.checkpoint_kind() == CRUSH_FRONTIER_KIND
                && frontier.checkpoint() == &TypedKey::Bytes(source.observation.revision().to_vec())
        })
}

pub(crate) fn closing_observation(
    source: &OpenedSource,
) -> CrushSourceBackedResultV0<SourceObservation> {
    source.database.source_root.revalidate()?;
    Ok(SourceObservation::new(
        source.database.source_key.clone(),
        CRUSH_SOURCE_REVISION_KIND,
        source.observation.revision().to_vec(),
    )?)
}

pub(crate) fn scan_source(
    source: &OpenedSource,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> CrushSourceBackedResultV0<CertifiedSource> {
    let scan = scan_source_in_snapshot(source, sink)?;
    Ok(SqliteLogicalSnapshot::new(
        CRUSH_PARSER_REVISION,
        source.schema.schema_fingerprint.as_bytes(),
        scan.content_digest,
        scan.counts,
    )
    .certify(source.database.source_key.clone())?)
}

fn scan_source_in_snapshot(
    source: &OpenedSource,
    sink: &mut ChangedDocumentSink<'_, '_>,
) -> CrushSourceBackedResultV0<SourceScan> {
    let context = deterministic_context(&source.database.canonical_path);
    let mut frontier = CrushNativeFrontier {
        phase: CrushNativePhase::Messages,
        after_rowid: None,
        next_ordinal: 0,
    };
    let mut digest = Sha256::new();
    digest.update(CRUSH_MESSAGE_DIGEST_DOMAIN);
    let mut counts = ScannedSourceCounts::default();
    let session_parents =
        load_session_parents(source.connection()?, &source.schema.session_columns)?;
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
        let admissible = candidates
            .iter()
            .copied()
            .filter(|candidate| candidate.observed_bytes <= CRUSH_NATIVE_MAX_ROW_BYTES)
            .collect::<Vec<_>>();
        let mut hydrated =
            hydrate_message_batch(source.connection()?, &source.schema, &admissible)?;

        for candidate in candidates {
            frontier.after_rowid = Some(candidate.rowid);
            frontier.next_ordinal = checked_add(frontier.next_ordinal, 1)?;
            counts.complete_records = checked_add(counts.complete_records, 1)?;
            counts.certified_bytes = checked_add(counts.certified_bytes, candidate.observed_bytes)?;
            if candidate.observed_bytes > CRUSH_NATIVE_MAX_ROW_BYTES {
                counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                hash_rejected_candidate(&mut digest, candidate, b"oversized");
                continue;
            }
            let row = hydrated
                .remove(&candidate.rowid)
                .ok_or(CaptureError::SourceChangedDuringCapture)?;
            let (row, session, digest_values) = match row {
                Ok(CrushHydratedRow::Message {
                    row,
                    session,
                    digest_values,
                    ..
                }) => (row, session, digest_values),
                Ok(_) => {
                    return Err(CaptureError::SystemInvariant(
                        "Crush message scan hydrated a non-message row",
                    )
                    .into())
                }
                Err(error) if row_decode_error_is_local(&error) => {
                    counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                    hash_rejected_candidate(&mut digest, candidate, error.to_string().as_bytes());
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let record_digest = message_record_digest_bytes(&digest_values);
            super::hash_field(&mut digest, &candidate.rowid.to_be_bytes());
            super::hash_field(&mut digest, &record_digest);

            match project_message(&row, session.as_ref(), &context)? {
                CrushRecordProjection::Rejection { .. } => {
                    counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                }
                CrushRecordProjection::Message(projection) if projection.event.is_some() => {
                    let session = session
                        .as_ref()
                        .ok_or(CrushSourceBackedErrorV0::UnexpectedNativeRow)?;
                    sink.emit_document(lexical_document(
                        source,
                        &session_parents,
                        &row,
                        session,
                        &digest_values,
                        record_digest,
                        &projection,
                    )?)?;
                    counts.retained_records = checked_add(counts.retained_records, 1)?;
                    counts.indexed_documents = checked_add(counts.indexed_documents, 1)?;
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

fn lexical_document(
    source: &OpenedSource,
    session_parents: &HashMap<String, Option<String>>,
    row: &super::super::projection::CrushMessageRow,
    session: &CrushSessionRow,
    digest_values: &[NativeSqliteValue],
    record_digest: [u8; 32],
    projection: &CrushMessageProjection,
) -> CrushSourceBackedResultV0<LexicalDocument> {
    let session_id = crush_session_id(&source.database.source_key, &row.session_id)?;
    let lineage = session_lineage(source, session_parents, session, session_id)?;
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
    let parent_rowid = parent_rowid(digest_values)?;
    let primary_key = TypedKey::composite(vec![
        TypedKey::I64(row.rowid),
        TypedKey::utf8(row.id.clone())?,
        TypedKey::I64(parent_rowid),
        TypedKey::utf8(row.session_id.clone())?,
    ])?;
    let locator = SourceRecordLocator::new(
        source.database.source_key.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: CRUSH_MESSAGE_RELATION.to_owned(),
            primary_key,
            row_version: Some(TypedKey::bytes(record_digest.to_vec())?),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        record_digest,
    )?;
    let event = projection
        .event
        .as_ref()
        .ok_or(CrushSourceBackedErrorV0::UnexpectedNativeRow)?;
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: lineage.parent_session_id,
        root_session_id: lineage.root_session_id,
        source: source.database.source_key.clone(),
        locator,
        provider_session_id: Some(row.session_id.clone()),
        // Crush's native schema has no branch-name field.
        branch: None,
        source_path: Some(
            source
                .database
                .canonical_path
                .to_str()
                .ok_or_else(|| {
                    CrushSourceBackedErrorV0::NonUtf8DatabasePath(
                        source.database.canonical_path.clone(),
                    )
                })?
                .to_owned(),
        ),
        agent_type: lineage.agent_type.as_str().to_owned(),
        is_primary: lineage.is_primary,
        event_sequence: u64::try_from(row.rowid)
            .map_err(|_| CrushSourceBackedErrorV0::UnexpectedNativeRow)?,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body: lexical_body(projection),
        workspace: None,
        cwd: None,
        touched_files: touched_paths(projection)?,
    })
}

fn lexical_body(projection: &CrushMessageProjection) -> String {
    let text = if projection.output.is_none() {
        projection
            .complete_text
            .as_deref()
            .unwrap_or(projection.event_type.as_str())
            .to_owned()
    } else if let Some(output) = projection.output.as_ref() {
        let outcome = match output.outcome.outcome {
            OutputOutcome::Success => "success",
            OutputOutcome::Failure => "failure",
            OutputOutcome::Timeout => "timeout",
            OutputOutcome::Unknown => "unknown",
        };
        let mut fields = vec![
            projection.event_type.as_str().to_owned(),
            outcome.to_owned(),
        ];
        if let Some(command) = output.command.as_ref() {
            fields.push(command.tool_name.clone());
        }
        if let Some(call_id) = output.call_id.as_ref() {
            fields.push(call_id.clone());
        }
        fields.join(" ")
    } else {
        projection.event_type.as_str().to_owned()
    };
    if text.is_empty() {
        "crush event".to_owned()
    } else {
        text
    }
}

fn touched_paths(projection: &CrushMessageProjection) -> CrushSourceBackedResultV0<Vec<String>> {
    let mut paths = HashSet::new();
    visit_provider_file_touch_drafts_with_limit(
        &projection.raw_parts,
        event_type_supports_structured_file_touches(projection.event_type),
        CRUSH_NATIVE_MAX_EVENT_TOUCHES,
        |(_, touch)| {
            paths.insert(touch.path);
            Ok::<(), CaptureError>(())
        },
    )?;
    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn deterministic_context(path: &Path) -> ProviderAdapterContext {
    ProviderAdapterContext {
        machine_id: "crush-source-backed".to_owned(),
        source_path: Some(path.to_path_buf()),
        source_root: path.parent().map(Path::to_path_buf),
        imported_at: DateTime::<Utc>::UNIX_EPOCH,
    }
}

fn parent_rowid(values: &[NativeSqliteValue]) -> CrushSourceBackedResultV0<i64> {
    match values.first() {
        Some(NativeSqliteValue::Integer(value)) if *value > 0 => Ok(*value),
        _ => Err(CrushSourceBackedErrorV0::UnexpectedNativeRow),
    }
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

#[cfg(test)]
#[path = "source_backed_tests.rs"]
mod tests;
