use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, BufRead, Cursor},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    CtxHistoryJsonlEdgeRecord, CtxHistoryJsonlEventRecord, CtxHistoryJsonlFileTouchRecord,
    CtxHistoryJsonlRecord, CtxHistoryJsonlSessionRecord, CtxHistoryJsonlSourceRecord, Event,
    EventType, Fidelity, FileTouched, ProviderSourceTrust, Run, RunStatus, RunType, Session,
    SessionEdge, SessionEdgeType, SyncCursor, CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    NativePathRetainedSourceEntities, NativePathSourceEntityFrontier, NativePathSourceEntityKind,
    NativePathSourceGenerationKey, ProviderEventHashAuthority, ProviderSourceLocatorObservation,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementDisposition,
    ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    common::io::{
        ensure_regular_provider_transcript_file, read_provider_jsonl_record_or_skip_oversized,
    },
    complete_content::{VerifiedContentLocatorsV1, VERIFIED_CONTENT_LOCATORS_METADATA_KEY},
    compute_payload_hash,
    provider::{
        importer::{
            compact_provider_result_payload, provider_edge_uuid,
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_session_uuid, provider_source_identity, provider_source_root,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
            ProviderEventImportIdentity,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, CustomHistoryJsonlV1ImportOptions,
    OutputAssociations, OutputCommandContext, OutputNativeCoordinate, OutputObservationKind,
    OutputOutcome, OutputOutcomeMetadata, OutputSourceIdentity, OutputSourceLocator,
    ProOutputObservation, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportSummary,
    ProviderImportWorkResult, Result,
};

use super::{
    custom_history_effective_raw_source_path, custom_history_internal_session_id,
    custom_history_jsonl_v1_cursor_stream, custom_history_key, custom_history_metadata,
    push_provider_import_failure, reject_invalid_custom_history_references,
    retain_custom_history_content_sessions, validate_custom_history_identifier,
    validate_custom_source_record,
};

const CUSTOM_NATIVE_CURSOR_VERSION: u32 = 1;
const CUSTOM_UPSTREAM_CURSOR_VERSION: u32 = 1;
const CUSTOM_OUTPUT_FRONTIER_VERSION: u32 = 1;
const CUSTOM_PARSER_REVISION: &str = "ctx-history-jsonl-v1-nativepath-parser-v1";
const CUSTOM_POLICY_REVISION: &str = "ctx-history-jsonl-v1-core-private-output-v1";
const CUSTOM_ROUTE_SOURCE_FORMAT: &str = "ctx_history_jsonl_v1";
const CUSTOM_CORE_UNITS_PER_PAGE: usize = 128;
const CUSTOM_UPSTREAM_CURSORS_PER_PAGE: usize = 128;
const CUSTOM_RETIREMENT_UNITS_PER_PAGE: usize = 512;
const CUSTOM_OUTPUTS_PER_PAGE: usize = 32;
const CUSTOM_OUTPUT_PAGE_BYTES: usize = 6 * 1024 * 1024;
const PAGE_ACCOUNTING_OVERHEAD_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CustomFileStamp {
    canonical_path: PathBuf,
    len: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl CustomFileStamp {
    fn observe(path: &Path) -> Result<Self> {
        ensure_regular_provider_transcript_file(path)?;
        let metadata = fs::symlink_metadata(path)?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let (device, inode) = (Some(metadata.dev()), Some(metadata.ino()));
        #[cfg(not(unix))]
        let (device, inode) = (None, None);

        Ok(Self {
            canonical_path: fs::canonicalize(path)?,
            len: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device,
            inode,
        })
    }

    fn revalidate(&self) -> Result<bool> {
        match Self::observe(&self.canonical_path) {
            Ok(current) => Ok(current == *self),
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn revision_material(&self, digest: &mut Sha256) {
        digest.update(self.canonical_path.as_os_str().as_encoded_bytes());
        digest.update(self.len.to_be_bytes());
        let (sign, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => (1_u8, duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                (0_u8, duration.as_secs(), duration.subsec_nanos())
            }
        };
        digest.update([sign]);
        digest.update(seconds.to_be_bytes());
        digest.update(nanos.to_be_bytes());
        digest.update([u8::from(self.readonly)]);
        digest.update(self.device.unwrap_or_default().to_be_bytes());
        digest.update(self.inode.unwrap_or_default().to_be_bytes());
    }
}

#[derive(Debug)]
struct ParsedCustomHistory {
    summary: ProviderImportSummary,
    sources: BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    sessions: BTreeMap<(String, String), (usize, CtxHistoryJsonlSessionRecord)>,
    events: Vec<(usize, CtxHistoryJsonlEventRecord)>,
    file_touches: Vec<(usize, CtxHistoryJsonlFileTouchRecord)>,
    edges: Vec<(usize, CtxHistoryJsonlEdgeRecord)>,
    source_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomAnchorAuthority {
    capture_source_id: Uuid,
    canonical_source_identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomRetirementFrontier {
    kind: String,
    id: Uuid,
}

impl CustomRetirementFrontier {
    fn from_store(frontier: NativePathSourceEntityFrontier) -> Self {
        Self {
            kind: frontier.kind.as_str().to_owned(),
            id: frontier.id,
        }
    }

    fn to_store(&self) -> Result<NativePathSourceEntityFrontier> {
        let kind = match self.kind.as_str() {
            "session" => NativePathSourceEntityKind::Session,
            "session_edge" => NativePathSourceEntityKind::SessionEdge,
            "run" => NativePathSourceEntityKind::Run,
            "event" => NativePathSourceEntityKind::Event,
            "file_touch" => NativePathSourceEntityKind::FileTouch,
            _ => {
                return Err(CaptureError::InvalidPayload(
                    "custom history NativePath retirement frontier is invalid".to_owned(),
                ))
            }
        };
        Ok(NativePathSourceEntityFrontier { kind, id: self.id })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum CustomCursorPhase {
    Publish {
        next_unit: u64,
    },
    Retire {
        after: Option<CustomRetirementFrontier>,
    },
    Blocked {
        next_unit: u64,
    },
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomNativeCursor {
    version: u32,
    parser_revision: String,
    policy_revision: String,
    logical_locator: String,
    source_revision: String,
    generation: u64,
    phase: CustomCursorPhase,
    anchor: Option<CustomAnchorAuthority>,
    retired: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomUpstreamCursor {
    version: u32,
    parser_revision: String,
    policy_revision: String,
    raw_cursor: String,
}

struct CustomUpstreamCursorTarget {
    machine_id: String,
    stream: String,
    raw_cursor: String,
    observed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomOutputFrontier {
    version: u32,
    source_revision: String,
    next_output: u64,
}

struct SessionUnit {
    line: usize,
    session: Session,
}

struct EventUnit {
    line: usize,
    event: Event,
    run: Option<Run>,
    authority: ProviderEventHashAuthority,
}

struct FileTouchUnit {
    line: usize,
    file: FileTouched,
}

struct EdgeUnit {
    line: usize,
    actor: CanonicalActor,
    edge: SessionEdge,
}

enum CoreUnit {
    Session(SessionUnit),
    Event(EventUnit),
    FileTouch(FileTouchUnit),
    Edge(EdgeUnit),
}

impl CoreUnit {
    fn retained(&self, retained: &mut NativePathRetainedSourceEntities) {
        match self {
            Self::Session(unit) => retained.session_ids.push(unit.session.id),
            Self::Event(unit) => {
                retained.event_ids.push(unit.event.id);
                if let Some(run) = &unit.run {
                    retained.run_ids.push(run.id);
                }
            }
            Self::FileTouch(unit) => retained.file_touch_ids.push(unit.file.id),
            Self::Edge(unit) => retained.session_edge_ids.push(unit.edge.id),
        }
    }

    fn retained_bytes(&self) -> Result<usize> {
        Ok(match self {
            Self::Session(unit) => serde_json::to_vec(&unit.session)?.len(),
            Self::Event(unit) => serde_json::to_vec(&unit.event)?.len().saturating_add(
                unit.run
                    .as_ref()
                    .map(serde_json::to_vec)
                    .transpose()?
                    .map_or(0, |encoded| encoded.len()),
            ),
            Self::FileTouch(unit) => serde_json::to_vec(&unit.file)?.len(),
            Self::Edge(unit) => serde_json::to_vec(&unit.edge)?.len(),
        })
    }
}

struct CanonicalCustomHistory {
    units: Vec<CoreUnit>,
    anchor_source: Option<CaptureSource>,
    sessions: BTreeMap<(String, String), Session>,
}

struct CustomOutput {
    source_id: String,
    session_id: String,
    event_index: u64,
    event_id: Option<String>,
    event_hash: String,
    event_type: EventType,
    occurred_at: DateTime<Utc>,
    parent_session_id: Option<String>,
    root_session_id: String,
    external_agent_id: Option<String>,
    payload: Value,
}

pub(crate) fn import_custom_history_nativepath(
    path: &Path,
    store: &mut Store,
    options: CustomHistoryJsonlV1ImportOptions,
) -> Result<ProviderImportSummary> {
    let logical_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let logical_locator = logical_locator(&logical_path);
    let stream = custom_native_cursor_stream(&logical_locator);
    let context = ProviderAdapterContext {
        machine_id: options.machine_id.clone(),
        source_path: Some(logical_path.clone()),
        source_root: None,
        imported_at: options.imported_at,
    };
    let stamp = match CustomFileStamp::observe(path) {
        Ok(stamp) => stamp,
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return retire_missing_source(
                store,
                &context,
                &options,
                &logical_locator,
                &stream,
                ProviderSourceRouteRetirementReason::SourceMissing,
            );
        }
        Err(error) => return Err(error),
    };
    let bytes = fs::read(&stamp.canonical_path)?;
    if !stamp.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let source_revision = source_revision(
        &bytes,
        Some(&stamp),
        options.inventory_observation_token.as_deref(),
    );
    let parsed = parse_custom_history(Cursor::new(bytes), source_revision)?;
    import_parsed(
        store,
        &context,
        &options,
        &logical_locator,
        &stream,
        parsed,
        Some(&stamp),
    )
}

pub(crate) fn import_custom_history_nativepath_reader(
    mut reader: impl BufRead,
    store: &mut Store,
    options: CustomHistoryJsonlV1ImportOptions,
) -> Result<ProviderImportSummary> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    let source_revision =
        source_revision(&bytes, None, options.inventory_observation_token.as_deref());
    let parsed = parse_custom_history(Cursor::new(bytes), source_revision)?;
    let logical_locator = options
        .source_path
        .as_ref()
        .map(|path| logical_locator(path))
        .unwrap_or_else(|| logical_reader_locator(&parsed));
    let stream = custom_native_cursor_stream(&logical_locator);
    let context = ProviderAdapterContext {
        machine_id: options.machine_id.clone(),
        source_path: options.source_path.clone(),
        source_root: None,
        imported_at: options.imported_at,
    };
    import_parsed(
        store,
        &context,
        &options,
        &logical_locator,
        &stream,
        parsed,
        None,
    )
}

pub(crate) fn validate_custom_history_nativepath(path: &Path) -> Result<ProviderImportSummary> {
    let stamp = CustomFileStamp::observe(path)?;
    let bytes = fs::read(&stamp.canonical_path)?;
    if !stamp.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let revision = source_revision(&bytes, Some(&stamp), None);
    Ok(parse_custom_history(Cursor::new(bytes), revision)?.summary)
}

pub(crate) fn validate_custom_history_nativepath_reader(
    mut reader: impl BufRead,
) -> Result<ProviderImportSummary> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    let revision = source_revision(&bytes, None, None);
    Ok(parse_custom_history(Cursor::new(bytes), revision)?.summary)
}

fn parse_custom_history(
    mut reader: impl BufRead,
    source_revision: String,
) -> Result<ParsedCustomHistory> {
    let mut summary = ProviderImportSummary::default();
    let mut manifest_line = None;
    let mut manifest_is_structurally_invalid = false;
    let mut sources = BTreeMap::<String, (usize, CtxHistoryJsonlSourceRecord)>::new();
    let mut sessions = BTreeMap::<(String, String), (usize, CtxHistoryJsonlSessionRecord)>::new();
    let mut events = Vec::<(usize, CtxHistoryJsonlEventRecord)>::new();
    let mut event_keys = BTreeSet::<(String, String, u64)>::new();
    let mut file_touches = Vec::<(usize, CtxHistoryJsonlFileTouchRecord)>::new();
    let mut touch_keys = BTreeSet::<(String, String, u64)>::new();
    let mut edges = Vec::<(usize, CtxHistoryJsonlEdgeRecord)>::new();
    let mut edge_keys = BTreeSet::<(String, String, String, String)>::new();
    let mut line = Vec::new();
    let mut line_number = 0_usize;

    while read_provider_jsonl_record_or_skip_oversized(
        &mut reader,
        &mut line,
        &mut line_number,
        &mut summary,
    )? {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let record = match serde_json::from_slice::<CtxHistoryJsonlRecord>(&line) {
            Ok(record) => record,
            Err(error) => {
                push_provider_import_failure(&mut summary, line_number, error.to_string());
                continue;
            }
        };
        match record {
            CtxHistoryJsonlRecord::Manifest(manifest) => {
                if manifest.schema_version != CTX_HISTORY_JSONL_V1_SCHEMA_VERSION {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        format!(
                            "unsupported custom history schema version `{}`",
                            manifest.schema_version
                        ),
                    );
                    manifest_is_structurally_invalid = true;
                }
                if manifest_line.replace(line_number).is_some() {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate manifest record".to_owned(),
                    );
                    manifest_is_structurally_invalid = true;
                }
            }
            CtxHistoryJsonlRecord::Source(source) => {
                let failures_before = summary.failed;
                validate_custom_source_record(&mut summary, line_number, &source);
                if sources.contains_key(&source.source_id) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate source_id".to_owned(),
                    );
                }
                if summary.failed == failures_before {
                    sources.insert(source.source_id.clone(), (line_number, source));
                }
            }
            CtxHistoryJsonlRecord::Session(session) => {
                let failures_before = summary.failed;
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &session.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "session_id",
                    &session.session_id,
                );
                let key = (session.source_id.clone(), session.session_id.clone());
                if sessions.contains_key(&key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate session record".to_owned(),
                    );
                }
                if summary.failed == failures_before {
                    sessions.insert(key, (line_number, session));
                }
            }
            CtxHistoryJsonlRecord::Event(event) => {
                let failures_before = summary.failed;
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &event.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "session_id",
                    &event.session_id,
                );
                let key = (
                    event.source_id.clone(),
                    event.session_id.clone(),
                    event.event_index,
                );
                if event_keys.contains(&key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate event_index for session".to_owned(),
                    );
                }
                if summary.failed == failures_before {
                    event_keys.insert(key);
                    events.push((line_number, event));
                }
            }
            CtxHistoryJsonlRecord::FileTouch(file_touch) => {
                let failures_before = summary.failed;
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &file_touch.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "session_id",
                    &file_touch.session_id,
                );
                if file_touch.path.trim().is_empty() {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "file_touch path must not be empty".to_owned(),
                    );
                }
                let key = (
                    file_touch.source_id.clone(),
                    file_touch.session_id.clone(),
                    file_touch.touch_index,
                );
                if touch_keys.contains(&key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate touch_index for session".to_owned(),
                    );
                }
                if summary.failed == failures_before {
                    touch_keys.insert(key);
                    file_touches.push((line_number, file_touch));
                }
            }
            CtxHistoryJsonlRecord::Edge(edge) => {
                let failures_before = summary.failed;
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "source_id",
                    &edge.source_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "from_session_id",
                    &edge.from_session_id,
                );
                validate_custom_history_identifier(
                    &mut summary,
                    line_number,
                    "to_session_id",
                    &edge.to_session_id,
                );
                let edge_key = edge.edge_id.clone().unwrap_or_else(|| {
                    format!(
                        "{}:{}:{}",
                        edge.from_session_id,
                        edge.to_session_id,
                        edge.edge_type.as_str()
                    )
                });
                let key = (
                    edge.source_id.clone(),
                    edge.from_session_id.clone(),
                    edge.to_session_id.clone(),
                    edge_key,
                );
                if edge_keys.contains(&key) {
                    push_provider_import_failure(
                        &mut summary,
                        line_number,
                        "duplicate edge record".to_owned(),
                    );
                }
                if summary.failed == failures_before {
                    edge_keys.insert(key);
                    edges.push((line_number, edge));
                }
            }
        }
    }

    if manifest_line.is_none() {
        push_provider_import_failure(
            &mut summary,
            0,
            "missing manifest record for ctx-history-jsonl-v1".to_owned(),
        );
        manifest_is_structurally_invalid = true;
    }
    if manifest_is_structurally_invalid {
        sources.clear();
        sessions.clear();
        events.clear();
        file_touches.clear();
        edges.clear();
        return Ok(ParsedCustomHistory {
            summary,
            sources,
            sessions,
            events,
            file_touches,
            edges,
            source_revision,
        });
    }

    reject_invalid_custom_history_references(
        &mut summary,
        &sources,
        &mut sessions,
        &mut events,
        &mut event_keys,
        &mut file_touches,
        &mut edges,
    );
    retain_custom_history_content_sessions(&mut sessions, &events, &file_touches, &edges);
    Ok(ParsedCustomHistory {
        summary,
        sources,
        sessions,
        events,
        file_touches,
        edges,
        source_revision,
    })
}

