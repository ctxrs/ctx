use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    compute_payload_hash, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor,
    CaptureSourceKind, ContentRef, Event, EventRole, EventType, Fidelity, Session, SessionStatus,
    SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderSourceLocatorObservation,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementDisposition,
    ProviderSourceRouteRetirementReason, Store,
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentRole,
        VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    native_source::{NativeLocator, NativeSqliteValue},
    provider::{
        importer::{
            provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
            provider_path_identity, provider_scoped_source_uuid, provider_session_uuid,
            provider_source_cursor_stream_for_path, provider_source_identity,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
        normalization::{
            provider_capped_json, provider_policy_body, provider_policy_event_text,
            provider_result_identifier_evidence, provider_result_outcome_evidence,
            provider_timestamp_seconds, text_id_index,
        },
        sqlite::{
            ensure_sqlite_table_columns, open_provider_sqlite_readonly, sqlite_schema_fingerprint,
            sqlite_table_columns, sqlite_table_exists, with_sqlite_read_snapshot,
            ProviderSqliteSourceSnapshot,
        },
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputSourceIdentity, ProOutputSink,
    ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    LINGMA_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

mod lifecycle;
mod publication;
mod records;
mod source_backed;

#[cfg(test)]
pub(super) use publication::lingma_logical_record_digest;
#[cfg(test)]
pub(super) use records::lingma_locator;
use records::{hash_bytes, hash_optional_bytes, hash_optional_i64, hash_optional_u64};
pub(super) use records::{lingma_complete_user_message, lingma_complete_values};
pub(crate) use source_backed::{
    scan_lingma_source_backed_v0, LingmaDatabaseScanV0, LingmaDatabaseSourceV0,
    LingmaSourceBackedErrorV0, LingmaSourceBackedRecordV0, LingmaSourceBackedResolverV0,
    LingmaSourceBackedResultV0, LingmaSourceBackedScanV0, LingmaSourceInventoryV0,
};

const CORE_CURSOR_VERSION: u32 = 1;
const OUTPUT_FRONTIER_VERSION: u32 = 1;
const CORE_PARSER_REVISION: &str = "lingma-nativepath-core-v1";
const OUTPUT_PARSER_REVISION: &str = "lingma-nativepath-output-v1";
const LEGACY_POSITION_KIND: &str = "lingma-chat-record-rowid-v5";
const LOCATOR_KIND: &str = "lingma-chat-record-v1";
const CORE_HASH_DOMAIN: &[u8] = b"ctx-lingma-nativepath-core-prefix-v1\0";
const PUBLICATION_DOMAIN: &[u8] = b"ctx-lingma-nativepath-publication-v1\0";
const RETIREMENT_DOMAIN: &[u8] = b"ctx-lingma-nativepath-retirement-v1\0";
const CORE_PAGE_MAX_ROWS: usize = 64;
const CORE_PAGE_LOOKAHEAD_ROWS: usize = CORE_PAGE_MAX_ROWS + 1;
const CORE_PAGE_MAX_SOURCE_BYTES: usize = 7 * 1024 * 1024;
const CORE_PAGE_FIXED_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SqliteEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CoreCheckpoint {
    version: u32,
    parser_revision: String,
    locator_identity: String,
    source_revision: String,
    generation: u64,
    next_rowid: Option<i64>,
    rows_seen: u64,
    prefix_sha256: [u8; 32],
    terminal: bool,
}

impl CoreCheckpoint {
    fn initial(locator_identity: String, source_revision: String, generation: u64) -> Self {
        Self {
            version: CORE_CURSOR_VERSION,
            parser_revision: CORE_PARSER_REVISION.to_owned(),
            locator_identity,
            source_revision,
            generation,
            next_rowid: None,
            rows_seen: 0,
            prefix_sha256: initial_prefix_digest(),
            terminal: false,
        }
    }

    fn validate(&self, locator_identity: &str) -> Result<()> {
        if self.version != CORE_CURSOR_VERSION
            || self.parser_revision != CORE_PARSER_REVISION
            || self.locator_identity != locator_identity
        {
            return Err(CaptureError::InvalidPayload(
                "Lingma NativePath cursor authority does not match this source".to_owned(),
            ));
        }
        if self.next_rowid.is_none() && self.rows_seen != 0 {
            return Err(CaptureError::InvalidPayload(
                "Lingma NativePath cursor has rows without a rowid frontier".to_owned(),
            ));
        }
        Ok(())
    }

    fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(CaptureError::from)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputCheckpoint {
    version: u32,
    parser_revision: String,
    locator_identity: String,
    source_revision: String,
    terminal: bool,
}

#[derive(Clone)]
struct SourceAuthority {
    raw_source_path: String,
    source_root: String,
    display_source_path: String,
    locator_identity: String,
    cursor_stream: String,
    source_revision: String,
    user_version: i64,
    schema_fingerprint: String,
    encoding: SqliteEncoding,
}

struct ResumeState {
    expected_cursor: Option<String>,
    checkpoint: CoreCheckpoint,
    hasher: Sha256,
    no_op: bool,
}

#[derive(Clone)]
struct Candidate {
    rowid: i64,
    encoded_bytes: usize,
    field_bytes: [Option<usize>; 6],
    gmt_create: Option<i64>,
}

impl Candidate {
    fn required_fields_present(&self) -> bool {
        self.field_bytes[0].is_some() && self.field_bytes[2].is_some()
    }

    fn can_hydrate(&self) -> bool {
        self.required_fields_present() && self.encoded_bytes <= CORE_PAGE_MAX_SOURCE_BYTES
    }
}

#[derive(Clone)]
struct LingmaRow {
    rowid: i64,
    session_id: String,
    request_id: Option<String>,
    chat_prompt: String,
    summary: Option<String>,
    error_result: Option<String>,
    gmt_create: Option<i64>,
    extra: Option<String>,
}

pub(super) struct LingmaCoreEvent {
    pub(super) provider_event_index: u64,
    pub(super) provider_event_hash: String,
    pub(super) released_provider_event_hash: String,
    pub(super) cursor: String,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) fidelity: Fidelity,
    pub(super) idempotency_key: String,
    pub(super) payload: Value,
    pub(super) metadata: Value,
}

enum PreparedRow {
    Accepted(LingmaRow),
    Skipped,
    Rejected(String),
}

struct CorePage {
    rows: Vec<(Candidate, PreparedRow)>,
    checkpoint: CoreCheckpoint,
    hasher: Sha256,
    retained_bytes: usize,
}

pub(super) fn import_lingma_native_path(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if context.source_path.is_none() {
        context.source_path = Some(path.to_path_buf());
    }

    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if options.import_profile.is_replay_only() {
                if let Some(sink) = options.import_profile.sink() {
                    sink.mark_behind(ProOutputSinkError::new(
                        "lingma_nativepath_source_missing",
                        "Lingma output replay source is missing",
                    ));
                }
                return Ok(ProviderImportSummary::default());
            }
            return lifecycle::retire_missing_source(path, store, &context);
        }
        Err(error) => return Err(error.into()),
    }

    let canonical_path = fs::canonicalize(path)?;
    let snapshot = lingma_source_snapshot(path)?;
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let encoding = detect_schema(&conn)?;
    let user_version = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let display_source_path = context
        .source_path
        .as_deref()
        .unwrap_or(path)
        .display()
        .to_string();
    let raw_source_path = canonical_path.display().to_string();
    let source_root = raw_source_path.clone();
    let locator_identity = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let source_revision = source_revision(
        &snapshot,
        user_version,
        &schema_fingerprint,
        options.inventory_observation_token.as_deref(),
    );
    let authority = SourceAuthority {
        raw_source_path,
        source_root,
        display_source_path,
        locator_identity,
        cursor_stream,
        source_revision,
        user_version,
        schema_fingerprint,
        encoding,
    };

    if options.import_profile.is_replay_only() {
        publication::replay_outputs_or_mark_behind(
            store,
            &context.machine_id,
            &authority,
            options.import_profile.sink().map(AsRef::as_ref),
        );
        return Ok(ProviderImportSummary::default());
    }

    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = import_core(
        path,
        &snapshot,
        &conn,
        store,
        &bulk_guard,
        &context,
        &options,
        &authority,
    );
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let mut summary = match (operation, finish) {
        (Ok(summary), Ok(())) => summary,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };

    publication::replay_outputs_or_mark_behind(
        store,
        &context.machine_id,
        &authority,
        options.import_profile.sink().map(AsRef::as_ref),
    );
    if summary.work_result() == ProviderImportWorkResult::Changed {
        summary.set_work_result(ProviderImportWorkResult::Changed);
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn import_core(
    path: &Path,
    snapshot: &ProviderSqliteSourceSnapshot,
    conn: &Connection,
    store: &Store,
    bulk_guard: &ctx_history_store::EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    authority: &SourceAuthority,
) -> Result<ProviderImportSummary> {
    let stored = store.get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?;
    let mut resume =
        with_sqlite_read_snapshot(conn, || resume_state(conn, authority, stored.as_ref()))?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    if resume.no_op {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let mut summary = ProviderImportSummary::default();
    loop {
        let page = with_sqlite_read_snapshot(conn, || {
            read_core_page(conn, authority, &resume.checkpoint, resume.hasher.clone())
        })?;
        if !snapshot.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let terminal = page.checkpoint.terminal;
        let page_summary = publication::publish_core_page(
            store,
            bulk_guard,
            context,
            options,
            authority,
            resume.expected_cursor.as_deref(),
            &page,
        )?;
        summary.merge_from(page_summary);
        resume.expected_cursor = store
            .get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?
            .map(|cursor| cursor.cursor);
        resume.checkpoint = page.checkpoint;
        resume.hasher = page.hasher;
        if terminal {
            break;
        }
        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
            summary.work_remaining = true;
            break;
        }
    }
    Ok(summary)
}

fn lingma_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "Lingma SQLite source must be a regular non-symlink file",
        "Lingma SQLite sidecar must be a regular non-symlink file",
    )
}

