use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    ContentRef, Event, EventRole, EventType, Fidelity, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    StoreError,
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
            provider_path_identity, provider_scoped_source_identity_key,
            provider_scoped_source_uuid, provider_source_cursor_stream_for_path,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        normalization::{
            provider_capped_json, provider_json_text, provider_local_preview, provider_policy_body,
            provider_policy_event_text, provider_result_identifier_evidence,
            provider_result_outcome_evidence, provider_timestamp_millis, provider_value_text,
        },
        sqlite::{open_provider_sqlite_readonly, sqlite_schema_fingerprint},
    },
    CaptureError, CaptureWorkLimit, ImportProfile, OutputAssociations, OutputNativeCoordinate,
    OutputNativeCursor, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceIdentity, OutputSourceLocator, ProOutputMaterializationPage, ProOutputObservation,
    ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    ASTRBOT_SQLITE_SOURCE_FORMAT, MAX_PROVIDER_SQLITE_VALUE_BYTES, PROVIDER_MAX_PREVIEW_CHARS,
    PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    model::{
        checkpoint_id, item_id, item_is_output, item_role, item_text, output_outcome,
        provider_session_id, ConversationRow, LegacyOrderKey, PlatformMessageLink,
        PlatformMessageRow,
    },
    preferences::astrbot_selected_conversation_bounded,
    source::{
        astrbot_source_revision, astrbot_source_snapshot, fetch_candidate, hydrate_conversation,
        hydrate_platform_message, AstrBotSql, RowCandidate,
    },
    ASTRBOT_CAPTURE_REVISION, ASTRBOT_POLICY_REVISION,
};

const CURSOR_VERSION: u32 = 1;
const FRONTIER_VERSION: u32 = 1;
const OUTPUT_CURSOR_VERSION: u32 = 1;
const PAGE_MAX_SOURCE_UNITS: usize = 64;
const PAGE_MAX_CORE_BYTES: usize = ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES;
const PAGE_MAX_OUTPUT_BYTES: usize =
    ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES - (256 * 1024);
