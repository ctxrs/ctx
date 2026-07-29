use std::{
    collections::HashSet,
    convert::Infallible,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource,
    EventIdentityInput, LocatorRevisionPolicy, NativeItemKey, NativeRecordCoordinate,
    NativeSessionKey, ProjectionContractError, ScannedSourceCounts, SessionIdentityInput,
    SourceAnchor, SourceKey, SourceObservation, SourceRecordLocator, SourceResolverContractError,
    StableEntityId, TypedKey,
};
use ctx_history_index::LexicalDocument;
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::open_provider_source_file,
    provider::file_touches::{
        event_type_supports_structured_file_touches, visit_provider_file_touch_drafts_with_limit,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
    },
    CaptureError, KIRO_SQLITE_SOURCE_FORMAT, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::super::history::{
    kiro_history_entry_events, kiro_history_events, kiro_provider_session_id,
    kiro_session_started_at, KiroConversationRow,
};
use super::{
    absolute_kiro_path,
    scan::{candidate_at, hydrate_row, next_candidate},
    KiroPhase, KiroSqliteDatabase, KiroTables,
};

const KIRO_SOURCE_ANCHOR_NAMESPACE: &str = "kiro.legacy-sqlite";
const KIRO_SOURCE_ANCHOR_KEY: &str = "default-history";
const KIRO_SOURCE_SCHEMA_VARIANT: &str = "kiro-legacy-conversations-sqlite-v1";
const KIRO_SOURCE_REVISION_KIND: &str = "kiro-sqlite-read-snapshot-v1";
const KIRO_SOURCE_BACKED_PARSER_REVISION: &str = "kiro-source-backed-v0";
const KIRO_NATIVE_SESSION_NAMESPACE: &str = "kiro.conversation";
const KIRO_NATIVE_EVENT_NAMESPACE: &str = "kiro.history-event";
const KIRO_LOGICAL_SESSION_KIND: &str = "kiro-conversation";
const KIRO_LOGICAL_EVENT_KIND: &str = "kiro-history-event";
const KIRO_SNAPSHOT_DIGEST_DOMAIN: &[u8] = b"ctx.kiro.sqlite-snapshot.v1\0";
const KIRO_ROW_DIGEST_DOMAIN: &[u8] = b"ctx.kiro.conversation-row.v1\0";
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const MAX_LEXICAL_METADATA_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub(crate) enum KiroSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    ResolverContract(#[from] SourceResolverContractError),
    #[error("unsupported Kiro history format: {0}")]
    UnsupportedFormat(&'static str),
    #[error("Kiro source-backed scan cannot certify an invalid row in {relation} at rowid {rowid}: {reason}")]
    UncertifiableRow {
        relation: &'static str,
        rowid: i64,
        reason: &'static str,
    },
    #[error("Kiro {relation} contains an ambiguous conversation key")]
    AmbiguousConversationKey { relation: &'static str },
    #[error("Kiro source-backed count overflow")]
    CountOverflow,
    #[error("locator is not a Kiro legacy SQLite conversation event")]
    InvalidLocator,
    #[error("the Kiro conversation row addressed by the locator is missing")]
    MissingConversationRow,
    #[error("the Kiro conversation row no longer matches its certified digest")]
    ConversationRowDigestMismatch,
    #[error("the Kiro event addressed within its conversation row is missing")]
    MissingConversationEvent,
}

pub(crate) type KiroSourceBackedResultV0<T> = Result<T, KiroSourceBackedErrorV0>;

#[derive(Debug)]
pub(crate) struct KiroSourceBackedScanV0 {
    pub(crate) source: SourceKey,
    pub(crate) documents: Vec<LexicalDocument>,
    pub(crate) certificate: CertifiedSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KiroHydratedRecordV0 {
    pub(crate) provider_bytes: Vec<u8>,
    pub(crate) decoded_display_text: String,
}

#[derive(Debug)]
pub(crate) struct KiroLocatorResolverV0 {
    source_path: PathBuf,
    source: SourceKey,
}

impl KiroLocatorResolverV0 {
    pub(crate) fn discover(
        source_path: impl AsRef<Path>,
        source_format: &str,
    ) -> KiroSourceBackedResultV0<Self> {
        let source_path = absolute_kiro_path(source_path.as_ref())?;
        let _ = open_kiro_database(&source_path, source_format)?;
        Ok(Self {
            source_path,
            source: kiro_source_key()?,
        })
    }

    pub(crate) fn source(&self) -> &SourceKey {
        &self.source
    }

    pub(crate) fn hydrate(
        &self,
        locator: &SourceRecordLocator,
    ) -> KiroSourceBackedResultV0<KiroHydratedRecordV0> {
        locator.validate_contract()?;
        let (phase, key, native_event_key) = validate_kiro_locator(&self.source, locator)?;
        let (database, tables) = open_kiro_database(&self.source_path, KIRO_SQLITE_SOURCE_FORMAT)?;
        if !phase_is_present(tables, phase) {
            return Err(KiroSourceBackedErrorV0::InvalidLocator);
        }
        let row = database.read(&self.source_path, |connection| {
            load_exact_conversation_row(connection, phase, &key)
        })?;
        let (record_digest, _) = canonical_row_digest(&row)?;
        if &record_digest != locator.record_digest() {
            return Err(KiroSourceBackedErrorV0::ConversationRowDigestMismatch);
        }
        let value: Value = serde_json::from_str(&row.value)?;
        let provider_session_id = kiro_provider_session_id(&row, &value);
        let started_at = kiro_session_started_at(&row, &value, DateTime::<Utc>::UNIX_EPOCH);
        let decoded = kiro_history_events(&row, &provider_session_id, &value, started_at)
            .find(|decoded| decoded.event.cursor == native_event_key)
            .ok_or(KiroSourceBackedErrorV0::MissingConversationEvent)?;
        let decoded_display_text = decoded.complete_text();
        Ok(KiroHydratedRecordV0 {
            provider_bytes: decoded_display_text.as_bytes().to_vec(),
            decoded_display_text,
        })
    }
}

pub(crate) fn scan_kiro_source_backed_v0(
    source_path: impl AsRef<Path>,
    source_format: &str,
) -> KiroSourceBackedResultV0<KiroSourceBackedScanV0> {
    let source_path = absolute_kiro_path(source_path.as_ref())?;
    let source = kiro_source_key()?;
    let (database, tables) = open_kiro_database(&source_path, source_format)?;
    let opening = source_observation(&source, database.evidence())?;
    let indexed_source_path = source_path.display().to_string();
    let mut scan = KiroLogicalScan::new(source.clone(), tables, indexed_source_path)?;
    database.read(&source_path, |connection| scan.scan(connection))?;
    database.revalidate()?;
    let closing = source_observation(&source, database.evidence())?;
    let (documents, counts, content_digest) = scan.finish();
    let certificate = CertifiedSource::certify(
        opening,
        closing,
        KIRO_SOURCE_BACKED_PARSER_REVISION,
        content_digest,
        counts,
    )?;
    Ok(KiroSourceBackedScanV0 {
        source,
        documents,
        certificate,
    })
}

fn open_kiro_database(
    source_path: &Path,
    source_format: &str,
) -> KiroSourceBackedResultV0<(KiroSqliteDatabase, KiroTables)> {
    require_legacy_sqlite_format(source_path, source_format)?;
    KiroSqliteDatabase::open(source_path, KiroTables::probe).map_err(Into::into)
}

fn require_legacy_sqlite_format(
    source_path: &Path,
    source_format: &str,
) -> KiroSourceBackedResultV0<()> {
    if source_format != KIRO_SQLITE_SOURCE_FORMAT {
        return Err(KiroSourceBackedErrorV0::UnsupportedFormat(
            "current ACP/v3 and saved-chat JSON remain detection-only",
        ));
    }
    let metadata = fs::symlink_metadata(source_path)?;
    if metadata.file_type().is_dir() {
        return Err(KiroSourceBackedErrorV0::UnsupportedFormat(
            "current ACP/v3 session directories remain detection-only",
        ));
    }
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: source_path.to_path_buf(),
            reason: "Kiro SQLite source must be a regular non-symlink file",
        }
        .into());
    }
    let opened = open_provider_source_file(source_path)?;
    let mut file = opened.file().try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut header = [0_u8; SQLITE_HEADER.len()];
    let read = file.read(&mut header)?;
    opened.revalidate()?;
    if read != SQLITE_HEADER.len() || &header != SQLITE_HEADER {
        return Err(KiroSourceBackedErrorV0::UnsupportedFormat(
            "saved-chat JSON and non-SQLite Kiro files remain detection-only",
        ));
    }
    Ok(())
}