fn detect_schema(conn: &Connection) -> Result<SqliteEncoding> {
    if !sqlite_table_exists(conn, "chat_record")? {
        return Err(CaptureError::InvalidPayload(
            "Lingma local.db is missing required chat_record table".to_owned(),
        ));
    }
    let columns = sqlite_table_columns(conn, "chat_record")?;
    ensure_sqlite_table_columns(
        &columns,
        "Lingma chat_record table",
        &[
            "session_id",
            "request_id",
            "chat_prompt",
            "summary",
            "error_result",
            "gmt_create",
            "extra",
        ],
    )?;
    let encoding = conn.pragma_query_value(None, "encoding", |row| row.get::<_, String>(0))?;
    match encoding.as_str() {
        "UTF-8" => Ok(SqliteEncoding::Utf8),
        "UTF-16le" => Ok(SqliteEncoding::Utf16Le),
        "UTF-16be" => Ok(SqliteEncoding::Utf16Be),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma SQLite source uses unsupported text encoding {encoding}"
        ))),
    }
}

fn source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    user_version: i64,
    schema_fingerprint: &str,
    inventory_token: Option<&str>,
) -> String {
    let raw = format!(
        "lingma-nativepath-source-v1:parser={CORE_PARSER_REVISION};user_version={user_version};schema={schema_fingerprint};{}",
        snapshot.revision_component()
    );
    let Some(token) = inventory_token else {
        return raw;
    };
    let mut digest = Sha256::new();
    digest.update(b"ctx-lingma-nativepath-inventory-v1\0");
    hash_bytes(&mut digest, raw.as_bytes());
    hash_bytes(&mut digest, token.as_bytes());
    format!(
        "lingma-nativepath-inventory-sha256-v1:{:x}",
        digest.finalize()
    )
}

