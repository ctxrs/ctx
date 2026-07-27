use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Event,
    Fidelity, Run, Session, SessionEdge, SessionEdgeType, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
};
use rusqlite::{types::ValueRef, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::{VerifiedContentLocatorsV1, VERIFIED_CONTENT_LOCATORS_METADATA_KEY},
    native_source::NativeSqliteValue,
    provider::{
        importer::{
            compact_provider_result_payload, provider_command_run, provider_edge_uuid,
            provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
            provider_path_identity, provider_scoped_source_identity_key,
            provider_scoped_source_uuid, provider_session_uuid,
            provider_source_cursor_stream_for_path, provider_source_edge_uuid,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
        sqlite::{open_provider_sqlite_readonly, sqlite_schema_fingerprint},
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputSourceIdentity, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, SHELLEY_SQLITE_SOURCE_FORMAT,
};

use super::{
    normalization::{
        shelley_core_event, shelley_output_classification, shelley_output_observation,
        shelley_timestamp, ShelleyCoreEvent,
    },
    relationships::{decode_shelley_conversation, decode_shelley_message},
    source::{
        shelley_conversation_columns, shelley_conversation_select_expressions,
        shelley_message_columns, shelley_message_select_expressions, shelley_require_message_index,
        shelley_retained_length_expr, shelley_source_revision, shelley_source_snapshot,
        with_shelley_length_preflight,
    },
    ShelleyConversationRow, ShelleyMessageRow,
};

const SHELLEY_NATIVE_CURSOR_VERSION: u32 = 1;
const SHELLEY_OUTPUT_FRONTIER_VERSION: u32 = 1;
const SHELLEY_OUTPUT_PARSER_REVISION: &str = "shelley-nativepath-output-v1";
const SHELLEY_PREFIX_DOMAIN: &[u8] = b"ctx-shelley-nativepath-prefix-v1\0";
const SHELLEY_PAGE_MAX_UNITS: usize = 64;
const SHELLEY_PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;
const SHELLEY_ROW_MAX_BYTES: usize = 3 * 1024 * 1024;
const SHELLEY_PAGE_FIXED_OVERHEAD: usize = 64 * 1024;
const SHELLEY_INVENTORY_TOKEN_MAX_BYTES: usize = 4 * 1024;
const LEGACY_SHELLEY_POSITION_KIND: &str = "shelley-native-message-keyset-v9";
const LEGACY_SHELLEY_POSITION_BYTES: usize = 21;
const LEGACY_SHELLEY_CAPTURE_REVISION: u32 = 9;
const LEGACY_SHELLEY_POLICY_REVISION: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ShelleyPhase {
    Conversations,
    Messages,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShelleyPrefix {
    after_rowid: Option<i64>,
    count: u64,
    digest: [u8; 32],
}

impl ShelleyPrefix {
    fn initial(kind: u8) -> Self {
        let mut digest = Sha256::new();
        digest.update(SHELLEY_PREFIX_DOMAIN);
        digest.update([kind]);
        Self {
            after_rowid: None,
            count: 0,
            digest: digest.finalize().into(),
        }
    }

    fn advance(&mut self, rowid: i64, row_digest: [u8; 32]) -> Result<()> {
        let mut digest = Sha256::new();
        digest.update(SHELLEY_PREFIX_DOMAIN);
        digest.update(self.digest);
        digest.update(rowid.to_le_bytes());
        digest.update(row_digest);
        self.digest = digest.finalize().into();
        self.after_rowid = Some(rowid);
        self.count = self
            .count
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Shelley NativePath prefix count overflowed",
            ))?;
        Ok(())
    }

    fn validate(&self, kind: u8) -> bool {
        if self.count == 0 {
            self.after_rowid.is_none() && self.digest == Self::initial(kind).digest
        } else {
            self.after_rowid.is_some()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShelleyNativeCursor {
    version: u32,
    provider: String,
    database_path: PathBuf,
    path_identity: String,
    route_epoch: u64,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    schema_fingerprint: String,
    sqlite_user_version: i64,
    generation: u64,
    phase: ShelleyPhase,
    conversations: ShelleyPrefix,
    messages: ShelleyPrefix,
    terminal: bool,
    route_retired: bool,
}

impl ShelleyNativeCursor {
    fn fresh(
        database_path: PathBuf,
        path_identity: String,
        route_epoch: u64,
        canonical_source_identity: String,
        source_revision: String,
        schema_fingerprint: String,
        sqlite_user_version: i64,
        generation: u64,
    ) -> Self {
        Self {
            version: SHELLEY_NATIVE_CURSOR_VERSION,
            provider: CaptureProvider::Shelley.as_str().to_owned(),
            locator_identity: locator_identity(&path_identity, route_epoch),
            database_path,
            path_identity,
            route_epoch,
            canonical_source_identity,
            source_revision,
            schema_fingerprint,
            sqlite_user_version,
            generation,
            phase: ShelleyPhase::Conversations,
            conversations: ShelleyPrefix::initial(b'c'),
            messages: ShelleyPrefix::initial(b'm'),
            terminal: false,
            route_retired: false,
        }
    }

    fn validate(&self, database_path: &Path, path_identity: &str) -> Result<()> {
        if self.version != SHELLEY_NATIVE_CURSOR_VERSION
            || self.provider != CaptureProvider::Shelley.as_str()
            || self.database_path != database_path
            || self.path_identity != path_identity
            || self.locator_identity != locator_identity(path_identity, self.route_epoch)
            || self.terminal != (self.phase == ShelleyPhase::Complete)
            || self.route_retired && !self.terminal
            || !self.conversations.validate(b'c')
            || !self.messages.validate(b'm')
            || self.phase == ShelleyPhase::Conversations && self.messages.count != 0
        {
            return Err(CaptureError::InvalidPayload(
                "Shelley NativePath cursor authority is inconsistent".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShelleyOutputFrontier {
    version: u32,
    generation: u64,
    messages: ShelleyPrefix,
    terminal: bool,
    retired: bool,
}

impl ShelleyOutputFrontier {
    fn initial(generation: u64) -> Self {
        Self {
            version: SHELLEY_OUTPUT_FRONTIER_VERSION,
            generation,
            messages: ShelleyPrefix::initial(b'm'),
            terminal: false,
            retired: false,
        }
    }
}

#[derive(Debug)]
enum ShelleyUnit<T> {
    Accepted {
        rowid: i64,
        retained_bytes: usize,
        value: T,
    },
    Rejected {
        rowid: i64,
        retained_bytes: usize,
        reason: String,
    },
}

impl<T> ShelleyUnit<T> {
    fn rowid(&self) -> i64 {
        match self {
            Self::Accepted { rowid, .. } | Self::Rejected { rowid, .. } => *rowid,
        }
    }

    fn retained_bytes(&self) -> usize {
        match self {
            Self::Accepted { retained_bytes, .. } | Self::Rejected { retained_bytes, .. } => {
                *retained_bytes
            }
        }
    }
}

#[derive(Debug)]
struct ShelleyMessage {
    message: ShelleyMessageRow,
    conversation: ShelleyConversationRow,
    parent_bearing: bool,
}

#[derive(Debug)]
enum ShelleyCorePageRows {
    Conversations(Vec<ShelleyUnit<ShelleyConversationRow>>),
    Messages(Vec<ShelleyUnit<ShelleyMessage>>),
    Observation,
}

#[derive(Debug)]
struct ShelleyCorePage {
    next_cursor: ShelleyNativeCursor,
    rows: ShelleyCorePageRows,
    logical_units: usize,
    retained_bytes: usize,
}

struct ShelleyScanner<'a> {
    conn: &'a Connection,
    snapshot: &'a crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    path: &'a Path,
    conversation_select: Vec<String>,
    message_select: Vec<String>,
    cursor: ShelleyNativeCursor,
    needs_observation: bool,
}

#[derive(Clone)]
struct ShelleyRouteAuthority {
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
}

struct PreparedCursor {
    cursor: ShelleyNativeCursor,
    retirement: Option<ShelleyRouteAuthority>,
    needs_observation: bool,
}

pub(super) fn import_shelley_native_path(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if context.source_path.is_none() {
        context.source_path = Some(path.to_path_buf());
    }
    let sink = import_options.import_profile.sink().cloned();
    if !path.exists() {
        return handle_missing_source(path, store, &context, &import_options, sink.as_deref());
    }

    let canonical_path = fs::canonicalize(path)?;
    let path_identity = provider_path_identity(&canonical_path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        &path_identity,
    );
    let snapshot = shelley_source_snapshot(&canonical_path)?;
    let conn = open_provider_sqlite_readonly(&canonical_path)?;
    if !snapshot.revalidate(&canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let conversation_columns = shelley_conversation_columns(&conn)?;
    let message_columns = shelley_message_columns(&conn)?;
    shelley_require_message_index(&conn, message_columns.contains("sequence_id"))?;
    let conversation_select = shelley_conversation_select_expressions(&conversation_columns, "c");
    let message_select = shelley_message_select_expressions(&message_columns, "m");
    if import_options
        .inventory_observation_token
        .as_ref()
        .is_some_and(|token| token.len() > SHELLEY_INVENTORY_TOKEN_MAX_BYTES)
    {
        return Err(CaptureError::InvalidPayload(
            "Shelley inventory observation token exceeds 4 KiB".to_owned(),
        ));
    }
    let source_revision = observed_source_revision(
        &snapshot,
        user_version,
        &schema_fingerprint,
        import_options.inventory_observation_token.as_deref(),
    );
    let raw_source_path = context
        .source_path
        .as_deref()
        .unwrap_or(&canonical_path)
        .display()
        .to_string();
    let source_root = context
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Shelley NativePath source has no canonical identity",
    ))?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let decoded = stored
        .as_ref()
        .map(decode_store_cursor)
        .transpose()?
        .flatten();
    if import_options.import_profile.is_replay_only() {
        match decoded.as_ref() {
            Some(DecodedCursor::Native(core)) => {
                core.validate(&canonical_path, &path_identity)?;
                replay_outputs_or_mark_behind(
                    &canonical_path,
                    &conn,
                    &snapshot,
                    &context,
                    core,
                    sink.as_deref(),
                );
            }
            Some(DecodedCursor::Legacy) | None => {
                if let Some(sink) = sink.as_deref() {
                    sink.mark_behind(ProOutputSinkError::new(
                        "shelley_nativepath_output_replay",
                        "Shelley Core has no committed NativePath frontier",
                    ));
                }
            }
        }
        return Ok(ProviderImportSummary::default());
    }
    let prepared = prepare_cursor(
        &conn,
        &canonical_path,
        &path_identity,
        source_revision,
        schema_fingerprint,
        user_version,
        proposed_source_identity,
        decoded,
    )?;

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let mut scanner = ShelleyScanner {
        conn: &conn,
        snapshot: &snapshot,
        path: &canonical_path,
        conversation_select,
        message_select,
        cursor: prepared.cursor,
        needs_observation: prepared.needs_observation,
    };
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut retirement = prepared.retirement;
        let mut changed_groups = 0_usize;
        while let Some(page) = scanner.next_page()? {
            if !snapshot.revalidate(&canonical_path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let expected = store
                .get_sync_cursor(None, &context.machine_id, &stream)?
                .map(|cursor| cursor.cursor);
            let page_summary = publish_core_page(
                store,
                &committed_store,
                &bulk_guard,
                &snapshot,
                &canonical_path,
                &raw_source_path,
                &source_root,
                &context,
                &import_options,
                &stream,
                expected,
                retirement.take(),
                page,
            )?;
            if page_summary.work_result() == ProviderImportWorkResult::Changed {
                changed_groups = changed_groups.saturating_add(1);
            }
            summary.merge_from(page_summary);
            if import_options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_groups != 0
            {
                summary.work_remaining = !scanner.cursor.terminal;
                break;
            }
        }
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let summary = match (operation, finish) {
        (Ok(summary), Ok(())) => summary,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };

    let committed = store
        .get_sync_cursor(None, &context.machine_id, &stream)?
        .and_then(|cursor| decode_native_provider_cursor(&cursor.cursor).ok());
    if let Some(committed) = committed.as_ref() {
        replay_outputs_or_mark_behind(
            &canonical_path,
            &conn,
            &snapshot,
            &context,
            committed,
            sink.as_deref(),
        );
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn prepare_cursor(
    conn: &Connection,
    database_path: &Path,
    path_identity: &str,
    source_revision: String,
    schema_fingerprint: String,
    sqlite_user_version: i64,
    proposed_source_identity: String,
    decoded: Option<DecodedCursor>,
) -> Result<PreparedCursor> {
    let Some(decoded) = decoded else {
        return Ok(PreparedCursor {
            cursor: ShelleyNativeCursor::fresh(
                database_path.to_path_buf(),
                path_identity.to_owned(),
                0,
                proposed_source_identity,
                source_revision,
                schema_fingerprint,
                sqlite_user_version,
                0,
            ),
            retirement: None,
            needs_observation: true,
        });
    };
    let DecodedCursor::Native(mut prior) = decoded else {
        return Ok(PreparedCursor {
            cursor: ShelleyNativeCursor::fresh(
                database_path.to_path_buf(),
                path_identity.to_owned(),
                0,
                proposed_source_identity,
                source_revision,
                schema_fingerprint,
                sqlite_user_version,
                0,
            ),
            retirement: None,
            needs_observation: true,
        });
    };
    prior.validate(database_path, path_identity)?;

    if prior.route_retired {
        return Ok(PreparedCursor {
            cursor: ShelleyNativeCursor::fresh(
                database_path.to_path_buf(),
                path_identity.to_owned(),
                prior
                    .route_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Shelley NativePath route epoch exhausted",
                    ))?,
                proposed_source_identity,
                source_revision,
                schema_fingerprint,
                sqlite_user_version,
                prior
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Shelley NativePath generation exhausted",
                    ))?,
            ),
            retirement: None,
            needs_observation: true,
        });
    }

    let schema_matches = prior.schema_fingerprint == schema_fingerprint
        && prior.sqlite_user_version == sqlite_user_version;
    let prefix_matches = schema_matches
        && verify_prefixes(
            conn,
            &prior.conversations,
            &prior.messages,
            &shelley_conversation_select_expressions(&shelley_conversation_columns(conn)?, "c"),
            &shelley_message_select_expressions(&shelley_message_columns(conn)?, "m"),
        )?;
    if !prefix_matches {
        let retirement = ShelleyRouteAuthority {
            locator_identity: prior.locator_identity.clone(),
            canonical_source_identity: prior.canonical_source_identity.clone(),
            source_revision: prior.source_revision.clone(),
        };
        return Ok(PreparedCursor {
            cursor: ShelleyNativeCursor::fresh(
                database_path.to_path_buf(),
                path_identity.to_owned(),
                prior
                    .route_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Shelley NativePath route epoch exhausted",
                    ))?,
                proposed_source_identity,
                source_revision,
                schema_fingerprint,
                sqlite_user_version,
                prior
                    .generation
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Shelley NativePath generation exhausted",
                    ))?,
            ),
            retirement: Some(retirement),
            needs_observation: true,
        });
    }

    let source_changed = prior.source_revision != source_revision;
    prior.source_revision = source_revision;
    prior.schema_fingerprint = schema_fingerprint;
    prior.sqlite_user_version = sqlite_user_version;
    prior.route_retired = false;
    if source_changed {
        prior.phase = ShelleyPhase::Conversations;
        prior.terminal = false;
    }
    Ok(PreparedCursor {
        cursor: prior,
        retirement: None,
        needs_observation: source_changed,
    })
}

impl ShelleyScanner<'_> {
    fn next_page(&mut self) -> Result<Option<ShelleyCorePage>> {
        loop {
            match self.cursor.phase {
                ShelleyPhase::Conversations => {
                    let mut next = self.cursor.clone();
                    let mut rows = Vec::new();
                    let mut retained_bytes = SHELLEY_PAGE_FIXED_OVERHEAD;
                    while rows.len() < SHELLEY_PAGE_MAX_UNITS {
                        let Some((unit, row_digest)) = next_conversation_unit(
                            self.conn,
                            &self.conversation_select,
                            next.conversations.after_rowid,
                            None,
                        )?
                        else {
                            next.phase = ShelleyPhase::Messages;
                            break;
                        };
                        let bytes = unit.retained_bytes();
                        if !rows.is_empty()
                            && retained_bytes.saturating_add(bytes) > SHELLEY_PAGE_MAX_BYTES
                        {
                            break;
                        }
                        next.conversations.advance(unit.rowid(), row_digest)?;
                        retained_bytes = retained_bytes.saturating_add(bytes);
                        rows.push(unit);
                    }
                    if !rows.is_empty() {
                        let logical_units = rows.len();
                        self.cursor = next.clone();
                        self.needs_observation = false;
                        return Ok(Some(ShelleyCorePage {
                            next_cursor: next,
                            rows: ShelleyCorePageRows::Conversations(rows),
                            logical_units,
                            retained_bytes,
                        }));
                    }
                    self.cursor = next;
                }
                ShelleyPhase::Messages => {
                    let mut next = self.cursor.clone();
                    let mut rows = Vec::new();
                    let mut retained_bytes = SHELLEY_PAGE_FIXED_OVERHEAD;
                    while rows.len() < SHELLEY_PAGE_MAX_UNITS {
                        let Some((unit, row_digest)) = next_message_unit(
                            self.conn,
                            &self.message_select,
                            &self.conversation_select,
                            next.messages.after_rowid,
                            None,
                        )?
                        else {
                            next.phase = ShelleyPhase::Complete;
                            next.terminal = true;
                            self.needs_observation = true;
                            break;
                        };
                        let bytes = unit.retained_bytes();
                        if !rows.is_empty()
                            && retained_bytes.saturating_add(bytes) > SHELLEY_PAGE_MAX_BYTES
                        {
                            break;
                        }
                        next.messages.advance(unit.rowid(), row_digest)?;
                        retained_bytes = retained_bytes.saturating_add(bytes);
                        rows.push(unit);
                    }
                    if !rows.is_empty() {
                        let logical_units = rows.len();
                        self.cursor = next.clone();
                        self.needs_observation = false;
                        return Ok(Some(ShelleyCorePage {
                            next_cursor: next,
                            rows: ShelleyCorePageRows::Messages(rows),
                            logical_units,
                            retained_bytes,
                        }));
                    }
                    self.cursor = next;
                }
                ShelleyPhase::Complete => {
                    if !self.needs_observation {
                        return Ok(None);
                    }
                    if !self.snapshot.revalidate(self.path)? {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    self.needs_observation = false;
                    return Ok(Some(ShelleyCorePage {
                        next_cursor: self.cursor.clone(),
                        rows: ShelleyCorePageRows::Observation,
                        logical_units: 1,
                        retained_bytes: SHELLEY_PAGE_FIXED_OVERHEAD,
                    }));
                }
            }
        }
    }
}

fn verify_prefixes(
    conn: &Connection,
    conversations: &ShelleyPrefix,
    messages: &ShelleyPrefix,
    conversation_select: &[String],
    message_select: &[String],
) -> Result<bool> {
    Ok(
        verify_conversation_prefix(conn, conversation_select, conversations)?
            && verify_message_prefix(conn, message_select, conversation_select, messages)?,
    )
}

fn verify_conversation_prefix(
    conn: &Connection,
    select: &[String],
    expected: &ShelleyPrefix,
) -> Result<bool> {
    let mut observed = ShelleyPrefix::initial(b'c');
    while let Some((unit, digest)) =
        next_conversation_unit(conn, select, observed.after_rowid, expected.after_rowid)?
    {
        observed.advance(unit.rowid(), digest)?;
    }
    Ok(&observed == expected)
}

fn verify_message_prefix(
    conn: &Connection,
    message_select: &[String],
    conversation_select: &[String],
    expected: &ShelleyPrefix,
) -> Result<bool> {
    let mut observed = ShelleyPrefix::initial(b'm');
    while let Some((unit, digest)) = next_message_unit(
        conn,
        message_select,
        conversation_select,
        observed.after_rowid,
        expected.after_rowid,
    )? {
        observed.advance(unit.rowid(), digest)?;
    }
    Ok(&observed == expected)
}

fn next_conversation_unit(
    conn: &Connection,
    select: &[String],
    after: Option<i64>,
    through: Option<i64>,
) -> Result<Option<(ShelleyUnit<ShelleyConversationRow>, [u8; 32])>> {
    let Some((rowid, retained_bytes)) =
        next_candidate(conn, "conversations", "c", select, after, through)?
    else {
        return Ok(None);
    };
    if retained_bytes > SHELLEY_ROW_MAX_BYTES {
        let reason =
            format!("Shelley conversation row {rowid} exceeds the NativePath row byte limit");
        return Ok(Some((
            ShelleyUnit::Rejected {
                rowid,
                retained_bytes: SHELLEY_PAGE_FIXED_OVERHEAD.min(SHELLEY_ROW_MAX_BYTES),
                reason: reason.clone(),
            },
            rejected_row_digest(b'c', rowid, retained_bytes, &reason),
        )));
    }
    let values = query_row_values(conn, "conversations", "c", select, rowid)?;
    let row_digest = values_row_digest(b'c', rowid, &values, None);
    let unit = match decode_shelley_conversation(&values) {
        Ok(conversation) => ShelleyUnit::Accepted {
            rowid,
            retained_bytes: retained_bytes.saturating_add(512),
            value: conversation,
        },
        Err(error) => ShelleyUnit::Rejected {
            rowid,
            retained_bytes: retained_bytes.saturating_add(256),
            reason: error.to_string(),
        },
    };
    Ok(Some((unit, row_digest)))
}

fn next_message_unit(
    conn: &Connection,
    message_select: &[String],
    conversation_select: &[String],
    after: Option<i64>,
    through: Option<i64>,
) -> Result<Option<(ShelleyUnit<ShelleyMessage>, [u8; 32])>> {
    let Some((rowid, retained_bytes)) =
        next_candidate(conn, "messages", "m", message_select, after, through)?
    else {
        return Ok(None);
    };
    if retained_bytes > SHELLEY_ROW_MAX_BYTES {
        let reason = format!("Shelley message row {rowid} exceeds the NativePath row byte limit");
        return Ok(Some((
            ShelleyUnit::Rejected {
                rowid,
                retained_bytes: SHELLEY_PAGE_FIXED_OVERHEAD.min(SHELLEY_ROW_MAX_BYTES),
                reason: reason.clone(),
            },
            rejected_row_digest(b'm', rowid, retained_bytes, &reason),
        )));
    }
    let values = query_row_values(conn, "messages", "m", message_select, rowid)?;
    let message = match decode_shelley_message(&values) {
        Ok(message) => message,
        Err(error) => {
            let digest = values_row_digest(b'm', rowid, &values, None);
            return Ok(Some((
                ShelleyUnit::Rejected {
                    rowid,
                    retained_bytes: retained_bytes.saturating_add(256),
                    reason: error.to_string(),
                },
                digest,
            )));
        }
    };
    let parent = load_conversation_for_message(conn, conversation_select, &message)?;
    let (conversation, parent_values, parent_bytes) = match parent {
        ParentConversation::Accepted {
            conversation,
            values,
            retained_bytes,
        } => (conversation, values, retained_bytes),
        ParentConversation::Rejected { reason, digest } => {
            let row_digest = values_row_digest(b'm', rowid, &values, Some(&digest));
            return Ok(Some((
                ShelleyUnit::Rejected {
                    rowid,
                    retained_bytes: retained_bytes.saturating_add(256),
                    reason,
                },
                row_digest,
            )));
        }
    };
    let parent_bearing: bool = conn.query_row(
        "select not exists (
             select 1 from messages previous
             where typeof(previous.conversation_id) = 'text'
               and previous.conversation_id = ?1
               and previous.rowid < ?2
         )",
        rusqlite::params![message.conversation_id, rowid],
        |row| row.get(0),
    )?;
    let parent_digest = values_row_digest(b'p', conversation.rowid, &parent_values, None);
    let row_digest = values_row_digest(b'm', rowid, &values, Some(&parent_digest));
    Ok(Some((
        ShelleyUnit::Accepted {
            rowid,
            retained_bytes: retained_bytes
                .saturating_add(parent_bytes)
                .saturating_add(1_024),
            value: ShelleyMessage {
                message,
                conversation,
                parent_bearing,
            },
        },
        row_digest,
    )))
}

enum ParentConversation {
    Accepted {
        conversation: ShelleyConversationRow,
        values: Vec<NativeSqliteValue>,
        retained_bytes: usize,
    },
    Rejected {
        reason: String,
        digest: [u8; 32],
    },
}

fn load_conversation_for_message(
    conn: &Connection,
    select: &[String],
    message: &ShelleyMessageRow,
) -> Result<ParentConversation> {
    let lengths = shelley_retained_length_expr(select);
    let sql = format!(
        "select c.rowid, {lengths}
         from conversations c
         where typeof(c.conversation_id) = 'text' and c.conversation_id = ?1
         order by c.rowid limit 2"
    );
    let candidates = with_shelley_length_preflight(conn, || {
        let mut statement = conn.prepare(&sql)?;
        let rows = statement
            .query_map([message.conversation_id.as_str()], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    let [(rowid, retained)] = candidates.as_slice() else {
        let reason = if candidates.is_empty() {
            format!(
                "Shelley message {} references missing conversation {}",
                message.message_id, message.conversation_id
            )
        } else {
            format!(
                "Shelley message {} references duplicate conversation {}",
                message.message_id, message.conversation_id
            )
        };
        return Ok(ParentConversation::Rejected {
            digest: rejected_row_digest(b'p', 0, candidates.len(), &reason),
            reason,
        });
    };
    let retained_bytes = usize::try_from(*retained).map_err(|_| {
        CaptureError::InvalidPayload(
            "Shelley conversation retained byte count must be nonnegative".to_owned(),
        )
    })?;
    if retained_bytes > SHELLEY_ROW_MAX_BYTES {
        let reason = format!(
            "Shelley message {} parent conversation exceeds the NativePath row byte limit",
            message.message_id
        );
        return Ok(ParentConversation::Rejected {
            digest: rejected_row_digest(b'p', *rowid, retained_bytes, &reason),
            reason,
        });
    }
    let values = query_row_values(conn, "conversations", "c", select, *rowid)?;
    match decode_shelley_conversation(&values) {
        Ok(conversation) => Ok(ParentConversation::Accepted {
            conversation,
            values,
            retained_bytes,
        }),
        Err(error) => Ok(ParentConversation::Rejected {
            digest: values_row_digest(b'p', *rowid, &values, None),
            reason: error.to_string(),
        }),
    }
}

fn next_candidate(
    conn: &Connection,
    table: &str,
    alias: &str,
    select: &[String],
    after: Option<i64>,
    through: Option<i64>,
) -> Result<Option<(i64, usize)>> {
    let lengths = shelley_retained_length_expr(select);
    let lower = after.map_or_else(String::new, |_| format!("and {alias}.rowid > ?1"));
    let upper_parameter = if after.is_some() { "?2" } else { "?1" };
    let upper = through.map_or_else(String::new, |_| {
        format!("and {alias}.rowid <= {upper_parameter}")
    });
    let sql = format!(
        "select {alias}.rowid, {lengths}
         from {table} {alias}
         where 1 = 1 {lower} {upper}
         order by {alias}.rowid limit 1"
    );
    let candidate: Option<(i64, i64)> =
        with_shelley_length_preflight(conn, || match (after, through) {
            (Some(after), Some(through)) => conn
                .query_row(&sql, rusqlite::params![after, through], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .optional(),
            (Some(after), None) => conn
                .query_row(&sql, [after], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional(),
            (None, Some(through)) => conn
                .query_row(&sql, [through], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional(),
            (None, None) => conn
                .query_row(&sql, [], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional(),
        })?;
    candidate
        .map(|(rowid, retained)| {
            let retained = usize::try_from(retained).map_err(|_| {
                CaptureError::InvalidPayload(format!(
                    "Shelley {table} retained byte count must be nonnegative"
                ))
            })?;
            Ok((rowid, retained.saturating_add(select.len() * 16)))
        })
        .transpose()
}

fn query_row_values(
    conn: &Connection,
    table: &str,
    alias: &str,
    select: &[String],
    rowid: i64,
) -> Result<Vec<NativeSqliteValue>> {
    let sql = format!(
        "select {} from {table} {alias} where {alias}.rowid = ?1",
        select.join(", ")
    );
    conn.query_row(&sql, [rowid], |row| {
        (0..select.len())
            .map(|index| native_value(row.get_ref(index)?))
            .collect::<rusqlite::Result<Vec<_>>>()
    })
    .map_err(CaptureError::from)
}

fn native_value(value: ValueRef<'_>) -> rusqlite::Result<NativeSqliteValue> {
    Ok(match value {
        ValueRef::Null => NativeSqliteValue::Null,
        ValueRef::Integer(value) => NativeSqliteValue::Integer(value),
        ValueRef::Real(value) => NativeSqliteValue::from_real(value),
        ValueRef::Text(value) => NativeSqliteValue::Text(
            std::str::from_utf8(value)
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        value.len(),
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?
                .to_owned(),
        ),
        ValueRef::Blob(value) => NativeSqliteValue::Blob(value.to_vec()),
    })
}

fn values_row_digest(
    kind: u8,
    rowid: i64,
    values: &[NativeSqliteValue],
    parent: Option<&[u8; 32]>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(SHELLEY_PREFIX_DOMAIN);
    digest.update([kind]);
    digest.update(rowid.to_le_bytes());
    digest.update((values.len() as u64).to_le_bytes());
    for value in values {
        match value {
            NativeSqliteValue::Null => digest.update([0]),
            NativeSqliteValue::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_le_bytes());
            }
            NativeSqliteValue::RealBits(value) => {
                digest.update([2]);
                digest.update(value.to_le_bytes());
            }
            NativeSqliteValue::Text(value) => {
                digest.update([3]);
                hash_bytes(&mut digest, value.as_bytes());
            }
            NativeSqliteValue::Blob(value) => {
                digest.update([4]);
                hash_bytes(&mut digest, value);
            }
        }
    }
    if let Some(parent) = parent {
        digest.update([1]);
        digest.update(parent);
    } else {
        digest.update([0]);
    }
    digest.finalize().into()
}

fn rejected_row_digest(kind: u8, rowid: i64, retained_bytes: usize, reason: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(SHELLEY_PREFIX_DOMAIN);
    digest.update([kind]);
    digest.update(rowid.to_le_bytes());
    digest.update((retained_bytes as u64).to_le_bytes());
    hash_bytes(&mut digest, reason.as_bytes());
    digest.finalize().into()
}

fn hash_bytes(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    canonical_path: &Path,
    raw_source_path: &str,
    source_root: &str,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
    stream: &str,
    expected_cursor: Option<String>,
    retirement: Option<ShelleyRouteAuthority>,
    page: ShelleyCorePage,
) -> Result<ProviderImportSummary> {
    let provider_cursor = serde_json::to_string(&page.next_cursor)?;
    let next = provider_sync_cursor(
        &context.machine_id,
        stream.to_owned(),
        provider_cursor,
        context.imported_at,
    );
    let transition = NativePathCursorTransition::new(expected_cursor, next);
    let publication_id = page_publication_id(&page, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, page.retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    if let Some(retirement) = retirement {
        group.retire_provider_source_route(&ProviderSourceRouteRetirement {
            provider: CaptureProvider::Shelley,
            source_format: SHELLEY_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: retirement.locator_identity,
            cursor_stream: stream.to_owned(),
            expected_canonical_source_identity: retirement.canonical_source_identity,
            expected_source_revision: retirement.source_revision,
            retired_at_ms: context.imported_at.timestamp_millis(),
            reason: ProviderSourceRouteRetirementReason::Replaced,
        })?;
    }
    let proposed_source_identity = page.next_cursor.canonical_source_identity.clone();
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Shelley,
            source_format: SHELLEY_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: page.next_cursor.locator_identity.clone(),
            cursor_stream: stream.to_owned(),
            proposed_source_identity,
            raw_source_path: Some(canonical_path.display().to_string()),
            source_revision: page.next_cursor.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;

    let mut summary = ProviderImportSummary::default();
    match &page.rows {
        ShelleyCorePageRows::Conversations(rows) => {
            publish_conversations(
                committed_store,
                &mut group,
                rows,
                &resolution.canonical_source_identity,
                &resolution.route_binding(),
                canonical_path,
                raw_source_path,
                source_root,
                context,
                import_options,
                &page.next_cursor,
                &mut summary,
            )?;
        }
        ShelleyCorePageRows::Messages(rows) => {
            publish_messages(
                committed_store,
                &mut group,
                rows,
                &resolution.canonical_source_identity,
                raw_source_path,
                context,
                import_options,
                &page.next_cursor,
                &mut summary,
            )?;
        }
        ShelleyCorePageRows::Observation => {}
    }
    record_page_failures(&page.rows, &mut summary);
    if !snapshot.revalidate(canonical_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn publish_conversations(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    rows: &[ShelleyUnit<ShelleyConversationRow>],
    canonical_source_identity: &str,
    route_binding: &ctx_history_store::ProviderSourceRouteBinding,
    canonical_path: &Path,
    raw_source_path: &str,
    source_root: &str,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
    cursor: &ShelleyNativeCursor,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let mut page_sessions = std::collections::BTreeMap::<Uuid, Session>::new();
    for row in rows {
        let ShelleyUnit::Accepted { value, .. } = row else {
            continue;
        };
        let capture_source_id = source_id(
            value.conversation_id.as_str(),
            raw_source_path,
            cursor.generation,
        );
        let stable_session_identity =
            generation_source_identity(canonical_source_identity, cursor.generation);
        let session_id = provider_import_session_uuid(
            committed_store,
            CaptureProvider::Shelley,
            &value.conversation_id,
            capture_source_id,
            Some(&stable_session_identity),
        )?;
        let parent_id = value
            .parent_conversation_id
            .as_deref()
            .map(|parent| {
                provider_import_session_uuid(
                    committed_store,
                    CaptureProvider::Shelley,
                    parent,
                    source_id(parent, raw_source_path, cursor.generation),
                    Some(&stable_session_identity),
                )
            })
            .transpose()?;
        let source = capture_source(
            value,
            capture_source_id,
            canonical_source_identity,
            canonical_path,
            raw_source_path,
            source_root,
            context,
            cursor,
        );
        group.upsert_capture_source(&source)?;
        group.bind_capture_source_provider_route(capture_source_id, route_binding)?;
        let session = session(
            value,
            session_id,
            parent_id,
            capture_source_id,
            context,
            import_options,
            cursor,
        );
        let existed = committed_store.get_session(session.id).is_ok()
            || page_sessions.contains_key(&session.id);
        group.upsert_session(&session)?;
        page_sessions.insert(session.id, session.clone());
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
        if let (Some(parent_id), Some(parent_external)) =
            (parent_id, value.parent_conversation_id.as_deref())
        {
            let parent = if let Some(parent) = page_sessions.get(&parent_id) {
                parent.clone()
            } else {
                match committed_store.get_session(parent_id) {
                    Ok(parent) => parent,
                    Err(ctx_history_store::StoreError::NotFound(_)) => {
                        let placeholder = relationship_placeholder(
                            parent_id,
                            parent_external,
                            context,
                            import_options,
                        );
                        group.upsert_session(&placeholder)?;
                        page_sessions.insert(parent_id, placeholder.clone());
                        placeholder
                    }
                    Err(error) => return Err(error.into()),
                }
            };
            let edge = relationship_edge(
                value,
                &session,
                parent_id,
                capture_source_id,
                &stable_session_identity,
                context,
            );
            let existed = committed_store.session_edge_exists(edge.id)?;
            group.upsert_projection_neutral_session_edge(&actor(&parent), &edge)?;
            if existed {
                summary.skipped_edges = summary.skipped_edges.saturating_add(1);
            } else {
                summary.imported_edges = summary.imported_edges.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn publish_messages(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    rows: &[ShelleyUnit<ShelleyMessage>],
    canonical_source_identity: &str,
    raw_source_path: &str,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
    cursor: &ShelleyNativeCursor,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let stable_session_identity =
        generation_source_identity(canonical_source_identity, cursor.generation);
    for row in rows {
        let ShelleyUnit::Accepted { rowid, value, .. } = row else {
            continue;
        };
        let Some(provider_event) = shelley_core_event(
            &value.message,
            &value.conversation,
            context,
            value.parent_bearing,
        )?
        else {
            continue;
        };
        let source_id = source_id(
            &value.conversation.conversation_id,
            raw_source_path,
            cursor.generation,
        );
        let session_id = provider_import_session_uuid(
            committed_store,
            CaptureProvider::Shelley,
            &value.conversation.conversation_id,
            source_id,
            Some(&stable_session_identity),
        )?;
        let event_hash = provider_event.provider_event_hash.clone();
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::Shelley,
            &value.conversation.conversation_id,
            source_id,
            provider_event.provider_event_index,
            provider_event.provider_event_index,
            &event_hash,
            None,
            None,
            session_id
                == provider_session_uuid(
                    CaptureProvider::Shelley,
                    &value.conversation.conversation_id,
                ),
        )?;
        let line_number = usize::try_from(*rowid).unwrap_or(usize::MAX);
        let (event, run) = shelley_canonical_event(
            &value.conversation.conversation_id,
            source_id,
            session_id,
            line_number,
            &provider_event,
            &event_hash,
            &identity,
            context,
            import_options,
        )?;
        if let Some(run) = run.as_ref() {
            group.upsert_run(run)?;
        }
        if group.reconcile_provider_event(&event, ProviderEventHashAuthority::ProviderSupplied)? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn shelley_canonical_event(
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    event: &ShelleyCoreEvent,
    event_hash: &str,
    identity: &crate::provider::importer::ProviderEventImportIdentity,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<(Event, Option<Run>)> {
    let mut provider_metadata = event.metadata.clone();
    let verified_content_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY))
        .map(|value| {
            VerifiedContentLocatorsV1::from_metadata_value(&value)
                .map(|locators| locators.to_metadata_value())
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(
                        "Shelley verified content locator annotation is malformed".to_owned(),
                    )
                })
        })
        .transpose()?;
    let run = provider_command_run(
        CaptureProvider::Shelley,
        provider_session_id,
        session_id,
        source_id,
        identity.run_source_id,
        options.history_record_id,
        event.event_type,
        event.occurred_at,
        Fidelity::Imported,
        event.provider_event_index,
        &event.payload,
        event_hash,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": ProviderEventHashAuthority::ProviderSupplied.as_str(),
        "cursor": event.cursor,
        "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Shelley.as_str(),
            provider_session_id,
            event.provider_event_index,
        ),
        "source_record_ordinal": Value::Null,
        "source_record_subrecord_index": Value::Null,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) =
        (sync_metadata.as_object_mut(), verified_content_locators)
    {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    Ok((
        Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id: options.history_record_id,
            session_id: Some(session_id),
            run_id: run.as_ref().map(|run| run.id),
            event_type: event.event_type,
            role: event.role,
            occurred_at: event.occurred_at,
            capture_source_id: Some(source_id),
            payload: json!({
                "provider": CaptureProvider::Shelley.as_str(),
                "provider_session_id": provider_session_id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "cursor": event.cursor,
                "artifacts": [],
                "body": compact_provider_result_payload(event.event_type, &event.payload),
            }),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key),
            sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
        },
        run,
    ))
}

fn record_page_failures(rows: &ShelleyCorePageRows, summary: &mut ProviderImportSummary) {
    match rows {
        ShelleyCorePageRows::Conversations(rows) => {
            record_failures_for_units(rows, summary);
        }
        ShelleyCorePageRows::Messages(rows) => {
            record_failures_for_units(rows, summary);
        }
        ShelleyCorePageRows::Observation => {}
    }
}

fn record_failures_for_units<T>(rows: &[ShelleyUnit<T>], summary: &mut ProviderImportSummary) {
    for row in rows {
        if let ShelleyUnit::Rejected { rowid, reason, .. } = row {
            summary.record_failure(ProviderImportFailure {
                line: usize::try_from(*rowid).unwrap_or(usize::MAX),
                error: reason.clone(),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_source(
    conversation: &ShelleyConversationRow,
    source_id: Uuid,
    canonical_source_identity: &str,
    canonical_path: &Path,
    raw_source_path: &str,
    source_root: &str,
    context: &ProviderAdapterContext,
    cursor: &ShelleyNativeCursor,
) -> CaptureSource {
    let started_at = shelley_timestamp(conversation.created_at.as_deref(), context.imported_at);
    let ended_at = conversation
        .updated_at
        .as_deref()
        .map(|value| shelley_timestamp(Some(value), context.imported_at));
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Shelley,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: conversation.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(SHELLEY_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(conversation.conversation_id.clone()),
        },
        started_at,
        ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": conversation.conversation_id,
                "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": cursor.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Shelley,
                    &conversation.conversation_id,
                    SHELLEY_SQLITE_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "nativepath": {
                    "database_path": canonical_path,
                    "locator_identity": cursor.locator_identity,
                    "route_epoch": cursor.route_epoch,
                    "generation": cursor.generation,
                    "schema_fingerprint": cursor.schema_fingerprint,
                    "sqlite_user_version": cursor.sqlite_user_version,
                },
            }),
        ),
    }
}

fn session(
    conversation: &ShelleyConversationRow,
    id: Uuid,
    parent_id: Option<Uuid>,
    source_id: Uuid,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
    cursor: &ShelleyNativeCursor,
) -> Session {
    let started_at = shelley_timestamp(conversation.created_at.as_deref(), context.imported_at);
    let ended_at = conversation
        .updated_at
        .as_deref()
        .map(|value| shelley_timestamp(Some(value), context.imported_at));
    let is_subagent = conversation.parent_conversation_id.is_some() || !conversation.user_initiated;
    Session {
        id,
        history_record_id: import_options.history_record_id,
        parent_session_id: parent_id,
        root_session_id: parent_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Shelley,
        external_session_id: Some(conversation.conversation_id.clone()),
        external_agent_id: None,
        agent_type: if is_subagent {
            AgentType::Subagent
        } else {
            AgentType::Primary
        },
        role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
        is_primary: !is_subagent,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": conversation.conversation_id,
                "parent_provider_session_id": conversation.parent_conversation_id,
                "root_provider_session_id": conversation.parent_conversation_id,
                "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": {
                    "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                    "conversation_id": conversation.conversation_id,
                    "slug": conversation.slug,
                    "title": conversation.slug,
                    "user_initiated": conversation.user_initiated,
                    "archived": conversation.archived,
                    "parent_conversation_id": conversation.parent_conversation_id,
                    "model": conversation.model,
                    "conversation_options": conversation
                        .conversation_options
                        .as_deref()
                        .map(crate::provider::normalization::provider_json_text),
                    "current_generation": conversation.current_generation,
                    "agent_working": conversation.agent_working,
                    "tags": conversation
                        .tags
                        .as_deref()
                        .map(crate::provider::normalization::provider_json_text),
                    "is_draft": conversation.is_draft,
                    "draft": conversation.draft,
                    "queued_messages": conversation
                        .queued_messages
                        .as_deref()
                        .map(crate::provider::normalization::provider_json_text),
                    "nativepath_generation": cursor.generation,
                },
            }),
        ),
    }
}

fn relationship_placeholder(
    id: Uuid,
    external_session_id: &str,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
) -> Session {
    Session {
        id,
        history_record_id: import_options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Shelley,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Unknown,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: false,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: context.imported_at,
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "relationship_placeholder": true,
                "provider_session_id": external_session_id,
                "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                "imported_at": context.imported_at,
            }),
        ),
    }
}

fn relationship_edge(
    conversation: &ShelleyConversationRow,
    session: &Session,
    parent_id: Uuid,
    source_id: Uuid,
    stable_session_identity: &str,
    context: &ProviderAdapterContext,
) -> SessionEdge {
    let id = if session.id
        == provider_session_uuid(CaptureProvider::Shelley, &conversation.conversation_id)
    {
        provider_edge_uuid(
            CaptureProvider::Shelley,
            &conversation.conversation_id,
            "parent_child",
        )
    } else {
        provider_source_edge_uuid(
            stable_session_identity,
            &conversation.conversation_id,
            "parent_child",
        )
    };
    SessionEdge {
        id,
        from_session_id: parent_id,
        to_session_id: session.id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: ctx_history_core::Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": conversation.conversation_id,
                "parent_provider_session_id": conversation.parent_conversation_id,
                "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                "imported_at": context.imported_at,
            }),
        ),
    }
}

fn actor(session: &Session) -> CanonicalActor {
    CanonicalActor {
        direct_session_id: session.id,
        root_session_id: session.root_session_id.unwrap_or(session.id),
        parent_session_id: session.parent_session_id,
        external_session_id: session.external_session_id.clone(),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type.as_str().to_owned(),
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
    }
}

fn source_id(provider_session_id: &str, raw_source_path: &str, generation: u64) -> Uuid {
    if generation == 0 {
        provider_scoped_source_uuid(
            CaptureProvider::Shelley,
            provider_session_id,
            SHELLEY_SQLITE_SOURCE_FORMAT,
            Some(raw_source_path),
        )
    } else {
        stable_capture_uuid(
            &format!(
                "shelley-nativepath-source:{generation}:{raw_source_path}:{provider_session_id}"
            ),
            "source",
        )
    }
}

fn generation_source_identity(canonical_source_identity: &str, generation: u64) -> String {
    if generation == 0 {
        canonical_source_identity.to_owned()
    } else {
        format!("{canonical_source_identity}:shelley-generation:{generation}")
    }
}

fn page_publication_id(page: &ShelleyCorePage, transition: &NativePathCursorTransition) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-shelley-nativepath-publication-v1\0");
    hash_bytes(&mut digest, transition.next().stream.as_bytes());
    hash_bytes(&mut digest, transition.next().cursor.as_bytes());
    digest.update((page.logical_units as u64).to_le_bytes());
    digest.update((page.retained_bytes as u64).to_le_bytes());
    format!("shelley-nativepath-v1:{:x}", digest.finalize())
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
                CaptureProvider::Shelley.as_str(),
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

enum DecodedCursor {
    Native(ShelleyNativeCursor),
    Legacy,
}

fn decode_store_cursor(cursor: &SyncCursor) -> Result<Option<DecodedCursor>> {
    if let Ok(committed) = decode_native_path_committed_cursor(&cursor.cursor) {
        let decoded: ShelleyNativeCursor = serde_json::from_str(committed.provider_cursor())
            .map_err(|error| {
                CaptureError::InvalidPayload(format!(
                    "Shelley NativePath committed cursor is malformed: {error}"
                ))
            })?;
        return Ok(Some(DecodedCursor::Native(decoded)));
    }
    let Some(legacy) = CertifiedProviderCursor::decode_if_certified(&cursor.cursor)? else {
        return Err(CaptureError::InvalidPayload(
            "Shelley cursor is neither NativePath nor a released migration cursor".to_owned(),
        ));
    };
    if legacy.parser_revision() != LEGACY_SHELLEY_CAPTURE_REVISION
        || legacy.policy_revision() != LEGACY_SHELLEY_POLICY_REVISION
    {
        return Err(CaptureError::InvalidPayload(
            "Shelley migration cursor has unreleased parser or policy revisions".to_owned(),
        ));
    }
    let _: () = legacy.parser_checkpoint().deserialize()?;
    let position = legacy.native_position();
    if !valid_legacy_shelley_position(position.kind(), position.value()) {
        return Err(CaptureError::InvalidPayload(
            "Shelley released cursor has an invalid native position".to_owned(),
        ));
    }
    Ok(Some(DecodedCursor::Legacy))
}

fn valid_legacy_shelley_position(kind: &str, value: &[u8]) -> bool {
    if kind != LEGACY_SHELLEY_POSITION_KIND {
        return false;
    }
    if value == [0] {
        return true;
    }
    value.len() == LEGACY_SHELLEY_POSITION_BYTES
        && matches!(value[0], 1..=3)
        && value[17..].iter().all(|flag| matches!(flag, 0 | 1))
}

fn decode_native_provider_cursor(encoded: &str) -> Result<ShelleyNativeCursor> {
    let committed = decode_native_path_committed_cursor(encoded)?;
    serde_json::from_str(committed.provider_cursor()).map_err(CaptureError::from)
}

fn observed_source_revision(
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    user_version: i64,
    schema_fingerprint: &str,
    inventory_token: Option<&str>,
) -> String {
    let base = shelley_source_revision(snapshot, user_version, schema_fingerprint);
    let mut digest = Sha256::new();
    digest.update(b"ctx-shelley-nativepath-source-revision-v1\0");
    hash_bytes(&mut digest, base.as_bytes());
    if let Some(token) = inventory_token {
        hash_bytes(&mut digest, token.as_bytes());
    }
    format!("shelley-nativepath-source-v1:{:x}", digest.finalize())
}

fn locator_identity(path_identity: &str, route_epoch: u64) -> String {
    format!("{path_identity}:shelley-route-epoch:{route_epoch}")
}

fn handle_missing_source(
    path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
    sink: Option<&dyn ProOutputSink>,
) -> Result<ProviderImportSummary> {
    let path_identity = provider_path_identity(path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Shelley,
        SHELLEY_SQLITE_SOURCE_FORMAT,
        &path_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Shelley shelley.db does not exist",
        });
    };
    let decoded = decode_store_cursor(&stored)?;
    if import_options.import_profile.is_replay_only() {
        match decoded {
            Some(DecodedCursor::Native(cursor)) => {
                cursor.validate(path, &path_identity)?;
                retire_output_or_mark_behind(path, context, &cursor, sink);
            }
            Some(DecodedCursor::Legacy) | None => {
                if let Some(sink) = sink {
                    sink.mark_behind(ProOutputSinkError::new(
                        "shelley_nativepath_output_retirement",
                        "Shelley Core has no committed NativePath frontier",
                    ));
                }
            }
        }
        return Ok(ProviderImportSummary::default());
    }
    let Some(DecodedCursor::Native(mut cursor)) = decoded else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Shelley source disappeared before its NativePath cursor migration",
        });
    };
    cursor.validate(path, &path_identity)?;
    if cursor.route_retired {
        retire_output_or_mark_behind(path, context, &cursor, sink);
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    cursor.route_retired = true;
    cursor.phase = ShelleyPhase::Complete;
    cursor.terminal = true;
    let next = provider_sync_cursor(
        &context.machine_id,
        stream.clone(),
        serde_json::to_string(&cursor)?,
        context.imported_at,
    );
    let transition = NativePathCursorTransition::new(Some(stored.cursor), next);
    let publication_id = missing_publication_id(&transition);
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
                    let disposition =
                        group.retire_provider_source_route(&ProviderSourceRouteRetirement {
                            provider: CaptureProvider::Shelley,
                            source_format: SHELLEY_SQLITE_SOURCE_FORMAT.to_owned(),
                            machine_id: context.machine_id.clone(),
                            locator_identity: cursor.locator_identity.clone(),
                            cursor_stream: stream.clone(),
                            expected_canonical_source_identity: cursor
                                .canonical_source_identity
                                .clone(),
                            expected_source_revision: cursor.source_revision.clone(),
                            retired_at_ms: context.imported_at.timestamp_millis(),
                            reason: if path.parent().is_some_and(Path::exists) {
                                ProviderSourceRouteRetirementReason::SourceMissing
                            } else {
                                ProviderSourceRouteRetirementReason::RootMissing
                            },
                        })?;
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
        let mut summary = ProviderImportSummary::default();
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
    let summary = match (operation, finish) {
        (Ok(summary), Ok(())) => summary,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };
    retire_output_or_mark_behind(path, context, &cursor, sink);
    Ok(summary)
}