fn kiro_source_key() -> KiroSourceBackedResultV0<SourceKey> {
    let anchor = SourceAnchor::provider_native(
        KIRO_SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(KIRO_SOURCE_ANCHOR_KEY)?,
    )?;
    Ok(SourceKey::derive(
        CaptureProvider::KiroCli.as_str(),
        KIRO_SQLITE_SOURCE_FORMAT,
        KIRO_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

fn source_observation(
    source: &SourceKey,
    evidence: &crate::provider_sources::SqliteSourceEvidence,
) -> KiroSourceBackedResultV0<SourceObservation> {
    Ok(SourceObservation::new(
        source.clone(),
        KIRO_SOURCE_REVISION_KIND,
        format!(
            "identity={};length={};revision={}",
            super::hex(evidence.identity()),
            evidence.length(),
            super::hex(evidence.revision()),
        )
        .into_bytes(),
    )?)
}

struct KiroLogicalScan {
    source: SourceKey,
    tables: KiroTables,
    documents: Vec<LexicalDocument>,
    counts: ScannedSourceCounts,
    content_digest: Sha256,
    seen_keys: HashSet<(&'static str, String)>,
    indexed_source_path: String,
}

impl KiroLogicalScan {
    fn new(
        source: SourceKey,
        tables: KiroTables,
        indexed_source_path: String,
    ) -> KiroSourceBackedResultV0<Self> {
        let mut scan = Self {
            source,
            tables,
            documents: Vec::new(),
            counts: ScannedSourceCounts::default(),
            content_digest: Sha256::new(),
            seen_keys: HashSet::new(),
            indexed_source_path,
        };
        scan.content_digest.update(KIRO_SNAPSHOT_DIGEST_DOMAIN);
        if tables.v2 {
            scan.hash_table_marker(KiroPhase::V2)?;
        }
        if tables.legacy {
            scan.hash_table_marker(KiroPhase::Legacy)?;
        }
        Ok(scan)
    }

    fn scan(&mut self, connection: &Connection) -> KiroSourceBackedResultV0<()> {
        if self.tables.v2 {
            self.scan_phase(connection, KiroPhase::V2)?;
        }
        if self.tables.legacy {
            self.scan_phase(connection, KiroPhase::Legacy)?;
        }
        Ok(())
    }

    fn scan_phase(
        &mut self,
        connection: &Connection,
        phase: KiroPhase,
    ) -> KiroSourceBackedResultV0<()> {
        let mut after_rowid = None;
        let mut row_ordinal = 0_u64;
        while let Some(candidate) = next_candidate(connection, phase, after_rowid, row_ordinal)? {
            after_rowid = Some(candidate.rowid);
            row_ordinal = checked_add(row_ordinal, 1)?;
            if let Some(reason) = candidate.rejection_reason() {
                return Err(KiroSourceBackedErrorV0::UncertifiableRow {
                    relation: phase.table(),
                    rowid: candidate.rowid,
                    reason,
                });
            }
            if candidate.retained_bytes > MAX_PROVIDER_SQLITE_VALUE_BYTES as u64 {
                return Err(KiroSourceBackedErrorV0::UncertifiableRow {
                    relation: phase.table(),
                    rowid: candidate.rowid,
                    reason: "row exceeds the provider SQLite value bound",
                });
            }
            let row = hydrate_row(connection, phase, candidate.rowid)?;
            if !self.seen_keys.insert((row.table, row.key.clone())) {
                return Err(KiroSourceBackedErrorV0::AmbiguousConversationKey {
                    relation: row.table,
                });
            }
            let (record_digest, canonical_bytes) = canonical_row_digest(&row)?;
            self.counts.certified_bytes =
                checked_add(self.counts.certified_bytes, canonical_bytes)?;
            self.content_digest.update([phase.tag()]);
            self.content_digest.update(record_digest);
            self.project_row(row, record_digest)?;
        }
        Ok(())
    }

    fn project_row(
        &mut self,
        row: KiroConversationRow,
        record_digest: [u8; 32],
    ) -> KiroSourceBackedResultV0<()> {
        let value: Value = match serde_json::from_str(&row.value) {
            Ok(value) => value,
            Err(_) => {
                self.classify_rejected_record()?;
                return Ok(());
            }
        };
        let history = match value.get("history") {
            Some(Value::Array(history)) => history,
            Some(_) => {
                self.classify_rejected_record()?;
                return Ok(());
            }
            None => {
                self.classify_ignored_record()?;
                return Ok(());
            }
        };
        if history.is_empty() {
            self.classify_ignored_record()?;
            return Ok(());
        }

        let provider_session_id = kiro_provider_session_id(&row, &value);
        let started_at = kiro_session_started_at(&row, &value, DateTime::<Utc>::UNIX_EPOCH);
        let session_id = kiro_session_identity(&self.source, &row, &provider_session_id)?;
        for (history_index, entry) in history.iter().enumerate() {
            let events = kiro_history_entry_events(
                &row,
                &provider_session_id,
                history_index,
                entry,
                started_at,
            );
            if events.is_empty() {
                self.classify_ignored_record()?;
                continue;
            }
            for native in events {
                let document = kiro_lexical_document(
                    &self.source,
                    session_id,
                    &row,
                    &provider_session_id,
                    &self.indexed_source_path,
                    entry,
                    native.event,
                    record_digest,
                )?;
                self.documents.push(document);
                self.counts.complete_records = checked_add(self.counts.complete_records, 1)?;
                self.counts.retained_records = checked_add(self.counts.retained_records, 1)?;
                self.counts.indexed_documents = checked_add(self.counts.indexed_documents, 1)?;
            }
        }
        Ok(())
    }

    fn classify_rejected_record(&mut self) -> KiroSourceBackedResultV0<()> {
        self.counts.complete_records = checked_add(self.counts.complete_records, 1)?;
        self.counts.rejected_records = checked_add(self.counts.rejected_records, 1)?;
        Ok(())
    }

    fn classify_ignored_record(&mut self) -> KiroSourceBackedResultV0<()> {
        self.counts.complete_records = checked_add(self.counts.complete_records, 1)?;
        self.counts.ignored_records = checked_add(self.counts.ignored_records, 1)?;
        Ok(())
    }

    fn hash_table_marker(&mut self, phase: KiroPhase) -> KiroSourceBackedResultV0<()> {
        self.content_digest.update([0]);
        let relation = phase.table().as_bytes();
        self.content_digest.update(
            u64::try_from(relation.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        self.content_digest.update(relation);
        self.counts.certified_bytes = checked_add(
            self.counts.certified_bytes,
            1_u64
                .checked_add(8)
                .and_then(|value| value.checked_add(relation.len() as u64))
                .ok_or(KiroSourceBackedErrorV0::CountOverflow)?,
        )?;
        Ok(())
    }

    fn finish(self) -> (Vec<LexicalDocument>, ScannedSourceCounts, [u8; 32]) {
        (
            self.documents,
            self.counts,
            self.content_digest.finalize().into(),
        )
    }
}

fn kiro_session_identity(
    source: &SourceKey,
    row: &KiroConversationRow,
    provider_session_id: &str,
) -> KiroSourceBackedResultV0<StableEntityId> {
    let native_session_key = NativeSessionKey::composite(
        KIRO_NATIVE_SESSION_NAMESPACE,
        vec![
            TypedKey::utf8(row.table)?,
            TypedKey::utf8(row.key.clone())?,
            TypedKey::utf8(provider_session_id)?,
        ],
    )?;
    Ok(derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: KIRO_LOGICAL_SESSION_KIND,
        native_session_key: &native_session_key,
    })?)
}

#[allow(clippy::too_many_arguments)]
fn kiro_lexical_document(
    source: &SourceKey,
    session_id: StableEntityId,
    row: &KiroConversationRow,
    provider_session_id: &str,
    source_path: &str,
    entry: &Value,
    event: super::super::event::KiroNativeEvent,
    record_digest: [u8; 32],
) -> KiroSourceBackedResultV0<LexicalDocument> {
    let native_item_key = NativeItemKey::native_id(
        KIRO_NATIVE_EVENT_NAMESPACE,
        TypedKey::utf8(event.cursor.clone())?,
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: KIRO_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let primary_key = TypedKey::composite(vec![
        TypedKey::utf8(row.key.clone())?,
        TypedKey::utf8(event.cursor.clone())?,
    ])?;
    let locator = SourceRecordLocator::new(
        source.clone(),
        NativeRecordCoordinate::ProviderSqlite {
            logical_relation: row.table.to_owned(),
            primary_key,
            row_version: None,
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        record_digest,
    )?;
    let touched_files = projected_touched_files(event.event_type, entry)?;
    let body = event
        .payload
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if body.is_empty() {
        return Err(KiroSourceBackedErrorV0::UncertifiableRow {
            relation: row.table,
            rowid: row.rowid,
            reason: "parser emitted an empty lexical body",
        });
    }
    Ok(LexicalDocument {
        event_id,
        session_id,
        parent_session_id: None,
        root_session_id: session_id,
        source: source.clone(),
        locator,
        provider_session_id: Some(provider_session_id.to_owned()),
        branch: None,
        source_path: Some(source_path.to_owned()),
        agent_type: AgentType::Primary.as_str().to_owned(),
        is_primary: true,
        event_sequence: event.provider_event_index,
        occurred_at_unix_ms: Some(event.occurred_at.timestamp_millis()),
        event_type: event.event_type.as_str().to_owned(),
        role: event.role.map(|role| role.as_str().to_owned()),
        body,
        workspace: None,
        cwd: (!row.key.trim().is_empty()).then(|| row.key.clone()),
        touched_files,
    })
}

fn projected_touched_files(
    event_type: ctx_history_core::EventType,
    entry: &Value,
) -> KiroSourceBackedResultV0<Vec<String>> {
    if !matches!(
        event_type,
        ctx_history_core::EventType::ToolCall
            | ctx_history_core::EventType::ToolOutput
            | ctx_history_core::EventType::CommandOutput
            | ctx_history_core::EventType::FileTouched
    ) {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    let outcome = visit_provider_file_touch_drafts_with_limit(
        entry,
        event_type_supports_structured_file_touches(event_type),
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(_, touch)| {
            if touch.path.len() <= MAX_LEXICAL_METADATA_BYTES {
                paths.push(touch.path);
            }
            Ok::<(), Infallible>(())
        },
    )
    .unwrap_or_else(|never| match never {});
    if outcome.limit_exceeded() {
        return Err(KiroSourceBackedErrorV0::CountOverflow);
    }
    Ok(paths)
}

fn validate_kiro_locator(
    expected_source: &SourceKey,
    locator: &SourceRecordLocator,
) -> KiroSourceBackedResultV0<(KiroPhase, String, String)> {
    if !expected_source.exact_descriptor_eq(locator.source())
        || locator.revision_policy() != LocatorRevisionPolicy::StableRecordEvidence
        || locator.certified_source_revision_digest().is_some()
    {
        return Err(KiroSourceBackedErrorV0::InvalidLocator);
    }
    let NativeRecordCoordinate::ProviderSqlite {
        logical_relation,
        primary_key,
        row_version,
    } = locator.coordinate()
    else {
        return Err(KiroSourceBackedErrorV0::InvalidLocator);
    };
    if row_version.is_some() {
        return Err(KiroSourceBackedErrorV0::InvalidLocator);
    }
    let phase = match logical_relation.as_str() {
        "conversations_v2" => KiroPhase::V2,
        "conversations" => KiroPhase::Legacy,
        _ => return Err(KiroSourceBackedErrorV0::InvalidLocator),
    };
    let TypedKey::Composite(parts) = primary_key else {
        return Err(KiroSourceBackedErrorV0::InvalidLocator);
    };
    let [TypedKey::Utf8(key), TypedKey::Utf8(native_event_key)] = parts.as_slice() else {
        return Err(KiroSourceBackedErrorV0::InvalidLocator);
    };
    Ok((phase, key.clone(), native_event_key.clone()))
}

fn load_exact_conversation_row(
    connection: &Connection,
    phase: KiroPhase,
    key: &str,
) -> KiroSourceBackedResultV0<KiroConversationRow> {
    let sql = format!(
        "select rowid from {} \
         where typeof(key) = 'text' and key collate binary = ?1 collate binary \
         order by rowid limit 2",
        phase.table()
    );
    let mut statement = connection.prepare(&sql)?;
    let rowids = statement
        .query_map([key], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let [rowid] = rowids.as_slice() else {
        return match rowids.len() {
            0 => Err(KiroSourceBackedErrorV0::MissingConversationRow),
            _ => Err(KiroSourceBackedErrorV0::AmbiguousConversationKey {
                relation: phase.table(),
            }),
        };
    };
    let candidate = candidate_at(connection, phase, *rowid, 0)?
        .ok_or(KiroSourceBackedErrorV0::MissingConversationRow)?;
    if let Some(reason) = candidate.rejection_reason() {
        return Err(KiroSourceBackedErrorV0::UncertifiableRow {
            relation: phase.table(),
            rowid: *rowid,
            reason,
        });
    }
    if candidate.retained_bytes > MAX_PROVIDER_SQLITE_VALUE_BYTES as u64 {
        return Err(KiroSourceBackedErrorV0::UncertifiableRow {
            relation: phase.table(),
            rowid: *rowid,
            reason: "row exceeds the provider SQLite value bound",
        });
    }
    hydrate_row(connection, phase, *rowid).map_err(Into::into)
}

fn phase_is_present(tables: KiroTables, phase: KiroPhase) -> bool {
    match phase {
        KiroPhase::V2 => tables.v2,
        KiroPhase::Legacy => tables.legacy,
    }
}

fn canonical_row_digest(row: &KiroConversationRow) -> KiroSourceBackedResultV0<([u8; 32], u64)> {
    let mut digest = Sha256::new();
    digest.update(KIRO_ROW_DIGEST_DOMAIN);
    let mut bytes = 0_u64;
    bytes = checked_add(bytes, hash_bytes(&mut digest, 1, row.table.as_bytes())?)?;
    bytes = checked_add(bytes, hash_bytes(&mut digest, 2, row.key.as_bytes())?)?;
    bytes = checked_add(
        bytes,
        hash_optional_bytes(
            &mut digest,
            3,
            row.conversation_id.as_deref().map(str::as_bytes),
        )?,
    )?;
    bytes = checked_add(bytes, hash_bytes(&mut digest, 4, row.value.as_bytes())?)?;
    bytes = checked_add(bytes, hash_optional_i64(&mut digest, 5, row.created_at)?)?;
    bytes = checked_add(bytes, hash_optional_i64(&mut digest, 6, row.updated_at)?)?;
    Ok((digest.finalize().into(), bytes))
}

fn hash_bytes(digest: &mut Sha256, tag: u8, value: &[u8]) -> KiroSourceBackedResultV0<u64> {
    let length = u64::try_from(value.len()).map_err(|_| KiroSourceBackedErrorV0::CountOverflow)?;
    digest.update([tag, 1]);
    digest.update(length.to_be_bytes());
    digest.update(value);
    length
        .checked_add(10)
        .ok_or(KiroSourceBackedErrorV0::CountOverflow)
}

fn hash_optional_bytes(
    digest: &mut Sha256,
    tag: u8,
    value: Option<&[u8]>,
) -> KiroSourceBackedResultV0<u64> {
    match value {
        Some(value) => hash_bytes(digest, tag, value),
        None => {
            digest.update([tag, 0]);
            Ok(2)
        }
    }
}

fn hash_optional_i64(
    digest: &mut Sha256,
    tag: u8,
    value: Option<i64>,
) -> KiroSourceBackedResultV0<u64> {
    match value {
        Some(value) => {
            digest.update([tag, 2]);
            digest.update(value.to_be_bytes());
            Ok(10)
        }
        None => {
            digest.update([tag, 0]);
            Ok(2)
        }
    }
}

fn checked_add(left: u64, right: u64) -> KiroSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(KiroSourceBackedErrorV0::CountOverflow)
}

#[cfg(test)]
#[path = "source_backed_tests.rs"]
mod tests;