fn import_parsed(
    store: &mut Store,
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    logical_locator: &str,
    stream: &str,
    mut parsed: ParsedCustomHistory,
    stamp: Option<&CustomFileStamp>,
) -> Result<ProviderImportSummary> {
    let committed_store = Store::open_read_only(store.path())?;
    let mut summary = std::mem::take(&mut parsed.summary);
    let canonical = build_canonical_history(
        &committed_store,
        context,
        options,
        logical_locator,
        &parsed,
        &mut summary,
    )?;
    parsed.summary = summary;
    let outputs = custom_outputs(&parsed, &canonical.sessions)?;
    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            store,
            context,
            options,
            logical_locator,
            stream,
            &parsed,
            &outputs,
        );
        return Ok(parsed.summary);
    }

    let stored = store.get_sync_cursor(None, &context.machine_id, stream)?;
    let current_anchor = canonical
        .anchor_source
        .as_ref()
        .map(|source| CustomAnchorAuthority {
            capture_source_id: source.id,
            canonical_source_identity: canonical_route_identity(logical_locator),
        });
    let (mut cursor, mut expected_cursor) = initial_cursor(
        stored.as_ref(),
        logical_locator,
        &parsed.source_revision,
        current_anchor,
    )?;
    if cursor.retired {
        return Err(CaptureError::SystemInvariant(
            "custom history reactivation retained a retired cursor",
        ));
    }
    if cursor.phase == CustomCursorPhase::Complete {
        parsed.summary.work_remaining = publish_upstream_cursors(
            store,
            context,
            options,
            &parsed.sources,
            &parsed.events,
            stamp,
            &mut parsed.summary,
        )?;
        if parsed.summary.work_result() != ProviderImportWorkResult::Changed {
            parsed
                .summary
                .set_work_result(ProviderImportWorkResult::NoOp);
        }
        replay_outputs_or_mark_behind(
            store,
            context,
            options,
            logical_locator,
            stream,
            &parsed,
            &outputs,
        );
        return Ok(parsed.summary);
    }
    if matches!(cursor.phase, CustomCursorPhase::Blocked { .. }) {
        parsed
            .summary
            .set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(parsed.summary);
    }

    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut changed_groups = 0_usize;
        loop {
            match cursor.phase.clone() {
                CustomCursorPhase::Publish { next_unit } => {
                    let next_unit = usize::try_from(next_unit).map_err(|_| {
                        CaptureError::InvalidPayload(
                            "custom history NativePath unit frontier exceeds platform limits"
                                .to_owned(),
                        )
                    })?;
                    let page_end = next_unit
                        .saturating_add(CUSTOM_CORE_UNITS_PER_PAGE)
                        .min(canonical.units.len());
                    let page = &canonical.units[next_unit..page_end];
                    let needs_anchor_only_page =
                        canonical.units.is_empty() && next_unit == 0 && cursor.anchor.is_some();
                    let next_phase = if page_end < canonical.units.len() {
                        CustomCursorPhase::Publish {
                            next_unit: u64::try_from(page_end).map_err(|_| {
                                CaptureError::InvalidPayload(
                                    "custom history NativePath unit frontier exceeds u64"
                                        .to_owned(),
                                )
                            })?,
                        }
                    } else if parsed.summary.failed != 0 {
                        CustomCursorPhase::Blocked {
                            next_unit: u64::try_from(page_end).unwrap_or(u64::MAX),
                        }
                    } else if cursor.anchor.is_some() {
                        CustomCursorPhase::Retire { after: None }
                    } else {
                        CustomCursorPhase::Complete
                    };
                    let mut next_cursor = cursor.clone();
                    next_cursor.phase = next_phase;
                    let changed = publish_core_page(
                        store,
                        &committed_store,
                        &bulk_guard,
                        context,
                        options,
                        logical_locator,
                        stream,
                        &parsed.source_revision,
                        &canonical,
                        page,
                        next_unit,
                        &cursor,
                        &next_cursor,
                        expected_cursor.clone(),
                        stamp,
                        needs_anchor_only_page,
                        &mut parsed.summary,
                    )?;
                    if changed {
                        changed_groups = changed_groups.saturating_add(1);
                    }
                    cursor = next_cursor;
                }
                CustomCursorPhase::Retire { after } => {
                    let (next_cursor, changed) = publish_retirement_page(
                        store,
                        &bulk_guard,
                        context,
                        logical_locator,
                        stream,
                        &cursor,
                        after.as_ref(),
                        expected_cursor.clone(),
                        stamp,
                    )?;
                    if changed {
                        changed_groups = changed_groups.saturating_add(1);
                        parsed
                            .summary
                            .set_work_result(ProviderImportWorkResult::Changed);
                    }
                    cursor = next_cursor;
                }
                CustomCursorPhase::Blocked { .. } | CustomCursorPhase::Complete => break,
            }
            expected_cursor = store
                .get_sync_cursor(None, &context.machine_id, stream)?
                .map(|stored| stored.cursor);
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && changed_groups != 0 {
                parsed.summary.work_remaining =
                    !matches!(cursor.phase, CustomCursorPhase::Complete);
                break;
            }
            if matches!(
                cursor.phase,
                CustomCursorPhase::Blocked { .. } | CustomCursorPhase::Complete
            ) {
                break;
            }
        }
        Ok(())
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(()), Ok(())) => {}
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    }

    if cursor.phase == CustomCursorPhase::Complete {
        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
            && parsed.summary.work_result() == ProviderImportWorkResult::Changed
        {
            parsed.summary.work_remaining =
                upstream_cursors_pending(store, context, &parsed.sources, &parsed.events)?;
        } else {
            parsed.summary.work_remaining = publish_upstream_cursors(
                store,
                context,
                options,
                &parsed.sources,
                &parsed.events,
                stamp,
                &mut parsed.summary,
            )?;
        }
        replay_outputs_or_mark_behind(
            store,
            context,
            options,
            logical_locator,
            stream,
            &parsed,
            &outputs,
        );
    }
    Ok(parsed.summary)
}

