use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, ContentRef, Event, EventRole, EventType, Fidelity, FileChangeKind, FileTouched,
    Run, RunStatus, RunType, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
};
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
    provider::{
        importer::{
            compact_provider_result_payload,
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_source_cursor_stream_for_path, provider_source_identity,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
        normalization::{
            provider_capped_json_value, provider_local_preview, provider_policy_body,
            provider_policy_event_text, provider_result_identifier_evidence,
            provider_result_outcome_evidence, provider_timestamp_millis,
        },
    },
    CaptureError, CaptureWorkLimit, ImportProfile, OutputAssociations, OutputCommandContext,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceIdentity, OutputSourceLocator, ProOutputObservation, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, JUNIE_SESSION_EVENTS_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    assistant::{
        junie_buffer_result_text, junie_merge_buffered_agent_event, junie_step_output_projection,
        JunieAssistantBuffer, JunieOutputOutcome, JunieStepAgg,
    },
    session_tree::{
        bounded_junie_index_meta, junie_provider_session_id, JunieIndexMeta, JunieSessionPath,
    },
    source::JunieSessionObservation,
    MAX_JUNIE_TRANSIENT_TURN_BYTES,
};

const CURSOR_VERSION: u32 = 1;
const OUTPUT_FRONTIER_VERSION: u32 = 1;
const OUTPUT_PARSER_REVISION: &str = "junie-nativepath-output-v1";
const PUBLICATION_REVISION: &str = "junie-nativepath-v1";
const RECORD_SET_KIND: &str = "junie-jsonl-record-set-v1";
const MAX_RECORD_SET_ENTRIES: usize = 64;
const RECORD_SET_DIGEST_DOMAIN: &[u8] = b"ctx-junie-jsonl-record-set-v1\0";
const CORE_PAGE_MAX_ROWS: usize = 48;
const CORE_PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;
const OUTPUT_PAGE_MAX_ROWS: usize = 32;
const OUTPUT_PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_CURSOR_BYTES: usize = 192 * 1024;
const MAX_FAILURES: usize = 16;
const MAX_FAILURE_BYTES: usize = 4 * 1024;
const GENERATION_EVENT_STRIDE: u64 = 1_000_000_000;

pub(crate) fn import_junie_nativepath(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let configured_source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    context.source_path = Some(path.to_path_buf());
    context.source_root = Some(configured_source_root.clone());

    let inventory = discover(path)?;
    let known = known_routes(store, &context.machine_id, &configured_source_root)?;
    if inventory.sessions.is_empty() {
        if known.is_empty() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "no Junie index.jsonl entries with session events.jsonl files were found",
            });
        }
        if options.import_profile.is_replay_only() {
            return Ok(ProviderImportSummary::default());
        }
        return retire_missing(
            store,
            &context,
            &known,
            &BTreeSet::new(),
            if inventory.root_missing {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
            options.capture_work_limit,
        );
    }

    let replay_only = options.import_profile.is_replay_only();
    let mut summary = ProviderImportSummary::default();
    if !replay_only {
        let committed_store = Store::open_read_only(store.path())?;
        let bulk = store.begin_event_search_bulk_mode()?;
        let operation = (|| {
            let mut changed_groups = 0_usize;
            for session_path in &inventory.sessions {
                let source = import_core_source(
                    store,
                    &committed_store,
                    &bulk,
                    session_path,
                    &context,
                    &options,
                    &mut changed_groups,
                )?;
                summary.merge_from(source);
                if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && changed_groups != 0
                {
                    summary.work_remaining = true;
                    return Ok(());
                }
            }
            summary.merge_from(retire_missing(
                store,
                &context,
                &known,
                &inventory.live_paths,
                ProviderSourceRouteRetirementReason::SourceMissing,
                options.capture_work_limit,
            )?);
            Ok(())
        })();
        let finish = store
            .finish_event_search_bulk_mode(&bulk)
            .map_err(CaptureError::from);
        match (operation, finish) {
            (Ok(()), Ok(())) => {}
            (_, Err(error)) => return Err(error),
            (Err(error), Ok(())) => return Err(error),
        }
    }

    if !summary.work_remaining {
        replay_outputs(
            store,
            &inventory.sessions,
            &configured_source_root,
            &context,
            &options.import_profile,
        );
    }
    Ok(summary)
}

struct Inventory {
    sessions: Vec<JunieSessionPath>,
    live_paths: BTreeSet<PathBuf>,
    root_missing: bool,
}

fn discover(path: &Path) -> Result<Inventory> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Inventory {
                sessions: Vec::new(),
                live_paths: BTreeSet::new(),
                root_missing: true,
            });
        }
        Err(error) => return Err(error.into()),
    }
    let mut sessions = Vec::new();
    super::session_tree::visit_junie_session_event_paths(path, &mut |session, _| {
        sessions.push(session);
        Ok(())
    })?;
    let mut live_paths = BTreeSet::new();
    for session in &sessions {
        live_paths.insert(fs::canonicalize(&session.events_path)?);
    }
    Ok(Inventory {
        sessions,
        live_paths,
        root_missing: false,
    })
}

#[derive(Clone)]
struct KnownRoute {
    path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    current_cursor: SyncCursor,
    cursor: JunieStoreCursor,
}

fn known_routes(store: &Store, machine_id: &str, source_root: &Path) -> Result<Vec<KnownRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Junie
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref()
                != Some(JUNIE_SESSION_EVENTS_SOURCE_FORMAT)
            || source.descriptor.source_root.as_deref() != Some(source_root.as_str())
        {
            continue;
        }
        let (Some(raw_source_path), Some(canonical_source_identity)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
        ) else {
            continue;
        };
        let path = PathBuf::from(raw_source_path);
        let locator_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Junie,
            JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let Ok(committed) = decode_native_path_committed_cursor(&current_cursor.cursor) else {
            // Released cursors are migration input, not route-retirement authority.
            continue;
        };
        let cursor = JunieStoreCursor::decode(committed.provider_cursor())?;
        let route = KnownRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            current_cursor,
            cursor,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Junie persisted duplicate current routes for one events file",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

fn retire_missing(
    store: &mut Store,
    context: &ProviderAdapterContext,
    known: &[KnownRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
    work_limit: CaptureWorkLimit,
) -> Result<ProviderImportSummary> {
    let missing = known
        .iter()
        .filter(|route| {
            let comparable = fs::canonicalize(&route.path).unwrap_or_else(|_| route.path.clone());
            !live_paths.contains(&comparable)
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(ProviderImportSummary::default());
    }
    let bulk = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let missing_count = missing.len();
        for (index, route) in missing.into_iter().enumerate() {
            if retire_route(store, &bulk, context, route, reason)? {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
                if work_limit == CaptureWorkLimit::OneSafeGroup {
                    summary.work_remaining = index + 1 < missing_count;
                    break;
                }
            }
        }
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn retire_route(
    store: &Store,
    bulk: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &KnownRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    if route.cursor.retired {
        return Ok(false);
    }
    let mut retired_cursor = route.cursor.clone();
    retired_cursor.retired = true;
    retired_cursor.terminal = true;
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        SyncCursor {
            id: Uuid::new_v4(),
            team_id: None,
            device_id: context.machine_id.clone(),
            stream: route.current_cursor.stream.clone(),
            cursor: retired_cursor.encode()?,
            last_synced_at: Some(context.imported_at),
            timestamps: timestamps(context.imported_at),
        },
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Junie,
        source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.current_cursor.stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.cursor.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement, &transition);
    let admission = store.admit_event_search_bulk_group(bulk)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                matches!(
                    disposition,
                    ProviderSourceRouteRetirementDisposition::Retired
                )
            }
        };
    group.commit()?;
    Ok(changed)
}