fn missing_publication_id(transition: &NativePathCursorTransition) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-shelley-nativepath-missing-v1\0");
    hash_bytes(&mut digest, transition.next().stream.as_bytes());
    hash_bytes(&mut digest, transition.next().cursor.as_bytes());
    format!("shelley-nativepath-missing-v1:{:x}", digest.finalize())
}

fn replay_outputs_or_mark_behind(
    path: &Path,
    conn: &Connection,
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    context: &ProviderAdapterContext,
    core: &ShelleyNativeCursor,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_outputs(path, conn, snapshot, context, core, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "shelley_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_outputs(
    path: &Path,
    conn: &Connection,
    snapshot: &crate::provider::sqlite::ProviderSqliteSourceSnapshot,
    context: &ProviderAdapterContext,
    core: &ShelleyNativeCursor,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    if core.route_retired {
        retire_output_or_mark_behind(path, context, core, Some(sink));
        return Ok(());
    }
    let conversation_select =
        shelley_conversation_select_expressions(&shelley_conversation_columns(conn)?, "c");
    let message_select = shelley_message_select_expressions(&shelley_message_columns(conn)?, "m");
    if !verify_message_prefix(conn, &message_select, &conversation_select, &core.messages)? {
        return Err(CaptureError::InvalidPayload(
            "Shelley output replay no longer matches committed Core authority".to_owned(),
        ));
    }
    let source = output_source(path, context)?;
    let progress = match sink.observe_source(&source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let progress_frontier = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == SHELLEY_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<ShelleyOutputFrontier>(&cursor.payload).ok())
        .filter(|frontier| {
            frontier.version == SHELLEY_OUTPUT_FRONTIER_VERSION
                && frontier.generation == core.generation
                && !frontier.retired
        });
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == SHELLEY_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress_frontier.as_ref().is_some_and(|frontier| {
                verify_message_prefix(
                    conn,
                    &message_select,
                    &conversation_select,
                    &frontier.messages,
                )
                .unwrap_or(false)
                    && frontier.messages.count <= core.messages.count
            })
    });
    let mut frontier = if can_resume {
        progress_frontier
            .clone()
            .ok_or(CaptureError::SystemInvariant(
                "Shelley output resume lost its frontier",
            ))?
    } else {
        ShelleyOutputFrontier::initial(core.generation)
    };
    let mut output_state =
        ShelleyOutputState::new(source, progress, can_resume, sink.materializer_revision())?;
    let mut emitted = false;
    loop {
        let expected = frontier.clone();
        let mut observations = Vec::new();
        let mut logical_units = 0_usize;
        let mut retained_bytes = SHELLEY_PAGE_FIXED_OVERHEAD;
        while logical_units < SHELLEY_PAGE_MAX_UNITS
            && frontier.messages.count < core.messages.count
        {
            let Some((unit, row_digest)) = next_message_unit(
                conn,
                &message_select,
                &conversation_select,
                frontier.messages.after_rowid,
                core.messages.after_rowid,
            )?
            else {
                return Err(CaptureError::InvalidPayload(
                    "Shelley output replay frontier ended before committed Core".to_owned(),
                ));
            };
            let bytes = unit.retained_bytes();
            if logical_units != 0 && retained_bytes.saturating_add(bytes) > SHELLEY_PAGE_MAX_BYTES {
                break;
            }
            frontier.messages.advance(unit.rowid(), row_digest)?;
            retained_bytes = retained_bytes.saturating_add(bytes);
            logical_units = logical_units.saturating_add(1);
            if let ShelleyUnit::Accepted { value, .. } = unit {
                if let Some(classification) = shelley_output_classification(&value.message) {
                    observations.push(shelley_output_observation(
                        &value.message,
                        &value.conversation,
                        value.parent_bearing,
                        context,
                        &classification,
                    )?);
                }
            }
        }
        frontier.terminal = core.terminal && frontier.messages == core.messages;
        if logical_units == 0 && emitted {
            break;
        }
        let output_bytes = observations.iter().fold(0_usize, |bytes, observation| {
            bytes
                .saturating_add(observation.content.len())
                .saturating_add(512)
        });
        let accounting = NativePageAccounting {
            logical_units: logical_units.max(1),
            conservative_serialized_bytes: retained_bytes
                .saturating_add(output_bytes)
                .min(crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES),
        };
        let expected_frontier = output_safe_frontier(&expected)?;
        let next_safe_frontier = output_safe_frontier(&frontier)?;
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_state.source.clone(),
            source_epoch: output_state.source_epoch,
            observed_revision: format!("{}:generation={}", core.source_revision, core.generation),
            parser_revision: SHELLEY_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: output_state.disposition,
            expected_prior_source_epoch: output_state.expected_source_epoch,
            expected_prior_frontier: output_state.expected_sink_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::Shelley.as_str(), &core.locator_identity),
            expected_frontier,
            next_safe_frontier.clone(),
            frontier.terminal,
            accounting,
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if process_pro_replay_only(replay, sink).is_err() {
            return Ok(());
        }
        emitted = true;
        output_state.expected_source_epoch = Some(output_state.source_epoch);
        output_state.expected_sink_frontier = Some(next_safe_frontier);
        output_state.disposition = ProOutputSourceDisposition::AppendOrResume;
        if frontier.terminal || frontier.messages == core.messages {
            break;
        }
        if !snapshot.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }
    Ok(())
}