fn upstream_cursor_targets(
    context: &ProviderAdapterContext,
    sources: &BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    events: &[(usize, CtxHistoryJsonlEventRecord)],
) -> Vec<CustomUpstreamCursorTarget> {
    sources
        .values()
        .filter_map(|(_, source)| {
            let source_checkpoint = source
                .cursor
                .as_ref()
                .and_then(|cursor| cursor.after.as_ref())
                .map(|checkpoint| (checkpoint.cursor.clone(), checkpoint.observed_at));
            let event_checkpoint = events
                .iter()
                .filter(|(_, event)| event.source_id == source.source_id)
                .filter_map(|(line, event)| {
                    event
                        .native_cursor
                        .as_ref()
                        .map(|cursor| (*line, cursor.clone(), event.occurred_at))
                })
                .max_by_key(|(line, _, _)| *line)
                .map(|(_, cursor, observed_at)| (cursor, observed_at));
            let (raw_cursor, observed_at) = source_checkpoint.or(event_checkpoint)?;
            Some(CustomUpstreamCursorTarget {
                machine_id: source
                    .machine_id
                    .clone()
                    .unwrap_or_else(|| context.machine_id.clone()),
                stream: custom_history_jsonl_v1_cursor_stream(
                    &source.provider_key,
                    &source.source_id,
                    &source.source_format,
                ),
                raw_cursor,
                observed_at,
            })
        })
        .collect()
}

fn pending_upstream_cursor_transitions(
    store: &Store,
    context: &ProviderAdapterContext,
    sources: &BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    events: &[(usize, CtxHistoryJsonlEventRecord)],
) -> Result<Vec<NativePathCursorTransition>> {
    let mut transitions = Vec::new();
    for target in upstream_cursor_targets(context, sources, events) {
        let stored = store.get_sync_cursor(None, &target.machine_id, &target.stream)?;
        let stored_is_native = stored
            .as_ref()
            .is_some_and(|cursor| decode_native_path_committed_cursor(&cursor.cursor).is_ok());
        let stored_raw = stored
            .as_ref()
            .map(|cursor| decode_released_or_native_upstream_cursor(&cursor.cursor))
            .transpose()?;
        if stored_is_native && stored_raw.as_deref() == Some(target.raw_cursor.as_str()) {
            continue;
        }
        let next = CustomUpstreamCursor {
            version: CUSTOM_UPSTREAM_CURSOR_VERSION,
            parser_revision: CUSTOM_PARSER_REVISION.to_owned(),
            policy_revision: CUSTOM_POLICY_REVISION.to_owned(),
            raw_cursor: target.raw_cursor,
        };
        transitions.push(NativePathCursorTransition::new(
            stored.map(|cursor| cursor.cursor),
            provider_sync_cursor(
                &target.machine_id,
                target.stream,
                serde_json::to_string(&next)?,
                target.observed_at,
            ),
        ));
        if transitions.len() == CUSTOM_UPSTREAM_CURSORS_PER_PAGE {
            break;
        }
    }
    Ok(transitions)
}

fn upstream_cursors_pending(
    store: &Store,
    context: &ProviderAdapterContext,
    sources: &BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    events: &[(usize, CtxHistoryJsonlEventRecord)],
) -> Result<bool> {
    Ok(!pending_upstream_cursor_transitions(store, context, sources, events)?.is_empty())
}

#[allow(clippy::too_many_arguments)]
fn publish_upstream_cursors(
    store: &mut Store,
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    sources: &BTreeMap<String, (usize, CtxHistoryJsonlSourceRecord)>,
    events: &[(usize, CtxHistoryJsonlEventRecord)],
    stamp: Option<&CustomFileStamp>,
    summary: &mut ProviderImportSummary,
) -> Result<bool> {
    let mut transitions = pending_upstream_cursor_transitions(store, context, sources, events)?;
    if transitions.is_empty() {
        return Ok(false);
    }

    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut changed_groups = 0_usize;
        loop {
            if !revalidate(stamp)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let publication_id = upstream_publication_id(&transitions);
            let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
            let mut group = store.begin_native_path_publication_group(
                admission,
                NativePathGroupAccounting::new(
                    1,
                    transitions.len(),
                    PAGE_ACCOUNTING_OVERHEAD_BYTES,
                )?,
            )?;
            match group.classify_cursor_set(&publication_id, &transitions)? {
                NativePathCursorSetClassification::AllNextSameGroup { .. } => {
                    group.commit()?;
                }
                NativePathCursorSetClassification::AllExpected => {
                    if !revalidate(stamp)? {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    group.prepare_journal_checkpoint()?;
                    group.publish_cursor_set()?;
                    group.commit()?;
                    summary.set_work_result(ProviderImportWorkResult::Changed);
                }
            }
            changed_groups = changed_groups.saturating_add(1);
            transitions = pending_upstream_cursor_transitions(store, context, sources, events)?;
            if transitions.is_empty()
                || (options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && changed_groups != 0)
            {
                break;
            }
        }
        Ok(!transitions.is_empty())
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(work_remaining), Ok(())) => Ok(work_remaining),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

pub(super) fn decode_released_or_native_upstream_cursor(encoded: &str) -> Result<String> {
    match decode_native_path_committed_cursor(encoded) {
        Ok(committed) => {
            let cursor: CustomUpstreamCursor = serde_json::from_str(committed.provider_cursor())
                .map_err(|_| {
                    CaptureError::InvalidPayload(
                        "custom history NativePath upstream cursor is corrupt".to_owned(),
                    )
                })?;
            if cursor.version != CUSTOM_UPSTREAM_CURSOR_VERSION
                || cursor.parser_revision != CUSTOM_PARSER_REVISION
                || cursor.policy_revision != CUSTOM_POLICY_REVISION
            {
                return Err(CaptureError::InvalidPayload(
                    "custom history NativePath upstream cursor has an unreleased revision"
                        .to_owned(),
                ));
            }
            Ok(cursor.raw_cursor)
        }
        Err(_) => {
            let looks_native = serde_json::from_str::<Value>(encoded)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .is_some_and(|object| {
                    object.contains_key("publication_id")
                        || object.contains_key("provider_cursor")
                        || object.contains_key("journal_checkpoint")
                });
            if looks_native {
                Err(CaptureError::InvalidPayload(
                    "custom history NativePath cursor envelope is corrupt".to_owned(),
                ))
            } else {
                Ok(encoded.to_owned())
            }
        }
    }
}

fn upstream_publication_id(transitions: &[NativePathCursorTransition]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-custom-history-nativepath-upstream-publication-v1\0");
    for transition in transitions {
        if let Some(expected) = transition.expected_cursor() {
            digest.update((expected.len() as u64).to_be_bytes());
            digest.update(expected.as_bytes());
        } else {
            digest.update(0_u64.to_be_bytes());
        }
        digest.update((transition.next().stream.len() as u64).to_be_bytes());
        digest.update(transition.next().stream.as_bytes());
        digest.update((transition.next().cursor.len() as u64).to_be_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    format!("custom-history-upstream-sha256-v1:{:x}", digest.finalize())
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    logical_locator: &str,
    stream: &str,
    source_revision: &str,
    canonical: &CanonicalCustomHistory,
    page: &[CoreUnit],
    page_start: usize,
    current_cursor: &CustomNativeCursor,
    next_cursor: &CustomNativeCursor,
    expected_cursor: Option<String>,
    stamp: Option<&CustomFileStamp>,
    anchor_only: bool,
    summary: &mut ProviderImportSummary,
) -> Result<bool> {
    if page.is_empty() && !anchor_only && current_cursor.anchor.is_none() {
        return publish_cursor_only(
            store,
            bulk_guard,
            context,
            stream,
            current_cursor,
            next_cursor,
            expected_cursor,
            stamp,
        );
    }
    let anchor = current_cursor
        .anchor
        .as_ref()
        .ok_or(CaptureError::SystemInvariant(
            "custom history NativePath Core page has no source anchor",
        ))?;
    let mut retained = NativePathRetainedSourceEntities::default();
    retained.capture_source_ids.push(anchor.capture_source_id);
    let mut retained_bytes = PAGE_ACCOUNTING_OVERHEAD_BYTES;
    for unit in page {
        unit.retained(&mut retained);
        retained_bytes = retained_bytes.saturating_add(unit.retained_bytes()?);
    }
    dedupe_retained(&mut retained);
    if retained_bytes > ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES {
        return Err(CaptureError::InvalidPayload(
            "custom history Core page exceeds the NativePath retained-byte bound".to_owned(),
        ));
    }
    let generation_key = generation_key(
        context,
        logical_locator,
        stream,
        source_revision,
        current_cursor.generation,
        anchor,
    );
    let transition = NativePathCursorTransition::new(
        expected_cursor,
        provider_sync_cursor(
            &context.machine_id,
            stream.to_owned(),
            encode_cursor(next_cursor)?,
            context.imported_at,
        ),
    );
    let publication_id = publication_id(
        logical_locator,
        current_cursor.generation,
        page_start,
        &transition,
    );
    if !revalidate(stamp)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, retained_bytes)?,
    )?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            return Ok(false);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    if page_start == 0 {
        let resolution =
            group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                provider: CaptureProvider::Custom,
                source_format: CUSTOM_ROUTE_SOURCE_FORMAT.to_owned(),
                machine_id: context.machine_id.clone(),
                locator_identity: logical_locator.to_owned(),
                cursor_stream: stream.to_owned(),
                proposed_source_identity: anchor.canonical_source_identity.clone(),
                raw_source_path: context
                    .source_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                source_revision: source_revision.to_owned(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            })?;
        if resolution.canonical_source_identity != anchor.canonical_source_identity {
            return Err(CaptureError::InvalidPayload(
                "custom history NativePath route canonical identity changed unexpectedly"
                    .to_owned(),
            ));
        }
        if let Some(anchor_source) = &canonical.anchor_source {
            group.upsert_capture_source(anchor_source)?;
        }
        apply_core_units(committed_store, &mut group, page, summary, options)?;
        group.bind_capture_source_provider_route(
            anchor.capture_source_id,
            &resolution.route_binding(),
        )?;
    } else {
        apply_core_units(committed_store, &mut group, page, summary, options)?;
    }
    group.stage_source_generation_page(&generation_key, &retained)?;
    if !revalidate(stamp)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    let _ = canonical;
    Ok(true)
}

