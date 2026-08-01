//! Kiro legacy-SQLite logical capture, projection, and exact-row resolution.

use std::{
    collections::{BTreeMap, HashSet},
    convert::Infallible,
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource, CoreRecord,
    CoreRecordError, EventIdentityInput, NativeItemKey, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, StableEntityId, TypedKey,
};
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
    provider_sources::{SqliteLogicalSnapshot, SqliteSourceEvidence},
    CaptureError, KIRO_SQLITE_SOURCE_FORMAT,
};

use super::super::history::{
    kiro_history_entry_events, kiro_provider_session_id, kiro_session_started_at,
    KiroConversationRow,
};
use super::{scan::stream_rows, KiroPhase, KiroTables};

const KIRO_SOURCE_ANCHOR_NAMESPACE: &str = "kiro.legacy-sqlite";
const KIRO_SOURCE_ANCHOR_KEY: &str = "default-history";
const KIRO_SOURCE_SCHEMA_VARIANT: &str = "kiro-legacy-conversations-sqlite-v1";
const KIRO_SOURCE_BACKED_PARSER_REVISION: &str = "kiro-source-backed-logical-v2";
const KIRO_NATIVE_SESSION_NAMESPACE: &str = "kiro.conversation";
const KIRO_NATIVE_EVENT_NAMESPACE: &str = "kiro.history-event";
const KIRO_LOGICAL_SESSION_KIND: &str = "kiro-conversation";
const KIRO_LOGICAL_EVENT_KIND: &str = "kiro-history-event";
const KIRO_SNAPSHOT_DIGEST_DOMAIN: &[u8] = b"ctx.kiro.logical-snapshot.v2\0";
const KIRO_LOGICAL_FINGERPRINT_DOMAIN: &[u8] = b"ctx.kiro.logical-fingerprint.v1\0";
const KIRO_ROW_DIGEST_DOMAIN: &[u8] = b"ctx.kiro.conversation-row.v1\0";
const KIRO_SCHEMA_DIGEST_DOMAIN: &[u8] = b"ctx.kiro.relevant-schema.v1\0";
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
const MAX_LEXICAL_METADATA_BYTES: usize = 64 * 1024;
pub(super) const SOURCE_BACKED_PAGE_ROWS: usize = 64;

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
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Route(#[from] crate::provider::source_backed::SourceBackedRouteError),
    #[error("unsupported Kiro history format: {0}")]
    UnsupportedFormat(&'static str),
    #[error(
        "Kiro source-backed scan cannot certify an invalid row in {relation} \
         at rowid {rowid}: {reason}"
    )]
    UncertifiableRow {
        relation: &'static str,
        rowid: i64,
        reason: &'static str,
    },
    #[error("Kiro {relation} contains an ambiguous conversation key")]
    AmbiguousConversationKey { relation: &'static str },
    #[error("Kiro source-backed count overflow")]
    CountOverflow,
}

pub(crate) type KiroSourceBackedResultV0<T> = Result<T, KiroSourceBackedErrorV0>;

#[path = "source_backed/registration.rs"]
pub(crate) mod registration;

#[derive(Debug)]
pub(super) struct KiroSourceBackedScan {
    pub(super) source: SourceKey,
    pub(super) certificate: CertifiedSource,
    pub(super) terminal_fence: SqliteSourceEvidence,
    pub(super) emitted_pages: u64,
    pub(super) row_decode_passes: u64,
    pub(super) decoded_rows: u64,
    pub(super) peak_buffered_rows: u64,
}

pub(super) fn scan_kiro_snapshot(
    connection: &Connection,
    source_path: &Path,
    source: SourceKey,
    terminal_fence: SqliteSourceEvidence,
    emit: &mut dyn FnMut(Vec<CoreRecord>) -> KiroSourceBackedResultV0<()>,
) -> KiroSourceBackedResultV0<KiroSourceBackedScan> {
    let tables = KiroTables::probe(connection)?;
    let schema_evidence = relevant_schema_evidence(connection, tables)?;
    let indexed_source_path = source_path.display().to_string();
    let mut scanner = KiroLogicalScan::new(source.clone(), tables, indexed_source_path, emit)?;
    scanner.scan(connection)?;
    let streamed = scanner.finish()?;
    let certificate = SqliteLogicalSnapshot::new(
        KIRO_SOURCE_BACKED_PARSER_REVISION,
        &schema_evidence,
        streamed.content_digest,
        streamed.counts,
    )
    .certify(source.clone())?;
    Ok(KiroSourceBackedScan {
        source,
        certificate,
        terminal_fence,
        emitted_pages: streamed.emitted_pages,
        row_decode_passes: streamed.row_decode_passes,
        decoded_rows: streamed.decoded_rows,
        peak_buffered_rows: streamed.peak_buffered_rows,
    })
}

