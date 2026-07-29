use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, BatchHydrationRequest, BatchHydrationResult,
    CaptureProvider, CertifiedSource, CertifiedSourceInventory, ContentSourceResolver,
    EventHydrationRequest, EventIdentityInput, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionHydrationRequest,
    SessionIdentityInput, SourceAnchor, SourceInventoryObservation, SourceKey, SourceObservation,
    SourceRecordLocator, SourceResolverContractError, StableEntityId, SubrecordSelector, TypedKey,
};
use ctx_history_index::LexicalDocument;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    detect_schema, hash_candidate, initial_prefix_hasher, load_candidates, load_raw_row,
    publication::{provider_event, EventDraft},
    records::{
        assistant_text, event_base_index, lingma_logical_record_sha256, lingma_timestamp,
        native_values, row_from_native_values,
    },
    LingmaRow, SqliteEncoding,
};
use crate::{
    common::io::ProviderSourceRoot,
    provider::sqlite::sqlite_schema_fingerprint,
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
        SqliteSourceReadSnapshot,
    },
    CaptureError, LINGMA_SQLITE_SOURCE_FORMAT, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

const SOURCE_ANCHOR_NAMESPACE: &str = "lingma.installed-database";
const SOURCE_SCHEMA_VARIANT: &str = "lingma-chat-record-v1";
const SOURCE_REVISION_KIND: &str = "lingma-sqlite-snapshot-v0";
const INVENTORY_AUTHORITY_NAMESPACE: &str = "lingma.installed-client-profile-version";
const INVENTORY_REVISION_KIND: &str = "lingma-finite-database-inventory-v0";
const INVENTORY_DISCOVERY_REVISION: &str = "lingma-installed-database-discovery-v0";
const PARSER_REVISION: &str = "lingma-source-backed-chat-record-v0";
const NATIVE_SESSION_NAMESPACE: &str = "lingma.session";
const NATIVE_REQUEST_NAMESPACE: &str = "lingma.chat-record.request";
const NATIVE_POSITION_KIND: &str = "lingma.chat-record.scan-ordinal";
const NATIVE_SUBRECORD_NAMESPACE: &str = "lingma.chat-record.body-kind";
const LOGICAL_SESSION_KIND: &str = "lingma-session";
const LOGICAL_EVENT_KIND: &str = "lingma-chat-record-event";
const LOGICAL_RELATION: &str = "chat_record";
const USER_PROMPT_COORDINATE: &str = "chat_prompt";
const ASSISTANT_SUMMARY_COORDINATE: &str = "assistant_summary";
const ASSISTANT_ERROR_COORDINATE: &str = "assistant_error_result";
const MAX_INVENTORY_DATABASES: usize = 1_024;
const SOURCE_REVISION_DOMAIN: &[u8] = b"ctx-lingma-source-backed-revision-v0\0";
const INVENTORY_REVISION_DOMAIN: &[u8] = b"ctx-lingma-source-backed-inventory-v0\0";

#[derive(Debug, Error)]
pub(crate) enum LingmaSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    SqliteSource(#[from] SqliteSourceAccessError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    Resolver(#[from] SourceResolverContractError),
    #[error("Lingma source inventory exceeds {MAX_INVENTORY_DATABASES} databases")]
    InventoryTooLarge,
    #[error("Lingma source inventory contains a duplicate database lineage")]
    DuplicateDatabaseLineage,
    #[error("Lingma source inventory contains one database path more than once")]
    DuplicateDatabasePath,
    #[error("Lingma source inventory changed while its databases were being scanned")]
    InventoryChangedDuringScan,
    #[error("Lingma source changed while its SQLite snapshot was being scanned")]
    SourceChangedDuringScan,
    #[error("Lingma source-backed count overflow")]
    CountOverflow,
    #[error("Lingma source-backed projection emitted an empty lexical body")]
    EmptyLexicalBody,
}

pub(crate) type LingmaSourceBackedResultV0<T> = Result<T, LingmaSourceBackedErrorV0>;

struct LingmaRootAuthorizedSource {
    source_root: ProviderSourceRoot,
    sqlite_authority: SqliteSourceDirectoryAuthority,
    database_name: OsString,
}

impl LingmaRootAuthorizedSource {
    fn retain(path: &Path) -> LingmaSourceBackedResultV0<Self> {
        let parent = path.parent().ok_or_else(|| {
            CaptureError::InvalidPayload("Lingma SQLite source has no parent directory".to_owned())
        })?;
        let database_name = path.file_name().map(OsString::from).ok_or_else(|| {
            CaptureError::InvalidPayload("Lingma SQLite source has no leaf name".to_owned())
        })?;
        let source_root = ProviderSourceRoot::open(parent)?;
        let directory = source_root.directory()?;
        let authority_handle = directory
            .try_clone_authority_handle()
            .map_err(CaptureError::from)?;
        let sqlite_authority = retain_sqlite_source_directory_authority(&authority_handle, parent)?;
        source_root.revalidate()?;
        Ok(Self {
            source_root,
            sqlite_authority,
            database_name,
        })
    }

