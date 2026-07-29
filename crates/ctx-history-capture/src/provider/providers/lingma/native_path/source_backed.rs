use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceInventory,
    EventHydrationRequest, EventIdentityInput, LocatorRevisionPolicy, NativeItemKey,
    NativeRecordCoordinate, NativeSessionKey, ProjectionContractError, ScannedSourceCounts,
    SessionIdentityInput, SourceAnchor, SourceInventoryObservation, SourceKey, SourceObservation,
    SourceRecordLocator, SourceResolverContractError, StableEntityId, SubrecordSelector, TypedKey,
};
use ctx_history_index::{LexicalDocument, MAX_BODY_PREVIEW_CHARS};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    detect_schema, hash_candidate, initial_prefix_hasher, load_candidates, load_raw_row,
    publication::{provider_event, EventDraft},
    records::{
        assistant_text, event_base_index, lingma_logical_record_sha256, lingma_timestamp,
        native_values,
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
    #[error("Lingma source-backed projection emitted an empty lexical preview")]
    EmptyLexicalPreview,
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
        let sqlite_authority = retain_sqlite_source_directory_authority(&authority_handle)?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LingmaExactContentCapabilityV0 {
    RowLocalUserPrompt,
    AssistantPreviewOnly,
}

impl LingmaExactContentCapabilityV0 {
    pub(crate) const fn can_reopen_exactly(self) -> bool {
        matches!(self, Self::RowLocalUserPrompt)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LingmaSourceBackedRecordV0 {
    document: LexicalDocument,
    exact_content: LingmaExactContentCapabilityV0,
}

impl LingmaSourceBackedRecordV0 {
    pub(crate) fn document(&self) -> &LexicalDocument {
        &self.document
    }

    pub(crate) const fn exact_content(&self) -> LingmaExactContentCapabilityV0 {
        self.exact_content
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
    let revision_scope = TypedKey::bytes(opening.revision().to_vec())?;
    let source_path = database.path.display().to_string();
    let parsed = scan_rows(connection, encoding, &source, &revision_scope, &source_path)?;
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
    let item_key = native_item_key(&parsed, request_counts, revision_scope)?;
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
        &item_key,
        parsed.record_digest,
        user_sequence,
        USER_PROMPT_COORDINATE,
        source_path,
        user_event,
        LingmaExactContentCapabilityV0::RowLocalUserPrompt,
    )?);

    if let Some((text, body_kind, event_type)) = assistant_text(&parsed.row) {
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
            &item_key,
            parsed.record_digest,
            user_sequence.saturating_add(1),
            coordinate,
            source_path,
            assistant_event,
            LingmaExactContentCapabilityV0::AssistantPreviewOnly,
        )?);
    }
    Ok(())
}

fn native_item_key(
    parsed: &ParsedRow,
    request_counts: &BTreeMap<(String, String), usize>,
    revision_scope: &TypedKey,
) -> Result<NativeItemKey, ProjectionContractError> {
    if let Some(request_id) = parsed
        .row
        .request_id
        .as_ref()
        .filter(|request_id| !request_id.trim().is_empty())
        .filter(|request_id| {
            request_counts.get(&(parsed.row.session_id.clone(), (*request_id).clone())) == Some(&1)
        })
    {
        return NativeItemKey::composite(
            NATIVE_REQUEST_NAMESPACE,
            vec![
                TypedKey::utf8(parsed.row.session_id.clone())?,
                TypedKey::utf8(request_id.clone())?,
            ],
        );
    }
    NativeItemKey::revision_scoped_position(
        NATIVE_POSITION_KIND,
        TypedKey::U64(parsed.ordinal),
        revision_scope.clone(),
    )
}

#[allow(clippy::too_many_arguments)]
fn project_event(
    source: &SourceKey,
    session_id: StableEntityId,
    row: &LingmaRow,
    item_key: &NativeItemKey,
    record_digest: [u8; 32],
    event_sequence: u64,
    coordinate_kind: &'static str,
    source_path: &str,
    event: super::LingmaCoreEvent,
    exact_content: LingmaExactContentCapabilityV0,
) -> LingmaSourceBackedResultV0<LingmaSourceBackedRecordV0> {
    let subrecord =
        SubrecordSelector::native_id(NATIVE_SUBRECORD_NAMESPACE, TypedKey::utf8(coordinate_kind)?)?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: item_key,
        subrecord_selector: Some(&subrecord),
    })?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: LOGICAL_RELATION.to_owned(),
            primary_key: TypedKey::composite(vec![
                TypedKey::I64(row.rowid),
                TypedKey::utf8(coordinate_kind)?,
            ])?,
            row_version: None,
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        record_digest,
    )?;
    let text = event
        .payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let body = bounded_preview(text);
    if body.is_empty() {
        return Err(LingmaSourceBackedErrorV0::EmptyLexicalPreview);
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
            body,
            workspace: None,
            cwd: None,
            touched_files: Vec::new(),
        },
        exact_content,
    })
}

