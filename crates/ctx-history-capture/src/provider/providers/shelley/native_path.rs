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
    compute_payload_hash,
    native_source::NativeSqliteValue,
    provider::{
        importer::{
            compact_provider_result_payload, provider_command_run, provider_edge_uuid,
            provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
            provider_path_identity, provider_scoped_source_uuid, provider_session_uuid,
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
    relationships::{
        decode_shelley_conversation, decode_shelley_message, shelley_collision_event_index,
        shelley_event_index, shelley_stable_event_index,
    },
    source::{
        shelley_conversation_columns, shelley_conversation_select_expressions,
        shelley_message_columns, shelley_message_select_expressions, shelley_require_message_index,
        shelley_retained_length_expr, shelley_source_revision, shelley_source_snapshot,
        with_shelley_length_preflight,
    },
    ShelleyConversationRow, ShelleyMessageRow,
};

mod cursor;
mod output;
mod publication;
mod scanner;

use self::{
    cursor::{
        canonical_source_key, decode_native_provider_cursor, decode_store_cursor,
        handle_missing_source, locator_identity, observed_source_revision, page_publication_id,
        provider_sync_cursor, retained_or_planned_event_index, DecodedCursor,
    },
    output::{replay_outputs_or_mark_behind, retire_output_or_mark_behind},
    publication::publish_core_page,
    scanner::{hash_bytes, next_message_unit, prepare_cursor, verify_message_prefix},
};

const SHELLEY_NATIVE_CURSOR_VERSION: u32 = 2;
const RELEASED_SHELLEY_NATIVE_CURSOR_VERSION: u32 = 1;
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
    // Keep every persisted authority field explicit at this serialization boundary.
    #[allow(clippy::too_many_arguments)]
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
        if !matches!(
            self.version,
            SHELLEY_NATIVE_CURSOR_VERSION | RELEASED_SHELLEY_NATIVE_CURSOR_VERSION
        ) || self.provider != CaptureProvider::Shelley.as_str()
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
    provider_event_index: u64,
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
    released_source_identity: Option<String>,
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
    has_message_sequence_id: bool,
    cursor: ShelleyNativeCursor,
    released_source_identity: Option<String>,
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
    released_source_identity: Option<String>,
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
    let has_message_sequence_id = message_columns.contains("sequence_id");
    shelley_require_message_index(&conn, has_message_sequence_id)?;
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
        None,
        Some(&path_identity),
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
        has_message_sequence_id,
        cursor: prepared.cursor,
        released_source_identity: prepared.released_source_identity,
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