    fn open_snapshot(&self) -> LingmaSourceBackedResultV0<SqliteSourceReadSnapshot> {
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&self.sqlite_authority, &self.database_name)?;
        let connection = snapshot.connection()?;
        let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
            .map_err(|_| LingmaSourceBackedErrorV0::CountOverflow)?;
        connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, value_limit);
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(CaptureError::from)?;
        self.source_root.revalidate()?;
        Ok(snapshot)
    }

    fn evidence_is_current(
        &self,
        expected: &SqliteSourceEvidence,
    ) -> LingmaSourceBackedResultV0<bool> {
        self.source_root.revalidate()?;
        let snapshot = self.open_snapshot()?;
        let current = snapshot.evidence().clone();
        snapshot.finish()?;
        self.source_root.revalidate()?;
        Ok(&current == expected)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LingmaDatabaseSourceV0 {
    path: PathBuf,
    catalog_lineage: TypedKey,
}

impl LingmaDatabaseSourceV0 {
    pub(crate) fn new(
        path: impl Into<PathBuf>,
        catalog_lineage: TypedKey,
    ) -> LingmaSourceBackedResultV0<Self> {
        let source = Self {
            path: path.into(),
            catalog_lineage,
        };
        source.source_key()?;
        Ok(source)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn source_key(&self) -> LingmaSourceBackedResultV0<SourceKey> {
        let anchor =
            SourceAnchor::provider_native(SOURCE_ANCHOR_NAMESPACE, self.catalog_lineage.clone())?;
        Ok(SourceKey::derive(
            CaptureProvider::Lingma.as_str(),
            LINGMA_SQLITE_SOURCE_FORMAT,
            SOURCE_SCHEMA_VARIANT,
            1,
            anchor,
        )?)
    }
}

/// A complete, finite inventory supplied by the installed-client/profile/version discovery lane.
///
/// The catalog lineage is deliberately caller-owned: physical database paths are resolver
/// locations and never enter stable source, session, or event identity.
#[derive(Debug, Clone)]
pub(crate) struct LingmaSourceInventoryV0 {
    authority_key: TypedKey,
    databases: Vec<LingmaDatabaseSourceV0>,
    observation: SourceInventoryObservation,
}

impl LingmaSourceInventoryV0 {
    pub(crate) fn new(
        authority_key: TypedKey,
        mut databases: Vec<LingmaDatabaseSourceV0>,
    ) -> LingmaSourceBackedResultV0<Self> {
        if databases.len() > MAX_INVENTORY_DATABASES {
            return Err(LingmaSourceBackedErrorV0::InventoryTooLarge);
        }
        databases.sort_by_key(|database| {
            database
                .source_key()
                .map(|source| source.identity().digest())
                .unwrap_or([0; 32])
        });
        let mut source_keys = Vec::with_capacity(databases.len());
        for database in &databases {
            source_keys.push(database.source_key()?);
        }
        if source_keys
            .windows(2)
            .any(|pair| pair[0].identity() == pair[1].identity())
        {
            return Err(LingmaSourceBackedErrorV0::DuplicateDatabaseLineage);
        }
        let revision = inventory_revision(&source_keys);
        let observation = SourceInventoryObservation::new(
            CaptureProvider::Lingma.as_str(),
            INVENTORY_AUTHORITY_NAMESPACE,
            authority_key.clone(),
            INVENTORY_REVISION_KIND,
            revision.to_vec(),
        )?;
        Ok(Self {
            authority_key,
            databases,
            observation,
        })
    }

    pub(crate) fn databases(&self) -> &[LingmaDatabaseSourceV0] {
        &self.databases
    }

    fn source_keys(&self) -> LingmaSourceBackedResultV0<Vec<SourceKey>> {
        self.databases
            .iter()
            .map(LingmaDatabaseSourceV0::source_key)
            .collect()
    }

    fn exact_inventory_eq(&self, other: &Self) -> LingmaSourceBackedResultV0<bool> {
        if self.authority_key != other.authority_key
            || self.databases.len() != other.databases.len()
        {
            return Ok(false);
        }
        for (left, right) in self.databases.iter().zip(&other.databases) {
            let left_key = left.source_key()?;
            let right_key = right.source_key()?;
            if !left_key.exact_descriptor_eq(&right_key) || left.path != right.path {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LingmaSourceBackedRecordV0 {
    document: LexicalDocument,
}

impl LingmaSourceBackedRecordV0 {
    pub(crate) fn document(&self) -> &LexicalDocument {
        &self.document
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LingmaDatabaseScanV0 {
    path: PathBuf,
    certificate: CertifiedSource,
    records: Vec<LingmaSourceBackedRecordV0>,
}

impl LingmaDatabaseScanV0 {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn certificate(&self) -> &CertifiedSource {
        &self.certificate
    }

    pub(crate) fn records(&self) -> &[LingmaSourceBackedRecordV0] {
        &self.records
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LingmaSourceBackedScanV0 {
    inventory: CertifiedSourceInventory,
    databases: Vec<LingmaDatabaseScanV0>,
}

#[cfg(test)]
thread_local! {
    static BEFORE_DATABASE_CERTIFICATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn set_before_database_certification(hook: Option<Box<dyn FnOnce()>>) {
    BEFORE_DATABASE_CERTIFICATION.with(|slot| {
        *slot.borrow_mut() = hook;
    });
}

#[cfg(test)]
fn before_database_certification() {
    BEFORE_DATABASE_CERTIFICATION.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn before_database_certification() {}

impl LingmaSourceBackedScanV0 {
    pub(crate) fn inventory(&self) -> &CertifiedSourceInventory {
        &self.inventory
    }

    pub(crate) fn databases(&self) -> &[LingmaDatabaseScanV0] {
        &self.databases
    }
}

pub(crate) fn scan_lingma_source_backed_v0<F>(
    opening_inventory: LingmaSourceInventoryV0,
    close_inventory: F,
) -> LingmaSourceBackedResultV0<LingmaSourceBackedScanV0>
where
    F: FnOnce() -> LingmaSourceBackedResultV0<LingmaSourceInventoryV0>,
{
    reject_duplicate_paths(&opening_inventory)?;
    let mut databases = Vec::with_capacity(opening_inventory.databases.len());
    for database in &opening_inventory.databases {
        databases.push(scan_database(database)?);
    }

    let closing_inventory = close_inventory()?;
    if !opening_inventory.exact_inventory_eq(&closing_inventory)? {
        return Err(LingmaSourceBackedErrorV0::InventoryChangedDuringScan);
    }
    let source_keys = opening_inventory.source_keys()?;
    let inventory = CertifiedSourceInventory::certify(
        opening_inventory.observation,
        closing_inventory.observation,
        INVENTORY_DISCOVERY_REVISION,
        source_keys,
    )?;
    Ok(LingmaSourceBackedScanV0 {
        inventory,
        databases,
    })
}

fn reject_duplicate_paths(inventory: &LingmaSourceInventoryV0) -> LingmaSourceBackedResultV0<()> {
    let mut paths = inventory
        .databases
        .iter()
        .map(|database| database.path.clone())
        .collect::<Vec<_>>();
    paths.sort();
    if paths.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(LingmaSourceBackedErrorV0::DuplicateDatabasePath);
    }
    Ok(())
}

fn scan_database(
    database: &LingmaDatabaseSourceV0,
) -> LingmaSourceBackedResultV0<LingmaDatabaseScanV0> {
    let source = database.source_key()?;
    let root_authority = LingmaRootAuthorizedSource::retain(&database.path)?;
    let sqlite_snapshot = root_authority.open_snapshot()?;
    let opening_evidence = sqlite_snapshot.evidence().clone();
    let connection = sqlite_snapshot.connection()?;
    let encoding = detect_schema(connection)?;
    let user_version = connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(CaptureError::from)?;
    let schema_fingerprint = sqlite_schema_fingerprint(connection)?;
    let opening = source_observation(
        source.clone(),
        &opening_evidence,
        user_version,
        &schema_fingerprint,
        encoding,
    )?;
    let source_revision_digest = source_revision_digest(&opening);
    let revision_scope = TypedKey::bytes(opening.revision().to_vec())?;
    let source_path = database.path.display().to_string();
    let parsed = scan_rows(
        connection,
        encoding,
        &source,
        &source_revision_digest,
        &revision_scope,
        &source_path,
    )?;
    sqlite_snapshot.finish()?;
    root_authority.source_root.revalidate()?;

    let closing_sqlite_snapshot = root_authority.open_snapshot()?;
    let closing_evidence = closing_sqlite_snapshot.evidence().clone();
    let closing_connection = closing_sqlite_snapshot.connection()?;
    let closing_encoding = detect_schema(closing_connection)?;
    let closing_user_version = closing_connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(CaptureError::from)?;
    let closing_schema_fingerprint = sqlite_schema_fingerprint(closing_connection)?;
    closing_sqlite_snapshot.finish()?;
    before_database_certification();
    if !root_authority.evidence_is_current(&closing_evidence)? {
        return Err(LingmaSourceBackedErrorV0::SourceChangedDuringScan);
    }
    let closing = source_observation(
        source,
        &closing_evidence,
        closing_user_version,
        &closing_schema_fingerprint,
        closing_encoding,
    )?;
    let certificate = CertifiedSource::certify(
        opening,
        closing,
        PARSER_REVISION,
        parsed.content_digest,
        ScannedSourceCounts {
            complete_records: parsed.complete_records,
            retained_records: parsed.retained_records,
            rejected_records: parsed.rejected_records,
            ignored_records: parsed.ignored_records,
            indexed_documents: u64::try_from(parsed.records.len())
                .map_err(|_| LingmaSourceBackedErrorV0::CountOverflow)?,
            certified_bytes: parsed.certified_bytes,
        },
    )?;
    Ok(LingmaDatabaseScanV0 {
        path: database.path.clone(),
        certificate,
        records: parsed.records,
    })
}

struct ParsedScan {
    records: Vec<LingmaSourceBackedRecordV0>,
    complete_records: u64,
    retained_records: u64,
    rejected_records: u64,
    ignored_records: u64,
    certified_bytes: u64,
    content_digest: [u8; 32],
}

struct ParsedRow {
    ordinal: u64,
    row: LingmaRow,
    record_digest: [u8; 32],
}

fn scan_rows(
    connection: &rusqlite::Connection,
    encoding: SqliteEncoding,
    source: &SourceKey,
    source_revision_digest: &[u8; 32],
    revision_scope: &TypedKey,
    source_path: &str,
) -> Result<ParsedScan, CaptureError> {
    let mut after_rowid = None;
    let mut physical_ordinal = 0_u64;
    let mut certified_bytes = 0_u64;
    let mut rejected_records = 0_u64;
    let mut ignored_records = 0_u64;
    let mut retained_records = 0_u64;
    let mut hasher = initial_prefix_hasher();
    let mut rows = Vec::new();

    loop {
        let candidates = load_candidates(connection, encoding, after_rowid, None)?;
        if candidates.is_empty() {
            break;
        }
        for candidate in &candidates {
            certified_bytes = certified_bytes
                .checked_add(u64::try_from(candidate.encoded_bytes).map_err(|_| {
                    CaptureError::SystemInvariant("Lingma certified byte count exceeds u64")
                })?)
                .ok_or(CaptureError::SystemInvariant(
                    "Lingma certified byte count exhausted",
                ))?;
            let raw = candidate
                .can_hydrate()
                .then(|| load_raw_row(connection, candidate.rowid))
                .transpose()?;
            hash_candidate(&mut hasher, candidate, raw.as_ref());
            let parsed = if !candidate.required_fields_present() || !candidate.can_hydrate() {
                None
            } else {
                raw.and_then(|raw| super::decode_raw_row(raw, encoding).ok())
            };
            match parsed {
                Some(row) if row.chat_prompt.trim().is_empty() => {
                    ignored_records = ignored_records.saturating_add(1);
                }
                Some(row) => {
                    let logical_records = 1_u64 + u64::from(assistant_text(&row).is_some());
                    retained_records = retained_records.saturating_add(logical_records);
                    let record_digest = lingma_logical_record_sha256(&native_values(&row));
                    rows.push(ParsedRow {
                        ordinal: physical_ordinal,
                        row,
                        record_digest,
                    });
                }
                None => {
                    rejected_records = rejected_records.saturating_add(1);
                }
            }
            physical_ordinal = physical_ordinal.saturating_add(1);
            after_rowid = Some(candidate.rowid);
        }
    }

    let complete_records = retained_records
        .checked_add(rejected_records)
        .and_then(|count| count.checked_add(ignored_records))
        .ok_or(CaptureError::SystemInvariant(
            "Lingma logical record count exhausted",
        ))?;
    let request_counts = request_identity_counts(&rows);
    let mut records = Vec::with_capacity(usize::try_from(retained_records).unwrap_or(usize::MAX));
    for parsed in rows {
        project_row(
            source,
            source_revision_digest,
            revision_scope,
            source_path,
            &request_counts,
            parsed,
            &mut records,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    }
    Ok(ParsedScan {
        records,
        complete_records,
        retained_records,
        rejected_records,
        ignored_records,
        certified_bytes,
        content_digest: hasher.finalize().into(),
    })
}

fn request_identity_counts(rows: &[ParsedRow]) -> BTreeMap<(String, String), usize> {
    let mut counts = BTreeMap::new();
    for parsed in rows {
        let Some(request_id) = parsed
            .row
            .request_id
            .as_ref()
            .filter(|request_id| !request_id.trim().is_empty())
        else {
            continue;
        };
        *counts
            .entry((parsed.row.session_id.clone(), request_id.clone()))
            .or_default() += 1;
    }
    counts
}

fn project_row(
    source: &SourceKey,
    source_revision_digest: &[u8; 32],
    revision_scope: &TypedKey,
    source_path: &str,
    request_counts: &BTreeMap<(String, String), usize>,
    parsed: ParsedRow,
    records: &mut Vec<LingmaSourceBackedRecordV0>,
) -> LingmaSourceBackedResultV0<()> {
    let session_key = NativeSessionKey::native_id(
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(parsed.row.session_id.clone())?,
    )?;
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?;
    let native_identity = native_item_identity(&parsed, request_counts, revision_scope)?;
    let user_sequence = parsed.ordinal.saturating_mul(2);
    let user_event = provider_event(
        &parsed.row,
        EventDraft {
            provider_event_index: event_base_index(&parsed.row),
            role: ctx_history_core::EventRole::User,
            event_type: ctx_history_core::EventType::Message,
            occurred_at: lingma_timestamp(parsed.row.gmt_create, DateTime::<Utc>::UNIX_EPOCH),
            text: parsed.row.chat_prompt.clone(),
            body_kind: USER_PROMPT_COORDINATE,
            fidelity: ctx_history_core::Fidelity::Imported,
        },
        false,
    )?;
    records.push(project_event(
        source,
        session_id,
        &parsed.row,
        &native_identity,
        source_revision_digest,
        parsed.record_digest,
        user_sequence,
        USER_PROMPT_COORDINATE,
        source_path,
        &parsed.row.chat_prompt,
        user_event,
    )?);

    if let Some((text, body_kind, event_type)) = assistant_text(&parsed.row) {
        let logical_text = text.clone();
        let coordinate = if body_kind == "summary" {
            ASSISTANT_SUMMARY_COORDINATE
        } else {
            ASSISTANT_ERROR_COORDINATE
        };
        let occurred_at = lingma_timestamp(parsed.row.gmt_create, DateTime::<Utc>::UNIX_EPOCH)
            .checked_add_signed(Duration::milliseconds(100))
            .unwrap_or_else(|| {
                lingma_timestamp(parsed.row.gmt_create, DateTime::<Utc>::UNIX_EPOCH)
            });
        let assistant_event = provider_event(
            &parsed.row,
            EventDraft {
                provider_event_index: event_base_index(&parsed.row).saturating_add(1),
                role: ctx_history_core::EventRole::Assistant,
                event_type,
                occurred_at,
                text,
                body_kind,
                fidelity: ctx_history_core::Fidelity::SummaryOnly,
            },
            false,
        )?;
        records.push(project_event(
            source,
            session_id,
            &parsed.row,
            &native_identity,
            source_revision_digest,
            parsed.record_digest,
            user_sequence.saturating_add(1),
            coordinate,
            source_path,
            &logical_text,
            assistant_event,
        )?);
    }
    Ok(())
}

struct LingmaNativeItemIdentity {
    item_key: NativeItemKey,
    coordinate: TypedKey,
}

fn native_item_identity(
    parsed: &ParsedRow,
    request_counts: &BTreeMap<(String, String), usize>,
    revision_scope: &TypedKey,
) -> Result<LingmaNativeItemIdentity, ProjectionContractError> {
    if let Some(request_id) = parsed
        .row
        .request_id
        .as_ref()
        .filter(|request_id| !request_id.trim().is_empty())
        .filter(|request_id| {
            request_counts.get(&(parsed.row.session_id.clone(), (*request_id).clone())) == Some(&1)
        })
    {
        let session = TypedKey::utf8(parsed.row.session_id.clone())?;
        let request = TypedKey::utf8(request_id.clone())?;
        return Ok(LingmaNativeItemIdentity {
            item_key: NativeItemKey::composite(
                NATIVE_REQUEST_NAMESPACE,
                vec![session.clone(), request.clone()],
            )?,
            coordinate: TypedKey::composite(vec![TypedKey::utf8("request")?, session, request])?,
        });
    }
    Ok(LingmaNativeItemIdentity {
        item_key: NativeItemKey::revision_scoped_position(
            NATIVE_POSITION_KIND,
            TypedKey::U64(parsed.ordinal),
            revision_scope.clone(),
        )?,
        coordinate: TypedKey::composite(vec![
            TypedKey::utf8("position")?,
            TypedKey::U64(parsed.ordinal),
            revision_scope.clone(),
        ])?,
    })
}

#[allow(clippy::too_many_arguments)]
fn project_event(
    source: &SourceKey,
    session_id: StableEntityId,
    row: &LingmaRow,
    native_identity: &LingmaNativeItemIdentity,
    source_revision_digest: &[u8; 32],
    record_digest: [u8; 32],
    event_sequence: u64,
    coordinate_kind: &'static str,
    source_path: &str,
    logical_text: &str,
    event: super::LingmaCoreEvent,
) -> LingmaSourceBackedResultV0<LingmaSourceBackedRecordV0> {
    let subrecord =
        SubrecordSelector::native_id(NATIVE_SUBRECORD_NAMESPACE, TypedKey::utf8(coordinate_kind)?)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_identity.item_key,
        subrecord_selector: Some(&subrecord),
    })?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: LOGICAL_RELATION.to_owned(),
            primary_key: TypedKey::composite(vec![
                TypedKey::I64(row.rowid),
                TypedKey::utf8(coordinate_kind)?,
                native_identity.coordinate.clone(),
            ])?,
            row_version: Some(TypedKey::bytes(record_digest.to_vec())?),
        },
        LocatorRevisionPolicy::ExactSourceRevision,
        Some(*source_revision_digest),
        Sha256::digest(logical_text.as_bytes()).into(),
    )?;
    if logical_text.is_empty() {
        return Err(LingmaSourceBackedErrorV0::EmptyLexicalBody);
    }
    Ok(LingmaSourceBackedRecordV0 {
        document: LexicalDocument {
            event_id,
            session_id,
            parent_session_id: None,
            root_session_id: session_id,
            source: source.clone(),
            locator,
            provider_session_id: Some(row.session_id.clone()),
            branch: None,
            source_path: Some(source_path.to_owned()),
            agent_type: "primary".to_owned(),
            is_primary: true,
            event_sequence,
            occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
            event_type: event.event_type.as_str().to_owned(),
            role: event.role.map(|role| role.as_str().to_owned()),
            body: logical_text.to_owned(),
            workspace: None,
            cwd: None,
            touched_files: Vec::new(),
        },
    })
}

fn source_observation(
    source: SourceKey,
    evidence: &SqliteSourceEvidence,
    user_version: i64,
    schema_fingerprint: &str,
    encoding: SqliteEncoding,
) -> LingmaSourceBackedResultV0<SourceObservation> {
    let mut revision = Sha256::new();
    revision.update(SOURCE_REVISION_DOMAIN);
    let revision_component = sqlite_evidence_revision_component(evidence);
    hash_revision_field(&mut revision, revision_component.as_bytes());
    revision.update(user_version.to_be_bytes());
    hash_revision_field(&mut revision, schema_fingerprint.as_bytes());
    revision.update([match encoding {
        SqliteEncoding::Utf8 => 1,
        SqliteEncoding::Utf16Le => 2,
        SqliteEncoding::Utf16Be => 3,
    }]);
    Ok(SourceObservation::new(
        source,
        SOURCE_REVISION_KIND,
        revision.finalize().to_vec(),
    )?)
}

fn source_revision_digest(observation: &SourceObservation) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx-source-revision-evidence-v1\0");
    hash_revision_field(&mut digest, observation.revision_kind().as_bytes());
    hash_revision_field(&mut digest, observation.revision());
    digest.finalize().into()
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

fn inventory_revision(sources: &[SourceKey]) -> [u8; 32] {
    let mut revision = Sha256::new();
    revision.update(INVENTORY_REVISION_DOMAIN);
    revision.update((sources.len() as u64).to_be_bytes());
    for source in sources {
        revision.update(source.identity().digest());
        revision.update(source.exact_descriptor_digest());
    }
    revision.finalize().into()
}

fn hash_revision_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LingmaBodyKind {
    UserPrompt,
    AssistantSummary,
    AssistantError,
}

impl LingmaBodyKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::UserPrompt => USER_PROMPT_COORDINATE,
            Self::AssistantSummary => ASSISTANT_SUMMARY_COORDINATE,
            Self::AssistantError => ASSISTANT_ERROR_COORDINATE,
        }
    }

    fn logical_text(self, row: &LingmaRow) -> Result<String, HydrationFailure> {
        match self {
            Self::UserPrompt if !row.chat_prompt.trim().is_empty() => Ok(row.chat_prompt.clone()),
            Self::UserPrompt => Err(hydration_failure(
                HydrationFailureKind::MissingRecord,
                "Lingma user-prompt subrecord has no meaningful text",
            )),
            Self::AssistantSummary | Self::AssistantError => {
                let expected = if self == Self::AssistantSummary {
                    "summary"
                } else {
                    "error_result"
                };
                assistant_text(row)
                    .filter(|(_, body_kind, _)| *body_kind == expected)
                    .map(|(text, _, _)| text)
                    .ok_or_else(|| {
                        hydration_failure(
                            HydrationFailureKind::MissingRecord,
                            "Lingma assistant subrecord is missing",
                        )
                    })
            }
        }
    }
}

#[derive(Debug, Clone)]
enum LingmaNativeIdentityCoordinate {
    Request {
        session_id: String,
        request_id: String,
    },
    Position {
        ordinal: u64,
        revision_scope: TypedKey,
    },
}

impl LingmaNativeIdentityCoordinate {
    fn validate_and_build(
        &self,
        connection: &rusqlite::Connection,
        row: &LingmaRow,
        current_revision_scope: &TypedKey,
    ) -> Result<NativeItemKey, HydrationFailure> {
        match self {
            Self::Request {
                session_id,
                request_id,
            } => {
                if &row.session_id != session_id
                    || row.request_id.as_deref() != Some(request_id.as_str())
                    || request_id.trim().is_empty()
                {
                    return Err(hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "Lingma request-native key does not match the reopened row",
                    ));
                }
                let matching_rows: i64 = connection
                    .query_row(
                        "select count(*)
                           from chat_record
                          where cast(session_id as text) = ?1
                            and cast(request_id as text) = ?2",
                        rusqlite::params![session_id, request_id],
                        |result| result.get(0),
                    )
                    .map_err(CaptureError::from)
                    .map_err(map_capture_hydration)?;
                if matching_rows != 1 {
                    return Err(hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "Lingma request-native key is not unique in the reopened source",
                    ));
                }
                NativeItemKey::composite(
                    NATIVE_REQUEST_NAMESPACE,
                    vec![
                        TypedKey::utf8(session_id.clone()).map_err(invalid_locator)?,
                        TypedKey::utf8(request_id.clone()).map_err(invalid_locator)?,
                    ],
                )
                .map_err(invalid_locator)
            }
            Self::Position {
                ordinal,
                revision_scope,
            } => {
                if revision_scope != current_revision_scope {
                    return Err(hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "Lingma position-native key has the wrong source revision scope",
                    ));
                }
                let observed_ordinal: i64 = connection
                    .query_row(
                        "select count(*) from chat_record where rowid < ?1",
                        [row.rowid],
                        |result| result.get(0),
                    )
                    .map_err(CaptureError::from)
                    .map_err(map_capture_hydration)?;
                if u64::try_from(observed_ordinal).ok() != Some(*ordinal) {
                    return Err(hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "Lingma position-native key does not match the row ordinal",
                    ));
                }
                NativeItemKey::revision_scoped_position(
                    NATIVE_POSITION_KIND,
                    TypedKey::U64(*ordinal),
                    revision_scope.clone(),
                )
                .map_err(invalid_locator)
            }
        }
    }
}

#[derive(Debug, Clone)]
struct LingmaCoordinate {
    rowid: i64,
    body_kind: LingmaBodyKind,
    row_digest: [u8; 32],
    native_identity: LingmaNativeIdentityCoordinate,
}

fn decode_lingma_locator(
    locator: &SourceRecordLocator,
) -> Result<LingmaCoordinate, HydrationFailure> {
    if locator.revision_policy() != LocatorRevisionPolicy::ExactSourceRevision {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator is not exact-revision scoped",
        ));
    }
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator is not a provider SQLite coordinate",
        ));
    };
    if logical_relation != LOGICAL_RELATION {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator has an unsupported logical relation",
        ));
    }
    let Some(TypedKey::Bytes(row_digest)) = row_version else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator has no typed row version",
        ));
    };
    let row_digest: [u8; 32] = row_digest.as_slice().try_into().map_err(|_| {
        hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator row version has an invalid length",
        )
    })?;
    let TypedKey::Composite(parts) = primary_key else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator primary key is not composite",
        ));
    };
    let [TypedKey::I64(rowid), TypedKey::Utf8(body_kind), TypedKey::Composite(native)] =
        parts.as_slice()
    else {
        return Err(hydration_failure(
            HydrationFailureKind::InvalidLocator,
            "Lingma locator primary key has an invalid shape",
        ));
    };
    let body_kind = match body_kind.as_str() {
        USER_PROMPT_COORDINATE => LingmaBodyKind::UserPrompt,
        ASSISTANT_SUMMARY_COORDINATE => LingmaBodyKind::AssistantSummary,
        ASSISTANT_ERROR_COORDINATE => LingmaBodyKind::AssistantError,
        _ => {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Lingma locator addresses an unsupported logical body",
            ));
        }
    };
    let native_identity = match native.as_slice() {
        [TypedKey::Utf8(kind), TypedKey::Utf8(session_id), TypedKey::Utf8(request_id)]
            if kind == "request" =>
        {
            LingmaNativeIdentityCoordinate::Request {
                session_id: session_id.clone(),
                request_id: request_id.clone(),
            }
        }
        [TypedKey::Utf8(kind), TypedKey::U64(ordinal), revision_scope] if kind == "position" => {
            LingmaNativeIdentityCoordinate::Position {
                ordinal: *ordinal,
                revision_scope: revision_scope.clone(),
            }
        }
        _ => {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Lingma locator native key has an invalid shape",
            ));
        }
    };
    Ok(LingmaCoordinate {
        rowid: *rowid,
        body_kind,
        row_digest,
        native_identity,
    })
}

