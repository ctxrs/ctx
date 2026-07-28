//! Source-backed lexical projection for Crush's finite project-database inventory.
//!
//! The selector owner supplies a re-observable inventory. This adapter owns
//! only Crush discovery binding, native SQLite parsing, stable provider
//! coordinates, and bounded lexical projection. Source lifecycle,
//! certification, and atomic publication remain in the shared contracts.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceAppend,
    CertifiedSourceDeletion, CertifiedSourceInventory, EventIdentityInput, LocatorRevisionPolicy,
    NativeItemKey, NativeRecordCoordinate, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceFrontier,
    SourceInventoryObservation, SourceKey, SourceObservation, SourceRecordLocator,
    SourceResolverContractError, StableEntityId, TypedKey,
};
use ctx_history_index::{
    CommitReceipt, GenerationWriter, IndexError, LexicalDocument, RevalidationTarget,
    VerifiedIndex, WriterOptions, MAX_BODY_PREVIEW_CHARS,
};
use rusqlite::{limits::Limit, Connection, OpenFlags};
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
    common::io::ensure_regular_provider_transcript_file,
    native_source::NativeSqliteValue,
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit,
        },
        sqlite::with_sqlite_read_snapshot,
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
    source::{source_revision, source_snapshot},
};

const CRUSH_SOURCE_ANCHOR_NAMESPACE: &str = "crush.project-database";
const CRUSH_INVENTORY_AUTHORITY_NAMESPACE: &str = "crush.project-inventory";
const CRUSH_INVENTORY_REVISION_KIND: &str = "crush-selected-registered-projects-v0";
const CRUSH_DISCOVERY_REVISION: &str = "crush-project-inventory-source-backed-v0";
const CRUSH_SOURCE_SCHEMA_VARIANT: &str = "crush-project-sqlite-v0";
const CRUSH_SOURCE_REVISION_KIND: &str = "crush-sqlite-snapshot-v1";
const CRUSH_FRONTIER_KIND: &str = "crush-sqlite-exact-snapshot-v0";
const CRUSH_PARSER_REVISION: &str = "crush-sqlite-source-backed-v0";
const CRUSH_NATIVE_SESSION_NAMESPACE: &str = "crush.session";
const CRUSH_NATIVE_MESSAGE_NAMESPACE: &str = "crush.message";
const CRUSH_LOGICAL_SESSION_KIND: &str = "crush-session";
const CRUSH_LOGICAL_EVENT_KIND: &str = "crush-message";
const CRUSH_MESSAGE_RELATION: &str = "crush.messages-with-parent-session";
const CRUSH_MESSAGE_DIGEST_DOMAIN: &[u8] = b"ctx-crush-source-backed-message-set-v0\0";
const MAX_CRUSH_PROJECT_DATABASES: usize = 128;

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
    #[error(
        "Crush project inventory exceeds the finite {MAX_CRUSH_PROJECT_DATABASES}-database bound"
    )]
    InventoryTooLarge,
    #[error("Crush project inventory changed while its generation was staged")]
    InventoryChanged,
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
    #[error("Crush source certificate does not support exact replay")]
    InvalidReplayCertificate,
    #[error("Crush message scan produced an unexpected native row")]
    UnexpectedNativeRow,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CrushSourceBackedCountersV0 {
    pub inventory_databases: u64,
    pub scanned_sources: u64,
    pub replayed_sources: u64,
    pub replaced_sources: u64,
    pub deleted_sources: u64,
    pub complete_records: u64,
    pub indexed_documents: u64,
    pub rejected_records: u64,
    pub ignored_records: u64,
    pub certified_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct CrushSourceBackedIngestReceiptV0 {
    pub commit: CommitReceipt,
    pub counters: CrushSourceBackedCountersV0,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrushHydratedRecordV0 {
    pub provider_session_id: String,
    pub native_record_id: String,
    pub normalized_payload_hash: Option<String>,
    pub decoded_display_text: Option<String>,
}

#[derive(Debug, Clone)]
struct BoundDatabase {
    source_key: SourceKey,
    canonical_path: PathBuf,
}

#[derive(Debug)]
struct FrozenInventory {
    observation: SourceInventoryObservation,
    databases: Vec<BoundDatabase>,
}

impl FrozenInventory {
    fn source_keys(&self) -> Vec<SourceKey> {
        self.databases
            .iter()
            .map(|database| database.source_key.clone())
            .collect()
    }

    fn contains_exact_source(&self, source: &SourceKey) -> bool {
        self.databases
            .iter()
            .any(|database| database.source_key.exact_descriptor_eq(source))
    }

    fn matches(
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

#[derive(Debug, Clone)]
struct SourceRevalidation {
    canonical_path: PathBuf,
    snapshot: crate::provider::sqlite::ProviderSqliteSourceSnapshot,
}

struct OpenedSource {
    database: BoundDatabase,
    snapshot: crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    connection: Connection,
    schema: CrushNativeSchema,
    observation: SourceObservation,
    revision_digest: [u8; 32],
}

#[derive(Debug, Clone, Copy)]
struct SourceScan {
    content_digest: [u8; 32],
    counts: ScannedSourceCounts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageAddress {
    rowid: i64,
    native_record_id: String,
    parent_rowid: i64,
    provider_session_id: String,
}

/// Builds or refreshes Crush's lexical projection over the exact finite
/// selected/registered project inventory.
pub fn ingest_crush_source_backed_v0(
    inventory_source: &dyn CrushProjectInventorySourceV0,
    global_index_root: impl AsRef<Path>,
) -> CrushSourceBackedResultV0<CrushSourceBackedIngestReceiptV0> {
    let opening_inventory = bind_inventory(inventory_source.observe()?)?;
    let mut counters = CrushSourceBackedCountersV0 {
        inventory_databases: to_u64(opening_inventory.databases.len())?,
        ..CrushSourceBackedCountersV0::default()
    };
    let mut writer = GenerationWriter::open(global_index_root, WriterOptions::default())?;
    let base_sources = writer
        .base_manifest()
        .map(|manifest| {
            manifest
                .sources
                .iter()
                .cloned()
                .map(|source| (source.observation().source().clone(), source))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut source_revalidation = HashMap::<SourceKey, SourceRevalidation>::new();

    for database in &opening_inventory.databases {
        let source = open_source(database.clone())?;
        let base = base_sources.get(&database.source_key);
        if base.is_some_and(|base| exact_replay_matches(base, &source)) {
            let base = base.ok_or(CrushSourceBackedErrorV0::InvalidReplayCertificate)?;
            let writer_base = writer.begin_source_append(database.source_key.clone())?;
            if writer_base != base {
                return Err(CrushSourceBackedErrorV0::InvalidReplayCertificate);
            }
            let frontier = base
                .frontier()
                .ok_or(CrushSourceBackedErrorV0::InvalidReplayCertificate)?;
            let append = CertifiedSourceAppend::certify(
                base,
                base.clone(),
                frontier.certified_prefix_bytes(),
                *frontier.certified_prefix_digest(),
            )?;
            writer.certify_source_append(append)?;
            counters.replayed_sources = checked_add(counters.replayed_sources, 1)?;
        } else {
            writer.begin_source(database.source_key.clone())?;
            let scan = scan_source(&source, &mut writer)?;
            let closing = closing_observation(&source)?;
            let frontier = SourceFrontier::new(
                CRUSH_FRONTIER_KIND,
                TypedKey::bytes(source.observation.revision().to_vec())?,
                scan.counts.certified_bytes,
                scan.content_digest,
            )?;
            let certificate = CertifiedSource::certify_with_frontier(
                source.observation.clone(),
                closing,
                CRUSH_PARSER_REVISION,
                scan.content_digest,
                scan.counts,
                Some(frontier),
            )?;
            writer.certify_source(certificate)?;
            counters.scanned_sources = checked_add(counters.scanned_sources, 1)?;
            if base.is_some() {
                counters.replaced_sources = checked_add(counters.replaced_sources, 1)?;
            }
            counters.complete_records =
                checked_add(counters.complete_records, scan.counts.complete_records)?;
            counters.indexed_documents =
                checked_add(counters.indexed_documents, scan.counts.indexed_documents)?;
            counters.rejected_records =
                checked_add(counters.rejected_records, scan.counts.rejected_records)?;
            counters.ignored_records =
                checked_add(counters.ignored_records, scan.counts.ignored_records)?;
            counters.certified_bytes =
                checked_add(counters.certified_bytes, scan.counts.certified_bytes)?;
        }
        source_revalidation.insert(
            database.source_key.clone(),
            SourceRevalidation {
                canonical_path: database.canonical_path.clone(),
                snapshot: source.snapshot,
            },
        );
    }

    let closing_inventory = bind_inventory(inventory_source.observe()?)?;
    if opening_inventory.observation != closing_inventory.observation
        || opening_inventory.databases.len() != closing_inventory.databases.len()
        || opening_inventory
            .databases
            .iter()
            .zip(&closing_inventory.databases)
            .any(|(left, right)| {
                !left.source_key.exact_descriptor_eq(&right.source_key)
                    || left.canonical_path != right.canonical_path
            })
    {
        return Err(CrushSourceBackedErrorV0::InventoryChanged);
    }
    let certified_inventory = CertifiedSourceInventory::certify(
        opening_inventory.observation.clone(),
        closing_inventory.observation,
        CRUSH_DISCOVERY_REVISION,
        opening_inventory.source_keys(),
    )?;

    for base in base_sources.values() {
        let source = base.observation().source();
        if source.provider() == CaptureProvider::Crush.as_str()
            && source.source_format() == CRUSH_SQLITE_SOURCE_FORMAT
            && source.schema_variant() == CRUSH_SOURCE_SCHEMA_VARIANT
            && !opening_inventory.contains_exact_source(source)
        {
            writer.delete_source(CertifiedSourceDeletion::from_inventory(
                source.clone(),
                &certified_inventory,
            )?)?;
            counters.deleted_sources = checked_add(counters.deleted_sources, 1)?;
        }
    }

    let mut inventory_revalidated = None;
    let commit = writer.commit(|target| {
        let inventory_is_current = *inventory_revalidated.get_or_insert_with(|| {
            inventory_source
                .observe()
                .and_then(|observation| opening_inventory.matches(observation))
                .unwrap_or(false)
        });
        if !inventory_is_current {
            return false;
        }
        match target {
            RevalidationTarget::Source(certificate) => source_revalidation
                .get(certificate.observation().source())
                .is_some_and(|evidence| {
                    evidence
                        .snapshot
                        .revalidate(&evidence.canonical_path)
                        .unwrap_or(false)
                }),
            RevalidationTarget::Deletion(deletion) => deletion.verifies(&certified_inventory),
        }
    })?;
    Ok(CrushSourceBackedIngestReceiptV0 { commit, counters })
}

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
        let hydrated = with_sqlite_read_snapshot(&source.connection, || {
            hydrate_row_from_connection(
                &source.connection,
                &source.schema,
                CrushNativePhase::Messages,
                address.rowid,
                0,
            )
        });
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
        if !source
            .snapshot
            .revalidate(&source.database.canonical_path)?
        {
            return Err(CrushSourceBackedErrorV0::StaleSourceEvidence);
        }
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
        Ok(CrushHydratedRecordV0 {
            provider_session_id: address.provider_session_id,
            native_record_id: address.native_record_id,
            normalized_payload_hash,
            decoded_display_text,
        })
    }
}

fn bind_inventory(
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
        ensure_regular_provider_transcript_file(&database.path)?;
        let canonical_path = std::fs::canonicalize(&database.path)?;
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

fn open_source(database: BoundDatabase) -> CrushSourceBackedResultV0<OpenedSource> {
    ensure_regular_provider_transcript_file(&database.canonical_path)?;
    let snapshot = source_snapshot(&database.canonical_path)?;
    let connection = open_direct_readonly(&database.canonical_path)?;
    let schema = with_sqlite_read_snapshot(&connection, || read_native_schema(&connection))?;
    if !snapshot.revalidate(&database.canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }
    let revision = source_revision(&snapshot, &schema.schema_fingerprint).into_bytes();
    let observation = SourceObservation::new(
        database.source_key.clone(),
        CRUSH_SOURCE_REVISION_KIND,
        revision,
    )?;
    let revision_digest = Sha256::digest(observation.revision()).into();
    Ok(OpenedSource {
        database,
        snapshot,
        connection,
        schema,
        observation,
        revision_digest,
    })
}

fn open_direct_readonly(path: &Path) -> CrushSourceBackedResultV0<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| CrushSourceBackedErrorV0::CountOverflow)?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "query_only", true)?;
    Ok(connection)
}

fn exact_replay_matches(base: &CertifiedSource, source: &OpenedSource) -> bool {
    base.parser_revision() == CRUSH_PARSER_REVISION
        && base.observation() == &source.observation
        && base.frontier().is_some_and(|frontier| {
            frontier.checkpoint_kind() == CRUSH_FRONTIER_KIND
                && frontier.checkpoint() == &TypedKey::Bytes(source.observation.revision().to_vec())
        })
}

fn closing_observation(source: &OpenedSource) -> CrushSourceBackedResultV0<SourceObservation> {
    if !source
        .snapshot
        .revalidate(&source.database.canonical_path)?
    {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }
    Ok(SourceObservation::new(
        source.database.source_key.clone(),
        CRUSH_SOURCE_REVISION_KIND,
        source.observation.revision().to_vec(),
    )?)
}

fn scan_source(
    source: &OpenedSource,
    writer: &mut GenerationWriter,
) -> CrushSourceBackedResultV0<SourceScan> {
    with_sqlite_read_snapshot(&source.connection, || {
        let context = deterministic_context(&source.database.canonical_path);
        let mut frontier = CrushNativeFrontier {
            phase: CrushNativePhase::Messages,
            after_rowid: None,
            next_ordinal: 0,
        };
        let mut digest = Sha256::new();
        digest.update(CRUSH_MESSAGE_DIGEST_DOMAIN);
        let mut counts = ScannedSourceCounts::default();
        while let Some(candidate) = next_candidate(&source.connection, &source.schema, &frontier)? {
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
                &source.connection,
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
                    ))
                }
                Err(error) if row_decode_error_is_local(&error) => {
                    counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                    hash_rejected_candidate(&mut digest, &candidate, error.to_string().as_bytes());
                    continue;
                }
                Err(error) => return Err(error),
            };
            let record_digest = message_record_digest_bytes(&digest_values);
            super::hash_field(&mut digest, &candidate.rowid.to_be_bytes());
            super::hash_field(&mut digest, &record_digest);

            match project_message(&row, session.as_ref(), &context)? {
                CrushRecordProjection::Rejection { .. } => {
                    counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                }
                CrushRecordProjection::Message(projection) if projection.event.is_some() => {
                    writer.add_document(lexical_document(
                        source,
                        &row,
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
    })
    .map_err(Into::into)
}

fn lexical_document(
    source: &OpenedSource,
    row: &super::super::projection::CrushMessageRow,
    digest_values: &[NativeSqliteValue],
    record_digest: [u8; 32],
    projection: &CrushMessageProjection,
) -> CrushSourceBackedResultV0<LexicalDocument> {
    let session_key = NativeSessionKey::native_id(
        CRUSH_NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(row.session_id.clone())?,
    )?;
    let session_id = derive_session_id(SessionIdentityInput {
        source: &source.database.source_key,
        logical_session_kind: CRUSH_LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?;
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
        source: source.database.source_key.clone(),
        locator,
        provider_session_id: Some(row.session_id.clone()),
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

fn to_u64(value: usize) -> CrushSourceBackedResultV0<u64> {
    u64::try_from(value).map_err(|_| CrushSourceBackedErrorV0::CountOverflow)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use rusqlite::Connection;
    use serde_json::json;

    use super::*;

    #[derive(Clone)]
    struct TestInventory {
        observation: Arc<Mutex<CrushProjectInventoryObservationV0>>,
    }

    impl TestInventory {
        fn new(observation: CrushProjectInventoryObservationV0) -> Self {
            Self {
                observation: Arc::new(Mutex::new(observation)),
            }
        }

        fn replace(&self, observation: CrushProjectInventoryObservationV0) {
            *self.observation.lock().unwrap() = observation;
        }
    }

    impl CrushProjectInventorySourceV0 for TestInventory {
        fn observe(&self) -> CrushSourceBackedResultV0<CrushProjectInventoryObservationV0> {
            Ok(self.observation.lock().unwrap().clone())
        }
    }

    #[test]
    fn source_backed_multi_db_cold_replay_and_exact_hydration() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let first = temp.path().join("first.db");
        let second = temp.path().join("second.db");
        write_database(&first, "session-a", "message-a", "alpha exact body");
        write_database(&second, "session-b", "message-b", "beta exact body");
        let inventory = TestInventory::new(inventory(
            b"inventory-1",
            vec![
                database("project-a", &first),
                database("project-b", &second),
            ],
        ));
        let index_root = temp.path().join("index");

        let cold = ingest_crush_source_backed_v0(&inventory, &index_root).unwrap();
        assert_eq!(cold.commit.indexed_documents, 2);
        assert_eq!(cold.commit.certified_sources, 2);
        assert_eq!(cold.counters.scanned_sources, 2);
        let index = VerifiedIndex::open(&index_root).unwrap();
        let alpha = index.search_event_candidates("alpha exact", 10).unwrap();
        assert_eq!(alpha.len(), 1);
        let event_id = alpha[0].event.event_id;
        let locator = alpha[0].event.locator.clone();
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

        let replay = ingest_crush_source_backed_v0(&inventory, &index_root).unwrap();
        assert_eq!(replay.counters.replayed_sources, 2);
        assert_eq!(replay.counters.scanned_sources, 0);
        assert_eq!(replay.commit.generation_id, cold.commit.generation_id);
        let replayed = VerifiedIndex::open(&index_root)
            .unwrap()
            .search_event_candidates("alpha exact", 10)
            .unwrap();
        assert_eq!(replayed[0].event.event_id, event_id);
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
        let index_root = temp.path().join("index");
        ingest_crush_source_backed_v0(&inventory, &index_root).unwrap();
        let before = VerifiedIndex::open(&index_root)
            .unwrap()
            .search_event_candidates("before replacement", 10)
            .unwrap()
            .remove(0)
            .event;

        let replacement = temp.path().join("replacement.db");
        write_database(&replacement, "session", "message", "after replacement");
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&replacement, &path).unwrap();

        let replaced = ingest_crush_source_backed_v0(&inventory, &index_root).unwrap();
        assert_eq!(replaced.counters.replaced_sources, 1);
        let after = VerifiedIndex::open(&index_root)
            .unwrap()
            .search_event_candidates("after replacement", 10)
            .unwrap()
            .remove(0)
            .event;
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

    #[test]
    fn source_backed_deregistration_retires_only_the_missing_project_source() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let first = temp.path().join("first.db");
        let second = temp.path().join("second.db");
        write_database(&first, "session-a", "message-a", "kept project");
        write_database(&second, "session-b", "message-b", "retired project");
        let inventory = TestInventory::new(inventory(
            b"inventory-1",
            vec![
                database("project-a", &first),
                database("project-b", &second),
            ],
        ));
        let index_root = temp.path().join("index");
        ingest_crush_source_backed_v0(&inventory, &index_root).unwrap();

        inventory.replace(inventory(
            b"inventory-2",
            vec![database("project-a", &first)],
        ));
        let retired = ingest_crush_source_backed_v0(&inventory, &index_root).unwrap();
        assert_eq!(retired.counters.replayed_sources, 1);
        assert_eq!(retired.counters.deleted_sources, 1);
        let index = VerifiedIndex::open(&index_root).unwrap();
        assert_eq!(index.document_count(), 1);
        assert_eq!(
            index
                .search_event_candidates("retired project", 10)
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            index
                .search_event_candidates("kept project", 10)
                .unwrap()
                .len(),
            1
        );
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
}