fn retirement_publication_id(
    retirement: &ProviderSourceRouteRetirement,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-junie-nativepath-route-retirement-v1\0");
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("junie-nativepath-retirement-v1:{:x}", digest.finalize())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeState {
    started_at_ms: i64,
    last_ts_ms: i64,
    ended_at_ms: Option<i64>,
    title: Option<String>,
    cwd: Option<String>,
    saw_supported_event: bool,
}

impl RuntimeState {
    fn fresh(meta: &JunieIndexMeta, imported_at: DateTime<Utc>) -> Self {
        let started_at = provider_timestamp_millis(meta.created_at, imported_at);
        Self {
            started_at_ms: started_at.timestamp_millis(),
            last_ts_ms: started_at.timestamp_millis(),
            ended_at_ms: meta
                .updated_at
                .map(|value| provider_timestamp_millis(Some(value), started_at).timestamp_millis()),
            title: meta.task_name.clone(),
            cwd: meta.project_dir.clone(),
            saw_supported_event: false,
        }
    }

    fn started_at(&self) -> DateTime<Utc> {
        timestamp(self.started_at_ms)
    }

    fn last_ts(&self) -> DateTime<Utc> {
        timestamp(self.last_ts_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingTurn {
    start_offset: u64,
    end_offset: u64,
    start_ordinal: u64,
    end_ordinal: u64,
    base_event_index: u64,
    next_event_index: u64,
    next_row: u32,
    row_count: u32,
    turn_sha256: [u8; 32],
    terminal: bool,
    after_state: RuntimeState,
    after_prefix_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frontier {
    offset: u64,
    next_ordinal: u64,
    next_event_index: u64,
    prefix_sha256: [u8; 32],
    state: RuntimeState,
    pending: Option<PendingTurn>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JunieStoreCursor {
    version: u32,
    provider: String,
    source_identity: String,
    source_revision: String,
    observed_length: u64,
    device: Option<u64>,
    inode: Option<u64>,
    generation: u64,
    terminal: bool,
    retired: bool,
    rejected_records: u64,
    frontier: Frontier,
}

impl JunieStoreCursor {
    fn encode(&self) -> Result<String> {
        let encoded = serde_json::to_string(self)?;
        if encoded.len() > MAX_CURSOR_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Junie NativePath cursor exceeds its provider-local bound".to_owned(),
            ));
        }
        Ok(encoded)
    }

    fn decode(encoded: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(encoded)?;
        if cursor.version != CURSOR_VERSION
            || cursor.provider != CaptureProvider::Junie.as_str()
            || cursor.source_identity.is_empty()
            || cursor.frontier.offset > cursor.observed_length
            || (cursor.terminal
                && (cursor.frontier.pending.is_some()
                    || cursor.frontier.offset != cursor.observed_length))
            || cursor.frontier.pending.as_ref().is_some_and(|pending| {
                pending.start_offset != cursor.frontier.offset
                    || pending.start_ordinal != cursor.frontier.next_ordinal
                    || pending.base_event_index != cursor.frontier.next_event_index
                    || pending.next_event_index < pending.base_event_index
                    || pending.start_offset >= pending.end_offset
                    || pending.next_row > pending.row_count
            })
        {
            return Err(CaptureError::InvalidPayload(
                "Junie NativePath cursor is malformed or inconsistent".to_owned(),
            ));
        }
        Ok(cursor)
    }
}

// Keep the decoded native cursor inline with its stored sync cursor: this short-lived
// planning value mirrors the persisted cursor variants and is consumed immediately.
#[allow(clippy::large_enum_variant)]
enum CursorOrigin {
    Fresh,
    Native {
        stored: SyncCursor,
        cursor: JunieStoreCursor,
    },
    Legacy {
        stored: SyncCursor,
    },
}

fn load_cursor(
    store: &Store,
    machine_id: &str,
    stream: &str,
    source_identity: &str,
) -> Result<CursorOrigin> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(CursorOrigin::Fresh);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        let cursor = JunieStoreCursor::decode(committed.provider_cursor())?;
        if cursor.source_identity != source_identity {
            return Err(CaptureError::InvalidPayload(
                "Junie NativePath cursor belongs to another source".to_owned(),
            ));
        }
        return Ok(CursorOrigin::Native { stored, cursor });
    }
    let Some(legacy) = CertifiedProviderCursor::decode_if_certified(&stored.cursor)? else {
        return Err(CaptureError::InvalidPayload(
            "Junie cursor is neither a released cursor nor a NativePath cursor".to_owned(),
        ));
    };
    if legacy.parser_revision() != 2 || legacy.policy_revision() != 5 {
        return Err(CaptureError::InvalidPayload(
            "Junie released cursor has unsupported revisions".to_owned(),
        ));
    }
    Ok(CursorOrigin::Legacy { stored })
}

struct CursorPlan {
    expected: Option<String>,
    cursor: JunieStoreCursor,
}

fn plan_cursor(
    path: &JunieSessionPath,
    observation: &JunieSessionObservation,
    source_identity: &str,
    imported_at: DateTime<Utc>,
    origin: CursorOrigin,
) -> Result<CursorPlan> {
    let fresh = || JunieStoreCursor {
        version: CURSOR_VERSION,
        provider: CaptureProvider::Junie.as_str().to_owned(),
        source_identity: source_identity.to_owned(),
        source_revision: observation.source_revision(),
        observed_length: observation.events_file.length,
        device: observation.events_file.device,
        inode: observation.events_file.inode,
        generation: 0,
        terminal: false,
        retired: false,
        rejected_records: 0,
        frontier: Frontier {
            offset: 0,
            next_ordinal: 0,
            next_event_index: 0,
            prefix_sha256: Sha256::digest([]).into(),
            state: RuntimeState::fresh(&bounded_junie_index_meta(&path.index_meta), imported_at),
            pending: None,
        },
    };
    match origin {
        CursorOrigin::Fresh => Ok(CursorPlan {
            expected: None,
            cursor: fresh(),
        }),
        CursorOrigin::Legacy { stored } => Ok(CursorPlan {
            expected: Some(stored.cursor),
            cursor: fresh(),
        }),
        CursorOrigin::Native { stored, mut cursor } => {
            let same_physical = cursor.device == observation.events_file.device
                && cursor.inode == observation.events_file.inode;
            let (prefix_boundary, expected_prefix) = cursor.frontier.pending.as_ref().map_or(
                (cursor.frontier.offset, cursor.frontier.prefix_sha256),
                |pending| (pending.end_offset, pending.after_prefix_sha256),
            );
            let prefix_matches = observation.events_file.length >= prefix_boundary
                && hash_prefix(&path.events_path, prefix_boundary)? == expected_prefix;
            if cursor.retired || !same_physical || !prefix_matches {
                let generation =
                    cursor
                        .generation
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "Junie source generation exhausted",
                        ))?;
                let mut reset = fresh();
                reset.generation = generation;
                return Ok(CursorPlan {
                    expected: Some(stored.cursor),
                    cursor: reset,
                });
            }
            cursor.retired = false;
            if cursor.frontier.pending.is_none() {
                let meta = bounded_junie_index_meta(&path.index_meta);
                cursor.frontier.state.title = meta.task_name.or(cursor.frontier.state.title);
                cursor.frontier.state.cwd = meta.project_dir.or(cursor.frontier.state.cwd);
                cursor.frontier.state.ended_at_ms =
                    meta.updated_at.or(cursor.frontier.state.ended_at_ms);
            }
            Ok(CursorPlan {
                expected: Some(stored.cursor),
                cursor,
            })
        }
    }
}

fn hash_prefix(path: &Path, length: u64) -> Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut remaining = length;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| CaptureError::SystemInvariant("Junie prefix length exceeds usize"))?;
        let read = file.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        digest.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(digest.finalize().into())
}

#[derive(Debug, Clone)]
struct BindingEntry {
    ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    payload_sha256: [u8; 32],
}

#[derive(Debug, Clone, Default)]
struct RecordSetBinding {
    entries: Vec<BindingEntry>,
    unavailable: bool,
}

impl RecordSetBinding {
    fn observe(&mut self, ordinal: u64, byte_start: u64, byte_end_exclusive: u64, payload: &[u8]) {
        if self.unavailable {
            return;
        }
        if byte_start >= byte_end_exclusive
            || self.entries.len() >= MAX_RECORD_SET_ENTRIES
            || self.entries.last().is_some_and(|prior| {
                prior.ordinal >= ordinal || prior.byte_end_exclusive > byte_start
            })
        {
            self.entries.clear();
            self.unavailable = true;
            return;
        }
        self.entries.push(BindingEntry {
            ordinal,
            byte_start,
            byte_end_exclusive,
            payload_sha256: Sha256::digest(payload).into(),
        });
    }

    fn encoded(&self, tag: u8, target: u32) -> Option<Vec<u8>> {
        if self.unavailable || self.entries.is_empty() {
            return None;
        }
        let count = u16::try_from(self.entries.len()).ok()?;
        let mut encoded = Vec::with_capacity(7 + self.entries.len() * 24);
        encoded.extend_from_slice(&count.to_be_bytes());
        encoded.push(tag);
        encoded.extend_from_slice(&target.to_be_bytes());
        for entry in &self.entries {
            encoded.extend_from_slice(&entry.ordinal.to_be_bytes());
            encoded.extend_from_slice(&entry.byte_start.to_be_bytes());
            encoded.extend_from_slice(&entry.byte_end_exclusive.to_be_bytes());
        }
        crate::complete_content::jsonl::valid_junie_record_set_locator(&encoded).then_some(encoded)
    }

    fn record_digest(&self) -> Option<CompleteContentBodyDigest> {
        if self.unavailable || self.entries.is_empty() {
            return None;
        }
        let mut digest = Sha256::new();
        digest.update(RECORD_SET_DIGEST_DOMAIN);
        digest.update((self.entries.len() as u64).to_be_bytes());
        for entry in &self.entries {
            digest.update(entry.ordinal.to_be_bytes());
            digest.update(entry.byte_start.to_be_bytes());
            digest.update(entry.byte_end_exclusive.to_be_bytes());
            digest.update(entry.payload_sha256);
        }
        CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize()))
    }

    fn native_record_id(&self, target: &str) -> Option<String> {
        Some(format!(
            "junie-records-{}-{}-{target}",
            self.entries.first()?.ordinal,
            self.entries.last()?.ordinal
        ))
    }
}

#[derive(Debug, Clone)]
struct FileChangeDraft {
    path: String,
    old_path: Option<String>,
    change_kind: FileChangeKind,
    touch_index: u64,
}

#[derive(Debug, Clone)]
struct EventDraft {
    event_index: u64,
    event_hash: String,
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: DateTime<Utc>,
    text: String,
    body: Value,
    metadata: Value,
    source_ordinal: u64,
    source_subrecord: u32,
    binding: Option<(RecordSetBinding, VerifiedContentRole, u8, u32, String)>,
    file_change: Option<FileChangeDraft>,
}

#[derive(Debug, Clone)]
struct OutputDraft {
    event_index: u64,
    source_ordinal: u64,
    source_subrecord: u32,
    byte_start: u64,
    byte_end_exclusive: u64,
    occurred_at: DateTime<Utc>,
    call_id: String,
    tool_name: String,
    command: Option<String>,
    outcome: OutputOutcome,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    locator_payload: Vec<u8>,
    native_record_id: String,
    content: Vec<u8>,
}