fn verify_record_digest(
    locator: &SourceRecordLocator,
    provider_bytes: &[u8],
) -> Result<(), HydrationFailure> {
    let digest: [u8; 32] = Sha256::digest(provider_bytes).into();
    if &digest == locator.record_digest() {
        Ok(())
    } else {
        Err(hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Lingma logical text digest no longer matches",
        ))
    }
}

fn invalid_locator(error: impl std::fmt::Display) -> HydrationFailure {
    hydration_failure(HydrationFailureKind::InvalidLocator, error.to_string())
}

fn hydration_failure(kind: HydrationFailureKind, detail: impl Into<String>) -> HydrationFailure {
    HydrationFailure {
        kind,
        detail: detail.into(),
    }
}

fn map_capture_hydration(error: CaptureError) -> HydrationFailure {
    match error {
        CaptureError::SourceChangedDuringCapture => hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "Lingma source changed during reopening",
        ),
        CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => hydration_failure(
            HydrationFailureKind::MissingRecord,
            "Lingma source record is missing",
        ),
        CaptureError::UnsupportedSchema(_) | CaptureError::UnsupportedSchemaVersion(_) => {
            hydration_failure(
                HydrationFailureKind::UnsupportedParserRevision,
                "Lingma SQLite schema is unsupported",
            )
        }
        CaptureError::InvalidPayload(_)
        | CaptureError::Json(_)
        | CaptureError::Sqlite(
            rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
            | rusqlite::Error::InvalidColumnType(..),
        ) => hydration_failure(
            HydrationFailureKind::StaleRecordEvidence,
            "Lingma SQLite row is malformed for the certified parser",
        ),
        _ => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            "Lingma source could not be reopened",
        ),
    }
}

