use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    ContentRef, Event, EventRole, EventType, Fidelity, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
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
            ExactLegacySourceEventCandidate,
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

const CORE_CURSOR_VERSION: u32 = 1;
const OUTPUT_FRONTIER_VERSION: u32 = 1;
const CORE_PARSER_REVISION: &str = "lingma-nativepath-core-v1";
const OUTPUT_PARSER_REVISION: &str = "lingma-nativepath-output-v1";
const LEGACY_POSITION_KIND: &str = "lingma-chat-record-rowid-v5";
const LOCATOR_KIND: &str = "lingma-chat-record-v1";
const CORE_HASH_DOMAIN: &[u8] = b"ctx-lingma-nativepath-core-prefix-v1\0";
const EVENT_HASH_DOMAIN: &[u8] = b"ctx-lingma-nativepath-event-v1\0";
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
            return retire_missing_source(path, store, &context);
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
    let raw_source_path = context
        .source_path
        .as_deref()
        .unwrap_or(path)
        .display()
        .to_string();
    let source_root = context
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
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
        locator_identity,
        cursor_stream,
        source_revision,
        user_version,
        schema_fingerprint,
        encoding,
    };

    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
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

    replay_outputs_or_mark_behind(
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
        let page_summary = publish_core_page(
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

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &Store,
    bulk_guard: &ctx_history_store::EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    authority: &SourceAuthority,
    expected_cursor: Option<&str>,
    page: &CorePage,
) -> Result<ProviderImportSummary> {
    let next_cursor = provider_sync_cursor(
        &context.machine_id,
        authority.cursor_stream.clone(),
        page.checkpoint.encode()?,
        context.imported_at,
    );
    let transition =
        NativePathCursorTransition::new(expected_cursor.map(str::to_owned), next_cursor);
    let publication_id = publication_id(authority, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, page.retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.skipped_events = page
                .rows
                .iter()
                .filter_map(|(_, row)| match row {
                    PreparedRow::Accepted(row) => Some(lingma_event_count(row)),
                    PreparedRow::Skipped | PreparedRow::Rejected(_) => None,
                })
                .sum();
            summary.skipped = summary.skipped_events;
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        Some(&authority.source_root),
        Some(&authority.raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Lingma NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Lingma,
            source_format: LINGMA_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: authority.locator_identity.clone(),
            cursor_stream: authority.cursor_stream.clone(),
            proposed_source_identity,
            raw_source_path: Some(authority.raw_source_path.clone()),
            source_revision: authority.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let source_id = native_source_id(&resolution.canonical_source_identity);
    let source = capture_source(
        store,
        source_id,
        context,
        authority,
        &resolution.canonical_source_identity,
    )?;
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

    let mut summary = ProviderImportSummary::default();
    let sessions = prepared_sessions(
        store,
        context,
        options,
        source_id,
        &resolution.canonical_source_identity,
        page,
    )?;
    for (session, existed) in sessions.values() {
        group.upsert_session(session)?;
        if *existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }

    for (candidate, prepared) in &page.rows {
        match prepared {
            PreparedRow::Accepted(row) => {
                let session = sessions
                    .get(&row.session_id)
                    .map(|(session, _)| session)
                    .ok_or(CaptureError::SystemInvariant(
                        "Lingma NativePath lost a prepared session",
                    ))?;
                publish_row_events(
                    &mut group,
                    store,
                    context,
                    options,
                    source_id,
                    session,
                    row,
                    &mut summary,
                )?;
            }
            PreparedRow::Skipped => {
                summary.skipped = summary.skipped.saturating_add(1);
            }
            PreparedRow::Rejected(reason) => {
                summary.record_failure(ProviderImportFailure {
                    line: usize::try_from(candidate.rowid).unwrap_or(usize::MAX),
                    error: reason.clone(),
                });
            }
        }
    }

    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn capture_source(
    store: &Store,
    source_id: Uuid,
    context: &ProviderAdapterContext,
    authority: &SourceAuthority,
    canonical_source_identity: &str,
) -> Result<CaptureSource> {
    let existing = store.get_capture_source(source_id).ok();
    Ok(CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Lingma,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(authority.raw_source_path.clone()),
            source_format: Some(LINGMA_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(authority.source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: None,
        },
        started_at: existing
            .as_ref()
            .map_or(context.imported_at, |source| source.started_at),
        ended_at: existing.as_ref().and_then(|source| source.ended_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": Value::Null,
                "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": authority.source_root,
                "source_revision": authority.source_revision,
                "sqlite_user_version": authority.user_version,
                "schema_fingerprint": authority.schema_fingerprint,
                "source_table": "chat_record",
                "source_fidelity": "user prompts plus assistant summaries/errors",
                "assistant_content_caveat": "assistant events are summaries/errors; original assistant answers may be encrypted, transformed, or unavailable",
            }),
        ),
    })
}

fn prepared_sessions(
    store: &Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_id: Uuid,
    canonical_source_identity: &str,
    page: &CorePage,
) -> Result<BTreeMap<String, (Session, bool)>> {
    let mut bounds = BTreeMap::<String, (DateTime<Utc>, DateTime<Utc>)>::new();
    for (_, prepared) in &page.rows {
        let PreparedRow::Accepted(row) = prepared else {
            continue;
        };
        let occurred_at = lingma_timestamp(row.gmt_create, context.imported_at);
        let ended_at = occurred_at
            .checked_add_signed(Duration::milliseconds(100))
            .unwrap_or(occurred_at);
        bounds
            .entry(row.session_id.clone())
            .and_modify(|(started, ended)| {
                *started = (*started).min(occurred_at);
                *ended = (*ended).max(ended_at);
            })
            .or_insert((occurred_at, ended_at));
    }

    bounds
        .into_iter()
        .map(|(provider_session_id, (page_started, page_ended))| {
            let id = provider_import_session_uuid(
                store,
                CaptureProvider::Lingma,
                &provider_session_id,
                source_id,
                Some(canonical_source_identity),
            )?;
            let existing = store.get_session(id).ok();
            let started_at = existing
                .as_ref()
                .map_or(page_started, |session| session.started_at.min(page_started));
            let ended_at = existing
                .as_ref()
                .and_then(|session| session.ended_at)
                .map_or(page_ended, |ended| ended.max(page_ended));
            let session = Session {
                id,
                history_record_id: options.history_record_id,
                parent_session_id: None,
                root_session_id: None,
                capture_source_id: Some(source_id),
                provider: CaptureProvider::Lingma,
                external_session_id: Some(provider_session_id.clone()),
                external_agent_id: None,
                agent_type: AgentType::Primary,
                role_hint: Some("primary".to_owned()),
                is_primary: true,
                status: SessionStatus::Imported,
                transcript_blob_id: None,
                started_at,
                ended_at: Some(ended_at),
                timestamps: timestamps(context.imported_at),
                sync: provider_sync_metadata(
                    Fidelity::Partial,
                    json!({
                        "provider_session_id": provider_session_id,
                        "parent_provider_session_id": Value::Null,
                        "root_provider_session_id": Value::Null,
                        "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
                        "source_trust": "provider_native",
                        "imported_at": context.imported_at,
                        "metadata": {
                            "source_table": "chat_record",
                            "source_fidelity": "partial",
                            "session_metadata_fidelity": "row-local temporal bounds",
                            "assistant_content_caveat": "assistant events are summaries/errors, not guaranteed full assistant bodies",
                        },
                    }),
                ),
            };
            Ok((provider_session_id, (session, existing.is_some())))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn publish_row_events(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    store: &Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_id: Uuid,
    session: &Session,
    row: &LingmaRow,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let base_index = event_base_index(row);
    let user = provider_event(
        row,
        EventDraft {
            provider_event_index: base_index,
            role: EventRole::User,
            event_type: EventType::Message,
            occurred_at: lingma_timestamp(row.gmt_create, context.imported_at),
            text: row.chat_prompt.clone(),
            body_kind: "chat_prompt",
            fidelity: Fidelity::Imported,
        },
        true,
    )?;
    publish_event(
        group, store, context, options, source_id, session, user, row.rowid, 0, summary,
    )?;

    if let Some((text, body_kind, event_type)) = assistant_text(row) {
        let occurred_at = lingma_timestamp(row.gmt_create, context.imported_at)
            .checked_add_signed(Duration::milliseconds(100))
            .unwrap_or_else(|| lingma_timestamp(row.gmt_create, context.imported_at));
        let assistant = provider_event(
            row,
            EventDraft {
                provider_event_index: base_index.saturating_add(1),
                role: EventRole::Assistant,
                event_type,
                occurred_at,
                text,
                body_kind,
                fidelity: Fidelity::SummaryOnly,
            },
            false,
        )?;
        publish_event(
            group, store, context, options, source_id, session, assistant, row.rowid, 1, summary,
        )?;
    }
    Ok(())
}

struct EventDraft {
    provider_event_index: u64,
    role: EventRole,
    event_type: EventType,
    occurred_at: DateTime<Utc>,
    text: String,
    body_kind: &'static str,
    fidelity: Fidelity,
}

fn provider_event(
    row: &LingmaRow,
    draft: EventDraft,
    attach_complete_prompt: bool,
) -> Result<LingmaCoreEvent> {
    let role_name = draft.role.as_str();
    let provider_event_hash = lingma_event_hash(row, &draft);
    let body = json!({
        "rowid": row.rowid,
        "session_id": row.session_id,
        "request_id": row.request_id,
        "role": role_name,
        "body_kind": draft.body_kind,
        "gmt_create": row.gmt_create,
    });
    let retained_text = provider_policy_event_text(draft.event_type, &draft.text, &body);
    let result_evidence = provider_result_identifier_evidence(draft.event_type, &draft.text, &body);
    let result_outcome = provider_result_outcome_evidence(draft.event_type, &body);
    let mut event = LingmaCoreEvent {
        provider_event_index: draft.provider_event_index,
        provider_event_hash,
        cursor: format!(
            "chat_record:{}:rowid:{}:{role_name}",
            row.session_id, row.rowid
        ),
        event_type: draft.event_type,
        role: Some(draft.role),
        occurred_at: draft.occurred_at,
        fidelity: draft.fidelity,
        idempotency_key: format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Lingma.as_str(),
            row.session_id,
            draft.provider_event_index
        ),
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "body": provider_capped_json(
                &provider_policy_body(draft.event_type, &body),
                PROVIDER_MAX_PREVIEW_CHARS,
            ),
        }),
        metadata: json!({
            "source": "lingma_chat_record",
            "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
            "rowid": row.rowid,
            "session_id": row.session_id,
            "request_id": row.request_id,
            "body_kind": draft.body_kind,
            "gmt_create": row.gmt_create,
            "content_fidelity": if draft.fidelity == Fidelity::SummaryOnly {
                "summary_only"
            } else {
                "imported"
            },
            "assistant_content_caveat": if draft.role == EventRole::Assistant {
                Some("summary/error_result only; original assistant body may be encrypted or unavailable")
            } else {
                None
            },
        }),
    };
    if attach_complete_prompt {
        let locator = lingma_locator(row.rowid)?;
        let values = native_values(row);
        attach_lingma_complete_content_locator(&mut event, &locator, &values, &row.chat_prompt)?;
    }
    Ok(event)
}

fn attach_lingma_complete_content_locator(
    event: &mut LingmaCoreEvent,
    locator: &NativeLocator,
    values: &[NativeSqliteValue],
    complete_text: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || event
            .payload
            .pointer("/text_retention/truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("SQLite content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "supported Lingma message route has no verified-content profile",
    ))?;
    let persisted = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        locator.kind(),
        locator.value(),
        event.provider_event_hash.clone(),
        lingma_logical_record_digest(values)?,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Lingma complete-content locator exceeds the bounded canonical schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, persisted).ok_or(
        CaptureError::SystemInvariant("Lingma verified-content locator collection is malformed"),
    )?;
    Ok(())
}