pub(super) fn observe_kiro_logical_snapshot(
    connection: &Connection,
) -> KiroSourceBackedResultV0<[u8; 32]> {
    let tables = KiroTables::probe(connection)?;
    let schema_evidence = relevant_schema_evidence(connection, tables)?;
    let mut digest = Sha256::new();
    digest.update(KIRO_LOGICAL_FINGERPRINT_DOMAIN);
    hash_unchecked(&mut digest, KIRO_SOURCE_BACKED_PARSER_REVISION);
    digest.update(
        u64::try_from(schema_evidence.len())
            .map_err(|_| KiroSourceBackedErrorV0::CountOverflow)?
            .to_be_bytes(),
    );
    digest.update(schema_evidence);
    let mut seen_keys = HashSet::new();
    let mut decoded_rows = 0_u64;
    for phase in [KiroPhase::V2, KiroPhase::Legacy] {
        digest.update([phase.tag(), u8::from(phase_is_present(tables, phase))]);
        if !phase_is_present(tables, phase) {
            continue;
        }
        let decoded = stream_rows(connection, phase, &mut |row| {
            if !seen_keys.insert((row.table, row.key.clone())) {
                return Err(KiroSourceBackedErrorV0::AmbiguousConversationKey {
                    relation: row.table,
                });
            }
            let (record_digest, canonical_bytes) = canonical_row_digest(&row)?;
            digest.update(record_digest);
            digest.update(canonical_bytes.to_be_bytes());
            Ok(())
        })?;
        decoded_rows = checked_add(decoded_rows, decoded)?;
    }
    digest.update(decoded_rows.to_be_bytes());
    Ok(digest.finalize().into())
}

pub(super) fn require_legacy_sqlite_format(
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

pub(super) fn kiro_source_key() -> KiroSourceBackedResultV0<SourceKey> {
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

fn relevant_schema_evidence(
    connection: &Connection,
    tables: KiroTables,
) -> KiroSourceBackedResultV0<Vec<u8>> {
    let mut digest = Sha256::new();
    digest.update(KIRO_SCHEMA_DIGEST_DOMAIN);
    let user_version =
        connection.query_row("pragma user_version", [], |row| row.get::<_, i64>(0))?;
    digest.update(user_version.to_le_bytes());
    for phase in [KiroPhase::V2, KiroPhase::Legacy] {
        digest.update([phase.tag(), u8::from(phase_is_present(tables, phase))]);
        if !phase_is_present(tables, phase) {
            continue;
        }
        let mut statement =
            connection.prepare(&format!("pragma table_xinfo('{}')", phase.table()))?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?;
        let columns = rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|column| (column.0.clone(), column))
            .collect::<BTreeMap<_, _>>();
        for required in required_columns(phase) {
            let column = columns.get(*required).ok_or_else(|| {
                CaptureError::UnsupportedSchema(format!(
                    "Kiro {} is missing required column {required}",
                    phase.table()
                ))
            })?;
            hash_bytes(&mut digest, 1, column.0.as_bytes())?;
            hash_bytes(
                &mut digest,
                2,
                column.1.trim().to_ascii_uppercase().as_bytes(),
            )?;
            digest.update(column.2.to_le_bytes());
            hash_optional_bytes(&mut digest, 3, column.3.as_deref().map(str::as_bytes))?;
            digest.update(column.4.to_le_bytes());
            digest.update(column.5.to_le_bytes());
        }
    }
    Ok(digest.finalize().to_vec())
}

fn required_columns(phase: KiroPhase) -> &'static [&'static str] {
    match phase {
        KiroPhase::V2 => &[
            "key",
            "conversation_id",
            "value",
            "created_at",
            "updated_at",
        ],
        KiroPhase::Legacy => &["key", "value"],
    }
}

struct KiroLogicalScan<'emit> {
    source: SourceKey,
    tables: KiroTables,
    counts: ScannedSourceCounts,
    content_digest: Sha256,
    seen_keys: HashSet<(&'static str, String)>,
    indexed_source_path: String,
    page: Vec<CoreRecord>,
    emit: &'emit mut dyn FnMut(Vec<CoreRecord>) -> KiroSourceBackedResultV0<()>,
    emitted_pages: u64,
    row_decode_passes: u64,
    decoded_rows: u64,
    peak_buffered_rows: u64,
}