struct ShelleyOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl ShelleyOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        can_resume: bool,
        materializer_revision: &str,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source,
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
            });
        };
        let prior = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let rewrite = !can_resume || progress.materializer_revision != materializer_revision;
        Ok(Self {
            source,
            source_epoch: if rewrite {
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Shelley output source epoch exhausted",
                    ))?
            } else {
                progress.source_epoch
            },
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: prior,
            disposition: if rewrite {
                ProOutputSourceDisposition::Rewrite
            } else {
                ProOutputSourceDisposition::AppendOrResume
            },
        })
    }
}

fn output_source(path: &Path, context: &ProviderAdapterContext) -> Result<OutputSourceIdentity> {
    Ok(OutputSourceIdentity {
        provider: CaptureProvider::Shelley.as_str().to_owned(),
        namespace_id: context
            .source_root_display()
            .unwrap_or_else(|| path.display().to_string()),
        source_id: provider_path_identity(path)?,
    })
}

fn output_safe_frontier(frontier: &ShelleyOutputFrontier) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        SHELLEY_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(frontier)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn retire_output_or_mark_behind(
    path: &Path,
    context: &ProviderAdapterContext,
    core: &ShelleyNativeCursor,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = retire_output(path, context, core, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "shelley_nativepath_output_retirement",
            error.to_string(),
        ));
    }
}