fn lingma_logical_record_digest(values: &[NativeSqliteValue]) -> Result<CompleteContentBodyDigest> {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update((values.len() as u64).to_be_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_be_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                digest.update((value.len() as u64).to_be_bytes());
                digest.update(value);
            }
        }
    }
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize())).ok_or(
        CaptureError::SystemInvariant("Lingma logical-row digest is not canonical SHA-256"),
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    store: &Store,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_id: Uuid,
    session: &Session,
    event: LingmaCoreEvent,
    raw_ordinal: i64,
    sub_ordinal: u32,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let provider_event_hash = event.provider_event_hash.as_str();
    let legacy_raw_source_path = context
        .source_path
        .as_deref()
        .map(|path| path.display().to_string());
    let identity = provider_event_import_identity_with_exact_legacy_source(
        store,
        CaptureProvider::Lingma,
        provider_session_id,
        source_id,
        event.provider_event_index,
        u64::from(sub_ordinal),
        provider_event_hash,
        Some(ExactLegacySourceEventCandidate {
            source_id: provider_scoped_source_uuid(
                CaptureProvider::Lingma,
                provider_session_id,
                LINGMA_SQLITE_SOURCE_FORMAT,
                legacy_raw_source_path.as_deref(),
            ),
            provider_event_index: event.provider_event_index,
        }),
        u64::try_from(raw_ordinal).ok(),
        session.id == provider_session_uuid(CaptureProvider::Lingma, provider_session_id),
    )?;
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        provider_event_hash,
    )
    .unwrap_or(identity.dedupe_key);
    let mut provider_metadata = event.metadata;
    let verified_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": provider_event_hash,
        "provider_event_hash_authority": "normalized_payload_fallback",
        "cursor": event.cursor,
        "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": raw_ordinal.saturating_add(1),
        "imported_at": context.imported_at,
        "source_record_ordinal": raw_ordinal,
        "source_record_subrecord_index": sub_ordinal,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (sync_metadata.as_object_mut(), verified_locators) {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Lingma.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": provider_event_hash,
            "cursor": event.cursor,
            "artifacts": [],
            "body": crate::provider::importer::compact_provider_result_payload(
                event.event_type,
                &event.payload,
            ),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(event.fidelity, sync_metadata),
    };
    if group.reconcile_provider_event(
        &normalized,
        ProviderEventHashAuthority::NormalizedPayloadFallback,
    )? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(())
}