fn resume_state(
    conn: &Connection,
    authority: &SourceAuthority,
    stored: Option<&SyncCursor>,
) -> Result<ResumeState> {
    let Some(stored) = stored else {
        return Ok(ResumeState {
            expected_cursor: None,
            checkpoint: CoreCheckpoint::initial(
                authority.locator_identity.clone(),
                authority.source_revision.clone(),
                0,
            ),
            hasher: initial_prefix_hasher(),
            no_op: false,
        });
    };
    let expected_cursor = Some(stored.cursor.clone());
    let encoded_value = serde_json::from_str::<Value>(&stored.cursor).ok();
    let looks_store_owned = encoded_value
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|object| {
            object.contains_key("publication_id") && object.contains_key("provider_cursor")
        });
    let checkpoint = if looks_store_owned {
        let committed = decode_native_path_committed_cursor(&stored.cursor)?;
        serde_json::from_str::<CoreCheckpoint>(committed.provider_cursor()).map_err(|_| {
            CaptureError::InvalidPayload(
                "Lingma NativePath committed cursor payload is malformed".to_owned(),
            )
        })?
    } else {
        decode_released_cursor_for_migration(&stored.cursor)?;
        return Ok(ResumeState {
            expected_cursor,
            checkpoint: CoreCheckpoint::initial(
                authority.locator_identity.clone(),
                authority.source_revision.clone(),
                1,
            ),
            hasher: initial_prefix_hasher(),
            no_op: false,
        });
    };
    checkpoint.validate(&authority.locator_identity)?;

    if checkpoint.terminal && checkpoint.source_revision == authority.source_revision {
        return Ok(ResumeState {
            expected_cursor,
            checkpoint,
            hasher: initial_prefix_hasher(),
            no_op: true,
        });
    }

    let verified = verify_prefix(conn, authority.encoding, &checkpoint)?;
    if let Some(hasher) = verified {
        let mut resumed = checkpoint;
        resumed.source_revision = authority.source_revision.clone();
        resumed.terminal = false;
        return Ok(ResumeState {
            expected_cursor,
            checkpoint: resumed,
            hasher,
            no_op: false,
        });
    }

    let generation = checkpoint
        .generation
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "Lingma NativePath generation exhausted",
        ))?;
    Ok(ResumeState {
        expected_cursor,
        checkpoint: CoreCheckpoint::initial(
            authority.locator_identity.clone(),
            authority.source_revision.clone(),
            generation,
        ),
        hasher: initial_prefix_hasher(),
        no_op: false,
    })
}