struct ParsedTurn {
    rows: Vec<EventDraft>,
    outputs: Vec<OutputDraft>,
    start_offset: u64,
    end_offset: u64,
    start_ordinal: u64,
    end_ordinal: u64,
    base_event_index: u64,
    next_event_index: u64,
    after_state: RuntimeState,
    terminal: bool,
    incomplete: bool,
    turn_sha256: [u8; 32],
    after_prefix_sha256: [u8; 32],
    rejection_count: u64,
    rejections: Vec<ProviderImportFailure>,
}

fn timestamp(millis: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(millis).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

fn parse_turn(path: &Path, frontier: &Frontier) -> Result<ParsedTurn> {
    crate::common::io::ensure_regular_provider_transcript_file(path)?;
    let mut reader = BufReader::new(File::open(path)?);
    reader.seek(SeekFrom::Start(frontier.offset))?;
    let start_offset = frontier.offset;
    let start_ordinal = frontier.next_ordinal;
    let base_event_index = frontier.next_event_index;
    let mut ordinal = start_ordinal;
    let mut event_index = base_event_index;
    let mut state = frontier.state.clone();
    let mut buffer = JunieAssistantBuffer::default();
    let mut binding = RecordSetBinding::default();
    let mut rows = Vec::new();
    let mut outputs = Vec::new();
    let mut failures = Vec::new();
    let mut rejection_count = 0_u64;
    let mut retained_turn_bytes = 0_usize;
    let mut line = Vec::new();
    let mut incomplete = false;
    let mut terminal = false;

    loop {
        if let Some(pending) = &frontier.pending {
            if reader.stream_position()? == pending.end_offset {
                terminal = pending.terminal;
                flush_assistant(
                    &mut buffer,
                    &binding,
                    &state,
                    ordinal.saturating_sub(1),
                    &mut event_index,
                    &mut rows,
                    &mut outputs,
                )?;
                break;
            }
        }
        let byte_start = reader.stream_position()?;
        let read =
            crate::common::io::read_provider_jsonl_line_or_skip_oversized(&mut reader, &mut line)?;
        let byte_end = reader.stream_position()?;
        if !matches!(&read, crate::common::io::ProviderJsonlLineRead::Eof)
            && byte_end.saturating_sub(start_offset) > MAX_JUNIE_TRANSIENT_TURN_BYTES as u64
        {
            rows.clear();
            outputs.clear();
            record_rejection(
                &mut failures,
                &mut rejection_count,
                failure(
                    ordinal,
                    format!(
                        "Junie turn scan exceeds the {MAX_JUNIE_TRANSIENT_TURN_BYTES} byte safe-boundary limit"
                    ),
                ),
            );
            ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Junie source ordinal exhausted",
            ))?;
            break;
        }
        match read {
            crate::common::io::ProviderJsonlLineRead::Eof => {
                terminal = true;
                flush_assistant(
                    &mut buffer,
                    &binding,
                    &state,
                    ordinal.saturating_sub(1),
                    &mut event_index,
                    &mut rows,
                    &mut outputs,
                )?;
                break;
            }
            crate::common::io::ProviderJsonlLineRead::Oversized { .. } => {
                record_rejection(
                    &mut failures,
                    &mut rejection_count,
                    failure(
                        ordinal,
                        format!(
                            "Junie events JSONL line exceeds the {} byte limit",
                            crate::MAX_PROVIDER_JSONL_LINE_BYTES
                        ),
                    ),
                );
                ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                    "Junie source ordinal exhausted",
                ))?;
                continue;
            }
            crate::common::io::ProviderJsonlLineRead::Line { .. } => {}
        }
        if line.last() != Some(&b'\n') {
            incomplete = true;
            record_rejection(
                &mut failures,
                &mut rejection_count,
                failure(ordinal, "incomplete trailing Junie JSONL record".to_owned()),
            );
            rows.clear();
            outputs.clear();
            return Ok(ParsedTurn {
                rows,
                outputs,
                start_offset,
                end_offset: start_offset,
                start_ordinal,
                end_ordinal: start_ordinal,
                base_event_index,
                next_event_index: base_event_index,
                after_state: frontier.state.clone(),
                terminal: false,
                incomplete,
                turn_sha256: Sha256::digest([]).into(),
                after_prefix_sha256: frontier.prefix_sha256,
                rejection_count,
                rejections: failures,
            });
        }
        let payload = strip_jsonl_ending(&line);
        let current_ordinal = ordinal;
        ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Junie source ordinal exhausted",
        ))?;
        if payload.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value = match serde_json::from_slice::<Value>(payload) {
            Ok(value) => value,
            Err(error) => {
                record_rejection(
                    &mut failures,
                    &mut rejection_count,
                    failure(
                        current_ordinal,
                        format!("malformed Junie events JSONL: {error}"),
                    ),
                );
                continue;
            }
        };
        match value.get("kind").and_then(Value::as_str).unwrap_or("") {
            "UserPromptEvent" => {
                flush_assistant(
                    &mut buffer,
                    &binding,
                    &state,
                    current_ordinal.saturating_sub(1),
                    &mut event_index,
                    &mut rows,
                    &mut outputs,
                )?;
                let prompt = value.get("prompt").and_then(Value::as_str).unwrap_or("");
                if !prompt.trim().is_empty() {
                    state.saw_supported_event = true;
                    let mut user_binding = RecordSetBinding::default();
                    user_binding.observe(current_ordinal, byte_start, byte_end, payload);
                    rows.push(EventDraft {
                        event_index,
                        event_hash: format!("line:{}:user", current_ordinal.saturating_add(1)),
                        event_type: EventType::Message,
                        role: Some(EventRole::User),
                        occurred_at: state.last_ts(),
                        text: prompt.to_owned(),
                        body: json!({
                            "kind": "UserPromptEvent",
                            "prompt": prompt,
                        }),
                        metadata: json!({
                            "source": "junie_user_prompt",
                            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                        }),
                        source_ordinal: current_ordinal,
                        source_subrecord: 0,
                        binding: Some((
                            user_binding,
                            VerifiedContentRole::MessageBody,
                            3,
                            0,
                            "user-prompt".to_owned(),
                        )),
                        file_change: None,
                    });
                    event_index =
                        event_index
                            .checked_add(1)
                            .ok_or(CaptureError::SystemInvariant(
                                "Junie provider event index exhausted",
                            ))?;
                }
                break;
            }
            "SessionA2uxEvent" => {
                if let Some(timestamp) = value
                    .get("timestampMs")
                    .and_then(json_i64)
                    .and_then(DateTime::<Utc>::from_timestamp_millis)
                {
                    state.last_ts_ms = timestamp.timestamp_millis();
                    state.ended_at_ms = Some(timestamp.timestamp_millis());
                }
                let agent = value
                    .get("event")
                    .and_then(|event| event.get("agentEvent"))
                    .unwrap_or(&Value::Null);
                let kind = agent.get("kind").and_then(Value::as_str).unwrap_or("");
                match kind {
                    "AgentTaskNameUpdatedEvent" => {
                        if let Some(title) = agent
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|value| !value.trim().is_empty())
                        {
                            state.title =
                                Some(provider_local_preview(title, PROVIDER_MAX_PREVIEW_CHARS).0);
                        }
                    }
                    "CurrentDirectoryUpdatedEvent" => {
                        if let Some(cwd) = agent
                            .get("currentDirectory")
                            .and_then(Value::as_str)
                            .filter(|value| !value.trim().is_empty())
                        {
                            state.cwd =
                                Some(provider_local_preview(cwd, PROVIDER_MAX_PREVIEW_CHARS).0);
                        }
                    }
                    "LlmResponseMetadataEvent"
                    | "ResultBlockUpdatedEvent"
                    | "AgentFailureEvent"
                    | "ToolBlockUpdatedEvent"
                    | "TerminalBlockUpdatedEvent"
                    | "ViewFilesBlockUpdatedEvent"
                    | "FileChangesBlockUpdatedEvent" => {
                        let retained = retained_turn_bytes.checked_add(payload.len());
                        if retained.is_none_or(|bytes| bytes > MAX_JUNIE_TRANSIENT_TURN_BYTES) {
                            buffer = JunieAssistantBuffer::default();
                            binding = RecordSetBinding::default();
                            retained_turn_bytes = 0;
                            record_rejection(&mut failures, &mut rejection_count, failure(
                                current_ordinal,
                                format!(
                                    "Junie assistant turn exceeds the {MAX_JUNIE_TRANSIENT_TURN_BYTES} byte transient buffer limit"
                                ),
                            ));
                            continue;
                        }
                        retained_turn_bytes = retained.unwrap_or_default();
                        binding.observe(current_ordinal, byte_start, byte_end, payload);
                        if junie_merge_buffered_agent_event(
                            &mut buffer,
                            agent,
                            current_ordinal.saturating_add(1),
                            state.last_ts(),
                        ) {
                            state.saw_supported_event = true;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let end_offset = reader.stream_position()?;
    let after_prefix_sha256 = hash_prefix(path, end_offset)?;
    let turn_sha256 = hash_range(path, start_offset, end_offset)?;
    Ok(ParsedTurn {
        rows,
        outputs,
        start_offset,
        end_offset,
        start_ordinal,
        end_ordinal: ordinal,
        base_event_index,
        next_event_index: event_index,
        after_state: state,
        terminal,
        incomplete,
        turn_sha256,
        after_prefix_sha256,
        rejection_count,
        rejections: failures,
    })
}

fn strip_jsonl_ending(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n")
        .unwrap_or(line)
        .strip_suffix(b"\r")
        .unwrap_or_else(|| line.strip_suffix(b"\n").unwrap_or(line))
}

fn hash_range(path: &Path, start: u64, end: u64) -> Result<[u8; 32]> {
    let length = end
        .checked_sub(start)
        .ok_or(CaptureError::SystemInvariant("Junie range moved backwards"))?;
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut remaining = length;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| CaptureError::SystemInvariant("Junie range exceeds usize"))?;
        let read = file.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        digest.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(digest.finalize().into())
}

fn json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value.round() as i64))
}