fn replay_outputs_or_mark_behind(
    store: &Store,
    machine_id: &str,
    authority: &SourceAuthority,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_outputs(store, machine_id, authority, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "lingma_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_outputs(
    store: &Store,
    machine_id: &str,
    authority: &SourceAuthority,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    committed_replay_authority(store, machine_id, authority)?;
    let source = OutputSourceIdentity {
        provider: CaptureProvider::Lingma.as_str().to_owned(),
        namespace_id: authority.source_root.clone(),
        source_id: authority.locator_identity.clone(),
    };
    let progress = match sink.observe_source(&source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    if progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress.observed_revision == authority.source_revision
            && progress.terminal
            && progress
                .cursor
                .as_ref()
                .and_then(|cursor| {
                    (cursor.version == OUTPUT_FRONTIER_VERSION)
                        .then(|| serde_json::from_slice::<OutputCheckpoint>(&cursor.payload).ok())
                        .flatten()
                })
                .is_some_and(|checkpoint| {
                    checkpoint.version == OUTPUT_FRONTIER_VERSION
                        && checkpoint.parser_revision == OUTPUT_PARSER_REVISION
                        && checkpoint.locator_identity == authority.locator_identity
                        && checkpoint.source_revision == authority.source_revision
                        && checkpoint.terminal
                })
    }) {
        return Ok(());
    }

    let prior_frontier = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let checkpoint = OutputCheckpoint {
        version: OUTPUT_FRONTIER_VERSION,
        parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
        locator_identity: authority.locator_identity.clone(),
        source_revision: authority.source_revision.clone(),
        terminal: true,
    };
    let next_frontier =
        NativeSafeFrontier::new(OUTPUT_FRONTIER_VERSION, serde_json::to_vec(&checkpoint)?)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let initial_frontier = NativeSafeFrontier::new(
        OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&OutputCheckpoint {
            version: OUTPUT_FRONTIER_VERSION,
            parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
            locator_identity: authority.locator_identity.clone(),
            source_revision: String::new(),
            terminal: false,
        })?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let rewrite = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision != OUTPUT_PARSER_REVISION
            || progress.materializer_revision != sink.materializer_revision()
            || progress.observed_revision != authority.source_revision
            || progress.terminal
    });
    let (source_epoch, expected_epoch, disposition) = match progress.as_ref() {
        None => (0, None, ProOutputSourceDisposition::NewSource),
        Some(progress) if rewrite => (
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Lingma output source epoch exhausted",
                ))?,
            Some(progress.source_epoch),
            ProOutputSourceDisposition::Rewrite,
        ),
        Some(progress) => (
            progress.source_epoch,
            Some(progress.source_epoch),
            ProOutputSourceDisposition::AppendOrResume,
        ),
    };
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source,
        source_epoch,
        observed_revision: authority.source_revision.clone(),
        parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition,
        expected_prior_source_epoch: expected_epoch,
        expected_prior_frontier: prior_frontier.clone(),
        observations: Vec::new(),
    };
    let accounting = NativePageAccounting {
        logical_units: 1,
        conservative_serialized_bytes: CORE_PAGE_FIXED_BYTES
            .saturating_add(authority.locator_identity.len())
            .saturating_add(authority.source_revision.len())
            .saturating_add(authority.source_root.len()),
    };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(
            CaptureProvider::Lingma.as_str(),
            &authority.locator_identity,
        ),
        prior_frontier.unwrap_or(initial_frontier),
        next_frontier,
        true,
        accounting,
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let _ = process_pro_replay_only(replay, sink);
    Ok(())
}