fn decode_released_cursor_for_migration(encoded: &str) -> Result<()> {
    let certified = CertifiedProviderCursor::decode_if_certified(encoded)?.ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Lingma cursor is neither NativePath nor a released certified cursor".to_owned(),
        )
    })?;
    let position = certified.native_position();
    if position.kind() != LEGACY_POSITION_KIND {
        return Err(CaptureError::InvalidPayload(
            "Lingma released cursor has an unexpected native position".to_owned(),
        ));
    }
    let value = position.value();
    if value == [0]
        || (value.len() == 18 && value.first() == Some(&1) && matches!(value.last(), Some(0 | 1)))
    {
        return Ok(());
    }
    Err(CaptureError::InvalidPayload(
        "Lingma released cursor native position is malformed".to_owned(),
    ))
}

fn verify_prefix(
    conn: &Connection,
    encoding: SqliteEncoding,
    checkpoint: &CoreCheckpoint,
) -> Result<Option<Sha256>> {
    let Some(terminal_rowid) = checkpoint.next_rowid else {
        return Ok((checkpoint.rows_seen == 0
            && checkpoint.prefix_sha256 == initial_prefix_digest())
        .then(initial_prefix_hasher));
    };
    let mut hasher = initial_prefix_hasher();
    let mut after = None;
    let mut seen = 0_u64;
    loop {
        let candidates = load_candidates(conn, encoding, after, Some(terminal_rowid))?;
        if candidates.is_empty() {
            break;
        }
        for candidate in &candidates {
            let raw = candidate
                .can_hydrate()
                .then(|| load_raw_row(conn, candidate.rowid))
                .transpose()?;
            hash_candidate(&mut hasher, candidate, raw.as_ref());
            seen = seen.saturating_add(1);
            after = Some(candidate.rowid);
        }
        if after == Some(terminal_rowid) || candidates.len() < CORE_PAGE_LOOKAHEAD_ROWS {
            break;
        }
    }
    if after != Some(terminal_rowid)
        || seen != checkpoint.rows_seen
        || hasher.clone().finalize().as_slice() != checkpoint.prefix_sha256
    {
        return Ok(None);
    }
    Ok(Some(hasher))
}