fn failure(ordinal: u64, mut error: String) -> ProviderImportFailure {
    if error.len() > MAX_FAILURE_BYTES {
        let mut boundary = MAX_FAILURE_BYTES;
        while !error.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        error.truncate(boundary);
    }
    ProviderImportFailure {
        line: usize::try_from(ordinal.saturating_add(1)).unwrap_or(usize::MAX),
        error,
    }
}

fn record_rejection(
    failures: &mut Vec<ProviderImportFailure>,
    rejection_count: &mut u64,
    failure: ProviderImportFailure,
) {
    *rejection_count = rejection_count.saturating_add(1);
    if failures.len() < MAX_FAILURES {
        failures.push(failure);
    }
}

fn flush_assistant(
    buffer: &mut JunieAssistantBuffer,
    binding: &RecordSetBinding,
    state: &RuntimeState,
    source_ordinal: u64,
    event_index: &mut u64,
    rows: &mut Vec<EventDraft>,
    outputs: &mut Vec<OutputDraft>,
) -> Result<()> {
    if !buffer.open {
        return Ok(());
    }
    let buffer = std::mem::take(buffer);
    let occurred_at = buffer.turn_ts.unwrap_or_else(|| state.started_at());
    for step_id in &buffer.step_ids_in_order {
        let step = buffer
            .steps
            .get(step_id)
            .ok_or(CaptureError::SystemInvariant(
                "Junie buffered step ordering lost a step",
            ))?;
        if step.changes.is_empty() {
            rows.push(step_event(*event_index, occurred_at, source_ordinal, step));
            *event_index = event_index
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Junie provider event index exhausted",
                ))?;
            if let Some(projected) = junie_step_output_projection(step) {
                let retained = matches!(
                    projected.outcome,
                    JunieOutputOutcome::Failure | JunieOutputOutcome::Timeout
                );
                let locator = binding
                    .encoded(2, u32::try_from(step.order).unwrap_or(u32::MAX))
                    .zip(binding.native_record_id(&format!("step-output-{}", step.order)));
                if retained {
                    rows.push(output_failure_event(
                        *event_index,
                        occurred_at,
                        source_ordinal,
                        step,
                        projected.details,
                        projected.outcome,
                    ));
                } else if let Some((locator_payload, native_record_id)) = locator {
                    let first = binding
                        .entries
                        .first()
                        .ok_or(CaptureError::SystemInvariant(
                            "Junie output binding lost its first source record",
                        ))?;
                    let last = binding.entries.last().ok_or(CaptureError::SystemInvariant(
                        "Junie output binding lost its last source record",
                    ))?;
                    outputs.push(OutputDraft {
                        event_index: *event_index,
                        source_ordinal: first.ordinal,
                        source_subrecord: u32::try_from(step.order).unwrap_or(u32::MAX),
                        byte_start: first.byte_start,
                        byte_end_exclusive: last.byte_end_exclusive,
                        occurred_at,
                        call_id: projected.call_id,
                        tool_name: projected.tool_name.to_owned(),
                        command: projected.command.map(str::to_owned),
                        outcome: match projected.outcome {
                            JunieOutputOutcome::Success => OutputOutcome::Success,
                            JunieOutputOutcome::Failure => OutputOutcome::Failure,
                            JunieOutputOutcome::Timeout => OutputOutcome::Timeout,
                            JunieOutputOutcome::Unknown => OutputOutcome::Unknown,
                        },
                        exit_code: projected.exit_code,
                        duration_ms: projected.duration_ms,
                        locator_payload,
                        native_record_id,
                        content: projected.details.as_bytes().to_vec(),
                    });
                }
                *event_index = event_index
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Junie provider event index exhausted",
                    ))?;
            }
            continue;
        }
        for (change_index, change) in step.changes.iter().enumerate() {
            if let Some(event) = file_change_event(
                *event_index,
                occurred_at,
                source_ordinal,
                step,
                change_index,
                change,
            ) {
                rows.push(event);
                *event_index = event_index
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Junie provider event index exhausted",
                    ))?;
            }
        }
    }
    let final_text = junie_buffer_result_text(&buffer);
    if !final_text.is_empty() {
        rows.push(EventDraft {
            event_index: *event_index,
            event_hash: format!("assistant-result:{}", *event_index),
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at,
            text: final_text.clone(),
            body: json!({
                "result_blocks": buffer.results,
                "model": buffer.usage.model,
                "usage": {
                    "input_tokens": buffer.usage.input_tokens,
                    "output_tokens": buffer.usage.output_tokens,
                    "cache_read_tokens": buffer.usage.cache_read_tokens,
                    "cache_write_tokens": buffer.usage.cache_write_tokens,
                },
            }),
            metadata: json!({
                "source": "junie_result_blocks",
                "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                "model": buffer.usage.model,
                "usage": {
                    "input_tokens": buffer.usage.input_tokens,
                    "output_tokens": buffer.usage.output_tokens,
                    "cache_read_tokens": buffer.usage.cache_read_tokens,
                    "cache_write_tokens": buffer.usage.cache_write_tokens,
                },
            }),
            source_ordinal,
            source_subrecord: u32::try_from(rows.len()).unwrap_or(u32::MAX),
            binding: Some((
                binding.clone(),
                VerifiedContentRole::MessageBody,
                1,
                0,
                "message".to_owned(),
            )),
            file_change: None,
        });
        *event_index = event_index
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Junie provider event index exhausted",
            ))?;
    }
    Ok(())
}

fn step_event(
    event_index: u64,
    occurred_at: DateTime<Utc>,
    source_ordinal: u64,
    step: &JunieStepAgg,
) -> EventDraft {
    let (tool_name, text, body) = if let Some(command) = &step.command {
        (
            "Bash",
            format!("Bash: {command}"),
            json!({
                "tool_name": "Bash",
                "command": command,
                "label": step.label,
                "status": step.status,
            }),
        )
    } else if let Some(files) = &step.files {
        (
            "view",
            step.label
                .clone()
                .unwrap_or_else(|| "View files".to_owned()),
            json!({
                "tool_name": "view",
                "label": step.label,
                "files": files,
                "status": step.status,
            }),
        )
    } else {
        (
            "tool",
            step.label
                .clone()
                .unwrap_or_else(|| "Junie tool step".to_owned()),
            json!({
                "tool_name": "tool",
                "label": step.label,
                "status": step.status,
            }),
        )
    };
    EventDraft {
        event_index,
        event_hash: format!("step:{}:tool", step.order),
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at,
        text,
        body,
        metadata: json!({
            "source": "junie_step",
            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            "tool_name": tool_name,
        }),
        source_ordinal,
        source_subrecord: u32::try_from(step.order).unwrap_or(u32::MAX),
        binding: None,
        file_change: None,
    }
}

fn output_failure_event(
    event_index: u64,
    occurred_at: DateTime<Utc>,
    source_ordinal: u64,
    step: &JunieStepAgg,
    details: &str,
    outcome: JunieOutputOutcome,
) -> EventDraft {
    let timed_out = outcome == JunieOutputOutcome::Timeout;
    let tool_name = if step.command.is_some() {
        "Bash"
    } else if step.files.is_some() {
        "view"
    } else {
        "tool"
    };
    EventDraft {
        event_index,
        event_hash: format!("step:{}:output", step.order),
        event_type: if step.command.is_some() {
            EventType::CommandOutput
        } else {
            EventType::ToolOutput
        },
        role: Some(EventRole::Tool),
        occurred_at,
        text: provider_local_preview(details, PROVIDER_MAX_PREVIEW_CHARS).0,
        body: json!({
            "tool_name": tool_name,
            "details": details,
            "output_preview": provider_local_preview(details, PROVIDER_MAX_PREVIEW_CHARS).0,
            "status": step.status,
            "call_id": format!("step:{}", step.order),
            "provider_step_id": step.provider_step_id,
            "command": step.command,
            "exit_code": step.exit_code,
            "duration_ms": step.duration_ms,
            "timed_out": timed_out,
            "result_outcome": "failure",
        }),
        metadata: json!({
            "source": "junie_step_details",
            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            "tool_name": tool_name,
        }),
        source_ordinal,
        source_subrecord: u32::try_from(step.order.saturating_add(1)).unwrap_or(u32::MAX),
        binding: None,
        file_change: None,
    }
}