fn committed_replay_authority(
    store: &Store,
    machine_id: &str,
    authority: &SourceAuthority,
) -> Result<CoreCheckpoint> {
    let stored = store
        .get_sync_cursor(None, machine_id, &authority.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Lingma output replay requires committed terminal NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor).map_err(|_| {
        CaptureError::InvalidPayload(
            "Lingma output replay requires a Store-committed NativePath Core cursor".to_owned(),
        )
    })?;
    let checkpoint: CoreCheckpoint =
        serde_json::from_str(committed.provider_cursor()).map_err(|_| {
            CaptureError::InvalidPayload(
                "Lingma output replay requires committed Lingma Core authority".to_owned(),
            )
        })?;
    checkpoint.validate(&authority.locator_identity)?;
    if !checkpoint.terminal || checkpoint.source_revision != authority.source_revision {
        return Err(CaptureError::InvalidPayload(
            "Lingma output replay source does not exactly match committed terminal Core authority"
                .to_owned(),
        ));
    }
    Ok(checkpoint)
}

#[derive(Clone)]
struct KnownRoute {
    raw_source_path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    cursor: SyncCursor,
}

fn retire_missing_source(
    requested_path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
) -> Result<ProviderImportSummary> {
    let requested = requested_path.display().to_string();
    let mut known = Vec::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Lingma
            || source.descriptor.machine_id != context.machine_id
            || source.descriptor.source_format.as_deref() != Some(LINGMA_SQLITE_SOURCE_FORMAT)
        {
            continue;
        }
        let Some(raw_source_path) = source.descriptor.raw_source_path.as_deref() else {
            continue;
        };
        if raw_source_path != requested
            && context
                .source_path
                .as_deref()
                .is_none_or(|path| path.display().to_string() != raw_source_path)
        {
            continue;
        }
        let Some(canonical_source_identity) = source.descriptor.source_identity.as_deref() else {
            continue;
        };
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let raw_source_path = PathBuf::from(raw_source_path);
        let locator_identity = provider_path_identity(&raw_source_path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Lingma,
            LINGMA_SQLITE_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(cursor) = store.get_sync_cursor(None, &context.machine_id, &stream)? else {
            continue;
        };
        known.push(KnownRoute {
            raw_source_path,
            locator_identity,
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            cursor,
        });
    }
    if known.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: requested_path.to_path_buf(),
            reason: "Lingma SQLite source does not exist",
        });
    }
    if known.len() != 1 {
        return Err(CaptureError::SystemInvariant(
            "Lingma NativePath found ambiguous current routes for one source",
        ));
    }
    let route = known.pop().ok_or(CaptureError::SystemInvariant(
        "Lingma NativePath missing-route inventory changed",
    ))?;
    let committed = decode_native_path_committed_cursor(&route.cursor.cursor)?;
    let transition = NativePathCursorTransition::new(
        Some(route.cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            route.cursor.stream.clone(),
            committed.provider_cursor().to_owned(),
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Lingma,
        source_format: LINGMA_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity,
        cursor_stream: route.cursor.stream,
        expected_canonical_source_identity: route.canonical_source_identity,
        expected_source_revision: route.source_revision,
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason: if context
            .source_root
            .as_deref()
            .is_some_and(|root| !root.exists())
        {
            ProviderSourceRouteRetirementReason::RootMissing
        } else {
            ProviderSourceRouteRetirementReason::SourceMissing
        },
    };
    let publication_id = retirement_publication_id(&retirement, &route.raw_source_path);
    if committed.publication_id() == publication_id {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(0, 1, 0)?,
        )?;
        let changed =
            match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
                NativePathCursorSetClassification::AllExpected => {
                    let disposition = group.retire_provider_source_route(&retirement)?;
                    group.prepare_journal_checkpoint()?;
                    group.publish_cursor_set()?;
                    matches!(
                        disposition,
                        ProviderSourceRouteRetirementDisposition::Retired
                    )
                }
                NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
            };
        group.commit()?;
        let mut summary = ProviderImportSummary {
            skipped: usize::from(changed),
            skipped_sessions: usize::from(changed),
            ..ProviderImportSummary::default()
        };
        summary.set_work_result(if changed {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        });
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn provider_sync_cursor(
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Lingma.as_str(),
                machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor,
        last_synced_at: Some(observed_at),
        timestamps: timestamps(observed_at),
    }
}

