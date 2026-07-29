//! Source-backed lexical projection for Crush's finite project-database inventory.
//!
//! The selector owner supplies a re-observable inventory. This adapter owns
//! only Crush discovery binding, native SQLite parsing, stable provider
//! coordinates, and bounded lexical projection. Source lifecycle,
//! certification, and atomic publication remain in the shared contracts.

use std::{
    collections::HashSet,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceInventoryObservation, SourceKey, SourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::{GenerationWriter, IndexError, LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use rusqlite::{limits::Limit, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    query::{
        hydrate_row_from_connection, next_candidate, row_decode_error_is_local, CrushCandidate,
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
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
        SqliteSourceReadSnapshot,
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

#[derive(Debug, Error)]
pub enum CrushSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Index(#[from] IndexError),
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

impl FrozenInventory {
    pub(crate) fn source_keys(&self) -> Vec<SourceKey> {
        self.databases
            .iter()
            .map(|database| database.source_key.clone())
            .collect()
    }

    pub(crate) fn contains_exact_source(&self, source: &SourceKey) -> bool {
        self.databases
            .iter()
            .any(|database| database.source_key.exact_descriptor_eq(source))
    }

    pub(crate) fn matches(
        &self,
        observation: CrushProjectInventoryObservationV0,
    ) -> CrushSourceBackedResultV0<bool> {
        let candidate = bind_inventory(observation)?;
        Ok(self.observation == candidate.observation
            && self.databases.len() == candidate.databases.len()
            && self
                .databases
                .iter()
                .zip(candidate.databases)
                .all(|(left, right)| {
                    left.source_key.exact_descriptor_eq(&right.source_key)
                        && left.canonical_path == right.canonical_path
                }))
    }
}

struct SourceRevalidation {
    source_root: ProviderSourceRoot,
    sqlite_authority: Arc<SqliteSourceDirectoryAuthority>,
    database_name: OsString,
    family_snapshot: SqliteSourceEvidence,
    _root_evidence: SqliteSourceEvidence,
}

pub(crate) struct OpenedSource {
    pub(crate) database: BoundDatabase,
    family_snapshot: SqliteSourceEvidence,
    read_snapshot: SqliteSourceReadSnapshot,
    schema: CrushNativeSchema,
    pub(crate) observation: SourceObservation,
    revision_digest: [u8; 32],
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageAddress {
    rowid: i64,
    native_record_id: String,
    parent_rowid: i64,
    provider_session_id: String,
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
}

impl CrushLocatorResolverV0 {
    pub fn discover(
        inventory_source: &dyn CrushProjectInventorySourceV0,
    ) -> CrushSourceBackedResultV0<Self> {
        Ok(Self {
            inventory: bind_inventory(inventory_source.observe()?)?,
        })
    }

    pub fn hydrate(
        &self,
        locator: &SourceRecordLocator,
    ) -> CrushSourceBackedResultV0<CrushHydratedRecordV0> {
        locator.validate_contract()?;
        let address = validate_message_locator(locator)?;
        let database = self
            .inventory
            .databases
            .iter()
            .find(|database| database.source_key.exact_descriptor_eq(locator.source()))
            .cloned()
            .ok_or(CrushSourceBackedErrorV0::LocatorSourceNotFound)?;
        let source = open_source(database)?;
        if locator.certified_source_revision_digest() != Some(&source.revision_digest) {
            return Err(CrushSourceBackedErrorV0::StaleSourceEvidence);
        }
        let hydrated = hydrate_row_from_connection(
            source.connection()?,
            &source.schema,
            CrushNativePhase::Messages,
            address.rowid,
            0,
        );
        let hydrated = match hydrated {
            Ok(CrushHydratedRow::Message {
                row,
                session,
                digest_values,
                ..
            }) => (row, session, digest_values),
            Ok(_) => return Err(CrushSourceBackedErrorV0::UnexpectedNativeRow),
            Err(CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => {
                return Err(CrushSourceBackedErrorV0::MissingRecord);
            }
            Err(error) => return Err(error.into()),
        };
        let (row, session, digest_values) = hydrated;
        let parent_rowid = parent_rowid(&digest_values)?;
        let actual_digest = message_record_digest_bytes(&digest_values);
        if row.rowid != address.rowid
            || row.id != address.native_record_id
            || row.session_id != address.provider_session_id
            || parent_rowid != address.parent_rowid
            || &actual_digest != locator.record_digest()
        {
            return Err(CrushSourceBackedErrorV0::StaleRecordEvidence);
        }

        let context = deterministic_context(&source.database.canonical_path);
        let projection = match project_message(&row, session.as_ref(), &context)? {
            CrushRecordProjection::Message(projection) => projection,
            CrushRecordProjection::Rejection { .. } => {
                return Err(CrushSourceBackedErrorV0::StaleRecordEvidence);
            }
        };
        if projection.event.is_none() {
            return Err(CrushSourceBackedErrorV0::StaleRecordEvidence);
        }
        let (normalized_payload_hash, decoded_display_text) = if projection.output.is_none() {
            let (provider_session_id, native_record_id, normalized_hash, text) =
                crush_complete_message(&digest_values)?;
            if provider_session_id != address.provider_session_id
                || native_record_id != address.native_record_id
            {
                return Err(CrushSourceBackedErrorV0::StaleRecordEvidence);
            }
            (Some(normalized_hash), Some(text))
        } else {
            let event = projection
                .event
                .as_ref()
                .ok_or(CrushSourceBackedErrorV0::StaleRecordEvidence)?;
            (
                Some(event.provider_event_hash.clone()),
                Some(lexical_preview(&projection)),
            )
        };
        let finished = finish_source(source)?;
        if !source_revalidation_is_current(&finished) {
            return Err(CrushSourceBackedErrorV0::StaleSourceEvidence);
        }
        Ok(CrushHydratedRecordV0 {
            provider_session_id: address.provider_session_id,
            native_record_id: address.native_record_id,
            normalized_payload_hash,
            decoded_display_text,
        })
    }
}

pub(crate) fn bind_inventory(
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
            retain_crush_sqlite_authority(&canonical_path)?;
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

fn crush_source_key(project_key: TypedKey) -> CrushSourceBackedResultV0<SourceKey> {
    let anchor = SourceAnchor::provider_native(CRUSH_SOURCE_ANCHOR_NAMESPACE, project_key)?;
    Ok(SourceKey::derive(
        CaptureProvider::Crush.as_str(),
        CRUSH_SQLITE_SOURCE_FORMAT,
        CRUSH_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

pub(crate) fn open_source(database: BoundDatabase) -> CrushSourceBackedResultV0<OpenedSource> {
    let read_snapshot = open_root_handle_sqlite_source_snapshot(
        &database.sqlite_authority,
        &database.database_name,
    )?;
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
    let revision_digest = Sha256::digest(observation.revision()).into();
    Ok(OpenedSource {
        database,
        family_snapshot,
        read_snapshot,
        schema,
        observation,
        revision_digest,
    })
}

fn finish_source(source: OpenedSource) -> CrushSourceBackedResultV0<SourceRevalidation> {
    let OpenedSource {
        database,
        family_snapshot,
        read_snapshot,
        ..
    } = source;
    let root_evidence = read_snapshot.finish()?;
    before_source_publication_revalidation();
    Ok(SourceRevalidation {
        source_root: database.source_root,
        sqlite_authority: database.sqlite_authority,
        database_name: database.database_name,
        family_snapshot,
        _root_evidence: root_evidence,
    })
}

/// Closes the guarded SQLite read transaction, validates its retained
/// DB/WAL/SHM evidence, and then revalidates the admitted family snapshot.
///
/// The shared coordinator consumes this boundary before it certifies a Crush
/// source so central publication cannot accidentally bypass the provider's
/// root-bound closing proof.
pub(crate) fn finish_opened_source(source: OpenedSource) -> CrushSourceBackedResultV0<bool> {
    let evidence = finish_source(source)?;
    Ok(source_revalidation_is_current(&evidence))
}

fn source_revalidation_is_current(evidence: &SourceRevalidation) -> bool {
    if evidence.source_root.revalidate().is_err() {
        return false;
    }
    let Ok(current) = open_root_handle_sqlite_source_snapshot(
        &evidence.sqlite_authority,
        &evidence.database_name,
    ) else {
        return false;
    };
    let current_evidence = current.evidence().clone();
    if current.finish().is_err() || evidence.source_root.revalidate().is_err() {
        return false;
    }
    current_evidence == evidence.family_snapshot
}

fn retain_crush_sqlite_authority(
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
    let sqlite_authority = retain_sqlite_source_directory_authority(&authority_handle, parent)?;
    source_root.revalidate()?;
    Ok((source_root, sqlite_authority, database_name))
}

fn crush_source_revision(evidence: &SqliteSourceEvidence, schema_fingerprint: &str) -> String {
    format!(
        "crush-sqlite-snapshot-v1:capture={CRUSH_CAPTURE_REVISION};policy={CRUSH_POLICY_REVISION};schema={schema_fingerprint};{}",
        sqlite_evidence_revision_component(evidence),
    )
}

fn sqlite_evidence_revision_component(evidence: &SqliteSourceEvidence) -> String {
    format!(
        "identity={};length={};revision={}",
        hex_bytes(evidence.identity()),
        evidence.length(),
        hex_bytes(evidence.revision()),
    )
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
    writer: &mut GenerationWriter,
) -> CrushSourceBackedResultV0<SourceScan> {
    scan_source_in_snapshot(source, writer)
}

fn scan_source_in_snapshot(
    source: &OpenedSource,
    writer: &mut GenerationWriter,
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
    while let Some(candidate) = next_candidate(source.connection()?, &source.schema, &frontier)? {
        frontier.after_rowid = Some(candidate.rowid);
        frontier.next_ordinal = checked_add(frontier.next_ordinal, 1)?;
        counts.complete_records = checked_add(counts.complete_records, 1)?;
        counts.certified_bytes = checked_add(counts.certified_bytes, candidate.observed_bytes)?;
        if candidate.observed_bytes > CRUSH_NATIVE_MAX_ROW_BYTES {
            counts.rejected_records = checked_add(counts.rejected_records, 1)?;
            hash_rejected_candidate(&mut digest, &candidate, b"oversized");
            continue;
        }

        let row = hydrate_row_from_connection(
            source.connection()?,
            &source.schema,
            CrushNativePhase::Messages,
            candidate.rowid,
            candidate.observed_bytes,
        );
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
                hash_rejected_candidate(&mut digest, &candidate, error.to_string().as_bytes());
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
                writer.add_document(lexical_document(
                    source,
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
    Ok(SourceScan {
        content_digest: digest.finalize().into(),
        counts,
    })
}

fn lexical_document(
    source: &OpenedSource,
    row: &super::super::projection::CrushMessageRow,
    session: &CrushSessionRow,
    digest_values: &[NativeSqliteValue],
    record_digest: [u8; 32],
    projection: &CrushMessageProjection,
) -> CrushSourceBackedResultV0<LexicalDocument> {
    let session_id = crush_session_id(&source.database.source_key, &row.session_id)?;
    let lineage = session_lineage(source, session, session_id)?;
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
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(source.revision_digest),
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
        body: lexical_preview(projection),
        workspace: None,
        cwd: None,
        touched_files: touched_paths(projection)?,
    })
}

#[derive(Debug, Clone, Copy)]
struct SessionLineage {
    parent_session_id: Option<StableEntityId>,
    root_session_id: StableEntityId,
    agent_type: AgentType,
    is_primary: bool,
}

fn session_lineage(
    source: &OpenedSource,
    session: &CrushSessionRow,
    session_id: StableEntityId,
) -> CrushSourceBackedResultV0<SessionLineage> {
    let Some(parent_provider_session_id) = session.parent_session_id.as_deref() else {
        return Ok(SessionLineage {
            parent_session_id: None,
            root_session_id: session_id,
            agent_type: AgentType::Primary,
            is_primary: true,
        });
    };
    let parent_session_id =
        crush_session_id(&source.database.source_key, parent_provider_session_id)?;
    let mut seen = HashSet::from([session.id.clone()]);
    let mut root_provider_session_id = parent_provider_session_id.to_owned();
    for depth in 0..MAX_CRUSH_SESSION_LINEAGE_DEPTH {
        if !seen.insert(root_provider_session_id.clone()) {
            return Err(CrushSourceBackedErrorV0::SessionLineageCycle(
                root_provider_session_id,
            ));
        }
        let next_parent = source
            .connection()?
            .query_row(
                "select parent_session_id
                   from sessions
                  where typeof(id) = 'text'
                    and id collate binary = ?1 collate binary",
                [&root_provider_session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        let Some(next_parent) = next_parent else {
            let root_session_id =
                crush_session_id(&source.database.source_key, &root_provider_session_id)?;
            return Ok(SessionLineage {
                parent_session_id: Some(parent_session_id),
                root_session_id,
                agent_type: AgentType::Subagent,
                is_primary: false,
            });
        };
        root_provider_session_id = next_parent;
        if depth + 1 == MAX_CRUSH_SESSION_LINEAGE_DEPTH {
            return Err(CrushSourceBackedErrorV0::SessionLineageTooDeep);
        }
    }
    Err(CrushSourceBackedErrorV0::SessionLineageTooDeep)
}

fn crush_session_id(
    source: &SourceKey,
    provider_session_id: &str,
) -> CrushSourceBackedResultV0<StableEntityId> {
    let session_key = NativeSessionKey::native_id(
        CRUSH_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(provider_session_id)?,
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: CRUSH_LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?)
}

fn lexical_preview(projection: &CrushMessageProjection) -> String {
    let text = if let Some(text) = projection.complete_text.as_deref() {
        text.to_owned()
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
    let bounded = text
        .chars()
        .take(MAX_BODY_PREVIEW_CHARS)
        .collect::<String>();
    if bounded.is_empty() {
        "crush event".to_owned()
    } else {
        bounded
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

fn validate_message_locator(
    locator: &SourceRecordLocator,
) -> CrushSourceBackedResultV0<MessageAddress> {
    if locator.source().provider() != CaptureProvider::Crush.as_str()
        || locator.source().source_format() != CRUSH_SQLITE_SOURCE_FORMAT
        || locator.source().schema_variant() != CRUSH_SOURCE_SCHEMA_VARIANT
        || locator.source().provider_identity_version() != 1
        || locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision
    {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    }
    let SourceAnchor::ProviderNative { namespace, .. } = locator.source().anchor() else {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    };
    if namespace != CRUSH_SOURCE_ANCHOR_NAMESPACE {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    }
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    };
    if logical_relation != CRUSH_MESSAGE_RELATION
        || row_version.as_ref() != Some(&TypedKey::Bytes(locator.record_digest().to_vec()))
    {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    }
    let TypedKey::Composite(parts) = primary_key else {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    };
    let [TypedKey::I64(rowid), TypedKey::Utf8(native_record_id), TypedKey::I64(parent_rowid), TypedKey::Utf8(provider_session_id)] =
        parts.as_slice()
    else {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    };
    if *rowid <= 0
        || *parent_rowid <= 0
        || native_record_id.is_empty()
        || provider_session_id.is_empty()
    {
        return Err(CrushSourceBackedErrorV0::InvalidLocator);
    }
    Ok(MessageAddress {
        rowid: *rowid,
        native_record_id: native_record_id.clone(),
        parent_rowid: *parent_rowid,
        provider_session_id: provider_session_id.clone(),
    })
}

fn checked_add(left: u64, right: u64) -> CrushSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(CrushSourceBackedErrorV0::CountOverflow)
}

#[cfg(test)]
mod tests {
    use rusqlite::{config::DbConfig, Connection};
    use serde_json::json;

    use super::*;

    #[derive(Clone)]
    struct TestInventory {
        observation: CrushProjectInventoryObservationV0,
    }

    impl TestInventory {
        fn new(observation: CrushProjectInventoryObservationV0) -> Self {
            Self { observation }
        }
    }

    impl CrushProjectInventorySourceV0 for TestInventory {
        fn observe(&self) -> CrushSourceBackedResultV0<CrushProjectInventoryObservationV0> {
            Ok(self.observation.clone())
        }
    }

    #[test]
    fn source_backed_multi_db_root_guards_and_exact_hydration() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let first = temp.path().join("first.db");
        let second = temp.path().join("second.db");
        write_database(&first, "session-a", "message-a", "alpha exact body");
        write_database(&second, "session-b", "message-b", "beta exact body");
        add_session_lineage(&first, "session-a", "middle-a", "root-a");
        let inventory = TestInventory::new(inventory(
            b"inventory-1",
            vec![
                database("project-a", &first),
                database("project-b", &second),
            ],
        ));
        let frozen = bind_inventory(inventory.observe().unwrap()).unwrap();
        assert_eq!(frozen.databases.len(), 2);

        let first_path = std::fs::canonicalize(&first).unwrap();
        let first_database = frozen
            .databases
            .iter()
            .find(|database| database.canonical_path == first_path)
            .unwrap()
            .clone();
        let first_source = open_source(first_database).unwrap();
        let alpha_event = document_for_only_message(&first_source);
        assert_eq!(
            alpha_event.provider_session_id.as_deref(),
            Some("session-a")
        );
        assert!(alpha_event.parent_session_id.is_some());
        assert_ne!(
            alpha_event.parent_session_id,
            Some(alpha_event.root_session_id)
        );
        assert_ne!(alpha_event.root_session_id, alpha_event.session_id);
        assert_eq!(alpha_event.branch, None);
        assert_eq!(alpha_event.source_path.as_deref(), first_path.to_str());
        assert_eq!(alpha_event.agent_type, AgentType::Subagent.as_str());
        assert!(!alpha_event.is_primary);
        assert!(finish_opened_source(first_source).unwrap());

        let second_path = std::fs::canonicalize(&second).unwrap();
        let second_database = frozen
            .databases
            .iter()
            .find(|database| database.canonical_path == second_path)
            .unwrap()
            .clone();
        let second_source = open_source(second_database).unwrap();
        let beta_event = document_for_only_message(&second_source);
        assert_eq!(beta_event.parent_session_id, None);
        assert_eq!(beta_event.root_session_id, beta_event.session_id);
        assert_eq!(beta_event.agent_type, AgentType::Primary.as_str());
        assert!(beta_event.is_primary);
        assert!(finish_opened_source(second_source).unwrap());

        let locator = alpha_event.locator.clone();
        let hydrated = CrushLocatorResolverV0::discover(&inventory)
            .unwrap()
            .hydrate(&locator)
            .unwrap();
        assert_eq!(hydrated.provider_session_id, "session-a");
        assert_eq!(hydrated.native_record_id, "message-a");
        assert_eq!(
            hydrated.decoded_display_text.as_deref(),
            Some("alpha exact body")
        );
    }

    #[test]
    fn source_backed_replacement_keeps_ids_and_rejects_stale_locator() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("project.db");
        write_database(&path, "session", "message", "before replacement");
        let inventory = TestInventory::new(inventory(
            b"inventory-stable",
            vec![database("project", &path)],
        ));
        let opening = bind_inventory(inventory.observe().unwrap()).unwrap();
        let source = open_source(opening.databases.into_iter().next().unwrap()).unwrap();
        let before = document_for_only_message(&source);
        assert!(finish_opened_source(source).unwrap());

        let replacement = temp.path().join("replacement.db");
        write_database(&replacement, "session", "message", "after replacement");
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&replacement, &path).unwrap();

        let replacement = bind_inventory(inventory.observe().unwrap()).unwrap();
        let source = open_source(replacement.databases.into_iter().next().unwrap()).unwrap();
        let after = document_for_only_message(&source);
        assert!(finish_opened_source(source).unwrap());
        assert_eq!(after.event_id, before.event_id);
        assert_ne!(after.locator, before.locator);
        assert!(matches!(
            CrushLocatorResolverV0::discover(&inventory)
                .unwrap()
                .hydrate(&before.locator),
            Err(CrushSourceBackedErrorV0::StaleSourceEvidence)
        ));
        assert_eq!(
            CrushLocatorResolverV0::discover(&inventory)
                .unwrap()
                .hydrate(&after.locator)
                .unwrap()
                .decoded_display_text
                .as_deref(),
            Some("after replacement")
        );
    }

    fn document_for_only_message(source: &OpenedSource) -> LexicalDocument {
        let frontier = CrushNativeFrontier {
            phase: CrushNativePhase::Messages,
            after_rowid: None,
            next_ordinal: 0,
        };
        let candidate = next_candidate(source.connection().unwrap(), &source.schema, &frontier)
            .unwrap()
            .unwrap();
        let CrushHydratedRow::Message {
            row,
            session: Some(session),
            digest_values,
            ..
        } = hydrate_row_from_connection(
            source.connection().unwrap(),
            &source.schema,
            CrushNativePhase::Messages,
            candidate.rowid,
            candidate.observed_bytes,
        )
        .unwrap()
        else {
            panic!("expected one parented Crush message row");
        };
        let projection = match project_message(
            &row,
            Some(&session),
            &deterministic_context(&source.database.canonical_path),
        )
        .unwrap()
        {
            CrushRecordProjection::Message(projection) => projection,
            CrushRecordProjection::Rejection { .. } => {
                panic!("expected the test message to project")
            }
        };
        lexical_document(
            source,
            &row,
            &session,
            &digest_values,
            message_record_digest_bytes(&digest_values),
            &projection,
        )
        .unwrap()
    }

    #[test]
    fn stock_sqlite_snapshot_scan_sees_committed_content_retained_in_active_wal() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("wal-project.db");
        write_database(&path, "wal-session", "wal-message", "main database body");
        let writer = Connection::open(&path).unwrap();
        let mode: String = writer
            .query_row("pragma journal_mode = wal", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer
            .execute_batch("pragma wal_autocheckpoint = 0")
            .unwrap();
        let parts =
            json!([{"type": "text", "data": {"text": "committed Crush WAL body"}}]).to_string();
        writer
            .execute(
                "update messages set parts = ?1 where id = 'wal-message'",
                [parts],
            )
            .unwrap();
        writer
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        drop(writer);
        assert!(path.with_file_name("wal-project.db-wal").exists());
        assert!(path.with_file_name("wal-project.db-shm").exists());

        let frozen = bind_inventory(inventory(
            b"wal-inventory",
            vec![database("wal-project", &path)],
        ))
        .unwrap();
        let source = open_source(frozen.databases.into_iter().next().unwrap()).unwrap();
        let document = document_for_only_message(&source);
        assert_eq!(document.body, "committed Crush WAL body");
        assert!(finish_opened_source(source).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stock_sqlite_snapshot_finish_precedes_publication_revalidation() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("project.db");
        let replacement = temp.path().join("replacement.db");
        write_database(&path, "session", "message", "opening body");
        write_database(
            &replacement,
            "session",
            "message",
            "replacement after finish",
        );
        let frozen =
            bind_inventory(inventory(b"finish-order", vec![database("project", &path)])).unwrap();
        let opened = open_source(frozen.databases[0].clone()).unwrap();
        let replaced_path = path.clone();
        set_before_source_publication_revalidation(Some(Box::new(move || {
            std::fs::rename(&replacement, &replaced_path).unwrap();
        })));

        assert!(!finish_opened_source(opened).unwrap());
    }

    fn inventory(
        revision: &[u8],
        databases: Vec<CrushProjectDatabaseV0>,
    ) -> CrushProjectInventoryObservationV0 {
        CrushProjectInventoryObservationV0::new(
            TypedKey::utf8("test-crush-project-registry").unwrap(),
            revision.to_vec(),
            databases,
        )
        .unwrap()
    }

    fn database(project: &str, path: &Path) -> CrushProjectDatabaseV0 {
        CrushProjectDatabaseV0::new(TypedKey::utf8(project).unwrap(), path.to_path_buf()).unwrap()
    }

    fn write_database(path: &Path, session_id: &str, message_id: &str, body: &str) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "create table sessions (
                    id text primary key,
                    parent_session_id text,
                    title text,
                    prompt_tokens integer,
                    completion_tokens integer,
                    cost real,
                    created_at integer,
                    updated_at integer,
                    summary_message_id text
                );
                create table messages (
                    id text primary key,
                    session_id text not null,
                    role text not null,
                    parts text not null,
                    created_at integer,
                    updated_at integer,
                    provider text,
                    model text,
                    is_summary_message integer not null default 0
                );",
            )
            .unwrap();
        connection
            .execute(
                "insert into sessions (
                    id, parent_session_id, title, prompt_tokens, completion_tokens,
                    cost, created_at, updated_at, summary_message_id
                 ) values (?1, null, 'test', 1, 1, 0, 1000, 2000, null)",
                [session_id],
            )
            .unwrap();
        let parts = json!([{"type": "text", "data": {"text": body}}]).to_string();
        connection
            .execute(
                "insert into messages (
                    id, session_id, role, parts, created_at, updated_at, provider,
                    model, is_summary_message
                 ) values (?1, ?2, 'assistant', ?3, 1001, 1001, 'test', 'model', 0)",
                (message_id, session_id, parts),
            )
            .unwrap();
    }

    fn add_session_lineage(path: &Path, child: &str, parent: &str, root: &str) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute(
                "insert into sessions (
                    id, parent_session_id, title, prompt_tokens, completion_tokens,
                    cost, created_at, updated_at, summary_message_id
                 ) values (?1, null, 'root', 1, 1, 0, 900, 900, null)",
                [root],
            )
            .unwrap();
        connection
            .execute(
                "insert into sessions (
                    id, parent_session_id, title, prompt_tokens, completion_tokens,
                    cost, created_at, updated_at, summary_message_id
                 ) values (?1, ?2, 'parent', 1, 1, 0, 950, 950, null)",
                (parent, root),
            )
            .unwrap();
        connection
            .execute(
                "update sessions
                    set parent_session_id = ?2
                  where id = ?1",
                (child, parent),
            )
            .unwrap();
    }
}