fn file_change_event(
    event_index: u64,
    occurred_at: DateTime<Utc>,
    source_ordinal: u64,
    step: &JunieStepAgg,
    change_index: usize,
    change: &Value,
) -> Option<EventDraft> {
    let before_path = change.get("beforeRelativePath").and_then(Value::as_str);
    let after_path = change.get("afterRelativePath").and_then(Value::as_str);
    let path = after_path
        .or(before_path)
        .filter(|path| !path.trim().is_empty())?;
    let change_kind = match (before_path, after_path) {
        (None, Some(_)) => FileChangeKind::Created,
        (Some(_), None) => FileChangeKind::Deleted,
        (Some(before), Some(after)) if before != after => FileChangeKind::Renamed,
        _ => FileChangeKind::Modified,
    };
    Some(EventDraft {
        event_index,
        event_hash: format!("step:{}:change:{change_index}", step.order),
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        occurred_at,
        text: format!("Edit: {path}"),
        body: json!({
            "tool_name": "Edit",
            "file_path": path,
            "old_string": file_content_text(change.get("beforeContent")),
            "new_string": file_content_text(change.get("afterContent")),
            "before_relative_path": before_path,
            "after_relative_path": after_path,
            "change_kind": change_kind.as_str(),
            "status": step.status,
        }),
        metadata: json!({
            "source": "junie_file_change",
            "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
            "tool_name": "Edit",
            "change_kind": change_kind.as_str(),
        }),
        source_ordinal,
        source_subrecord: u32::try_from(change_index).unwrap_or(u32::MAX),
        binding: None,
        file_change: Some(FileChangeDraft {
            path: path.to_owned(),
            old_path: before_path
                .filter(|before| after_path.is_some_and(|after| after != *before))
                .map(str::to_owned),
            change_kind,
            touch_index: event_index
                .saturating_mul(1_000)
                .saturating_add(change_index as u64),
        }),
    })
}

fn file_content_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.as_str())
        .map(str::to_owned)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JunieOutputCursor {
    version: u32,
    provider: String,
    source_identity: String,
    source_revision: String,
    observed_length: u64,
    device: Option<u64>,
    inode: Option<u64>,
    generation: u64,
    terminal: bool,
    frontier: Frontier,
}

impl JunieOutputCursor {
    fn encode(&self) -> Result<Vec<u8>> {
        let encoded = serde_json::to_vec(self)?;
        if encoded.len() > MAX_CURSOR_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Junie output replay cursor exceeds its provider-local bound".to_owned(),
            ));
        }
        Ok(encoded)
    }

    fn decode(encoded: &[u8]) -> Result<Self> {
        let cursor: Self = serde_json::from_slice(encoded)?;
        if cursor.version != OUTPUT_FRONTIER_VERSION
            || cursor.provider != CaptureProvider::Junie.as_str()
            || cursor.source_identity.is_empty()
            || cursor.frontier.offset > cursor.observed_length
            || (cursor.terminal
                && (cursor.frontier.pending.is_some()
                    || cursor.frontier.offset != cursor.observed_length))
            || cursor.frontier.pending.as_ref().is_some_and(|pending| {
                pending.start_offset != cursor.frontier.offset
                    || pending.start_ordinal != cursor.frontier.next_ordinal
                    || pending.base_event_index != cursor.frontier.next_event_index
                    || pending.next_event_index < pending.base_event_index
                    || pending.start_offset >= pending.end_offset
                    || pending.next_row > pending.row_count
            })
        {
            return Err(CaptureError::InvalidPayload(
                "Junie output replay cursor is malformed or inconsistent".to_owned(),
            ));
        }
        Ok(cursor)
    }
}

struct OutputReplayState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    cursor: JunieOutputCursor,
}

fn replay_outputs(
    store: &Store,
    sessions: &[JunieSessionPath],
    source_root: &Path,
    context: &ProviderAdapterContext,
    profile: &ImportProfile,
) {
    let Some(sink) = profile.sink().map(std::sync::Arc::as_ref) else {
        return;
    };
    for session in sessions {
        if let Err(error) = replay_output_source(store, session, source_root, context, sink) {
            sink.mark_behind(ProOutputSinkError::new(
                "junie_nativepath_output_replay",
                error.to_string(),
            ));
        }
    }
}

fn replay_output_source(
    store: &Store,
    session_path: &JunieSessionPath,
    source_root: &Path,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let observation = JunieSessionObservation::read(session_path)?;
    let provider_session_id = junie_provider_session_id(session_path)?;
    let locator_identity = provider_path_identity(&session_path.events_path)?;
    let canonical_identity = provider_path_identity(&observation.canonical_path)?;
    let source_identity = format!("junie-session-events:{canonical_identity}");
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &locator_identity,
    );
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, &stream)? else {
        return Ok(());
    };
    let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) else {
        return Ok(());
    };
    let core_cursor = JunieStoreCursor::decode(committed.provider_cursor())?;
    if core_cursor.source_identity != source_identity
        || core_cursor.retired
        || !core_cursor.terminal
        || core_cursor.frontier.pending.is_some()
        || core_cursor.source_revision != observation.source_revision()
        || core_cursor.observed_length != observation.events_file.length
        || core_cursor.device != observation.events_file.device
        || core_cursor.inode != observation.events_file.inode
        || core_cursor.frontier.offset != observation.events_file.length
        || hash_prefix(&session_path.events_path, core_cursor.frontier.offset)?
            != core_cursor.frontier.prefix_sha256
    {
        return Ok(());
    }

    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Junie.as_str().to_owned(),
        namespace_id: source_root.display().to_string(),
        source_id: locator_identity.clone(),
    };
    let progress = sink
        .observe_source(&output_source)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let mut state = output_replay_state(
        session_path,
        &observation,
        &source_identity,
        context.imported_at,
        sink,
        output_source,
        progress,
    )?;
    if state.cursor.terminal
        && state.cursor.source_revision == observation.source_revision()
        && state.cursor.observed_length == observation.events_file.length
    {
        return Ok(());
    }

    loop {
        if !observation.revalidate(session_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let parsed = parse_turn(&session_path.events_path, &state.cursor.frontier)?;
        validate_output_pending_replay(&state.cursor.frontier, &parsed)?;
        if parsed.incomplete {
            return Ok(());
        }
        let pending_start = state
            .cursor
            .frontier
            .pending
            .as_ref()
            .map_or(0_usize, |pending| pending.next_row as usize);
        if pending_start > parsed.outputs.len() {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let mut output_start = pending_start;
        loop {
            let output_end = output_page_end(&parsed.outputs, output_start)?;
            let mut next = state.cursor.clone();
            next.source_revision = observation.source_revision();
            next.observed_length = observation.events_file.length;
            next.device = observation.events_file.device;
            next.inode = observation.events_file.inode;
            if output_end < parsed.outputs.len() {
                next.terminal = false;
                next.frontier.pending = Some(PendingTurn {
                    start_offset: parsed.start_offset,
                    end_offset: parsed.end_offset,
                    start_ordinal: parsed.start_ordinal,
                    end_ordinal: parsed.end_ordinal,
                    base_event_index: parsed.base_event_index,
                    next_event_index: parsed.next_event_index,
                    next_row: u32::try_from(output_end).map_err(|_| {
                        CaptureError::InvalidPayload(
                            "Junie output turn count exceeds u32".to_owned(),
                        )
                    })?,
                    row_count: u32::try_from(parsed.outputs.len()).map_err(|_| {
                        CaptureError::InvalidPayload(
                            "Junie output turn count exceeds u32".to_owned(),
                        )
                    })?,
                    turn_sha256: parsed.turn_sha256,
                    terminal: parsed.terminal,
                    after_state: parsed.after_state.clone(),
                    after_prefix_sha256: parsed.after_prefix_sha256,
                });
            } else {
                next.frontier = Frontier {
                    offset: parsed.end_offset,
                    next_ordinal: parsed.end_ordinal,
                    next_event_index: parsed.next_event_index,
                    prefix_sha256: parsed.after_prefix_sha256,
                    state: parsed.after_state.clone(),
                    pending: None,
                };
                next.terminal =
                    parsed.terminal && parsed.end_offset == observation.events_file.length;
            }
            if !observation.revalidate(session_path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            if !publish_output_page(
                sink,
                session_path,
                &provider_session_id,
                &locator_identity,
                &observation,
                &mut state,
                next,
                &parsed.outputs[output_start..output_end],
                &parsed.after_state,
            )? {
                return Ok(());
            }
            output_start = output_end;
            if output_start >= parsed.outputs.len() {
                break;
            }
        }
        if state.cursor.frontier.pending.is_some() {
            continue;
        }
        if state.cursor.terminal {
            break;
        }
    }
    Ok(())
}

fn output_replay_state(
    session_path: &JunieSessionPath,
    observation: &JunieSessionObservation,
    source_identity: &str,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
    source: OutputSourceIdentity,
    progress: Option<ProOutputProgress>,
) -> Result<OutputReplayState> {
    let fresh = |generation| JunieOutputCursor {
        version: OUTPUT_FRONTIER_VERSION,
        provider: CaptureProvider::Junie.as_str().to_owned(),
        source_identity: source_identity.to_owned(),
        source_revision: observation.source_revision(),
        observed_length: observation.events_file.length,
        device: observation.events_file.device,
        inode: observation.events_file.inode,
        generation,
        terminal: false,
        frontier: Frontier {
            offset: 0,
            next_ordinal: 0,
            next_event_index: 0,
            prefix_sha256: Sha256::digest([]).into(),
            state: RuntimeState::fresh(
                &bounded_junie_index_meta(&session_path.index_meta),
                imported_at,
            ),
            pending: None,
        },
    };
    let Some(progress) = progress else {
        return Ok(OutputReplayState {
            source,
            source_epoch: 0,
            expected_source_epoch: None,
            expected_sink_frontier: None,
            disposition: ProOutputSourceDisposition::NewSource,
            cursor: fresh(0),
        });
    };
    let prior_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
        .transpose()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let candidate = progress
        .cursor
        .as_ref()
        .filter(|cursor| cursor.version == OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| JunieOutputCursor::decode(&cursor.payload).ok());
    let can_resume = progress.parser_revision == OUTPUT_PARSER_REVISION
        && progress.materializer_revision == sink.materializer_revision()
        && candidate.as_ref().is_some_and(|cursor| {
            let (prefix_boundary, expected_prefix) = cursor.frontier.pending.as_ref().map_or(
                (cursor.frontier.offset, cursor.frontier.prefix_sha256),
                |pending| (pending.end_offset, pending.after_prefix_sha256),
            );
            cursor.source_identity == source_identity
                && cursor.device == observation.events_file.device
                && cursor.inode == observation.events_file.inode
                && observation.events_file.length >= prefix_boundary
                && hash_prefix(&session_path.events_path, prefix_boundary)
                    .is_ok_and(|digest| digest == expected_prefix)
        });
    let rewrite = !can_resume;
    let source_epoch = if rewrite {
        progress
            .source_epoch
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "Junie output source epoch exhausted",
            ))?
    } else {
        progress.source_epoch
    };
    Ok(OutputReplayState {
        source,
        source_epoch,
        expected_source_epoch: Some(progress.source_epoch),
        expected_sink_frontier: prior_frontier,
        disposition: if rewrite {
            ProOutputSourceDisposition::Rewrite
        } else {
            ProOutputSourceDisposition::AppendOrResume
        },
        cursor: if rewrite {
            fresh(source_epoch)
        } else {
            candidate.ok_or(CaptureError::SystemInvariant(
                "Junie resumable output cursor disappeared",
            ))?
        },
    })
}