fn publication_id(authority: &SourceAuthority, transition: &NativePathCursorTransition) -> String {
    let mut digest = Sha256::new();
    digest.update(PUBLICATION_DOMAIN);
    hash_bytes(&mut digest, authority.locator_identity.as_bytes());
    hash_bytes(&mut digest, authority.source_revision.as_bytes());
    hash_bytes(&mut digest, transition.key().stream().as_bytes());
    hash_optional_bytes(&mut digest, transition.expected_cursor().map(str::as_bytes));
    hash_bytes(&mut digest, transition.next().cursor.as_bytes());
    format!("lingma-nativepath-v1:{:x}", digest.finalize())
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement, path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(RETIREMENT_DOMAIN);
    hash_bytes(&mut digest, retirement.machine_id.as_bytes());
    hash_bytes(&mut digest, retirement.locator_identity.as_bytes());
    hash_bytes(
        &mut digest,
        retirement.expected_canonical_source_identity.as_bytes(),
    );
    hash_bytes(&mut digest, retirement.expected_source_revision.as_bytes());
    hash_bytes(&mut digest, path.as_os_str().as_encoded_bytes());
    format!("lingma-nativepath-retirement-v1:{:x}", digest.finalize())
}

fn native_source_id(canonical_source_identity: &str) -> Uuid {
    stable_capture_uuid(
        &format!(
            "native-path-provider-source-v1\0{}\0{}\0{}\0<database>",
            CaptureProvider::Lingma.as_str(),
            LINGMA_SQLITE_SOURCE_FORMAT,
            canonical_source_identity,
        ),
        "source",
    )
}