fn apply_core_units(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    page: &[CoreUnit],
    summary: &mut ProviderImportSummary,
    _options: &CustomHistoryJsonlV1ImportOptions,
) -> Result<()> {
    for unit in page {
        match unit {
            CoreUnit::Session(unit) => {
                let existed = committed_store.get_session(unit.session.id).is_ok();
                group.upsert_session(&unit.session)?;
                if existed {
                    summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                    summary.skipped = summary.skipped.saturating_add(1);
                } else {
                    summary.imported_sessions = summary.imported_sessions.saturating_add(1);
                    summary.imported = summary.imported.saturating_add(1);
                }
            }
            CoreUnit::Event(unit) => {
                if let Some(run) = &unit.run {
                    group.upsert_run(run)?;
                }
                if group.reconcile_provider_event(&unit.event, unit.authority)? {
                    summary.imported_events = summary.imported_events.saturating_add(1);
                    summary.imported = summary.imported.saturating_add(1);
                } else {
                    summary.skipped_events = summary.skipped_events.saturating_add(1);
                    summary.skipped = summary.skipped.saturating_add(1);
                }
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
            }
            CoreUnit::FileTouch(unit) => {
                group.upsert_file_touched(&unit.file)?;
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
            }
            CoreUnit::Edge(unit) => {
                let existed = committed_store.session_edge_exists(unit.edge.id)?;
                group.upsert_projection_neutral_session_edge(&unit.actor, &unit.edge)?;
                if existed {
                    summary.skipped_edges = summary.skipped_edges.saturating_add(1);
                    summary.skipped = summary.skipped.saturating_add(1);
                } else {
                    summary.imported_edges = summary.imported_edges.saturating_add(1);
                    summary.imported = summary.imported.saturating_add(1);
                }
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn publish_retirement_page(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    logical_locator: &str,
    stream: &str,
    cursor: &CustomNativeCursor,
    after: Option<&CustomRetirementFrontier>,
    expected_cursor: Option<String>,
    stamp: Option<&CustomFileStamp>,
) -> Result<(CustomNativeCursor, bool)> {
    let anchor = cursor.anchor.as_ref().ok_or(CaptureError::SystemInvariant(
        "custom history NativePath retirement has no source anchor",
    ))?;
    let key = generation_key(
        context,
        logical_locator,
        stream,
        &cursor.source_revision,
        cursor.generation,
        anchor,
    );
    let after_store = after.map(CustomRetirementFrontier::to_store).transpose()?;
    if !revalidate(stamp)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, PAGE_ACCOUNTING_OVERHEAD_BYTES)?,
    )?;
    let preview = group.preview_source_generation_retirement_page(
        &key,
        after_store.as_ref(),
        CUSTOM_RETIREMENT_UNITS_PER_PAGE,
    )?;
    let mut next = cursor.clone();
    next.phase = if preview.done {
        CustomCursorPhase::Complete
    } else {
        CustomCursorPhase::Retire {
            after: preview
                .next_after
                .clone()
                .map(CustomRetirementFrontier::from_store),
        }
    };
    let transition = NativePathCursorTransition::new(
        expected_cursor,
        provider_sync_cursor(
            &context.machine_id,
            stream.to_owned(),
            encode_cursor(&next)?,
            context.imported_at,
        ),
    );
    let publication_id = retirement_publication_id(logical_locator, cursor.generation, &transition);
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            return Ok((next, false));
        }
        NativePathCursorSetClassification::AllExpected => {}
    }
    let retired = group.retire_source_generation_page(
        &key,
        after_store.as_ref(),
        CUSTOM_RETIREMENT_UNITS_PER_PAGE,
        context.imported_at.timestamp_millis(),
    )?;
    if retired != preview {
        return Err(CaptureError::SystemInvariant(
            "custom history NativePath retirement preview changed",
        ));
    }
    if !revalidate(stamp)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    Ok((next, true))
}

#[allow(clippy::too_many_arguments)]
fn publish_cursor_only(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    stream: &str,
    current: &CustomNativeCursor,
    next: &CustomNativeCursor,
    expected_cursor: Option<String>,
    stamp: Option<&CustomFileStamp>,
) -> Result<bool> {
    if !revalidate(stamp)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let transition = NativePathCursorTransition::new(
        expected_cursor,
        provider_sync_cursor(
            &context.machine_id,
            stream.to_owned(),
            encode_cursor(next)?,
            context.imported_at,
        ),
    );
    let publication_id =
        publication_id(&current.logical_locator, current.generation, 0, &transition);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, PAGE_ACCOUNTING_OVERHEAD_BYTES)?,
    )?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            Ok(false)
        }
        NativePathCursorSetClassification::AllExpected => {
            group.prepare_journal_checkpoint()?;
            group.publish_cursor_set()?;
            group.commit()?;
            Ok(true)
        }
    }
}

fn initial_cursor(
    stored: Option<&SyncCursor>,
    logical_locator: &str,
    source_revision: &str,
    current_anchor: Option<CustomAnchorAuthority>,
) -> Result<(CustomNativeCursor, Option<String>)> {
    let Some(stored) = stored else {
        return Ok((
            new_cursor(logical_locator, source_revision, 0, current_anchor),
            None,
        ));
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        let prior = decode_cursor(committed.provider_cursor())?;
        validate_cursor(&prior, logical_locator)?;
        if prior.source_revision == source_revision && !prior.retired {
            return Ok((prior, Some(stored.cursor.clone())));
        }
        let generation = prior
            .generation
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "custom history NativePath generation exhausted",
            ))?;
        return Ok((
            new_cursor(
                logical_locator,
                source_revision,
                generation,
                current_anchor.or(prior.anchor),
            ),
            Some(stored.cursor.clone()),
        ));
    }
    if CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.is_none() {
        return Err(CaptureError::InvalidPayload(
            "custom history cursor is neither NativePath nor a released migration cursor"
                .to_owned(),
        ));
    }
    Ok((
        new_cursor(logical_locator, source_revision, 0, current_anchor),
        Some(stored.cursor.clone()),
    ))
}

fn new_cursor(
    logical_locator: &str,
    source_revision: &str,
    generation: u64,
    anchor: Option<CustomAnchorAuthority>,
) -> CustomNativeCursor {
    CustomNativeCursor {
        version: CUSTOM_NATIVE_CURSOR_VERSION,
        parser_revision: CUSTOM_PARSER_REVISION.to_owned(),
        policy_revision: CUSTOM_POLICY_REVISION.to_owned(),
        logical_locator: logical_locator.to_owned(),
        source_revision: source_revision.to_owned(),
        generation,
        phase: CustomCursorPhase::Publish { next_unit: 0 },
        anchor,
        retired: false,
    }
}