impl<'emit> KiroLogicalScan<'emit> {
    fn new(
        source: SourceKey,
        tables: KiroTables,
        indexed_source_path: String,
        emit: &'emit mut dyn FnMut(Vec<CoreRecord>) -> KiroSourceBackedResultV0<()>,
    ) -> KiroSourceBackedResultV0<Self> {
        let mut scan = Self {
            source,
            tables,
            counts: ScannedSourceCounts::default(),
            content_digest: Sha256::new(),
            seen_keys: HashSet::new(),
            indexed_source_path,
            page: Vec::with_capacity(SOURCE_BACKED_PAGE_ROWS),
            emit,
            emitted_pages: 0,
            row_decode_passes: 0,
            decoded_rows: 0,
            peak_buffered_rows: 0,
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
        self.row_decode_passes = checked_add(self.row_decode_passes, 1)?;
        for phase in [KiroPhase::V2, KiroPhase::Legacy] {
            if !phase_is_present(self.tables, phase) {
                continue;
            }
            let decoded = stream_rows(connection, phase, &mut |row| self.process_row(phase, row))?;
            self.decoded_rows = checked_add(self.decoded_rows, decoded)?;
        }
        Ok(())
    }

    fn process_row(
        &mut self,
        phase: KiroPhase,
        row: KiroConversationRow,
    ) -> KiroSourceBackedResultV0<()> {
        if !self.seen_keys.insert((row.table, row.key.clone())) {
            return Err(KiroSourceBackedErrorV0::AmbiguousConversationKey {
                relation: row.table,
            });
        }
        let (record_digest, canonical_bytes) = canonical_row_digest(&row)?;
        self.counts.certified_bytes = checked_add(self.counts.certified_bytes, canonical_bytes)?;
        self.content_digest.update([phase.tag()]);
        self.content_digest.update(record_digest);
        let before = self.counts;
        let mut projection_digest = Sha256::new();
        self.project_row(row, record_digest, &mut projection_digest)?;
        hash_projection_counts(&mut self.content_digest, before, self.counts);
        self.content_digest.update(projection_digest.finalize());
        Ok(())
    }

    fn project_row(
        &mut self,
        row: KiroConversationRow,
        record_digest: [u8; 32],
        projection_digest: &mut Sha256,
    ) -> KiroSourceBackedResultV0<()> {
        let value: Value = match serde_json::from_str(&row.value) {
            Ok(value) => value,
            Err(_) => return self.classify_rejected_record(),
        };
        let history = match value.get("history") {
            Some(Value::Array(history)) => history,
            Some(_) => return self.classify_rejected_record(),
            None => return self.classify_ignored_record(),
        };
        if history.is_empty() {
            return self.classify_ignored_record();
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
                let document = kiro_core_record(
                    &self.source,
                    session_id,
                    &row,
                    &provider_session_id,
                    &self.indexed_source_path,
                    entry,
                    native.event,
                    native.complete_text,
                    record_digest,
                )?;
                hash_projected_document(projection_digest, &document);
                self.push_document(document)?;
                self.counts.complete_records = checked_add(self.counts.complete_records, 1)?;
                self.counts.retained_records = checked_add(self.counts.retained_records, 1)?;
                self.counts.indexed_documents = checked_add(self.counts.indexed_documents, 1)?;
            }
        }
        Ok(())
    }

    fn push_document(&mut self, document: CoreRecord) -> KiroSourceBackedResultV0<()> {
        self.page.push(document);
        self.peak_buffered_rows = self.peak_buffered_rows.max(
            u64::try_from(self.page.len()).map_err(|_| KiroSourceBackedErrorV0::CountOverflow)?,
        );
        if self.page.len() == SOURCE_BACKED_PAGE_ROWS {
            let page =
                std::mem::replace(&mut self.page, Vec::with_capacity(SOURCE_BACKED_PAGE_ROWS));
            (self.emit)(page)?;
            self.emitted_pages = checked_add(self.emitted_pages, 1)?;
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
                .map_err(|_| KiroSourceBackedErrorV0::CountOverflow)?
                .to_be_bytes(),
        );
        self.content_digest.update(relation);
        self.counts.certified_bytes = checked_add(
            self.counts.certified_bytes,
            u64::try_from(relation.len())
                .map_err(|_| KiroSourceBackedErrorV0::CountOverflow)?
                .checked_add(9)
                .ok_or(KiroSourceBackedErrorV0::CountOverflow)?,
        )?;
        Ok(())
    }