fn read_core_page(
    conn: &Connection,
    authority: &SourceAuthority,
    checkpoint: &CoreCheckpoint,
    mut hasher: Sha256,
) -> Result<CorePage> {
    let candidates = load_candidates(conn, authority.encoding, checkpoint.next_rowid, None)?;
    if candidates.is_empty() {
        let mut next = checkpoint.clone();
        next.source_revision = authority.source_revision.clone();
        next.terminal = true;
        next.prefix_sha256 = hasher.clone().finalize().into();
        return Ok(CorePage {
            rows: Vec::new(),
            checkpoint: next,
            hasher,
            retained_bytes: CORE_PAGE_FIXED_BYTES,
        });
    }

    let selected_count = select_candidate_prefix(&candidates);
    let selected = &candidates[..selected_count];
    let terminal = candidates.len() == selected_count;
    let mut rows = Vec::with_capacity(selected.len());
    let mut retained_bytes = CORE_PAGE_FIXED_BYTES;
    for candidate in selected {
        let raw = candidate
            .can_hydrate()
            .then(|| load_raw_row(conn, candidate.rowid))
            .transpose()?;
        hash_candidate(&mut hasher, candidate, raw.as_ref());
        let prepared = if !candidate.required_fields_present() {
            PreparedRow::Rejected(format!(
                "Lingma SQLite row {} is missing required text",
                candidate.rowid
            ))
        } else if candidate.encoded_bytes > CORE_PAGE_MAX_SOURCE_BYTES {
            PreparedRow::Rejected(format!(
                "Lingma SQLite row {} exceeds the bounded NativePath page",
                candidate.rowid
            ))
        } else {
            let raw = raw.ok_or(CaptureError::SystemInvariant(
                "Lingma accepted row was not hydrated",
            ))?;
            match decode_raw_row(raw, authority.encoding) {
                Ok(row) if row.chat_prompt.trim().is_empty() => PreparedRow::Skipped,
                Ok(row) => PreparedRow::Accepted(row),
                Err(rowid) => PreparedRow::Rejected(format!(
                    "Lingma SQLite row {rowid} contains malformed text encoding"
                )),
            }
        };
        retained_bytes = retained_bytes.saturating_add(if candidate.can_hydrate() {
            candidate.encoded_bytes
        } else {
            128
        });
        rows.push((candidate.clone(), prepared));
    }
    let selected_u64 = u64::try_from(selected.len())
        .map_err(|_| CaptureError::SystemInvariant("Lingma page row count exceeds u64"))?;
    let rows_seen =
        checkpoint
            .rows_seen
            .checked_add(selected_u64)
            .ok_or(CaptureError::SystemInvariant(
                "Lingma NativePath row count exhausted",
            ))?;
    let next_rowid = selected.last().map(|candidate| candidate.rowid);
    let mut next = checkpoint.clone();
    next.source_revision = authority.source_revision.clone();
    next.next_rowid = next_rowid;
    next.rows_seen = rows_seen;
    next.prefix_sha256 = hasher.clone().finalize().into();
    next.terminal = terminal;
    Ok(CorePage {
        rows,
        checkpoint: next,
        hasher,
        retained_bytes,
    })
}