fn event_base_index(row: &LingmaRow) -> u64 {
    let rowid = u64::try_from(row.rowid).unwrap_or_else(|_| text_id_index(&row.session_id, 0));
    rowid.saturating_sub(1).saturating_mul(2)
}

fn lingma_timestamp(raw: Option<i64>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    raw.map(|timestamp| provider_timestamp_seconds(Some(timestamp as f64), fallback))
        .unwrap_or(fallback)
}

fn assistant_text(row: &LingmaRow) -> Option<(String, &'static str, EventType)> {
    if let Some(summary) = row
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Some((summary.to_owned(), "summary", EventType::Message));
    }
    row.error_result
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty() && *text != "{}")
        .map(|error| {
            (
                format!("Lingma error result: {error}"),
                "error_result",
                EventType::Notice,
            )
        })
}

fn lingma_event_hash(row: &LingmaRow, draft: &EventDraft) -> String {
    let mut digest = Sha256::new();
    digest.update(EVENT_HASH_DOMAIN);
    hash_bytes(&mut digest, row.session_id.as_bytes());
    hash_optional_bytes(&mut digest, row.request_id.as_deref().map(str::as_bytes));
    hash_bytes(&mut digest, &row.rowid.to_le_bytes());
    hash_bytes(&mut digest, &draft.provider_event_index.to_le_bytes());
    hash_bytes(&mut digest, draft.role.as_str().as_bytes());
    hash_bytes(&mut digest, draft.event_type.as_str().as_bytes());
    hash_bytes(&mut digest, draft.body_kind.as_bytes());
    hash_bytes(&mut digest, draft.text.as_bytes());
    hash_optional_i64(&mut digest, row.gmt_create);
    format!("{:x}", digest.finalize())
}

fn lingma_event_count(row: &LingmaRow) -> usize {
    1 + usize::from(assistant_text(row).is_some())
}