fn validate_output_pending_replay(frontier: &Frontier, parsed: &ParsedTurn) -> Result<()> {
    let Some(pending) = &frontier.pending else {
        return Ok(());
    };
    if pending.start_offset != parsed.start_offset
        || pending.end_offset != parsed.end_offset
        || pending.start_ordinal != parsed.start_ordinal
        || pending.end_ordinal != parsed.end_ordinal
        || pending.base_event_index != parsed.base_event_index
        || pending.next_event_index != parsed.next_event_index
        || pending.row_count as usize != parsed.outputs.len()
        || pending.turn_sha256 != parsed.turn_sha256
        || pending.terminal != parsed.terminal
        || pending.after_state != parsed.after_state
        || pending.after_prefix_sha256 != parsed.after_prefix_sha256
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

fn output_page_end(outputs: &[OutputDraft], start: usize) -> Result<usize> {
    if start >= outputs.len() {
        return Ok(start);
    }
    let mut bytes = 0_usize;
    let mut end = start;
    while end < outputs.len() && end - start < OUTPUT_PAGE_MAX_ROWS {
        let output = &outputs[end];
        let next = output
            .content
            .len()
            .saturating_add(output.locator_payload.len())
            .saturating_add(output.call_id.len())
            .saturating_add(output.command.as_ref().map_or(0, String::len))
            .saturating_add(2 * 1024);
        if next > OUTPUT_PAGE_MAX_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Junie transient output exceeds the bounded Pro page".to_owned(),
            ));
        }
        if end != start && bytes.saturating_add(next) > OUTPUT_PAGE_MAX_BYTES {
            break;
        }
        bytes = bytes.saturating_add(next);
        end += 1;
    }
    Ok(end)
}

#[allow(clippy::too_many_arguments)]
fn publish_output_page(
    sink: &dyn ProOutputSink,
    session_path: &JunieSessionPath,
    provider_session_id: &str,
    locator_identity: &str,
    observation: &JunieSessionObservation,
    state: &mut OutputReplayState,
    next: JunieOutputCursor,
    outputs: &[OutputDraft],
    runtime: &RuntimeState,
) -> Result<bool> {
    let expected_frontier = output_safe_frontier(&state.cursor)?;
    let next_frontier = output_safe_frontier(&next)?;
    let observations = outputs
        .iter()
        .map(|output| output_observation(provider_session_id, output, runtime))
        .collect::<Vec<_>>();
    let claimed_bytes = observations.iter().fold(
        expected_frontier
            .bytes
            .len()
            .saturating_add(next_frontier.bytes.len())
            .saturating_add(locator_identity.len())
            .saturating_add(64 * 1024),
        |bytes, output| {
            bytes
                .saturating_add(output.content.len())
                .saturating_add(output.locator.payload.len())
                .saturating_add(2 * 1024)
        },
    );
    if claimed_bytes > crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Junie transient output page exceeds the NativePath byte bound".to_owned(),
        ));
    }
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source: state.source.clone(),
        source_epoch: state.source_epoch,
        observed_revision: observation.source_revision(),
        parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_sink_frontier.clone(),
        observations,
    };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::Junie.as_str(), locator_identity),
        expected_frontier,
        next_frontier.clone(),
        next.terminal,
        NativePageAccounting {
            logical_units: outputs.len().max(1),
            conservative_serialized_bytes: claimed_bytes,
        },
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if process_pro_replay_only(replay, sink).is_err() {
        sink.mark_behind(ProOutputSinkError::new(
            "junie_nativepath_output_page",
            format!(
                "failed to materialize Junie output page for {}",
                session_path.events_path.display()
            ),
        ));
        return Ok(false);
    }
    state.cursor = next;
    state.expected_source_epoch = Some(state.source_epoch);
    state.expected_sink_frontier = Some(next_frontier);
    state.disposition = ProOutputSourceDisposition::AppendOrResume;
    Ok(true)
}