fn load_candidates(
    conn: &Connection,
    encoding: SqliteEncoding,
    after_rowid: Option<i64>,
    through_rowid: Option<i64>,
) -> Result<Vec<Candidate>> {
    let after = if after_rowid.is_some() {
        "c.rowid > ?1"
    } else {
        "?1 is null"
    };
    let through = if through_rowid.is_some() {
        "and c.rowid <= ?2"
    } else {
        "and ?2 is null"
    };
    let sql = format!(
        "select c.rowid, octet_length(c.session_id), octet_length(c.request_id), \
                octet_length(c.chat_prompt), octet_length(c.summary), \
                octet_length(c.error_result), octet_length(c.extra), \
                cast(c.gmt_create as integer) \
         from chat_record c where {after} {through} \
         order by c.rowid limit {CORE_PAGE_LOOKAHEAD_ROWS}"
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map((after_rowid, through_rowid), |row| {
        let raw_bytes = [
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, Option<i64>>(5)?,
            row.get::<_, Option<i64>>(6)?,
        ];
        Ok((row.get::<_, i64>(0)?, raw_bytes, row.get(7)?))
    })?;
    rows.map(|row| {
        let (rowid, raw, gmt_create) = row?;
        let mut field_bytes = [None; 6];
        for (index, raw) in raw.into_iter().enumerate() {
            field_bytes[index] = raw
                .map(|bytes| {
                    usize::try_from(bytes).map_err(|_| {
                        CaptureError::InvalidPayload(
                            "Lingma SQLite text length must be nonnegative".to_owned(),
                        )
                    })
                })
                .transpose()?
                .map(|bytes| retained_utf8_bound(bytes, encoding));
        }
        let encoded_bytes = field_bytes
            .iter()
            .flatten()
            .fold(128_usize, |total, bytes| total.saturating_add(*bytes));
        Ok(Candidate {
            rowid,
            encoded_bytes,
            field_bytes,
            gmt_create,
        })
    })
    .collect()
}

fn select_candidate_prefix(candidates: &[Candidate]) -> usize {
    let mut count = 0_usize;
    let mut bytes = CORE_PAGE_FIXED_BYTES;
    for candidate in candidates.iter().take(CORE_PAGE_MAX_ROWS) {
        let candidate_bytes = if candidate.can_hydrate() {
            candidate.encoded_bytes
        } else {
            128
        };
        if count != 0 && bytes.saturating_add(candidate_bytes) > CORE_PAGE_MAX_SOURCE_BYTES {
            break;
        }
        bytes = bytes.saturating_add(candidate_bytes);
        count = count.saturating_add(1);
        if bytes > CORE_PAGE_MAX_SOURCE_BYTES {
            break;
        }
    }
    count.max(1)
}

struct RawRow {
    rowid: i64,
    session_id: Option<Vec<u8>>,
    request_id: Option<Vec<u8>>,
    chat_prompt: Option<Vec<u8>>,
    summary: Option<Vec<u8>>,
    error_result: Option<Vec<u8>>,
    gmt_create: Option<i64>,
    extra: Option<Vec<u8>>,
}

fn load_raw_row(conn: &Connection, rowid: i64) -> Result<RawRow> {
    conn.query_row(
        "select c.rowid, cast(cast(c.session_id as text) as blob), \
                cast(cast(c.request_id as text) as blob), \
                cast(cast(c.chat_prompt as text) as blob), \
                cast(cast(c.summary as text) as blob), \
                cast(cast(c.error_result as text) as blob), \
                cast(c.gmt_create as integer), cast(cast(c.extra as text) as blob) \
         from chat_record c where c.rowid = ?1",
        [rowid],
        |row| {
            Ok(RawRow {
                rowid: row.get(0)?,
                session_id: row.get(1)?,
                request_id: row.get(2)?,
                chat_prompt: row.get(3)?,
                summary: row.get(4)?,
                error_result: row.get(5)?,
                gmt_create: row.get(6)?,
                extra: row.get(7)?,
            })
        },
    )
    .map_err(CaptureError::from)
}

fn decode_raw_row(row: RawRow, encoding: SqliteEncoding) -> std::result::Result<LingmaRow, i64> {
    let rowid = row.rowid;
    let required = |value: Option<Vec<u8>>| {
        value
            .and_then(|bytes| decode_sqlite_text(encoding, &bytes))
            .ok_or(rowid)
    };
    let optional = |value: Option<Vec<u8>>| {
        value
            .map(|bytes| decode_sqlite_text(encoding, &bytes).ok_or(rowid))
            .transpose()
    };
    Ok(LingmaRow {
        rowid,
        session_id: required(row.session_id)?,
        request_id: optional(row.request_id)?,
        chat_prompt: required(row.chat_prompt)?,
        summary: optional(row.summary)?,
        error_result: optional(row.error_result)?,
        gmt_create: row.gmt_create,
        extra: optional(row.extra)?,
    })
}

fn decode_sqlite_text(encoding: SqliteEncoding, bytes: &[u8]) -> Option<String> {
    match encoding {
        SqliteEncoding::Utf8 => std::str::from_utf8(bytes).ok().map(str::to_owned),
        SqliteEncoding::Utf16Le | SqliteEncoding::Utf16Be => {
            if !bytes.len().is_multiple_of(2) {
                return None;
            }
            let little_endian = encoding == SqliteEncoding::Utf16Le;
            let units = bytes.chunks_exact(2).map(|pair| {
                if little_endian {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            });
            char::decode_utf16(units)
                .collect::<std::result::Result<String, _>>()
                .ok()
        }
    }
}

fn retained_utf8_bound(bytes: usize, encoding: SqliteEncoding) -> usize {
    match encoding {
        SqliteEncoding::Utf8 => bytes,
        SqliteEncoding::Utf16Le | SqliteEncoding::Utf16Be => bytes.div_ceil(2).saturating_mul(3),
    }
}

fn initial_prefix_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(CORE_HASH_DOMAIN);
    hasher
}

fn initial_prefix_digest() -> [u8; 32] {
    initial_prefix_hasher().finalize().into()
}

fn hash_candidate(hasher: &mut Sha256, candidate: &Candidate, raw: Option<&RawRow>) {
    hasher.update(candidate.rowid.to_le_bytes());
    for bytes in candidate.field_bytes {
        hash_optional_u64(hasher, bytes.and_then(|value| u64::try_from(value).ok()));
    }
    hash_optional_i64(hasher, candidate.gmt_create);
    if let Some(raw) = raw {
        hash_optional_bytes(hasher, raw.session_id.as_deref());
        hash_optional_bytes(hasher, raw.request_id.as_deref());
        hash_optional_bytes(hasher, raw.chat_prompt.as_deref());
        hash_optional_bytes(hasher, raw.summary.as_deref());
        hash_optional_bytes(hasher, raw.error_result.as_deref());
        // `extra` can contain provider-private result bodies. Its byte count is
        // authority for Core; its body is never persisted into Core.
        hash_optional_bytes(hasher, raw.extra.as_deref());
    }
}