    fn finish(mut self) -> KiroSourceBackedResultV0<StreamedLogicalRows> {
        if !self.page.is_empty() {
            (self.emit)(std::mem::take(&mut self.page))?;
            self.emitted_pages = checked_add(self.emitted_pages, 1)?;
        }
        Ok(StreamedLogicalRows {
            counts: self.counts,
            content_digest: self.content_digest.finalize().into(),
            emitted_pages: self.emitted_pages,
            row_decode_passes: self.row_decode_passes,
            decoded_rows: self.decoded_rows,
            peak_buffered_rows: self.peak_buffered_rows,
        })
    }
}

#[derive(Debug)]
struct StreamedLogicalRows {
    counts: ScannedSourceCounts,
    content_digest: [u8; 32],
    emitted_pages: u64,
    row_decode_passes: u64,
    decoded_rows: u64,
    peak_buffered_rows: u64,
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
fn kiro_core_record(
    source: &SourceKey,
    session_id: StableEntityId,
    row: &KiroConversationRow,
    provider_session_id: &str,
    _source_path: &str,
    entry: &Value,
    event: super::super::event::KiroNativeEvent,
    complete_text: String,
    _record_digest: [u8; 32],
) -> KiroSourceBackedResultV0<CoreRecord> {
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
        TypedKey::utf8(event.cursor)?,
    ])?;
    let touched_files = projected_touched_files(event.event_type, entry)?;
    let body = complete_text;
    if body.is_empty() {
        return Err(KiroSourceBackedErrorV0::UncertifiableRow {
            relation: row.table,
            rowid: row.rowid,
            reason: "parser emitted empty normalized content",
        });
    }
    let mut record = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        event.provider_event_index,
        event.event_type.as_str(),
        AgentType::Primary.as_str(),
        true,
        KIRO_SOURCE_BACKED_PARSER_REVISION,
        body,
    )?;
    record.provider_session_id = Some(provider_session_id.to_owned());
    record.native_event_id = Some(primary_key);
    record.occurred_at_unix_ms = Some(event.occurred_at.timestamp_millis());
    record.role = event.role.map(|role| role.as_str().to_owned());
    record.cwd = (!row.key.trim().is_empty()).then(|| row.key.clone());
    if !touched_files.is_empty() {
        record.metadata.insert(
            "provider_native_file_touches".to_owned(),
            serde_json::json!(touched_files),
        );
    }
    record.validate_contract()?;
    Ok(record)
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

pub(super) fn phase_is_present(tables: KiroTables, phase: KiroPhase) -> bool {
    match phase {
        KiroPhase::V2 => tables.v2,
        KiroPhase::Legacy => tables.legacy,
    }
}

pub(super) fn canonical_row_digest(
    row: &KiroConversationRow,
) -> KiroSourceBackedResultV0<([u8; 32], u64)> {
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

fn hash_projected_document(digest: &mut Sha256, document: &CoreRecord) {
    digest.update(b"retained\0");
    hash_unchecked(
        digest,
        document.provider_session_id.as_deref().unwrap_or_default(),
    );
    digest.update(document.event_sequence.to_le_bytes());
    digest.update(
        document
            .occurred_at_unix_ms
            .unwrap_or_default()
            .to_le_bytes(),
    );
    hash_unchecked(digest, &document.event_type);
    hash_unchecked(digest, document.role.as_deref().unwrap_or_default());
    hash_unchecked(
        digest,
        document
            .content
            .normalized_body
            .as_deref()
            .unwrap_or_default(),
    );
    hash_unchecked(digest, document.cwd.as_deref().unwrap_or_default());
    if let Some(paths) = document.metadata.get("provider_native_file_touches") {
        hash_unchecked(digest, &paths.to_string());
    }
}

fn hash_projection_counts(
    digest: &mut Sha256,
    before: ScannedSourceCounts,
    after: ScannedSourceCounts,
) {
    digest.update(b"projection-counts\0");
    digest.update(
        after
            .complete_records
            .saturating_sub(before.complete_records)
            .to_le_bytes(),
    );
    digest.update(
        after
            .retained_records
            .saturating_sub(before.retained_records)
            .to_le_bytes(),
    );
    digest.update(
        after
            .rejected_records
            .saturating_sub(before.rejected_records)
            .to_le_bytes(),
    );
    digest.update(
        after
            .ignored_records
            .saturating_sub(before.ignored_records)
            .to_le_bytes(),
    );
}

fn hash_unchecked(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value.as_bytes());
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

pub(super) fn checked_add(left: u64, right: u64) -> KiroSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(KiroSourceBackedErrorV0::CountOverflow)
}

#[cfg(test)]
#[path = "source_backed_tests.rs"]
mod tests;