fn bounded_preview(text: &str) -> String {
    text.chars().take(MAX_BODY_PREVIEW_CHARS).collect()
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
pub(crate) enum LingmaExactContentFailureKindV0 {
    ExactContentUnavailable,
    InvalidLocator,
    SourceUnavailable,
    RecordMissing,
    StaleRecordEvidence,
}

#[derive(Debug, Error)]
#[error("Lingma exact content failed for {event_id}: {kind:?}")]
pub(crate) struct LingmaExactContentFailureV0 {
    pub(crate) event_id: StableEntityId,
    pub(crate) kind: LingmaExactContentFailureKindV0,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LingmaHydratedContentV0 {
    pub(crate) event_id: StableEntityId,
    pub(crate) text: String,
}

#[derive(Debug, Clone)]
pub(crate) struct LingmaSourceBackedResolverV0 {
    sources: Vec<(SourceKey, PathBuf)>,
}

impl LingmaSourceBackedResolverV0 {
    pub(crate) fn new(inventory: &LingmaSourceInventoryV0) -> LingmaSourceBackedResultV0<Self> {
        let mut sources = Vec::with_capacity(inventory.databases.len());
        for database in &inventory.databases {
            sources.push((database.source_key()?, database.path.clone()));
        }
        Ok(Self { sources })
    }

    pub(crate) fn hydrate_record(
        &self,
        record: &LingmaSourceBackedRecordV0,
    ) -> Result<LingmaHydratedContentV0, LingmaExactContentFailureV0> {
        self.hydrate(record.document.event_id, &record.document.locator)
    }

    pub(crate) fn hydrate(
        &self,
        event_id: StableEntityId,
        locator: &SourceRecordLocator,
    ) -> Result<LingmaHydratedContentV0, LingmaExactContentFailureV0> {
        let invalid = || LingmaExactContentFailureV0 {
            event_id,
            kind: LingmaExactContentFailureKindV0::InvalidLocator,
        };
        if EventHydrationRequest::new(event_id, locator.clone()).is_err() {
            return Err(invalid());
        }
        let NativeRecordCoordinate::ProviderSqlite {
            logical_relation,
            primary_key,
            row_version,
        } = locator.coordinate()
        else {
            return Err(invalid());
        };
        let TypedKey::Composite(parts) = primary_key else {
            return Err(invalid());
        };
        let [TypedKey::I64(rowid), TypedKey::Utf8(coordinate_kind)] = parts.as_slice() else {
            return Err(invalid());
        };
        if logical_relation != LOGICAL_RELATION || row_version.is_some() {
            return Err(invalid());
        }
        if coordinate_kind != USER_PROMPT_COORDINATE {
            return Err(LingmaExactContentFailureV0 {
                event_id,
                kind: LingmaExactContentFailureKindV0::ExactContentUnavailable,
            });
        }
        let Some((_, path)) = self.sources.iter().find(|(source, _)| {
            source.identity() == locator.source().identity()
                && source.exact_descriptor_eq(locator.source())
        }) else {
            return Err(LingmaExactContentFailureV0 {
                event_id,
                kind: LingmaExactContentFailureKindV0::SourceUnavailable,
            });
        };
        let root_authority =
            LingmaRootAuthorizedSource::retain(path).map_err(|_| LingmaExactContentFailureV0 {
                event_id,
                kind: LingmaExactContentFailureKindV0::SourceUnavailable,
            })?;
        let sqlite_snapshot =
            root_authority
                .open_snapshot()
                .map_err(|_| LingmaExactContentFailureV0 {
                    event_id,
                    kind: LingmaExactContentFailureKindV0::SourceUnavailable,
                })?;
        let source_evidence = sqlite_snapshot.evidence().clone();
        let connection = sqlite_snapshot
            .connection()
            .map_err(|_| LingmaExactContentFailureV0 {
                event_id,
                kind: LingmaExactContentFailureKindV0::SourceUnavailable,
            })?;
        let values = super::records::lingma_complete_values(connection, *rowid)
            .map_err(|_| LingmaExactContentFailureV0 {
                event_id,
                kind: LingmaExactContentFailureKindV0::SourceUnavailable,
            })?
            .ok_or(LingmaExactContentFailureV0 {
                event_id,
                kind: LingmaExactContentFailureKindV0::RecordMissing,
            })?;
        sqlite_snapshot
            .finish()
            .map_err(|_| LingmaExactContentFailureV0 {
                event_id,
                kind: LingmaExactContentFailureKindV0::SourceUnavailable,
            })?;
        if !root_authority
            .evidence_is_current(&source_evidence)
            .map_err(|_| LingmaExactContentFailureV0 {
                event_id,
                kind: LingmaExactContentFailureKindV0::SourceUnavailable,
            })?
        {
            return Err(LingmaExactContentFailureV0 {
                event_id,
                kind: LingmaExactContentFailureKindV0::SourceUnavailable,
            });
        }
        if &lingma_logical_record_sha256(&values) != locator.record_digest() {
            return Err(LingmaExactContentFailureV0 {
                event_id,
                kind: LingmaExactContentFailureKindV0::StaleRecordEvidence,
            });
        }
        let (_, text) = super::records::lingma_complete_user_message(&values).map_err(|_| {
            LingmaExactContentFailureV0 {
                event_id,
                kind: LingmaExactContentFailureKindV0::StaleRecordEvidence,
            }
        })?;
        Ok(LingmaHydratedContentV0 { event_id, text })
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
    fn root_handle_vfs_finish_rejects_leaf_swap_after_snapshot_open() {
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

    #[test]
    fn source_backed_cold_scan_certifies_multiple_databases_with_stable_bounded_records() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let first_path = temp.path().join("vscode-local.db");
        let second_path = temp.path().join("jetbrains-local.db");
        let first = create_database(&first_path);
        insert_row(
            &first,
            "vscode-session",
            "vscode-request",
            &format!("vscode prompt {}", "v".repeat(MAX_BODY_PREVIEW_CHARS + 32)),
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
        assert!(all_records(&scan)
            .iter()
            .all(|record| record.document.body.chars().count() <= MAX_BODY_PREVIEW_CHARS));
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

    #[cfg(target_os = "linux")]
    #[test]
    fn root_handle_vfs_scan_sees_committed_content_retained_in_active_wal() {
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
        let closing = opening.clone();
        let scan = scan_lingma_source_backed_v0(opening, || Ok(closing)).unwrap();
        let user = all_records(&scan)
            .into_iter()
            .find(|record| record.document.role.as_deref() == Some("user"))
            .unwrap();
        assert_eq!(user.document.body, "committed Lingma WAL prompt");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn root_handle_vfs_database_finish_precedes_source_certification() {
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
    fn source_backed_user_prompt_locator_reopens_exact_row_local_content() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("local.db");
        let prompt = format!(
            "exact row-local Lingma prompt {}",
            "x".repeat(MAX_BODY_PREVIEW_CHARS + 64)
        );
        let connection = create_database(&path);
        insert_row(
            &connection,
            "exact-session",
            "exact-request",
            &prompt,
            Some("preview-only summary"),
        );
        drop(connection);
        let inventory = inventory(vec![database(&path, "vscode:profile:exact")]);
        let closing = inventory.clone();
        let scan = scan_lingma_source_backed_v0(inventory.clone(), || Ok(closing)).unwrap();
        let user = all_records(&scan)
            .into_iter()
            .find(|record| record.document.role.as_deref() == Some("user"))
            .unwrap();
        assert_eq!(
            user.exact_content,
            LingmaExactContentCapabilityV0::RowLocalUserPrompt
        );
        assert!(user.exact_content.can_reopen_exactly());
        assert!(user.document.body.chars().count() < prompt.chars().count());
        assert!(matches!(
            user.document.locator.coordinate(),
            NativeRecordCoordinate::ProviderSqlite {
                logical_relation,
                primary_key: TypedKey::Composite(parts),
                row_version: None,
            } if logical_relation == LOGICAL_RELATION
                && matches!(
                    parts.as_slice(),
                    [TypedKey::I64(1), TypedKey::Utf8(kind)]
                        if kind == USER_PROMPT_COORDINATE
                )
        ));

        let hydrated = LingmaSourceBackedResolverV0::new(&inventory)
            .unwrap()
            .hydrate_record(user)
            .unwrap();
        assert_eq!(hydrated.text, prompt);
    }

    #[test]
    fn source_backed_assistant_summary_typed_fails_instead_of_returning_preview_as_exact() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let path = temp.path().join("local.db");
        let summary = format!(
            "assistant summary preview {}",
            "s".repeat(MAX_BODY_PREVIEW_CHARS + 64)
        );
        let connection = create_database(&path);
        insert_row(
            &connection,
            "summary-session",
            "summary-request",
            "user prompt",
            Some(&summary),
        );
        drop(connection);
        let inventory = inventory(vec![database(&path, "jetbrains:idea:summary")]);
        let closing = inventory.clone();
        let scan = scan_lingma_source_backed_v0(inventory.clone(), || Ok(closing)).unwrap();
        let assistant = all_records(&scan)
            .into_iter()
            .find(|record| record.document.role.as_deref() == Some("assistant"))
            .unwrap();
        assert_eq!(
            assistant.exact_content,
            LingmaExactContentCapabilityV0::AssistantPreviewOnly
        );
        assert!(!assistant.exact_content.can_reopen_exactly());
        assert!(summary.starts_with(&assistant.document.body));

        let failure = LingmaSourceBackedResolverV0::new(&inventory)
            .unwrap()
            .hydrate_record(assistant)
            .unwrap_err();
        assert_eq!(
            failure.kind,
            LingmaExactContentFailureKindV0::ExactContentUnavailable
        );
    }
}