fn output_safe_frontier(cursor: &JunieOutputCursor) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(OUTPUT_FRONTIER_VERSION, cursor.encode()?)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn output_observation(
    provider_session_id: &str,
    output: &OutputDraft,
    runtime: &RuntimeState,
) -> ProOutputObservation {
    ProOutputObservation {
        kind: if output.command.is_some() {
            OutputObservationKind::Command
        } else {
            OutputObservationKind::Tool
        },
        coordinate: OutputNativeCoordinate {
            unit_key: output.call_id.clone(),
            native_sequence: output.event_index,
            native_record_id: Some(output.native_record_id.clone()),
            source_record_ordinal: Some(output.source_ordinal),
            source_record_subrecord_index: Some(output.source_subrecord),
            byte_start: Some(output.byte_start),
            byte_end_exclusive: Some(output.byte_end_exclusive),
        },
        occurred_at_unix_ms: Some(output.occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: provider_session_id.to_owned(),
            root_session_id: provider_session_id.to_owned(),
            parent_session_id: None,
            provider_session_id: Some(provider_session_id.to_owned()),
            agent_id: None,
            repository: None,
        },
        call_id: Some(output.call_id.clone()),
        command: output.command.as_ref().and_then(|command| {
            let command = provider_local_preview(command, PROVIDER_MAX_PREVIEW_CHARS).0;
            (!command.contains('\0')).then(|| OutputCommandContext {
                tool_name: output.tool_name.clone(),
                command,
                working_directory: runtime.cwd.as_deref().and_then(|cwd| {
                    (!cwd.is_empty()
                        && cwd.len() <= PROVIDER_MAX_PREVIEW_CHARS
                        && !cwd.chars().any(char::is_control))
                    .then(|| cwd.to_owned())
                }),
            })
        }),
        outcome: OutputOutcomeMetadata {
            outcome: output.outcome,
            exit_code: output.exit_code,
            duration_ms: output.duration_ms,
        },
        locator: OutputSourceLocator {
            version: 1,
            kind: RECORD_SET_KIND.to_owned(),
            payload: output.locator_payload.clone(),
        },
        content: output.content.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
fn import_core_source(
    store: &mut Store,
    committed_store: &Store,
    bulk: &EventSearchBulkGuard,
    session_path: &JunieSessionPath,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    changed_groups: &mut usize,
) -> Result<ProviderImportSummary> {
    let observation = JunieSessionObservation::read(session_path)?;
    let provider_session_id = junie_provider_session_id(session_path)?;
    let locator_identity = provider_path_identity(&session_path.events_path)?;
    let canonical_identity = provider_path_identity(&observation.canonical_path)?;
    let source_identity = format!("junie-session-events:{canonical_identity}");
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &locator_identity,
    );
    let origin = load_cursor(store, &context.machine_id, &stream, &source_identity)?;
    let mut plan = plan_cursor(
        session_path,
        &observation,
        &source_identity,
        context.imported_at,
        origin,
    )?;
    if plan.cursor.terminal
        && plan.cursor.frontier.pending.is_none()
        && plan.cursor.source_revision == observation.source_revision()
        && plan.cursor.observed_length == observation.events_file.length
        && plan.cursor.frontier.offset == observation.events_file.length
    {
        let mut summary = ProviderImportSummary {
            skipped_sessions: 1,
            skipped: 1,
            ..ProviderImportSummary::default()
        };
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let mut summary = ProviderImportSummary::default();
    let mut published_any = false;
    loop {
        if !observation.revalidate(session_path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let parsed = parse_turn(&session_path.events_path, &plan.cursor.frontier)?;
        validate_pending_replay(&plan.cursor.frontier, &parsed)?;
        if parsed.next_event_index > GENERATION_EVENT_STRIDE {
            return Err(CaptureError::InvalidPayload(
                "Junie session exceeds the provider-local generation event bound".to_owned(),
            ));
        }
        if parsed.incomplete {
            let retained = parsed.rejections.len() as u64;
            for rejection in parsed.rejections {
                summary.record_failure(rejection);
            }
            summary.failed = summary.failed.saturating_add(
                usize::try_from(parsed.rejection_count.saturating_sub(retained))
                    .unwrap_or(usize::MAX),
            );
            break;
        }
        if parsed.terminal
            && !parsed.after_state.saw_supported_event
            && session_path.require_supported_events
        {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: session_path.events_path.clone(),
                reason: "Junie events.jsonl contained no supported session events",
            });
        }
        if parsed.start_offset == parsed.end_offset
            && parsed.rows.is_empty()
            && plan.cursor.terminal
            && plan.cursor.source_revision == observation.source_revision()
        {
            break;
        }
        let pending_start = plan
            .cursor
            .frontier
            .pending
            .as_ref()
            .map_or(0_usize, |pending| pending.next_row as usize);
        if pending_start > parsed.rows.len() {
            return Err(CaptureError::InvalidPayload(
                "Junie pending page frontier exceeds the reparsed turn".to_owned(),
            ));
        }
        let mut row_start = pending_start;
        let first_publication_for_turn = plan.cursor.frontier.pending.is_none();
        loop {
            let row_end = core_page_end(&parsed.rows, row_start)?;
            let mut next_cursor = plan.cursor.clone();
            next_cursor.source_revision = observation.source_revision();
            next_cursor.observed_length = observation.events_file.length;
            next_cursor.device = observation.events_file.device;
            next_cursor.inode = observation.events_file.inode;
            next_cursor.retired = false;
            if first_publication_for_turn && row_start == 0 {
                next_cursor.rejected_records = next_cursor
                    .rejected_records
                    .saturating_add(parsed.rejection_count);
            }
            if row_end < parsed.rows.len() {
                next_cursor.terminal = false;
                next_cursor.frontier.pending = Some(PendingTurn {
                    start_offset: parsed.start_offset,
                    end_offset: parsed.end_offset,
                    start_ordinal: parsed.start_ordinal,
                    end_ordinal: parsed.end_ordinal,
                    base_event_index: parsed.base_event_index,
                    next_event_index: parsed.next_event_index,
                    next_row: u32::try_from(row_end).map_err(|_| {
                        CaptureError::InvalidPayload("Junie turn row count exceeds u32".to_owned())
                    })?,
                    row_count: u32::try_from(parsed.rows.len()).map_err(|_| {
                        CaptureError::InvalidPayload("Junie turn row count exceeds u32".to_owned())
                    })?,
                    turn_sha256: parsed.turn_sha256,
                    terminal: parsed.terminal,
                    after_state: parsed.after_state.clone(),
                    after_prefix_sha256: parsed.after_prefix_sha256,
                });
            } else {
                next_cursor.frontier = Frontier {
                    offset: parsed.end_offset,
                    next_ordinal: parsed.end_ordinal,
                    next_event_index: parsed.next_event_index,
                    prefix_sha256: parsed.after_prefix_sha256,
                    state: parsed.after_state.clone(),
                    pending: None,
                };
                next_cursor.terminal =
                    parsed.terminal && parsed.end_offset == observation.events_file.length;
            }
            if !observation.revalidate(session_path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let page = publish_core_page(
                store,
                committed_store,
                bulk,
                session_path,
                context,
                options,
                &provider_session_id,
                &source_identity,
                &stream,
                plan.expected.clone(),
                &plan.cursor,
                &next_cursor,
                &parsed.rows[row_start..row_end],
            )?;
            if page.work_result() == ProviderImportWorkResult::Changed {
                *changed_groups = changed_groups.saturating_add(1);
                published_any = true;
            }
            summary.merge_from(page);
            if first_publication_for_turn && row_start == 0 {
                let retained = parsed.rejections.len() as u64;
                for rejection in &parsed.rejections {
                    summary.record_failure(rejection.clone());
                }
                summary.failed = summary.failed.saturating_add(
                    usize::try_from(parsed.rejection_count.saturating_sub(retained))
                        .unwrap_or(usize::MAX),
                );
            }
            plan.cursor = next_cursor;
            plan.expected = store
                .get_sync_cursor(None, &context.machine_id, &stream)?
                .map(|cursor| cursor.cursor);
            row_start = row_end;
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && published_any {
                return Ok(summary);
            }
            if row_start >= parsed.rows.len() {
                break;
            }
        }
        if plan.cursor.frontier.pending.is_some() {
            continue;
        }
        if plan.cursor.terminal {
            break;
        }
    }
    if !published_any && summary.failed == 0 {
        summary.skipped_sessions = 1;
        summary.skipped = summary.skipped.saturating_add(1);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
    }
    Ok(summary)
}

fn validate_pending_replay(frontier: &Frontier, parsed: &ParsedTurn) -> Result<()> {
    let Some(pending) = &frontier.pending else {
        return Ok(());
    };
    if pending.start_offset != parsed.start_offset
        || pending.end_offset != parsed.end_offset
        || pending.start_ordinal != parsed.start_ordinal
        || pending.end_ordinal != parsed.end_ordinal
        || pending.base_event_index != parsed.base_event_index
        || pending.next_event_index != parsed.next_event_index
        || pending.row_count as usize != parsed.rows.len()
        || pending.turn_sha256 != parsed.turn_sha256
        || pending.terminal != parsed.terminal
        || pending.after_state != parsed.after_state
        || pending.after_prefix_sha256 != parsed.after_prefix_sha256
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

fn core_page_end(rows: &[EventDraft], start: usize) -> Result<usize> {
    if start >= rows.len() {
        return Ok(start);
    }
    let mut bytes = 0_usize;
    let mut end = start;
    while end < rows.len() && end - start < CORE_PAGE_MAX_ROWS {
        let next = serde_json::to_vec(&rows[end].body)?
            .len()
            .saturating_add(serde_json::to_vec(&rows[end].metadata)?.len());
        if end != start && bytes.saturating_add(next) > CORE_PAGE_MAX_BYTES {
            break;
        }
        if next > CORE_PAGE_MAX_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Junie normalized Core row exceeds the bounded NativePath page".to_owned(),
            ));
        }
        bytes = bytes.saturating_add(next);
        end += 1;
    }
    Ok(end)
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk: &EventSearchBulkGuard,
    session_path: &JunieSessionPath,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    source_identity: &str,
    stream: &str,
    expected_cursor: Option<String>,
    prior: &JunieStoreCursor,
    next: &JunieStoreCursor,
    rows: &[EventDraft],
) -> Result<ProviderImportSummary> {
    let encoded = next.encode()?;
    let next_sync = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: stream.to_owned(),
        cursor: encoded,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition = NativePathCursorTransition::new(expected_cursor, next_sync);
    let publication_id = publication_id(source_identity, next, rows, &transition);
    let retained_bytes = rows.iter().try_fold(next.encode()?.len(), |bytes, row| {
        let row_bytes = serde_json::to_vec(&row.body)?
            .len()
            .saturating_add(serde_json::to_vec(&row.metadata)?.len())
            .saturating_add(row.text.len());
        Ok::<_, CaptureError>(bytes.saturating_add(row_bytes))
    })?;
    let accounting = NativePathGroupAccounting::new(1, 1, retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let mut summary = ProviderImportSummary::default();
    let resolved = resolve_source(
        committed_store,
        &mut group,
        session_path,
        context,
        options,
        provider_session_id,
        next,
        &mut summary,
    )?;
    for row in rows {
        publish_event(
            committed_store,
            &mut group,
            context,
            options,
            provider_session_id,
            &resolved,
            next.generation,
            row,
            &mut summary,
        )?;
    }
    let observation = JunieSessionObservation::read(session_path)?;
    if observation.source_revision() != next.source_revision
        || observation.events_file.length != next.observed_length
        || observation.events_file.device != next.device
        || observation.events_file.inode != next.inode
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    if prior.generation != next.generation && rows.is_empty() {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
    }
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

struct ResolvedSource {
    source_id: Uuid,
    session: Session,
}

#[allow(clippy::too_many_arguments)]
fn resolve_source(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    session_path: &JunieSessionPath,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    cursor: &JunieStoreCursor,
    summary: &mut ProviderImportSummary,
) -> Result<ResolvedSource> {
    let raw_source_path = session_path.events_path.display().to_string();
    let source_root = context
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
    let locator_identity = provider_path_identity(&session_path.events_path)?;
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Junie NativePath source has no canonical identity",
    ))?;
    let route_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &locator_identity,
    );
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Junie,
            source_format: JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity,
            cursor_stream: route_stream,
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: cursor.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let existing_source = committed_store.capture_source_by_canonical_identity_session(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        &context.machine_id,
        &resolution.canonical_source_identity,
        provider_session_id,
    )?;
    let source_id = existing_source
        .as_ref()
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Junie,
                provider_session_id,
                JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                Some(&raw_source_path),
            )
        });
    let source = CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Junie,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: cursor.frontier.state.cwd.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_format: Some(JUNIE_SESSION_EVENTS_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.clone()),
            source_identity: Some(resolution.canonical_source_identity.clone()),
            external_session_id: Some(provider_session_id.to_owned()),
        },
        started_at: cursor.frontier.state.started_at(),
        ended_at: cursor.frontier.state.ended_at_ms.map(timestamp),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": resolution.canonical_source_identity,
                "source_root": source_root,
                "source_revision": cursor.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Junie,
                    provider_session_id,
                    JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                    Some(&raw_source_path),
                ),
                "nativepath_publication": PUBLICATION_REVISION,
            }),
        ),
    };
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Junie,
        provider_session_id,
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let existed = committed_store.get_session(session_id).is_ok();
    let meta = bounded_junie_index_meta(&session_path.index_meta);
    let session = Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Junie,
        external_session_id: Some(provider_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: if cursor.terminal {
            SessionStatus::Completed
        } else {
            SessionStatus::Imported
        },
        transcript_blob_id: None,
        started_at: cursor.frontier.state.started_at(),
        ended_at: cursor.frontier.state.ended_at_ms.map(timestamp),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "session_idempotency_key":
                    format!("provider-session:junie:{provider_session_id}"),
                "metadata": {
                    "title": cursor.frontier.state.title,
                    "project_dir": cursor.frontier.state.cwd,
                    "index": provider_capped_json_value(
                        &meta.raw,
                        PROVIDER_MAX_PREVIEW_CHARS,
                    ),
                    "nativepath_publication": PUBLICATION_REVISION,
                },
            }),
        ),
    };
    group.upsert_session(&session)?;
    if !existed {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(ResolvedSource { source_id, session })
}