fn lingma_locator(rowid: i64) -> Result<NativeLocator> {
    NativeLocator::new(LOCATOR_KIND, ordered_i64(rowid).to_be_bytes().to_vec())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn ordered_i64(value: i64) -> u64 {
    (value as u64) ^ (1_u64 << 63)
}

fn native_values(row: &LingmaRow) -> Vec<NativeSqliteValue> {
    vec![
        NativeSqliteValue::Integer(row.rowid),
        NativeSqliteValue::Text(row.session_id.clone()),
        optional_native_text(row.request_id.clone()),
        NativeSqliteValue::Text(row.chat_prompt.clone()),
        optional_native_text(row.summary.clone()),
        optional_native_text(row.error_result.clone()),
        row.gmt_create
            .map_or(NativeSqliteValue::Null, NativeSqliteValue::Integer),
        optional_native_text(row.extra.clone()),
    ]
}

fn optional_native_text(value: Option<String>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Text)
}

pub(super) fn lingma_complete_values(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<Vec<NativeSqliteValue>>> {
    let encoding = detect_schema(conn)?;
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
    .optional()?
    .map(|raw| {
        decode_raw_row(raw, encoding)
            .map(|row| native_values(&row))
            .map_err(|_| {
                CaptureError::InvalidPayload(
                    "Lingma complete-content row contains malformed text encoding".to_owned(),
                )
            })
    })
    .transpose()
}

pub(super) fn lingma_complete_user_message(
    values: &[NativeSqliteValue],
) -> Result<(LingmaCoreEvent, String)> {
    let row = row_from_native_values(values)?;
    let text = row.chat_prompt.clone();
    let event = provider_event(
        &row,
        EventDraft {
            provider_event_index: event_base_index(&row),
            role: EventRole::User,
            event_type: EventType::Message,
            occurred_at: lingma_timestamp(row.gmt_create, DateTime::<Utc>::UNIX_EPOCH),
            text: text.clone(),
            body_kind: "chat_prompt",
            fidelity: Fidelity::Imported,
        },
        false,
    )?;
    Ok((event, text))
}

fn row_from_native_values(values: &[NativeSqliteValue]) -> Result<LingmaRow> {
    if values.len() != 8 {
        return Err(CaptureError::InvalidPayload(
            "Lingma logical row has an unexpected value count".to_owned(),
        ));
    }
    Ok(LingmaRow {
        rowid: native_integer(values, 0, "rowid")?,
        session_id: native_text(values, 1, "session_id")?,
        request_id: optional_native_text_value(values, 2, "request_id")?,
        chat_prompt: native_text(values, 3, "chat_prompt")?,
        summary: optional_native_text_value(values, 4, "summary")?,
        error_result: optional_native_text_value(values, 5, "error_result")?,
        gmt_create: optional_native_integer(values, 6, "gmt_create")?,
        extra: optional_native_text_value(values, 7, "extra")?,
    })
}

fn native_value<'a>(
    values: &'a [NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<&'a NativeSqliteValue> {
    values.get(index).ok_or_else(|| {
        CaptureError::InvalidPayload(format!("Lingma logical row is missing {field}"))
    })
}

fn native_text(values: &[NativeSqliteValue], index: usize, field: &str) -> Result<String> {
    match native_value(values, index, field)? {
        NativeSqliteValue::Text(value) => Ok(value.clone()),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be text"
        ))),
    }
}

fn optional_native_text_value(
    values: &[NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<String>> {
    match native_value(values, index, field)? {
        NativeSqliteValue::Null => Ok(None),
        NativeSqliteValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be text or null"
        ))),
    }
}

fn native_integer(values: &[NativeSqliteValue], index: usize, field: &str) -> Result<i64> {
    match native_value(values, index, field)? {
        NativeSqliteValue::Integer(value) => Ok(*value),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be an integer"
        ))),
    }
}

fn optional_native_integer(
    values: &[NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<i64>> {
    match native_value(values, index, field)? {
        NativeSqliteValue::Null => Ok(None),
        NativeSqliteValue::Integer(value) => Ok(Some(*value)),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Lingma logical row {field} must be an integer or null"
        ))),
    }
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(bytes);
}

fn hash_optional_bytes(hasher: &mut Sha256, bytes: Option<&[u8]>) {
    hasher.update([u8::from(bytes.is_some())]);
    if let Some(bytes) = bytes {
        hash_bytes(hasher, bytes);
    }
}

fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}

fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    hasher.update([u8::from(value.is_some())]);
    if let Some(value) = value {
        hasher.update(value.to_le_bytes());
    }
}