fn map_parser_hydration(error: CaptureError) -> HydrationFailure {
    match error {
        CaptureError::InvalidPayload(_)
        | CaptureError::UnsupportedSchema(_)
        | CaptureError::UnsupportedSchemaVersion(_) => hydration_failure(
            HydrationFailureKind::UnsupportedParserRevision,
            "Lingma SQLite schema is unsupported",
        ),
        error => map_capture_hydration(error),
    }
}

fn map_sqlite_hydration(error: SqliteSourceAccessError) -> HydrationFailure {
    match error {
        SqliteSourceAccessError::SourceChanged
        | SqliteSourceAccessError::ConnectionIdentityMismatch => hydration_failure(
            HydrationFailureKind::StaleSourceEvidence,
            "Lingma SQLite source changed during reopening",
        ),
        SqliteSourceAccessError::Io { source, .. }
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            hydration_failure(
                HydrationFailureKind::ConfirmedDeleted,
                "Lingma database leaf is absent beneath the admitted source root",
            )
        }
        error => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            error.to_string(),
        ),
    }
}

fn map_lingma_source_hydration(error: LingmaSourceBackedErrorV0) -> HydrationFailure {
    match error {
        LingmaSourceBackedErrorV0::Capture(error) => map_capture_hydration(error),
        LingmaSourceBackedErrorV0::SqliteSource(error) => map_sqlite_hydration(error),
        LingmaSourceBackedErrorV0::Projection(error) => invalid_locator(error),
        LingmaSourceBackedErrorV0::Resolver(error) => invalid_locator(error),
        error => hydration_failure(
            HydrationFailureKind::TemporarilyUnavailable,
            error.to_string(),
        ),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LingmaSourceBackedResolverV0 {
    sources: BTreeMap<[u8; 32], (SourceKey, PathBuf)>,
}

impl LingmaSourceBackedResolverV0 {
    pub(crate) fn new(inventory: &LingmaSourceInventoryV0) -> LingmaSourceBackedResultV0<Self> {
        let mut sources = BTreeMap::new();
        for database in &inventory.databases {
            let source = database.source_key()?;
            sources.insert(source.identity().digest(), (source, database.path.clone()));
        }
        Ok(Self { sources })
    }

    pub(crate) fn hydrate_record(
        &self,
        record: &LingmaSourceBackedRecordV0,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        let request =
            EventHydrationRequest::new(record.document.event_id, record.document.locator.clone())
                .map_err(invalid_locator)?;
        self.hydrate_event(&request)
    }

    pub(crate) fn hydrate_requests(
        &self,
        requests: &[EventHydrationRequest],
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let coordinates = requests
            .iter()
            .map(|request| decode_lingma_locator(request.locator()))
            .collect::<Result<Vec<_>, _>>()?;
        let source_key = requests[0].locator().source();
        if requests
            .iter()
            .any(|request| !request.locator().source().exact_descriptor_eq(source_key))
        {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Lingma hydration batch spans multiple source descriptors",
            ));
        }
        let (source, path) = self
            .sources
            .get(&source_key.identity().digest())
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::ConfirmedDeleted,
                    "Lingma source is absent from the complete admitted inventory",
                )
            })?;
        if !source.exact_descriptor_eq(source_key) {
            return Err(hydration_failure(
                HydrationFailureKind::InvalidLocator,
                "Lingma source descriptor does not match the admitted source identity",
            ));
        }

        let root_authority =
            LingmaRootAuthorizedSource::retain(path).map_err(map_lingma_source_hydration)?;
        let sqlite_snapshot = root_authority
            .open_snapshot()
            .map_err(map_lingma_source_hydration)?;
        let hydration = (|| {
            let connection = sqlite_snapshot.connection().map_err(map_sqlite_hydration)?;
            let encoding = detect_schema(connection).map_err(map_parser_hydration)?;
            let user_version = connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .map_err(CaptureError::from)
                .map_err(map_capture_hydration)?;
            let schema_fingerprint =
                sqlite_schema_fingerprint(connection).map_err(map_capture_hydration)?;
            let observation = source_observation(
                source.clone(),
                sqlite_snapshot.evidence(),
                user_version,
                &schema_fingerprint,
                encoding,
            )
            .map_err(invalid_locator)?;
            let current_revision_digest = source_revision_digest(&observation);
            if requests.iter().any(|request| {
                request.locator().certified_source_revision_digest()
                    != Some(&current_revision_digest)
            }) {
                return Err(hydration_failure(
                    HydrationFailureKind::StaleSourceEvidence,
                    "Lingma SQLite snapshot no longer matches the certified revision",
                ));
            }

            let current_revision_scope =
                TypedKey::bytes(observation.revision().to_vec()).map_err(invalid_locator)?;
            let mut values_by_row = BTreeMap::new();
            let mut hydrated = Vec::with_capacity(requests.len());
            for (request, coordinate) in requests.iter().zip(coordinates) {
                if !values_by_row.contains_key(&coordinate.rowid) {
                    let values =
                        super::records::lingma_complete_values(connection, coordinate.rowid)
                            .map_err(map_capture_hydration)?
                            .ok_or_else(|| {
                                hydration_failure(
                                    HydrationFailureKind::MissingRecord,
                                    "Lingma chat_record row is missing",
                                )
                            })?;
                    values_by_row.insert(coordinate.rowid, values);
                }
                let values = values_by_row.get(&coordinate.rowid).ok_or_else(|| {
                    hydration_failure(
                        HydrationFailureKind::MissingRecord,
                        "Lingma chat_record row is missing",
                    )
                })?;
                if lingma_logical_record_sha256(values) != coordinate.row_digest {
                    return Err(hydration_failure(
                        HydrationFailureKind::StaleRecordEvidence,
                        "Lingma logical row version no longer matches",
                    ));
                }
                let row = row_from_native_values(values).map_err(map_capture_hydration)?;
                let native_item_key = coordinate.native_identity.validate_and_build(
                    connection,
                    &row,
                    &current_revision_scope,
                )?;
                let session_key = NativeSessionKey::native_id(
                    NATIVE_SESSION_NAMESPACE,
                    TypedKey::utf8(row.session_id.clone()).map_err(invalid_locator)?,
                )
                .map_err(invalid_locator)?;
                let session_id = derive_session_id(SessionIdentityInput {
                    source,
                    logical_session_kind: LOGICAL_SESSION_KIND,
                    native_session_key: &session_key,
                })
                .map_err(invalid_locator)?;
                let body_kind = coordinate.body_kind.as_str();
                let subrecord = SubrecordSelector::native_id(
                    NATIVE_SUBRECORD_NAMESPACE,
                    TypedKey::utf8(body_kind).map_err(invalid_locator)?,
                )
                .map_err(invalid_locator)?;
                let expected_event_id = derive_event_id(EventIdentityInput {
                    source,
                    session_id,
                    logical_item_kind: LOGICAL_EVENT_KIND,
                    native_item_key: &native_item_key,
                    subrecord_selector: Some(&subrecord),
                })
                .map_err(invalid_locator)?;
                if expected_event_id != request.event_id() {
                    return Err(hydration_failure(
                        HydrationFailureKind::InvalidLocator,
                        "Lingma native key does not derive the requested event identity",
                    ));
                }
                let text = coordinate.body_kind.logical_text(&row)?;
                verify_record_digest(request.locator(), text.as_bytes())?;
                hydrated.push(HydratedProviderRecord {
                    event_id: request.event_id(),
                    provider_bytes: text.into_bytes(),
                });
            }
            Ok(hydrated)
        })();
        let finished = sqlite_snapshot.finish().map_err(map_sqlite_hydration);
        let root_current = root_authority
            .source_root
            .revalidate()
            .map_err(map_capture_hydration);
        finished?;
        root_current?;
        hydration
    }

    pub(crate) fn hydrate_batch_request(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let result = BatchHydrationResult::new(self.hydrate_requests(request.events())?).map_err(
            |error| {
                hydration_failure(
                    HydrationFailureKind::InvalidLocator,
                    format!("invalid Lingma batch hydration result: {error}"),
                )
            },
        )?;
        result.validate_for_request(request)?;
        Ok(result)
    }
}