#[allow(clippy::too_many_arguments)]
fn publish_event(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    resolved: &ResolvedSource,
    generation: u64,
    draft: &EventDraft,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let identity_index = generation
        .checked_mul(GENERATION_EVENT_STRIDE)
        .and_then(|base| base.checked_add(draft.event_index))
        .ok_or(CaptureError::SystemInvariant(
            "Junie generation event identity exhausted",
        ))?;
    if draft.event_index >= GENERATION_EVENT_STRIDE {
        return Err(CaptureError::InvalidPayload(
            "Junie session exceeds the provider-local generation event bound".to_owned(),
        ));
    }
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Junie,
        provider_session_id,
        resolved.source_id,
        identity_index,
        identity_index,
        &draft.event_hash,
        None,
        None,
        generation == 0,
    )?;
    let retained = provider_policy_event_text(draft.event_type, &draft.text, &draft.body);
    let policy_body = provider_policy_body(draft.event_type, &draft.body);
    let provider_payload = json!({
        "text": retained.text,
        "text_retention": retained.retention.as_json(),
        "result_evidence": provider_result_identifier_evidence(
            draft.event_type,
            &draft.text,
            &draft.body,
        ),
        "result_outcome": provider_result_outcome_evidence(draft.event_type, &draft.body),
        "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        "body": provider_capped_json_value(&policy_body, PROVIDER_MAX_PREVIEW_CHARS),
    });
    let mut provider_metadata = draft.metadata.clone();
    let locator = draft
        .binding
        .as_ref()
        .and_then(|(binding, role, tag, target, suffix)| {
            verified_locator(binding, *role, *tag, *target, suffix, &draft.text)
        });
    if let Some(locator) = locator {
        attach_verified_content_locator(&mut provider_metadata, locator).ok_or(
            CaptureError::SystemInvariant("Junie verified-content locator collection is malformed"),
        )?;
    }
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": identity_index,
        "native_event_index": draft.event_index,
        "provider_event_hash": draft.event_hash,
        "provider_event_hash_authority": "provider_supplied",
        "cursor": format!(
            "{}:line:{}:event:{}",
            resolved
                .session
                .capture_source_id
                .map(|_| "junie-session-events")
                .unwrap_or("junie"),
            draft.source_ordinal.saturating_add(1),
            identity_index,
        ),
        "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": usize::try_from(draft.source_ordinal.saturating_add(1))
            .unwrap_or(usize::MAX),
        "imported_at": context.imported_at,
        "event_idempotency_key":
            format!("provider-event:junie:{provider_session_id}:{identity_index}"),
        "source_record_ordinal": draft.source_ordinal,
        "source_record_subrecord_index": draft.source_subrecord,
        "metadata": provider_metadata,
        "nativepath_generation": generation,
    });
    if let Some(locators) = sync_metadata
        .pointer_mut("/metadata")
        .and_then(Value::as_object_mut)
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY))
    {
        sync_metadata[VERIFIED_CONTENT_LOCATORS_METADATA_KEY] = locators;
    }
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &draft.event_hash)
            .unwrap_or(identity.dedupe_key);
    let run = command_run(
        draft,
        options,
        provider_session_id,
        resolved,
        identity.run_source_id,
        generation,
    )?;
    if let Some(run) = &run {
        group.upsert_run(run)?;
    }
    let event = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(resolved.session.id),
        run_id: run.as_ref().map(|run| run.id),
        event_type: draft.event_type,
        role: draft.role,
        occurred_at: draft.occurred_at,
        capture_source_id: Some(resolved.source_id),
        payload: json!({
            "provider": CaptureProvider::Junie.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": identity_index,
            "native_event_index": draft.event_index,
            "provider_event_hash": draft.event_hash,
            "cursor": format!("line:{}", draft.source_ordinal.saturating_add(1)),
            "artifacts": [],
            "body": compact_provider_result_payload(draft.event_type, &provider_payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    let inserted =
        group.reconcile_provider_event(&event, ProviderEventHashAuthority::ProviderSupplied)?;
    if inserted {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    if let Some(change) = &draft.file_change {
        let touch_id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::Junie,
            provider_session_id,
            resolved.source_id,
            Some(identity_index),
            change.touch_index,
            generation == 0,
        )?;
        group.upsert_file_touched(&FileTouched {
            id: touch_id,
            history_record_id: options.history_record_id,
            run_id: None,
            event_id: Some(event.id),
            vcs_workspace_id: None,
            path: change.path.clone(),
            change_kind: Some(change.change_kind),
            old_path: change.old_path.clone(),
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(draft.occurred_at),
            source_id: Some(resolved.source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": CaptureProvider::Junie.as_str(),
                    "provider_session_id": provider_session_id,
                    "provider_touch_index": change.touch_index,
                    "provider_event_index": identity_index,
                    "native_event_index": draft.event_index,
                    "source_id": resolved.source_id,
                    "source_format": JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
                    "session_id": resolved.session.id,
                    "nativepath_generation": generation,
                }),
            ),
        })?;
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    }
    Ok(())
}

fn verified_locator(
    binding: &RecordSetBinding,
    role: VerifiedContentRole,
    tag: u8,
    target: u32,
    suffix: &str,
    content: &str,
) -> Option<VerifiedContentLocatorV1> {
    if role == VerifiedContentRole::MessageBody
        && content.chars().count() <= crate::PROVIDER_MAX_TEXT_CHARS
    {
        return None;
    }
    let encoded = binding.encoded(tag, target)?;
    let record_digest = binding.record_digest()?;
    let content_ref = ContentRef::from_bytes(content.as_bytes())?;
    let profile = verified_content_profile(
        CaptureProvider::Junie,
        JUNIE_SESSION_EVENTS_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        role,
    )?;
    VerifiedContentLocatorV1::new(
        role,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        RECORD_SET_KIND,
        &encoded,
        binding.native_record_id(suffix)?,
        record_digest,
    )
}

fn command_run(
    draft: &EventDraft,
    options: &ProviderImportOptions,
    provider_session_id: &str,
    resolved: &ResolvedSource,
    run_source_id: Option<Uuid>,
    generation: u64,
) -> Result<Option<Run>> {
    if draft.event_type != EventType::CommandOutput {
        return Ok(None);
    }
    let timed_out = draft
        .body
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let exit_code = draft
        .body
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let call_id = draft
        .body
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or(&draft.event_hash);
    let run_key = if generation == 0 {
        call_id.to_owned()
    } else {
        format!("generation:{generation}:{call_id}")
    };
    let id = run_source_id.map_or_else(
        || {
            crate::stable_capture_uuid(
                &format!(
                    "provider:{}:{provider_session_id}:run:{run_key}",
                    CaptureProvider::Junie.as_str()
                ),
                "run",
            )
        },
        |source_id| {
            crate::stable_capture_uuid(&format!("provider-source:{source_id}:run:{run_key}"), "run")
        },
    );
    let duration = draft
        .body
        .get("duration_ms")
        .and_then(Value::as_u64)
        .and_then(|value| i64::try_from(value).ok())
        .and_then(chrono::Duration::try_milliseconds);
    Ok(Some(Run {
        id,
        history_record_id: options.history_record_id,
        session_id: Some(resolved.session.id),
        run_type: RunType::Command,
        status: if timed_out {
            RunStatus::Cancelled
        } else if exit_code.is_some_and(|code| code != 0) {
            RunStatus::Failed
        } else {
            RunStatus::Partial
        },
        started_at: duration
            .and_then(|duration| draft.occurred_at.checked_sub_signed(duration))
            .unwrap_or(draft.occurred_at),
        ended_at: Some(draft.occurred_at),
        exit_code,
        cwd: resolved
            .session
            .sync
            .metadata
            .pointer("/metadata/project_dir")
            .and_then(Value::as_str)
            .map(str::to_owned),
        command_preview: draft
            .body
            .get("command")
            .and_then(Value::as_str)
            .map(|value| provider_local_preview(value, PROVIDER_MAX_PREVIEW_CHARS).0),
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(draft.occurred_at),
        source_id: Some(resolved.source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": draft.event_index,
                "provider_event_hash": draft.event_hash,
                "call_id": call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
}

fn publication_id(
    source_identity: &str,
    cursor: &JunieStoreCursor,
    rows: &[EventDraft],
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-junie-nativepath-publication-v1\0");
    digest.update(source_identity.as_bytes());
    digest.update(cursor.generation.to_le_bytes());
    digest.update(transition.next().stream.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    for row in rows {
        digest.update(row.event_index.to_le_bytes());
        digest.update(row.event_hash.as_bytes());
        digest.update(row.event_type.as_str().as_bytes());
        digest.update(row.text.as_bytes());
        digest.update(serde_json::to_vec(&row.body).unwrap_or_default());
    }
    format!("junie-nativepath-v1:{:x}", digest.finalize())
}

#[cfg(test)]
mod tests;