fn build_canonical_history(
    store: &Store,
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    logical_locator: &str,
    parsed: &ParsedCustomHistory,
    summary: &mut ProviderImportSummary,
) -> Result<CanonicalCustomHistory> {
    let physical_anchor =
        nativepath_anchor_source(context, logical_locator, &parsed.source_revision);
    let ordered_session_keys = ordered_sessions(&parsed.sessions, summary);
    let mut sessions = BTreeMap::new();
    let mut session_units = Vec::new();
    for key in ordered_session_keys {
        let (line, record) = &parsed.sessions[&key];
        let source = &parsed.sources[&record.source_id].1;
        let unit =
            canonical_session_unit(context, options, *line, physical_anchor.id, source, record);
        sessions.insert(key, unit.session.clone());
        session_units.push(CoreUnit::Session(unit));
    }
    let mut event_units = Vec::new();
    let mut event_ids = BTreeMap::<(String, String, u64), Uuid>::new();
    for (line, record) in &parsed.events {
        let source = &parsed.sources[&record.source_id].1;
        let Some(session) = sessions.get(&(record.source_id.clone(), record.session_id.clone()))
        else {
            continue;
        };
        let provider_session_id =
            session
                .external_session_id
                .as_deref()
                .ok_or(CaptureError::SystemInvariant(
                    "custom history canonical session has no external ID",
                ))?;
        let capture_source_id = session
            .capture_source_id
            .ok_or(CaptureError::SystemInvariant(
                "custom history canonical session has no capture source",
            ))?;
        let identity_source_id = provider_scoped_source_uuid(
            CaptureProvider::Custom,
            provider_session_id,
            &source.source_format,
            custom_history_effective_raw_source_path(source, context).as_deref(),
        );
        let payload_hash = match &record.event_hash {
            Some(hash) => hash.clone(),
            None => compute_payload_hash(&record.payload)?,
        };
        let authority = if record.event_hash.is_some() {
            ProviderEventHashAuthority::ProviderSupplied
        } else {
            ProviderEventHashAuthority::NormalizedPayloadFallback
        };
        let identity = provider_event_import_identity_with_exact_legacy_source(
            store,
            CaptureProvider::Custom,
            provider_session_id,
            identity_source_id,
            record.event_index,
            record.event_index,
            &payload_hash,
            None,
            None,
            true,
        )?;
        let (event, run) = custom_history_canonical_event(
            provider_session_id,
            source,
            record,
            source.observed_at.unwrap_or(context.imported_at),
            options.history_record_id,
            capture_source_id,
            session.id,
            *line,
            &payload_hash,
            authority,
            &identity,
        )?;
        event_ids.insert(
            (
                record.source_id.clone(),
                record.session_id.clone(),
                record.event_index,
            ),
            event.id,
        );
        event_units.push(CoreUnit::Event(EventUnit {
            line: *line,
            event,
            run,
            authority,
        }));
    }

    let mut touch_units = Vec::new();
    for (line, record) in &parsed.file_touches {
        let source = &parsed.sources[&record.source_id].1;
        let Some(session) = sessions.get(&(record.source_id.clone(), record.session_id.clone()))
        else {
            continue;
        };
        let capture_source_id = session
            .capture_source_id
            .ok_or(CaptureError::SystemInvariant(
                "custom history file touch session has no capture source",
            ))?;
        let provider_session_id =
            session
                .external_session_id
                .as_deref()
                .ok_or(CaptureError::SystemInvariant(
                    "custom history file touch session has no external ID",
                ))?;
        let event_id = record.event_index.and_then(|index| {
            event_ids
                .get(&(record.source_id.clone(), record.session_id.clone(), index))
                .copied()
        });
        let identity_source_id = provider_scoped_source_uuid(
            CaptureProvider::Custom,
            provider_session_id,
            &source.source_format,
            custom_history_effective_raw_source_path(source, context).as_deref(),
        );
        let touch_id = provider_file_touch_import_id(
            store,
            CaptureProvider::Custom,
            provider_session_id,
            identity_source_id,
            record.event_index,
            record.touch_index,
            true,
        )?;
        let file = custom_history_canonical_file_touch(
            context,
            source,
            record,
            provider_session_id,
            options.history_record_id,
            capture_source_id,
            session.id,
            event_id,
            touch_id,
        );
        touch_units.push(CoreUnit::FileTouch(FileTouchUnit { line: *line, file }));
    }

    let mut edge_units = BTreeMap::<Uuid, EdgeUnit>::new();
    for ((source_id, _), session) in &sessions {
        let Some(parent_id) = session.parent_session_id else {
            continue;
        };
        let provider_session_id =
            session
                .external_session_id
                .as_deref()
                .ok_or(CaptureError::SystemInvariant(
                    "custom history child session has no external ID",
                ))?;
        let source = &parsed.sources[source_id].1;
        let edge = SessionEdge {
            id: provider_edge_uuid(CaptureProvider::Custom, provider_session_id, "parent_child"),
            from_session_id: parent_id,
            to_session_id: session.id,
            edge_type: SessionEdgeType::ParentChild,
            confidence: ctx_history_core::Confidence::Explicit,
            source_id: session.capture_source_id,
            timestamps: timestamps(source.observed_at.unwrap_or(context.imported_at)),
            sync: provider_sync_metadata(
                session.sync.fidelity,
                json!({
                    "provider_session_id": provider_session_id,
                    "parent_provider_session_id": session
                        .sync
                        .metadata
                        .get("parent_provider_session_id"),
                    "source_format": source.source_format,
                    "fixture_line": parsed.sessions[&(source_id.clone(), source_session_id(session)?)]
                        .0,
                    "imported_at": source.observed_at.unwrap_or(context.imported_at),
                }),
            ),
        };
        let parent = sessions
            .values()
            .find(|candidate| candidate.id == parent_id)
            .ok_or(CaptureError::SystemInvariant(
                "custom history parent session vanished after validation",
            ))?;
        edge_units.insert(
            edge.id,
            EdgeUnit {
                line: parsed.sessions[&(source_id.clone(), source_session_id(session)?)].0,
                actor: canonical_actor(parent),
                edge,
            },
        );
    }
    for (line, record) in &parsed.edges {
        let source = &parsed.sources[&record.source_id].1;
        let (Some(from), Some(to)) = (
            sessions.get(&(record.source_id.clone(), record.from_session_id.clone())),
            sessions.get(&(record.source_id.clone(), record.to_session_id.clone())),
        ) else {
            continue;
        };
        let from_provider_session_id =
            from.external_session_id
                .as_deref()
                .ok_or(CaptureError::SystemInvariant(
                    "custom history edge source session has no external ID",
                ))?;
        let to_provider_session_id =
            to.external_session_id
                .as_deref()
                .ok_or(CaptureError::SystemInvariant(
                    "custom history edge target session has no external ID",
                ))?;
        let edge_id = if record.edge_type == SessionEdgeType::ParentChild {
            provider_edge_uuid(
                CaptureProvider::Custom,
                to_provider_session_id,
                "parent_child",
            )
        } else {
            let key = custom_history_key(json!({
                "schema": CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
                "kind": "session_edge",
                "provider_key": source.provider_key,
                "source_id": source.source_id,
                "from_provider_session_id": from_provider_session_id,
                "to_provider_session_id": to_provider_session_id,
                "edge_type": record.edge_type.as_str(),
                "edge_id": record.edge_id,
            }));
            stable_capture_uuid(&key, "session-edge")
        };
        let edge = SessionEdge {
            id: edge_id,
            from_session_id: from.id,
            to_session_id: to.id,
            edge_type: record.edge_type,
            confidence: record.confidence,
            source_id: to.capture_source_id,
            timestamps: timestamps(record.occurred_at.unwrap_or(context.imported_at)),
            sync: provider_sync_metadata(
                record.fidelity,
                json!({
                    "provider_key": source.provider_key,
                    "source_id": source.source_id,
                    "history_record_id": options.history_record_id,
                    "metadata": custom_history_metadata(
                        record.metadata.clone(),
                        json!({
                            "provider_key": source.provider_key,
                            "source_id": record.source_id,
                            "from_session_id": record.from_session_id,
                            "to_session_id": record.to_session_id,
                            "edge_id": record.edge_id,
                        }),
                    ),
                }),
            ),
        };
        if edge_units.contains_key(&edge_id) {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
            summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
            continue;
        }
        edge_units.insert(
            edge_id,
            EdgeUnit {
                line: *line,
                actor: canonical_actor(from),
                edge,
            },
        );
    }

    let mut units = session_units;
    units.extend(event_units);
    units.extend(touch_units);
    units.extend(edge_units.into_values().map(CoreUnit::Edge));
    let anchor_source = (!units.is_empty()).then_some(physical_anchor);
    Ok(CanonicalCustomHistory {
        units,
        anchor_source,
        sessions,
    })
}

#[allow(clippy::too_many_arguments)]
fn custom_history_canonical_event(
    provider_session_id: &str,
    source: &CtxHistoryJsonlSourceRecord,
    record: &CtxHistoryJsonlEventRecord,
    imported_at: DateTime<Utc>,
    history_record_id: Option<Uuid>,
    capture_source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    payload_hash: &str,
    hash_authority: ProviderEventHashAuthority,
    identity: &ProviderEventImportIdentity,
) -> Result<(Event, Option<Run>)> {
    let payload = record.payload.clone();
    let mut provider_metadata = custom_history_metadata(
        record.metadata.clone(),
        json!({
            "provider_key": source.provider_key,
            "source_id": record.source_id,
            "session_id": record.session_id,
            "event_id": record.event_id,
            "native_cursor": record.native_cursor,
            "preview": record.preview,
        }),
    );
    let source_record_coordinates = take_custom_source_record_coordinates(&mut provider_metadata)?;
    let verified_content_locators = take_custom_verified_content_locators(&mut provider_metadata)?;
    let run = custom_history_command_run(
        provider_session_id,
        session_id,
        capture_source_id,
        identity.run_source_id,
        history_record_id,
        record,
        &payload,
        payload_hash,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, payload_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": record.event_index,
        "provider_event_hash": payload_hash,
        "provider_event_hash_authority": hash_authority.as_str(),
        "cursor": record.native_cursor,
        "source_format": source.source_format,
        "source_trust": effective_trust(source.trust),
        "fixture_line": line_number,
        "imported_at": imported_at,
        "event_idempotency_key": record.idempotency_key,
        "source_record_ordinal": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.0),
        "source_record_subrecord_index": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.1),
        "metadata": provider_metadata,
    });
    if let Some(locators) = verified_content_locators {
        if let Some(metadata) = sync_metadata.as_object_mut() {
            metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
        }
    }
    Ok((
        Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id,
            session_id: Some(session_id),
            run_id: run.as_ref().map(|run| run.id),
            event_type: record.event_type,
            role: record.role,
            occurred_at: record.occurred_at,
            capture_source_id: Some(capture_source_id),
            payload: json!({
                "provider": CaptureProvider::Custom.as_str(),
                "provider_session_id": provider_session_id,
                "provider_event_index": record.event_index,
                "provider_event_hash": payload_hash,
                "cursor": record.native_cursor,
                "artifacts": record.artifacts,
                "body": compact_provider_result_payload(record.event_type, &payload),
            }),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key),
            sync: provider_sync_metadata(record.fidelity, sync_metadata),
        },
        run,
    ))
}

#[allow(clippy::too_many_arguments)]
fn custom_history_command_run(
    provider_session_id: &str,
    session_id: Uuid,
    source_id: Uuid,
    run_source_id: Option<Uuid>,
    history_record_id: Option<Uuid>,
    record: &CtxHistoryJsonlEventRecord,
    payload: &Value,
    event_hash: &str,
) -> Result<Option<Run>> {
    if record.event_type != EventType::CommandOutput {
        return Ok(None);
    }
    let arguments_preview = payload.get("arguments_preview");
    let command_preview = payload
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| arguments_preview.and_then(crate::provider::tool_input::command));
    let cwd = payload
        .get("workdir")
        .or_else(|| payload.get("cwd"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| arguments_preview.and_then(crate::provider::tool_input::working_directory));
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let key = call_id.unwrap_or(event_hash);
    let started_at = custom_command_started_at(record.occurred_at, payload)?;
    let id = run_source_id.map_or_else(
        || {
            stable_capture_uuid(
                &format!(
                    "provider:{}:{provider_session_id}:run:{key}",
                    CaptureProvider::Custom.as_str()
                ),
                "run",
            )
        },
        |run_source_id| {
            stable_capture_uuid(&format!("provider-source:{run_source_id}:run:{key}"), "run")
        },
    );
    Ok(Some(Run {
        id,
        history_record_id,
        session_id: Some(session_id),
        run_type: RunType::Command,
        status: custom_command_run_status(payload),
        started_at,
        ended_at: Some(record.occurred_at),
        exit_code: payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        cwd,
        command_preview,
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(record.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            record.fidelity,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": record.event_index,
                "provider_event_hash": event_hash,
                "call_id": call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
}

fn custom_command_started_at(occurred_at: DateTime<Utc>, payload: &Value) -> Result<DateTime<Utc>> {
    let Some(value) = payload.get("duration_ms") else {
        return Ok(occurred_at);
    };
    if value.is_null() {
        return Ok(occurred_at);
    }
    let duration = value
        .as_i64()
        .ok_or_else(|| CaptureError::InvalidPayload("duration_ms must be an integer".to_owned()))?;
    if duration < 0 {
        return Err(CaptureError::InvalidPayload(format!(
            "duration_ms must be nonnegative, got {duration}"
        )));
    }
    let span = chrono::Duration::try_milliseconds(duration).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "duration_ms is not representable as milliseconds: {duration}"
        ))
    })?;
    occurred_at.checked_sub_signed(span).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "duration_ms moves command start before representable time: {duration}"
        ))
    })
}