const CURSOR_STREAM_REVISION: &str = "astrbot-nativepath-v1";
const OUTPUT_PARSER_REVISION: &str = "astrbot-nativepath-output-v1";
const PUBLICATION_PREFIX: &str = "astrbot-nativepath-page-v1:";
const RETIREMENT_PREFIX: &str = "astrbot-nativepath-retirement-v1:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ScanPhase {
    Conversations,
    PlatformMessages,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationInRow {
    physical_rowid: i64,
    row_sha256: [u8; 32],
    next_item_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AstrBotFrontier {
    version: u32,
    phase: ScanPhase,
    conversation_after_rowid: Option<i64>,
    conversation_in_row: Option<ConversationInRow>,
    platform_after_rowid: Option<i64>,
    conversation_prefix_sha256: [u8; 32],
    platform_prefix_sha256: [u8; 32],
    last_conversation_order: Option<LegacyOrderKey>,
    last_platform_order: Option<LegacyOrderKey>,
    next_native_ordinal: u64,
}

impl AstrBotFrontier {
    fn initial() -> Self {
        Self {
            version: FRONTIER_VERSION,
            phase: ScanPhase::Conversations,
            conversation_after_rowid: None,
            conversation_in_row: None,
            platform_after_rowid: None,
            conversation_prefix_sha256: [0; 32],
            platform_prefix_sha256: [0; 32],
            last_conversation_order: None,
            last_platform_order: None,
            next_native_ordinal: 0,
        }
    }

    fn append_start(&self) -> Self {
        let mut frontier = self.clone();
        frontier.phase = ScanPhase::Conversations;
        frontier
    }

    fn terminal(&self) -> bool {
        self.phase == ScanPhase::Complete
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AstrBotStoreCursor {
    version: u32,
    provider: String,
    source_format: String,
    locator_identity: String,
    source_identity: String,
    source_revision: String,
    source_incarnation: String,
    schema_authority: String,
    frontier: AstrBotFrontier,
    terminal: bool,
    generation: u64,
    rejected_records: u64,
    retired: bool,
}

impl AstrBotStoreCursor {
    fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(CaptureError::from)
    }

    fn decode(encoded: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(encoded)?;
        if cursor.version != CURSOR_VERSION
            || cursor.provider != CaptureProvider::AstrBot.as_str()
            || cursor.source_format != ASTRBOT_SQLITE_SOURCE_FORMAT
            || cursor.frontier.version != FRONTIER_VERSION
            || cursor.terminal != cursor.frontier.terminal()
        {
            return Err(CaptureError::InvalidPayload(
                "AstrBot NativePath cursor authority is invalid".to_owned(),
            ));
        }
        Ok(cursor)
    }
}

#[derive(Debug)]
// Cursor decoding keeps released and current wire shapes explicit; boxing
// would add allocation to this short-lived control-flow value.
#[allow(clippy::large_enum_variant)]
enum PriorCursor {
    None,
    Native {
        encoded: String,
        cursor: AstrBotStoreCursor,
    },
    Released {
        encoded: String,
        rejected_records: u64,
    },
}

#[derive(Debug, Clone)]
struct SourceAuthority {
    display_source_path: String,
    raw_source_path: String,
    source_root: String,
    locator_identity: String,
    cursor_stream: String,
    source_identity: String,
    source_revision: String,
    source_incarnation: String,
    schema_authority: String,
    user_version: i64,
    schema_fingerprint: String,
    selected_conversation: Option<String>,
}

#[derive(Debug, Clone)]
struct SessionFact {
    provider_session_id: String,
    external_agent_id: Option<String>,
    role_hint: &'static str,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    metadata: Value,
    preserve_existing: bool,
}

#[derive(Debug)]
struct EventFact {
    provider_event_index: u64,
    legacy_provider_event_index: Option<u64>,
    legacy_provider_event_hash: String,
    released_v025_payload_hash: Option<String>,
    cursor: String,
    source_record_ordinal: u64,
    source_record_subrecord_index: u32,
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: DateTime<Utc>,
    payload: Value,
    metadata: Value,
}

#[derive(Debug)]
struct CoreUnit {
    session: SessionFact,
    event: Option<EventFact>,
}

#[derive(Debug)]
struct OutputFact {
    observation: ProOutputObservation,
    estimated_bytes: usize,
}

#[derive(Debug)]
struct PageRejection {
    line: usize,
    detail: String,
}

#[derive(Debug)]
struct AstrBotPage {
    expected_frontier: AstrBotFrontier,
    next_frontier: AstrBotFrontier,
    terminal: bool,
    retained_core_bytes: usize,
    units: Vec<CoreUnit>,
    outputs: Vec<OutputFact>,
    rejections: Vec<PageRejection>,
}

struct ActiveConversation {
    physical_rowid: i64,
    order: LegacyOrderKey,
    row_sha256: [u8; 32],
    row: ConversationRow,
    items: Vec<Value>,
    content_is_array: bool,
    next_item_index: usize,
    rejection: Option<String>,
}

struct AstrBotReader<'a> {
    conn: &'a Connection,
    sql: AstrBotSql,
    frontier: AstrBotFrontier,
    active_conversation: Option<ActiveConversation>,
    relationship_projection_ready: bool,
}

pub(super) fn import_astrbot_native_path(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if context.source_path.is_none() {
        context.source_path = Some(path.to_path_buf());
    }
    if !path.exists() {
        return retire_missing_source(path, store, &context);
    }
    if let Some(expected) = options.inventory_observation_token.as_deref() {
        let current = crate::observe_ordinary_file(path)?;
        if current.token_hex() != expected {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }

    let snapshot = astrbot_source_snapshot(path)?;
    let canonical_path = fs::canonicalize(path)?;
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let sql = AstrBotSql::new(&conn)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let selected_conversation = astrbot_selected_conversation_bounded(&conn)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }

    let display_source_path = context
        .source_path
        .as_deref()
        .unwrap_or(path)
        .display()
        .to_string();
    let canonical_source_root = context
        .source_root
        .as_deref()
        .map(fs::canonicalize)
        .transpose()?
        .filter(|root| canonical_path.starts_with(root))
        .or_else(|| canonical_path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| canonical_path.clone());
    let raw_source_path = canonical_path.display().to_string();
    let source_root = canonical_source_root.display().to_string();
    let locator_identity = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let source_identity = provider_source_identity(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "AstrBot NativePath source has no canonical identity",
    ))?;
    let source_revision = astrbot_source_revision(&snapshot, user_version, &schema_fingerprint);
    let authority = SourceAuthority {
        display_source_path,
        raw_source_path,
        source_root,
        locator_identity,
        cursor_stream,
        source_identity,
        source_incarnation: source_incarnation(&source_revision),
        schema_authority: format!(
            "capture={ASTRBOT_CAPTURE_REVISION};policy={ASTRBOT_POLICY_REVISION};user_version={user_version};schema={schema_fingerprint}"
        ),
        source_revision,
        user_version,
        schema_fingerprint,
        selected_conversation,
    };

    let stored = store.get_sync_cursor(None, &context.machine_id, &authority.cursor_stream)?;
    let prior = decode_prior_cursor(stored)?;
    let core_terminal_unchanged = matches!(
        &prior,
        PriorCursor::Native { cursor, .. }
            if !cursor.retired
                && cursor.terminal
                && cursor.source_revision == authority.source_revision
                && cursor.schema_authority == authority.schema_authority
                && cursor.locator_identity == authority.locator_identity
                && cursor.source_identity == authority.source_identity
    );

    let summary = if options.import_profile.is_replay_only() {
        ProviderImportSummary::default()
    } else if core_terminal_unchanged {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        summary
    } else {
        import_core(
            path, store, &conn, &sql, &snapshot, &authority, &context, &options, prior,
        )?
    };

    if matches!(
        options.import_profile,
        ImportProfile::CoreAndPro(_) | ImportProfile::ProReplayOnly(_)
    ) && !summary.work_remaining
    {
        if let Err(error) = replay_outputs(
            path,
            store,
            &conn,
            &snapshot,
            &authority,
            &context,
            &options.import_profile,
        ) {
            if let Some(sink) = options.import_profile.sink() {
                sink.mark_behind(crate::ProOutputSinkError::new(
                    "astrbot_nativepath_output_replay",
                    error.to_string(),
                ));
            }
        }
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
mod core_import;
mod cursor;
mod event_projection;
mod identity;
mod relationships;
mod replay;
mod sessions;
pub(crate) mod source_backed;
mod source_projection;

use core_import::*;
use cursor::*;
pub(super) use event_projection::*;
use identity::*;
use relationships::*;
use replay::*;
use sessions::*;
use source_projection::*;