impl ContentSourceResolver for LingmaSourceBackedResolverV0 {
    fn hydrate_event(
        &self,
        request: &EventHydrationRequest,
    ) -> Result<HydratedProviderRecord, HydrationFailure> {
        self.hydrate_requests(std::slice::from_ref(request))?
            .into_iter()
            .next()
            .ok_or_else(|| {
                hydration_failure(
                    HydrationFailureKind::MissingRecord,
                    "Lingma hydration returned no record",
                )
            })
    }

    fn hydrate_batch(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        self.hydrate_batch_request(request)
    }

    fn hydrate_session(
        &self,
        request: &SessionHydrationRequest,
    ) -> Result<Vec<HydratedProviderRecord>, HydrationFailure> {
        self.hydrate_requests(request.events())
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::{config::DbConfig, Connection};

    use super::*;

    fn create_database(path: &Path) -> Connection {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "create table chat_record (
                    session_id text not null,
                    request_id text,
                    chat_prompt text,
                    summary text,
                    error_result text,
                    gmt_create integer,
                    extra text
                 );",
            )
            .unwrap();
        connection
    }

    fn insert_row(
        connection: &Connection,
        session_id: &str,
        request_id: &str,
        prompt: &str,
        summary: Option<&str>,
    ) {
        connection
            .execute(
                "insert into chat_record (
                    session_id, request_id, chat_prompt, summary, error_result, gmt_create, extra
                 ) values (?1, ?2, ?3, ?4, null, 1700000000, null)",
                rusqlite::params![session_id, request_id, prompt, summary],
            )
            .unwrap();
    }

    fn database(path: &Path, lineage: &str) -> LingmaDatabaseSourceV0 {
        LingmaDatabaseSourceV0::new(path, TypedKey::utf8(lineage).unwrap()).unwrap()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stock_sqlite_snapshot_finish_rejects_leaf_swap_after_open() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("local.db");
        let attacker = temp.path().join("attacker.db");
        let original = temp.path().join("original.db");
        drop(create_database(&path));
        drop(create_database(&attacker));

        let authority = LingmaRootAuthorizedSource::retain(&path).unwrap();
        let snapshot = authority.open_snapshot().unwrap();
        std::fs::rename(&path, &original).unwrap();
        std::fs::rename(&attacker, &path).unwrap();
        assert!(snapshot.finish().is_err());
    }

    fn inventory(databases: Vec<LingmaDatabaseSourceV0>) -> LingmaSourceInventoryV0 {
        LingmaSourceInventoryV0::new(TypedKey::utf8("test-installed-clients").unwrap(), databases)
            .unwrap()
    }

    fn all_records(scan: &LingmaSourceBackedScanV0) -> Vec<&LingmaSourceBackedRecordV0> {
        scan.databases
            .iter()
            .flat_map(|database| database.records.iter())
            .collect()
    }

    fn event_request(record: &LingmaSourceBackedRecordV0) -> EventHydrationRequest {
        EventHydrationRequest::new(record.document.event_id, record.document.locator.clone())
            .unwrap()
    }

    fn current_source_revision(source: &LingmaDatabaseSourceV0) -> [u8; 32] {
        let authority = LingmaRootAuthorizedSource::retain(&source.path).unwrap();
        let snapshot = authority.open_snapshot().unwrap();
        let digest = {
            let connection = snapshot.connection().unwrap();
            let encoding = detect_schema(connection).unwrap();
            let user_version = connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap();
            let schema_fingerprint = sqlite_schema_fingerprint(connection).unwrap();
            let observation = source_observation(
                source.source_key().unwrap(),
                snapshot.evidence(),
                user_version,
                &schema_fingerprint,
                encoding,
            )
            .unwrap();
            source_revision_digest(&observation)
        };
        snapshot.finish().unwrap();
        authority.source_root.revalidate().unwrap();
        digest
    }

    fn request_with_locator_evidence(
        record: &LingmaSourceBackedRecordV0,
        source_revision: [u8; 32],
        coordinate: NativeRecordCoordinate,
        record_digest: [u8; 32],
    ) -> EventHydrationRequest {
        let locator = SourceRecordLocator::new(
            record.document.source.clone(),
            coordinate,
            LocatorRevisionPolicy::ExactSourceRevision,
            Some(source_revision),
            record_digest,
        )
        .unwrap();
        EventHydrationRequest::new(record.document.event_id, locator).unwrap()
    }

    #[test]
    fn source_backed_cold_scan_certifies_stable_full_meaningful_bodies() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let first_path = temp.path().join("vscode-local.db");
        let second_path = temp.path().join("jetbrains-local.db");
        let long_prompt = format!("vscode prompt {} lingma-full-body-tail", "v".repeat(4_096));
        let first = create_database(&first_path);
        insert_row(
            &first,
            "vscode-session",
            "vscode-request",
            &long_prompt,
            Some("vscode summary"),
        );
        drop(first);
        let second = create_database(&second_path);
        insert_row(
            &second,
            "jetbrains-session",
            "jetbrains-request",
            "jetbrains prompt",
            Some("jetbrains summary"),
        );
        drop(second);

        let opening = inventory(vec![
            database(&first_path, "vscode:stable:default"),
            database(&second_path, "jetbrains:idea:2026.2"),
        ]);
        let closing = opening.clone();
        let scan = scan_lingma_source_backed_v0(opening, || Ok(closing.clone())).unwrap();
        assert_eq!(scan.inventory.observed_sources(), 2);
        assert_eq!(scan.databases.len(), 2);
        assert_eq!(all_records(&scan).len(), 4);
        let long_user = all_records(&scan)
            .into_iter()
            .find(|record| {
                record.document.provider_session_id.as_deref() == Some("vscode-session")
                    && record.document.role.as_deref() == Some("user")
            })
            .unwrap();
        assert_eq!(long_user.document.body, long_prompt);
        assert!(long_user.document.body.ends_with("lingma-full-body-tail"));
        assert!(all_records(&scan).iter().all(|record| {
            record.document.parent_session_id.is_none()
                && record.document.root_session_id == record.document.session_id
                && record.document.provider_session_id.is_some()
                && record.document.branch.is_none()
                && record.document.source_path.is_some()
                && record.document.agent_type == "primary"
                && record.document.is_primary
        }));
        assert!(scan.databases.iter().all(|database| {
            database.certificate.counts().indexed_documents == 2
                && database.certificate.counts().certified_bytes != 0
        }));

        let reversed = inventory(vec![
            database(&second_path, "jetbrains:idea:2026.2"),
            database(&first_path, "vscode:stable:default"),
        ]);
        let replay = scan_lingma_source_backed_v0(reversed.clone(), || Ok(reversed)).unwrap();
        let mut first_ids = all_records(&scan)
            .into_iter()
            .map(|record| record.document.event_id.digest())
            .collect::<Vec<_>>();
        let mut replay_ids = all_records(&replay)
            .into_iter()
            .map(|record| record.document.event_id.digest())
            .collect::<Vec<_>>();
        first_ids.sort();
        replay_ids.sort();
        assert_eq!(first_ids, replay_ids);
    }

    #[test]
    fn stock_sqlite_snapshot_scan_sees_committed_content_retained_in_active_wal() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("local.db");
        let writer = create_database(&path);
        insert_row(
            &writer,
            "wal-session",
            "wal-request",
            "main database prompt",
            None,
        );
        let mode: String = writer
            .query_row("pragma journal_mode = wal", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer
            .execute_batch("pragma wal_autocheckpoint = 0")
            .unwrap();
        writer
            .execute(
                "update chat_record
                    set chat_prompt = 'committed Lingma WAL prompt'
                  where request_id = 'wal-request'",
                [],
            )
            .unwrap();
        writer
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        drop(writer);
        assert!(path.with_file_name("local.db-wal").exists());
        assert!(path.with_file_name("local.db-shm").exists());

        let opening = inventory(vec![database(&path, "vscode:stable:wal")]);
        let admitted = opening.clone();
        let closing = opening.clone();
        let scan = scan_lingma_source_backed_v0(opening, || Ok(closing)).unwrap();
        let user = all_records(&scan)
            .into_iter()
            .find(|record| record.document.role.as_deref() == Some("user"))
            .unwrap();
        assert_eq!(user.document.body, "committed Lingma WAL prompt");
        let hydrated = LingmaSourceBackedResolverV0::new(&admitted)
            .unwrap()
            .hydrate_record(user)
            .unwrap();
        assert_eq!(hydrated.provider_bytes, b"committed Lingma WAL prompt");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stock_sqlite_snapshot_finish_precedes_source_certification() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("local.db");
        let replacement = temp.path().join("replacement.db");
        let opening = create_database(&path);
        insert_row(&opening, "session", "request", "opening body", None);
        drop(opening);
        let replacement_connection = create_database(&replacement);
        insert_row(
            &replacement_connection,
            "session",
            "request",
            "replacement body",
            None,
        );
        drop(replacement_connection);
        let inventory = inventory(vec![database(&path, "vscode:stable:finish-order")]);
        let closing = inventory.clone();
        let replaced_path = path.clone();
        set_before_database_certification(Some(Box::new(move || {
            std::fs::rename(&replacement, &replaced_path).unwrap();
        })));

        let result = scan_lingma_source_backed_v0(inventory, || Ok(closing));
        assert!(matches!(
            result,
            Err(LingmaSourceBackedErrorV0::SourceChangedDuringScan
                | LingmaSourceBackedErrorV0::Capture(_))
        ));
    }

    #[test]
    fn source_backed_exact_hydration_and_native_batch_preserve_order_and_full_text() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("local.db");
        let prompt = format!(
            "exact row-local Lingma prompt {} lingma-user-tail",
            "x".repeat(4_096)
        );
        let summary = format!(
            "exact Lingma assistant summary {} lingma-summary-tail",
            "s".repeat(4_096)
        );
        let connection = create_database(&path);
        insert_row(
            &connection,
            "exact-session",
            "exact-request",
            &prompt,
            Some(&summary),
        );
        insert_row(
            &connection,
            "error-session",
            "error-request",
            "error prompt",
            None,
        );
        connection
            .execute(
                "update chat_record
                    set error_result = ?1
                  where request_id = 'error-request'",
                [format!(
                    "provider failure {} lingma-error-tail",
                    "e".repeat(4_096)
                )],
            )
            .unwrap();
        drop(connection);
        let inventory = inventory(vec![database(&path, "vscode:profile:exact")]);
        let closing = inventory.clone();
        let scan = scan_lingma_source_backed_v0(inventory.clone(), || Ok(closing)).unwrap();
        let records = all_records(&scan);
        let user = records
            .iter()
            .copied()
            .find(|record| record.document.body.ends_with("lingma-user-tail"))
            .unwrap();
        let assistant = records
            .iter()
            .copied()
            .find(|record| record.document.body.ends_with("lingma-summary-tail"))
            .unwrap();
        let error = records
            .iter()
            .copied()
            .find(|record| record.document.body.ends_with("lingma-error-tail"))
            .unwrap();
        assert_eq!(user.document.body, prompt);
        assert_eq!(assistant.document.body, summary);
        assert!(error.document.body.starts_with("Lingma error result: "));
        assert!(matches!(
            user.document.locator.coordinate(),
            NativeRecordCoordinate::ProviderSqlite {
                logical_relation,
                primary_key: TypedKey::Composite(parts),
                row_version: Some(TypedKey::Bytes(version)),
            } if logical_relation == LOGICAL_RELATION
                && matches!(
                    parts.as_slice(),
                    [
                        TypedKey::I64(1),
                        TypedKey::Utf8(kind),
                        TypedKey::Composite(_)
                    ]
                        if kind == USER_PROMPT_COORDINATE
                )
                && version.len() == 32
        ));
        assert_eq!(
            user.document.locator.revision_policy(),
            LocatorRevisionPolicy::ExactSourceRevision
        );
        assert!(user
            .document
            .locator
            .certified_source_revision_digest()
            .is_some());

        let resolver = LingmaSourceBackedResolverV0::new(&inventory).unwrap();
        assert_eq!(
            resolver.hydrate_record(user).unwrap().provider_bytes,
            prompt.as_bytes()
        );
        assert_eq!(
            resolver.hydrate_record(assistant).unwrap().provider_bytes,
            summary.as_bytes()
        );
        assert!(resolver
            .hydrate_record(error)
            .unwrap()
            .provider_bytes
            .ends_with(b"lingma-error-tail"));

        let requested = vec![
            event_request(error),
            event_request(user),
            event_request(assistant),
        ];
        let batch = BatchHydrationRequest::new(requested.clone()).unwrap();
        let hydrated = resolver.hydrate_batch_request(&batch).unwrap();
        assert_eq!(
            hydrated
                .records()
                .iter()
                .map(|record| record.event_id)
                .collect::<Vec<_>>(),
            requested
                .iter()
                .map(EventHydrationRequest::event_id)
                .collect::<Vec<_>>()
        );
        assert!(hydrated.records()[0]
            .provider_bytes
            .ends_with(b"lingma-error-tail"));
        assert_eq!(hydrated.records()[1].provider_bytes, prompt.as_bytes());
        assert_eq!(hydrated.records()[2].provider_bytes, summary.as_bytes());
    }

    #[test]
    fn source_backed_hydration_types_stale_source_and_record_digest() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let stale_path = temp.path().join("stale.db");
        let connection = create_database(&stale_path);
        insert_row(
            &connection,
            "stale-session",
            "stale-request",
            "original prompt",
            None,
        );
        drop(connection);
        let stale_inventory = inventory(vec![database(&stale_path, "jetbrains:idea:stale-source")]);
        let scan =
            scan_lingma_source_backed_v0(stale_inventory.clone(), || Ok(stale_inventory.clone()))
                .unwrap();
        let stale_record = all_records(&scan)
            .into_iter()
            .find(|record| record.document.role.as_deref() == Some("user"))
            .unwrap();
        Connection::open(&stale_path)
            .unwrap()
            .execute(
                "update chat_record set chat_prompt = 'rewritten prompt'",
                [],
            )
            .unwrap();
        let failure = LingmaSourceBackedResolverV0::new(&stale_inventory)
            .unwrap()
            .hydrate_record(stale_record)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::StaleSourceEvidence);

        let digest_path = temp.path().join("digest.db");
        let connection = create_database(&digest_path);
        insert_row(
            &connection,
            "digest-session",
            "digest-request",
            "digest prompt",
            None,
        );
        drop(connection);
        let digest_inventory = inventory(vec![database(&digest_path, "vscode:stable:bad-digest")]);
        let scan =
            scan_lingma_source_backed_v0(digest_inventory.clone(), || Ok(digest_inventory.clone()))
                .unwrap();
        let digest_record = all_records(&scan)
            .into_iter()
            .find(|record| record.document.role.as_deref() == Some("user"))
            .unwrap();
        let NativeRecordCoordinate::ProviderSqlite {
            logical_relation,
            primary_key,
            ..
        } = digest_record.document.locator.coordinate()
        else {
            panic!("expected provider SQLite locator");
        };
        let coordinate = NativeRecordCoordinate::ProviderSqlite {
            logical_relation: logical_relation.clone(),
            primary_key: primary_key.clone(),
            row_version: Some(TypedKey::bytes(vec![0x5a; 32]).unwrap()),
        };
        let request = request_with_locator_evidence(
            digest_record,
            *digest_record
                .document
                .locator
                .certified_source_revision_digest()
                .unwrap(),
            coordinate,
            *digest_record.document.locator.record_digest(),
        );
        let failure = LingmaSourceBackedResolverV0::new(&digest_inventory)
            .unwrap()
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);
        let request = request_with_locator_evidence(
            digest_record,
            *digest_record
                .document
                .locator
                .certified_source_revision_digest()
                .unwrap(),
            digest_record.document.locator.coordinate().clone(),
            [0xa5; 32],
        );
        let failure = LingmaSourceBackedResolverV0::new(&digest_inventory)
            .unwrap()
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);

        let native_path = temp.path().join("native-key.db");
        let connection = create_database(&native_path);
        insert_row(
            &connection,
            "native-session",
            "native-request",
            "native prompt",
            None,
        );
        drop(connection);
        let native_source = database(&native_path, "vscode:stable:native-key");
        let native_inventory = inventory(vec![native_source.clone()]);
        let scan =
            scan_lingma_source_backed_v0(native_inventory.clone(), || Ok(native_inventory.clone()))
                .unwrap();
        let native_record = all_records(&scan)[0];
        let connection = Connection::open(&native_path).unwrap();
        insert_row(
            &connection,
            "native-session",
            "native-request",
            "duplicate native prompt",
            None,
        );
        drop(connection);
        let request = request_with_locator_evidence(
            native_record,
            current_source_revision(&native_source),
            native_record.document.locator.coordinate().clone(),
            *native_record.document.locator.record_digest(),
        );
        let failure = LingmaSourceBackedResolverV0::new(&native_inventory)
            .unwrap()
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::InvalidLocator);
    }

    #[test]
    fn source_backed_hydration_distinguishes_missing_row_deletion_and_unavailable_root() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let missing_path = temp.path().join("missing-row.db");
        let connection = create_database(&missing_path);
        insert_row(
            &connection,
            "missing-session",
            "missing-request",
            "missing prompt",
            None,
        );
        drop(connection);
        let missing_source = database(&missing_path, "vscode:stable:missing-row");
        let missing_inventory = inventory(vec![missing_source.clone()]);
        let scan = scan_lingma_source_backed_v0(missing_inventory.clone(), || {
            Ok(missing_inventory.clone())
        })
        .unwrap();
        let record = all_records(&scan)
            .into_iter()
            .find(|record| record.document.role.as_deref() == Some("user"))
            .unwrap();
        Connection::open(&missing_path)
            .unwrap()
            .execute("delete from chat_record", [])
            .unwrap();
        let request = request_with_locator_evidence(
            record,
            current_source_revision(&missing_source),
            record.document.locator.coordinate().clone(),
            *record.document.locator.record_digest(),
        );
        let failure = LingmaSourceBackedResolverV0::new(&missing_inventory)
            .unwrap()
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::MissingRecord);

        let deleted_path = temp.path().join("deleted.db");
        let connection = create_database(&deleted_path);
        insert_row(
            &connection,
            "deleted-session",
            "deleted-request",
            "deleted prompt",
            None,
        );
        drop(connection);
        let deleted_inventory = inventory(vec![database(&deleted_path, "vscode:stable:deleted")]);
        let scan = scan_lingma_source_backed_v0(deleted_inventory.clone(), || {
            Ok(deleted_inventory.clone())
        })
        .unwrap();
        let request = event_request(all_records(&scan)[0]);
        std::fs::remove_file(&deleted_path).unwrap();
        let failure = LingmaSourceBackedResolverV0::new(&deleted_inventory)
            .unwrap()
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::ConfirmedDeleted);

        let available_root = temp.path().join("available-root");
        std::fs::create_dir(&available_root).unwrap();
        let unavailable_path = available_root.join("local.db");
        let connection = create_database(&unavailable_path);
        insert_row(
            &connection,
            "offline-session",
            "offline-request",
            "offline prompt",
            None,
        );
        drop(connection);
        let unavailable_inventory = inventory(vec![database(
            &unavailable_path,
            "jetbrains:idea:unavailable-root",
        )]);
        let scan = scan_lingma_source_backed_v0(unavailable_inventory.clone(), || {
            Ok(unavailable_inventory.clone())
        })
        .unwrap();
        let request = event_request(all_records(&scan)[0]);
        std::fs::rename(&available_root, temp.path().join("offline-root")).unwrap();
        let failure = LingmaSourceBackedResolverV0::new(&unavailable_inventory)
            .unwrap()
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::TemporarilyUnavailable);
    }

    #[test]
    fn source_backed_hydration_types_malformed_row_and_unsupported_schema() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let malformed_path = temp.path().join("malformed.db");
        let connection = create_database(&malformed_path);
        insert_row(
            &connection,
            "malformed-session",
            "malformed-request",
            "valid prompt",
            None,
        );
        drop(connection);
        let malformed_source = database(&malformed_path, "vscode:stable:malformed");
        let malformed_inventory = inventory(vec![malformed_source.clone()]);
        let scan = scan_lingma_source_backed_v0(malformed_inventory.clone(), || {
            Ok(malformed_inventory.clone())
        })
        .unwrap();
        let record = all_records(&scan)[0];
        Connection::open(&malformed_path)
            .unwrap()
            .execute(
                "update chat_record set chat_prompt = cast(x'80' as text)",
                [],
            )
            .unwrap();
        let request = request_with_locator_evidence(
            record,
            current_source_revision(&malformed_source),
            record.document.locator.coordinate().clone(),
            *record.document.locator.record_digest(),
        );
        let failure = LingmaSourceBackedResolverV0::new(&malformed_inventory)
            .unwrap()
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::StaleRecordEvidence);

        let unsupported_path = temp.path().join("unsupported.db");
        let connection = create_database(&unsupported_path);
        insert_row(
            &connection,
            "unsupported-session",
            "unsupported-request",
            "valid prompt",
            None,
        );
        drop(connection);
        let unsupported_inventory = inventory(vec![database(
            &unsupported_path,
            "jetbrains:idea:unsupported",
        )]);
        let scan = scan_lingma_source_backed_v0(unsupported_inventory.clone(), || {
            Ok(unsupported_inventory.clone())
        })
        .unwrap();
        let request = event_request(all_records(&scan)[0]);
        Connection::open(&unsupported_path)
            .unwrap()
            .execute_batch("drop table chat_record;")
            .unwrap();
        let failure = LingmaSourceBackedResolverV0::new(&unsupported_inventory)
            .unwrap()
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(
            failure.kind,
            HydrationFailureKind::UnsupportedParserRevision
        );
    }

    #[test]
    fn source_backed_hydration_rejects_malformed_coordinate_and_forbidden_fallbacks() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("local.db");
        let connection = create_database(&path);
        insert_row(
            &connection,
            "invalid-session",
            "invalid-request",
            "invalid prompt",
            None,
        );
        drop(connection);
        let inventory = inventory(vec![database(&path, "vscode:stable:invalid")]);
        let scan =
            scan_lingma_source_backed_v0(inventory.clone(), || Ok(inventory.clone())).unwrap();
        let record = all_records(&scan)[0];
        let malformed_coordinate = NativeRecordCoordinate::ProviderSqlite {
            logical_relation: LOGICAL_RELATION.to_owned(),
            primary_key: TypedKey::I64(1),
            row_version: Some(TypedKey::bytes(vec![0; 32]).unwrap()),
        };
        let request = request_with_locator_evidence(
            record,
            *record
                .document
                .locator
                .certified_source_revision_digest()
                .unwrap(),
            malformed_coordinate,
            *record.document.locator.record_digest(),
        );
        let failure = LingmaSourceBackedResolverV0::new(&inventory)
            .unwrap()
            .hydrate_event(&request)
            .unwrap_err();
        assert_eq!(failure.kind, HydrationFailureKind::InvalidLocator);

        let provider_source = include_str!("source_backed.rs");
        for forbidden in [
            ["work", ".sqlite"].concat(),
            ["ctx_history_", "store"].concat(),
            ["MAX_BODY_", "PREVIEW_CHARS"].concat(),
            ["provider_local_", "preview"].concat(),
        ] {
            assert!(
                !provider_source.contains(&forbidden),
                "Lingma source-backed path contains forbidden fallback {forbidden}"
            );
        }
        let route_source = include_str!("../../../source_backed.rs");
        let route = route_source
            .split_once("pub fn register_lingma_source_backed_route")
            .unwrap()
            .1
            .split_once("fn discovered_lingma_inventory_source")
            .unwrap()
            .0;
        assert!(route.contains("with_batch_hydration"));
        assert!(route.contains("LingmaSourceBackedResolverV0"));
        assert!(!route.contains(&["work", ".sqlite"].concat()));
        assert!(!route.contains(&["ctx_history_", "store"].concat()));
    }
}