fn custom_command_run_status(payload: &Value) -> RunStatus {
    if payload
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return RunStatus::Cancelled;
    }
    match payload.get("exit_code").and_then(Value::as_i64) {
        Some(0) => RunStatus::Succeeded,
        Some(_) => RunStatus::Failed,
        None => {
            let outcome = payload
                .get("result_outcome")
                .or_else(|| payload.get("outcome"))
                .or_else(|| payload.get("status"))
                .and_then(Value::as_str)
                .map(str::trim)
                .map(str::to_ascii_lowercase);
            match outcome.as_deref() {
                Some("timeout" | "timed_out" | "timedout" | "cancelled" | "canceled") => {
                    RunStatus::Cancelled
                }
                Some("failure" | "failed" | "error" | "errored") => RunStatus::Failed,
                Some("success" | "succeeded" | "complete" | "completed" | "ok" | "passed") => {
                    RunStatus::Succeeded
                }
                _ => RunStatus::Partial,
            }
        }
    }
}

fn take_custom_verified_content_locators(metadata: &mut Value) -> Result<Option<Value>> {
    let Some(object) = metadata.as_object_mut() else {
        return Ok(None);
    };
    let Some(value) = object.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY) else {
        return Ok(None);
    };
    let locators = VerifiedContentLocatorsV1::from_metadata_value(&value).ok_or_else(|| {
        CaptureError::InvalidPayload("verified content locator annotation is malformed".to_owned())
    })?;
    Ok(Some(locators.to_metadata_value()))
}

fn take_custom_source_record_coordinates(metadata: &mut Value) -> Result<Option<(u64, u32)>> {
    let Some(object) = metadata.as_object_mut() else {
        return Ok(None);
    };
    let ordinal = object.remove("source_record_ordinal");
    let subrecord = object.remove("source_record_subrecord_index");
    if ordinal.is_none() && subrecord.is_none() {
        return Ok(None);
    }
    let ordinal = ordinal.and_then(|value| value.as_u64()).ok_or_else(|| {
        CaptureError::InvalidPayload("source record ordinal annotation is malformed".to_owned())
    })?;
    let subrecord = subrecord
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "source record subrecord annotation is malformed".to_owned(),
            )
        })?;
    Ok(Some((ordinal, subrecord)))
}

#[allow(clippy::too_many_arguments)]
fn custom_history_canonical_file_touch(
    context: &ProviderAdapterContext,
    source: &CtxHistoryJsonlSourceRecord,
    record: &CtxHistoryJsonlFileTouchRecord,
    provider_session_id: &str,
    history_record_id: Option<Uuid>,
    source_id: Uuid,
    session_id: Uuid,
    event_id: Option<Uuid>,
    touch_id: Uuid,
) -> FileTouched {
    let raw_source_path = custom_history_effective_raw_source_path(source, context);
    let declared_source_root = context
        .source_root_display()
        .or_else(|| source.raw_source_path.clone())
        .or_else(|| source.raw_uri.clone());
    let source_root =
        provider_source_root(declared_source_root.as_deref(), raw_source_path.as_deref());
    FileTouched {
        id: touch_id,
        history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: record.path.clone(),
        change_kind: record.change_kind,
        old_path: record.old_path.clone(),
        line_count_delta: record.line_count_delta,
        confidence: record.confidence,
        timestamps: timestamps(record.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::Custom.as_str(),
                "provider_session_id": provider_session_id,
                "provider_touch_index": record.touch_index,
                "provider_event_index": record.event_index,
                "raw_source_path": raw_source_path,
                "source_id": source_id,
                "source_format": source.source_format,
                "source_root": source_root,
                "metadata": custom_history_metadata(
                    record.metadata.clone(),
                    json!({
                        "provider_key": source.provider_key,
                        "source_id": record.source_id,
                        "session_id": record.session_id,
                    }),
                ),
                "session_id": session_id,
            }),
        ),
    }
}

fn nativepath_anchor_source(
    context: &ProviderAdapterContext,
    logical_locator: &str,
    source_revision: &str,
) -> CaptureSource {
    let canonical_source_identity = canonical_route_identity(logical_locator);
    CaptureSource {
        id: stable_capture_uuid(
            &format!("custom-history-nativepath:{logical_locator}"),
            "source",
        ),
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Custom,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: context
                .source_path
                .as_ref()
                .map(|path| path.display().to_string()),
            source_format: Some(CUSTOM_ROUTE_SOURCE_FORMAT.to_owned()),
            source_root: None,
            source_identity: Some(canonical_source_identity.clone()),
            external_session_id: None,
        },
        started_at: context.imported_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "source_format": CUSTOM_ROUTE_SOURCE_FORMAT,
                "source_trust": "provider_export",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_revision": source_revision,
                "nativepath_publication": CUSTOM_PARSER_REVISION,
                "physical_jsonl_anchor": true,
            }),
        ),
    }
}

fn canonical_session_unit(
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    line: usize,
    capture_source_id: Uuid,
    source: &CtxHistoryJsonlSourceRecord,
    record: &CtxHistoryJsonlSessionRecord,
) -> SessionUnit {
    let provider_session_id = custom_history_internal_session_id(
        &source.provider_key,
        &source.source_id,
        &record.session_id,
    );
    let raw_source_path = custom_history_effective_raw_source_path(source, context);
    let source_root = context
        .source_root_display()
        .or_else(|| source.raw_source_path.clone())
        .or_else(|| source.raw_uri.clone());
    let source_metadata = custom_history_metadata(
        source.metadata.clone(),
        json!({
            "provider_key": source.provider_key,
            "source_id": source.source_id,
            "source_format": source.source_format,
            "raw_uri": source.raw_uri,
            "raw_source_path": source.raw_source_path,
            "fingerprint": source.fingerprint,
            "importer_version": source.importer_version,
            "cursor": source.cursor,
        }),
    );
    let source_identity = provider_source_identity(
        CaptureProvider::Custom,
        &source.source_format,
        source_root.as_deref(),
        raw_source_path.as_deref(),
        Some(&format!(
            "ctx-history-jsonl-v1:{}:{}",
            source.provider_key, source.source_id
        )),
        &source_metadata,
    );
    let semantic_source_id = provider_scoped_source_uuid(
        CaptureProvider::Custom,
        &provider_session_id,
        &source.source_format,
        raw_source_path.as_deref(),
    );
    let imported_at = source.observed_at.unwrap_or(context.imported_at);
    let parent_session_id = record.parent_session_id.as_ref().map(|parent| {
        provider_session_uuid(
            CaptureProvider::Custom,
            &custom_history_internal_session_id(&source.provider_key, &source.source_id, parent),
        )
    });
    let root_session_id = record
        .root_session_id
        .as_ref()
        .map(|root| {
            provider_session_uuid(
                CaptureProvider::Custom,
                &custom_history_internal_session_id(&source.provider_key, &source.source_id, root),
            )
        })
        .or(parent_session_id);
    let session_metadata = custom_history_metadata(
        record.metadata.clone(),
        json!({
            "provider_key": source.provider_key,
            "source_id": source.source_id,
            "session_id": record.session_id,
            "native_session_id": record.native_session_id,
            "parent_session_id": record.parent_session_id,
            "root_session_id": record.root_session_id,
        }),
    );
    let session = Session {
        id: provider_session_uuid(CaptureProvider::Custom, &provider_session_id),
        history_record_id: options.history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(capture_source_id),
        provider: CaptureProvider::Custom,
        external_session_id: Some(provider_session_id.clone()),
        external_agent_id: record.external_agent_id.clone(),
        agent_type: record.agent_type,
        role_hint: record.role_hint.clone(),
        is_primary: record.is_primary,
        status: record.status,
        transcript_blob_id: None,
        started_at: record.started_at,
        ended_at: record.ended_at,
        timestamps: timestamps(imported_at),
        sync: provider_sync_metadata(
            record.fidelity,
            json!({
                "provider_session_id": provider_session_id,
                "parent_provider_session_id": record.parent_session_id.as_ref().map(|parent| {
                    custom_history_internal_session_id(
                        &source.provider_key,
                        &source.source_id,
                        parent,
                    )
                }),
                "root_provider_session_id": record.root_session_id.as_ref().map(|root| {
                    custom_history_internal_session_id(
                        &source.provider_key,
                        &source.source_id,
                        root,
                    )
                }),
                "source_format": source.source_format,
                "source_trust": effective_trust(source.trust),
                "source_cursor": source.cursor,
                "source_identity": source_identity,
                "source_root": source_root,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Custom,
                    &provider_session_id,
                    &source.source_format,
                    raw_source_path.as_deref(),
                ),
                "source_metadata": source_metadata,
                "semantic_capture_source_id": semantic_source_id,
                "fixture_line": line,
                "imported_at": imported_at,
                "session_idempotency_key": record.idempotency_key.clone().or_else(|| Some(format!(
                    "ctx-history-jsonl-v1:{}:{}:{}",
                    source.provider_key, source.source_id, record.session_id
                ))),
                "artifacts": record.artifacts,
                "metadata": session_metadata,
                "nativepath_publication": CUSTOM_PARSER_REVISION,
            }),
        ),
    };
    SessionUnit { line, session }
}