fn retire_output(
    path: &Path,
    context: &ProviderAdapterContext,
    core: &ShelleyNativeCursor,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let source = output_source(path, context)?;
    let Some(progress) = sink
        .observe_source(&source)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?
    else {
        return Ok(());
    };
    if progress.terminal
        && progress
            .cursor
            .as_ref()
            .and_then(|cursor| {
                serde_json::from_slice::<ShelleyOutputFrontier>(&cursor.payload).ok()
            })
            .is_some_and(|frontier| frontier.retired)
    {
        return Ok(());
    }
    let expected = progress
        .cursor
        .as_ref()
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let retired = ShelleyOutputFrontier {
        version: SHELLEY_OUTPUT_FRONTIER_VERSION,
        generation: core.generation,
        messages: core.messages.clone(),
        terminal: true,
        retired: true,
    };
    let next = output_safe_frontier(&retired)?;
    let output =
        NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source,
            source_epoch: progress.source_epoch.checked_add(1).ok_or(
                CaptureError::SystemInvariant("Shelley output source epoch exhausted"),
            )?,
            observed_revision: "shelley-source-missing".to_owned(),
            parser_revision: SHELLEY_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: ProOutputSourceDisposition::Rewrite,
            expected_prior_source_epoch: Some(progress.source_epoch),
            expected_prior_frontier: expected.clone(),
            observations: Vec::new(),
        };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::Shelley.as_str(), &core.locator_identity),
        NativeSafeFrontier::new(
            SHELLEY_OUTPUT_FRONTIER_VERSION,
            serde_json::to_vec(&ShelleyOutputFrontier::initial(core.generation))?,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        next,
        true,
        NativePageAccounting {
            logical_units: 1,
            conservative_serialized_bytes: SHELLEY_PAGE_FIXED_OVERHEAD,
        },
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let _ = process_pro_replay_only(replay, sink);
    Ok(())
}