fn ordered_sessions(
    sessions: &BTreeMap<(String, String), (usize, CtxHistoryJsonlSessionRecord)>,
    summary: &mut ProviderImportSummary,
) -> Vec<(String, String)> {
    let mut remaining = sessions.keys().cloned().collect::<BTreeSet<_>>();
    let mut emitted = BTreeSet::new();
    let mut ordered = Vec::new();
    loop {
        let ready = remaining
            .iter()
            .filter(|key| {
                let session = &sessions[*key].1;
                [
                    session.parent_session_id.as_ref(),
                    session.root_session_id.as_ref(),
                ]
                .into_iter()
                .flatten()
                .all(|dependency| {
                    dependency == &session.session_id
                        || emitted.contains(&(session.source_id.clone(), dependency.clone()))
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            break;
        }
        for key in ready {
            remaining.remove(&key);
            emitted.insert(key.clone());
            ordered.push(key);
        }
    }
    for key in remaining {
        let line = sessions[&key].0;
        push_provider_import_failure(
            summary,
            line,
            format!(
                "session `{}` in source `{}` has a cyclic parent/root relationship",
                key.1, key.0
            ),
        );
    }
    ordered
}

fn canonical_actor(session: &Session) -> CanonicalActor {
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

fn source_session_id(session: &Session) -> Result<String> {
    session
        .sync
        .metadata
        .pointer("/metadata/ctx_history_jsonl_v1/session_id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(CaptureError::SystemInvariant(
            "custom history session metadata lost native session ID",
        ))
}

fn effective_trust(trust: ProviderSourceTrust) -> ProviderSourceTrust {
    match trust {
        ProviderSourceTrust::Unknown => ProviderSourceTrust::ProviderExport,
        other => other,
    }
}

fn custom_outputs(
    parsed: &ParsedCustomHistory,
    canonical_sessions: &BTreeMap<(String, String), Session>,
) -> Result<Vec<CustomOutput>> {
    let mut outputs = Vec::new();
    for (_, event) in &parsed.events {
        if !matches!(
            event.event_type,
            EventType::ToolOutput | EventType::CommandOutput
        ) {
            continue;
        }
        if !canonical_sessions.contains_key(&(event.source_id.clone(), event.session_id.clone())) {
            continue;
        }
        let session = &parsed.sessions[&(event.source_id.clone(), event.session_id.clone())].1;
        let source = &parsed.sources[&event.source_id].1;
        let direct_session_id = custom_history_internal_session_id(
            &source.provider_key,
            &source.source_id,
            &session.session_id,
        );
        let root_session_id = session
            .root_session_id
            .as_ref()
            .map(|root| {
                custom_history_internal_session_id(&source.provider_key, &source.source_id, root)
            })
            .unwrap_or_else(|| direct_session_id.clone());
        let parent_session_id = session.parent_session_id.as_ref().map(|parent| {
            custom_history_internal_session_id(&source.provider_key, &source.source_id, parent)
        });
        outputs.push(CustomOutput {
            source_id: event.source_id.clone(),
            session_id: direct_session_id,
            event_index: event.event_index,
            event_id: event.event_id.clone(),
            event_hash: event
                .event_hash
                .clone()
                .unwrap_or(compute_payload_hash(&event.payload)?),
            event_type: event.event_type,
            occurred_at: event.occurred_at,
            parent_session_id,
            root_session_id,
            external_agent_id: session.external_agent_id.clone(),
            payload: event.payload.clone(),
        });
    }
    Ok(outputs)
}

fn replay_outputs_or_mark_behind(
    store: &Store,
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    logical_locator: &str,
    stream: &str,
    parsed: &ParsedCustomHistory,
    outputs: &[CustomOutput],
) {
    let Some(sink) = options.import_profile.sink().map(AsRef::as_ref) else {
        return;
    };
    if let Err(error) = verify_committed_core(
        store,
        context,
        logical_locator,
        stream,
        &parsed.source_revision,
    ) {
        sink.mark_behind(ProOutputSinkError::new(
            "custom_history_nativepath_output_core",
            error.to_string(),
        ));
        return;
    }
    if let Err(error) = replay_outputs(sink, logical_locator, &parsed.source_revision, outputs) {
        sink.mark_behind(ProOutputSinkError::new(
            "custom_history_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn verify_committed_core(
    store: &Store,
    context: &ProviderAdapterContext,
    logical_locator: &str,
    stream: &str,
    source_revision: &str,
) -> Result<()> {
    let stored = store
        .get_sync_cursor(None, &context.machine_id, stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "custom history output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let cursor = decode_cursor(committed.provider_cursor())?;
    validate_cursor(&cursor, logical_locator)?;
    if cursor.retired
        || cursor.source_revision != source_revision
        || cursor.phase != CustomCursorPhase::Complete
    {
        return Err(CaptureError::InvalidPayload(
            "custom history output source no longer matches committed Core authority".to_owned(),
        ));
    }
    Ok(())
}

fn replay_outputs(
    sink: &dyn ProOutputSink,
    logical_locator: &str,
    source_revision: &str,
    outputs: &[CustomOutput],
) -> Result<()> {
    let source = OutputSourceIdentity {
        provider: CaptureProvider::Custom.as_str().to_owned(),
        namespace_id: logical_locator.to_owned(),
        source_id: logical_locator.to_owned(),
    };
    let progress = match sink.observe_source(&source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let mut state = CustomOutputState::new(
        source,
        progress,
        source_revision,
        sink.materializer_revision(),
    )?;
    let mut next_output = state.next_output.min(outputs.len());
    if outputs.is_empty() {
        publish_output_page(
            sink,
            logical_locator,
            source_revision,
            outputs,
            &mut state,
            0,
            0,
            true,
        )?;
        return Ok(());
    }
    while next_output < outputs.len() {
        let mut end = next_output;
        let mut content_bytes = 0_usize;
        while end < outputs.len() && end.saturating_sub(next_output) < CUSTOM_OUTPUTS_PER_PAGE {
            let bytes = output_content(&outputs[end].payload)?.len();
            let next_bytes = content_bytes.saturating_add(bytes);
            if end != next_output && next_bytes > CUSTOM_OUTPUT_PAGE_BYTES {
                break;
            }
            if next_bytes > CUSTOM_OUTPUT_PAGE_BYTES {
                return Err(CaptureError::InvalidPayload(
                    "one custom history output exceeds the bounded Pro page".to_owned(),
                ));
            }
            content_bytes = next_bytes;
            end = end.saturating_add(1);
        }
        let terminal = end == outputs.len();
        if !publish_output_page(
            sink,
            logical_locator,
            source_revision,
            outputs,
            &mut state,
            next_output,
            end,
            terminal,
        )? {
            break;
        }
        next_output = end;
    }
    Ok(())
}

struct CustomOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    next_output: usize,
}

impl CustomOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        observed_revision: &str,
        materializer_revision: &str,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source,
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
                next_output: 0,
            });
        };
        let decoded = progress
            .cursor
            .as_ref()
            .filter(|cursor| cursor.version == CUSTOM_OUTPUT_FRONTIER_VERSION)
            .and_then(|cursor| serde_json::from_slice::<CustomOutputFrontier>(&cursor.payload).ok())
            .filter(|frontier| frontier.version == CUSTOM_OUTPUT_FRONTIER_VERSION);
        let can_resume = progress.parser_revision == CUSTOM_PARSER_REVISION
            && progress.materializer_revision == materializer_revision
            && progress.observed_revision == observed_revision
            && decoded
                .as_ref()
                .is_some_and(|frontier| frontier.source_revision == observed_revision);
        let expected_sink_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let source_epoch = if can_resume {
            progress.source_epoch
        } else {
            progress
                .source_epoch
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "custom history output source epoch exhausted",
                ))?
        };
        Ok(Self {
            source,
            source_epoch,
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier,
            disposition: if can_resume {
                ProOutputSourceDisposition::AppendOrResume
            } else {
                ProOutputSourceDisposition::Rewrite
            },
            next_output: if can_resume {
                decoded
                    .and_then(|frontier| usize::try_from(frontier.next_output).ok())
                    .unwrap_or(0)
            } else {
                0
            },
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_output_page(
    sink: &dyn ProOutputSink,
    logical_locator: &str,
    source_revision: &str,
    outputs: &[CustomOutput],
    state: &mut CustomOutputState,
    start: usize,
    end: usize,
    terminal: bool,
) -> Result<bool> {
    let expected_frontier = output_frontier(source_revision, start)?;
    let next_frontier = output_frontier(source_revision, end)?;
    let observations = outputs[start..end]
        .iter()
        .map(custom_output_observation)
        .collect::<Result<Vec<_>>>()?;
    let content_bytes = observations.iter().fold(0_usize, |total, output| {
        total.saturating_add(output.content.len())
    });
    let output = NativeProOutputPage {
        inventory_generation: sink.inventory_generation(),
        source: state.source.clone(),
        source_epoch: state.source_epoch,
        observed_revision: source_revision.to_owned(),
        parser_revision: CUSTOM_PARSER_REVISION.to_owned(),
        materializer_revision: sink.materializer_revision().to_owned(),
        disposition: state.disposition,
        expected_prior_source_epoch: state.expected_source_epoch,
        expected_prior_frontier: state.expected_sink_frontier.clone(),
        observations,
    };
    let replay = NativeProReplayPage::new_with_source_identity(
        NativeSourceIdentity::new(CaptureProvider::Custom.as_str(), logical_locator),
        expected_frontier,
        next_frontier.clone(),
        terminal,
        NativePageAccounting {
            logical_units: end.saturating_sub(start).max(1),
            conservative_serialized_bytes: content_bytes
                .saturating_add(PAGE_ACCOUNTING_OVERHEAD_BYTES),
        },
        output,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if process_pro_replay_only(replay, sink).is_err() {
        return Ok(false);
    }
    state.expected_source_epoch = Some(state.source_epoch);
    state.expected_sink_frontier = Some(next_frontier);
    state.disposition = ProOutputSourceDisposition::AppendOrResume;
    Ok(true)
}

fn custom_output_observation(output: &CustomOutput) -> Result<ProOutputObservation> {
    let call_id = output
        .payload
        .get("call_id")
        .or_else(|| output.payload.get("tool_call_id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let command = output
        .payload
        .get("command")
        .and_then(Value::as_str)
        .map(|command| OutputCommandContext {
            tool_name: if output.event_type == EventType::CommandOutput {
                "command".to_owned()
            } else {
                output
                    .payload
                    .get("tool")
                    .or_else(|| output.payload.get("tool_name"))
                    .and_then(Value::as_str)
                    .unwrap_or("custom")
                    .to_owned()
            },
            command: command.to_owned(),
            working_directory: output
                .payload
                .get("cwd")
                .or_else(|| output.payload.get("workdir"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    Ok(ProOutputObservation {
        kind: if output.event_type == EventType::CommandOutput {
            OutputObservationKind::Command
        } else {
            OutputObservationKind::Tool
        },
        coordinate: OutputNativeCoordinate {
            unit_key: format!(
                "{}:{}:{}",
                output.source_id, output.session_id, output.event_index
            ),
            native_sequence: output.event_index,
            native_record_id: output.event_id.clone(),
            source_record_ordinal: Some(output.event_index),
            source_record_subrecord_index: Some(0),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: Some(output.occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: output.session_id.clone(),
            root_session_id: output.root_session_id.clone(),
            parent_session_id: output.parent_session_id.clone(),
            provider_session_id: Some(output.session_id.clone()),
            agent_id: output.external_agent_id.clone(),
            repository: None,
        },
        call_id,
        command,
        outcome: output_outcome(&output.payload),
        locator: OutputSourceLocator {
            version: 1,
            kind: "ctx-history-jsonl-v1-event-v1".to_owned(),
            payload: serde_json::to_vec(&json!({
                "source_id": output.source_id,
                "session_id": output.session_id,
                "event_index": output.event_index,
                "event_id": output.event_id,
                "event_hash": output.event_hash,
            }))?,
        },
        content: output_content(&output.payload)?,
    })
}

fn output_content(payload: &Value) -> Result<Vec<u8>> {
    for key in ["body", "output", "content", "text", "result"] {
        if let Some(value) = payload.get(key) {
            if let Some(text) = value.as_str() {
                return Ok(text.as_bytes().to_vec());
            }
            if !value.is_null() {
                return serde_json::to_vec(value).map_err(CaptureError::from);
            }
        }
    }
    serde_json::to_vec(payload).map_err(CaptureError::from)
}

fn output_outcome(payload: &Value) -> OutputOutcomeMetadata {
    let exit_code = payload
        .get("exit_code")
        .or_else(|| payload.get("exitCode"))
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok());
    let duration_ms = payload
        .get("duration_ms")
        .or_else(|| payload.get("durationMs"))
        .and_then(Value::as_u64);
    let status = payload
        .get("result_outcome")
        .or_else(|| payload.get("outcome"))
        .or_else(|| payload.get("status"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    let outcome = if payload
        .get("timed_out")
        .or_else(|| payload.get("timedOut"))
        .and_then(Value::as_bool)
        == Some(true)
        || matches!(
            status.as_deref(),
            Some("timeout" | "timed_out" | "timedout")
        ) {
        OutputOutcome::Timeout
    } else if payload
        .get("is_error")
        .or_else(|| payload.get("isError"))
        .and_then(Value::as_bool)
        == Some(true)
        || exit_code.is_some_and(|code| code != 0)
        || matches!(
            status.as_deref(),
            Some("failure" | "failed" | "error" | "errored")
        )
    {
        OutputOutcome::Failure
    } else if payload
        .get("is_error")
        .or_else(|| payload.get("isError"))
        .and_then(Value::as_bool)
        == Some(false)
        || exit_code == Some(0)
        || matches!(
            status.as_deref(),
            Some("success" | "succeeded" | "complete" | "completed" | "ok")
        )
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    OutputOutcomeMetadata {
        outcome,
        exit_code,
        duration_ms,
    }
}

fn output_frontier(source_revision: &str, next_output: usize) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        CUSTOM_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&CustomOutputFrontier {
            version: CUSTOM_OUTPUT_FRONTIER_VERSION,
            source_revision: source_revision.to_owned(),
            next_output: u64::try_from(next_output).map_err(|_| {
                CaptureError::InvalidPayload(
                    "custom history output frontier exceeds u64".to_owned(),
                )
            })?,
        })?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn retire_missing_source(
    store: &Store,
    context: &ProviderAdapterContext,
    options: &CustomHistoryJsonlV1ImportOptions,
    logical_locator: &str,
    stream: &str,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let Some(stored) = store.get_sync_cursor(None, &context.machine_id, stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: context.source_path.clone().unwrap_or_default(),
            reason: "custom history JSONL source does not exist",
        });
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let prior = decode_cursor(committed.provider_cursor())?;
    validate_cursor(&prior, logical_locator)?;
    if prior.retired {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let mut next = prior.clone();
    next.retired = true;
    next.phase = CustomCursorPhase::Complete;
    let transition = NativePathCursorTransition::new(
        Some(stored.cursor),
        provider_sync_cursor(
            &context.machine_id,
            stream.to_owned(),
            encode_cursor(&next)?,
            context.imported_at,
        ),
    );
    let retirement = prior
        .anchor
        .as_ref()
        .map(|anchor| ProviderSourceRouteRetirement {
            provider: CaptureProvider::Custom,
            source_format: CUSTOM_ROUTE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: logical_locator.to_owned(),
            cursor_stream: stream.to_owned(),
            expected_canonical_source_identity: anchor.canonical_source_identity.clone(),
            expected_source_revision: prior.source_revision.clone(),
            retired_at_ms: context.imported_at.timestamp_millis(),
            reason,
        });
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(1, 1, PAGE_ACCOUNTING_OVERHEAD_BYTES)?,
        )?;
        let publication_id = missing_publication_id(logical_locator, &transition);
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllNextSameGroup { .. } => {
                group.commit()?;
                return Ok(false);
            }
            NativePathCursorSetClassification::AllExpected => {}
        }
        if let Some(retirement) = &retirement {
            let disposition = group.retire_provider_source_route(retirement)?;
            if disposition != ProviderSourceRouteRetirementDisposition::Retired {
                return Err(CaptureError::InvalidPayload(
                    "custom history source route was already retired before publication".to_owned(),
                ));
            }
        }
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
        group.commit()?;
        Ok(true)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let changed = match (operation, finish) {
        (Ok(changed), Ok(())) => changed,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };
    let mut summary = ProviderImportSummary::default();
    if changed {
        summary.skipped = 1;
        summary.skipped_sessions = 1;
        summary.set_work_result(ProviderImportWorkResult::Changed);
    } else {
        summary.set_work_result(ProviderImportWorkResult::NoOp);
    }
    let _ = options;
    Ok(summary)
}

fn generation_key(
    context: &ProviderAdapterContext,
    logical_locator: &str,
    stream: &str,
    source_revision: &str,
    generation: u64,
    anchor: &CustomAnchorAuthority,
) -> NativePathSourceGenerationKey {
    NativePathSourceGenerationKey {
        provider: CaptureProvider::Custom,
        source_format: CUSTOM_ROUTE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        canonical_source_identity: anchor.canonical_source_identity.clone(),
        locator_identity: logical_locator.to_owned(),
        cursor_stream: stream.to_owned(),
        source_revision: source_revision.to_owned(),
        generation_id: format!("custom-history-nativepath-v1:{generation}:{source_revision}"),
    }
}

fn dedupe_retained(retained: &mut NativePathRetainedSourceEntities) {
    retained.capture_source_ids.sort_unstable();
    retained.capture_source_ids.dedup();
    retained.session_ids.sort_unstable();
    retained.session_ids.dedup();
    retained.session_edge_ids.sort_unstable();
    retained.session_edge_ids.dedup();
    retained.run_ids.sort_unstable();
    retained.run_ids.dedup();
    retained.event_ids.sort_unstable();
    retained.event_ids.dedup();
    retained.file_touch_ids.sort_unstable();
    retained.file_touch_ids.dedup();
}

fn logical_locator(path: &Path) -> String {
    let display = path.display().to_string();
    let normalized = display.replace('\\', "/");
    format!(
        "custom-history-logical-v1:{}",
        stable_capture_uuid(&normalized, "custom-history-logical-locator")
    )
}

fn logical_reader_locator(parsed: &ParsedCustomHistory) -> String {
    let identities = parsed
        .sources
        .values()
        .map(|(_, source)| {
            (
                source.provider_key.as_str(),
                source.source_id.as_str(),
                source.source_format.as_str(),
            )
        })
        .collect::<Vec<_>>();
    format!(
        "custom-history-reader-v1:{}",
        stable_capture_uuid(
            &serde_json::to_string(&identities).unwrap_or_default(),
            "custom-history-reader-locator",
        )
    )
}

fn custom_native_cursor_stream(logical_locator: &str) -> String {
    format!(
        "provider:custom:ctx-history-jsonl-v1:{}",
        stable_capture_uuid(logical_locator, "custom-history-native-cursor-stream")
    )
}

fn canonical_route_identity(logical_locator: &str) -> String {
    stable_capture_uuid(logical_locator, "custom-history-native-canonical-source").to_string()
}

fn source_revision(
    bytes: &[u8],
    stamp: Option<&CustomFileStamp>,
    inventory_token: Option<&str>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-custom-history-nativepath-source-v1\0");
    if let Some(stamp) = stamp {
        stamp.revision_material(&mut digest);
    }
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
    if let Some(token) = inventory_token {
        digest.update((token.len() as u64).to_be_bytes());
        digest.update(token.as_bytes());
    }
    format!(
        "custom-history-nativepath-sha256-v1:{:x}",
        digest.finalize()
    )
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
                CaptureProvider::Custom.as_str(),
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

fn encode_cursor(cursor: &CustomNativeCursor) -> Result<String> {
    serde_json::to_string(cursor).map_err(CaptureError::from)
}

fn decode_cursor(encoded: &str) -> Result<CustomNativeCursor> {
    serde_json::from_str(encoded).map_err(|error| {
        CaptureError::InvalidPayload(format!("invalid custom history NativePath cursor: {error}"))
    })
}

fn validate_cursor(cursor: &CustomNativeCursor, logical_locator: &str) -> Result<()> {
    if cursor.version != CUSTOM_NATIVE_CURSOR_VERSION
        || cursor.parser_revision != CUSTOM_PARSER_REVISION
        || cursor.policy_revision != CUSTOM_POLICY_REVISION
        || cursor.logical_locator != logical_locator
    {
        return Err(CaptureError::InvalidPayload(
            "custom history NativePath cursor is incompatible with this source".to_owned(),
        ));
    }
    Ok(())
}

fn publication_id(
    logical_locator: &str,
    generation: u64,
    page_start: usize,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-custom-history-nativepath-publication-v1\0");
    digest.update(logical_locator.as_bytes());
    digest.update(generation.to_be_bytes());
    digest.update((page_start as u64).to_be_bytes());
    digest.update(transition.next().stream.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("custom-history-nativepath-v1:{:x}", digest.finalize())
}

fn retirement_publication_id(
    logical_locator: &str,
    generation: u64,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-custom-history-nativepath-retirement-v1\0");
    digest.update(logical_locator.as_bytes());
    digest.update(generation.to_be_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!(
        "custom-history-nativepath-retirement-v1:{:x}",
        digest.finalize()
    )
}

fn missing_publication_id(
    logical_locator: &str,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-custom-history-nativepath-missing-v1\0");
    digest.update(logical_locator.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!(
        "custom-history-nativepath-missing-v1:{:x}",
        digest.finalize()
    )
}

fn revalidate(stamp: Option<&CustomFileStamp>) -> Result<bool> {
    stamp.map(CustomFileStamp::revalidate).unwrap_or(Ok(true))
}
