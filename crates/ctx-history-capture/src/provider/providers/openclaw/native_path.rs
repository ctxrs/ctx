use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, EventRole, EventType, Fidelity, FileChangeKind, FileTouched, Session,
    SessionEdge, SessionEdgeType, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    provider::{
        file_touches::visit_all_file_touch_drafts,
        importer::{
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_source_cursor_stream_for_path,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
        normalization::{provider_capped_json, provider_local_preview, provider_timestamp_value},
    },
    released_jsonl_cursor::released_jsonl_position_offset,
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputAssociations,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputSourceIdentity,
    OutputSourceLocator, ProOutputObservation, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    MAX_PROVIDER_JSONL_LINE_BYTES, OPENCLAW_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    complete_content, normalization, openclaw_agent_id, openclaw_output_metadata,
    OpenClawFrozenFileMetadata, OpenClawSessionObservation, OPENCLAW_RELEASED_CAPTURE_REVISION,
    OPENCLAW_RELEASED_POLICY_REVISION,
};

const CURSOR_VERSION: u32 = 1;
const PARSER_REVISION: u32 = 1;
const POLICY_REVISION: u32 = 1;
const OUTPUT_FRONTIER_VERSION: u32 = 1;
const OUTPUT_PARSER_REVISION: &str = "openclaw-nativepath-jsonl-v1";
const PREFIX_HASH_DOMAIN: &[u8] = b"ctx-openclaw-nativepath-prefix-v1\0";
const PUBLICATION_DOMAIN: &[u8] = b"ctx-openclaw-nativepath-publication-v1\0";
const RETIREMENT_DOMAIN: &[u8] = b"ctx-openclaw-nativepath-retirement-v1\0";

const PAGE_MAX_RECORDS: usize = 64;
const PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;
const PAGE_ENVELOPE_BYTES: usize = 2 * 1024;
const EVENT_ENVELOPE_BYTES: usize = 1024;
const GROUP_MAX_PAGES: usize = 32;
const GROUP_MAX_SOURCES: usize = 64;
const GROUP_MAX_BYTES: usize = 6 * 1024 * 1024;
const GROUP_MAX_ESTIMATED_MUTATIONS: usize = 3_000;
const FAILURE_PREVIEW_CHARS: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ObservedTime {
    before_epoch: bool,
    seconds: u64,
    nanos: u32,
}

impl ObservedTime {
    fn from_system_time(value: SystemTime) -> Self {
        match value.duration_since(UNIX_EPOCH) {
            Ok(duration) => Self {
                before_epoch: false,
                seconds: duration.as_secs(),
                nanos: duration.subsec_nanos(),
            },
            Err(error) => {
                let duration = error.duration();
                Self {
                    before_epoch: true,
                    seconds: duration.as_secs(),
                    nanos: duration.subsec_nanos(),
                }
            }
        }
    }

    fn to_system_time(self) -> Result<SystemTime> {
        let duration = Duration::new(self.seconds, self.nanos);
        if self.before_epoch {
            UNIX_EPOCH.checked_sub(duration)
        } else {
            UNIX_EPOCH.checked_add(duration)
        }
        .ok_or(CaptureError::SystemInvariant(
            "OpenClaw observed time is outside SystemTime",
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FrozenMetadata {
    length: u64,
    modified: ObservedTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl FrozenMetadata {
    fn from_live(value: &OpenClawFrozenFileMetadata) -> Self {
        Self {
            length: value.length,
            modified: ObservedTime::from_system_time(value.modified),
            readonly: value.readonly,
            device: value.device,
            inode: value.inode,
        }
    }

    fn to_live(&self) -> Result<OpenClawFrozenFileMetadata> {
        Ok(OpenClawFrozenFileMetadata {
            length: self.length,
            modified: self.modified.to_system_time()?,
            readonly: self.readonly,
            device: self.device,
            inode: self.inode,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SourceObservation {
    transcript: FrozenMetadata,
    index_file: Option<FrozenMetadata>,
    index_revision: u64,
}

impl SourceObservation {
    fn from_live(value: &OpenClawSessionObservation) -> Self {
        Self {
            transcript: FrozenMetadata::from_live(&value.transcript),
            index_file: value.index_file.as_ref().map(FrozenMetadata::from_live),
            index_revision: value.index_revision,
        }
    }

    fn matches_live(&self, value: &OpenClawSessionObservation) -> Result<bool> {
        Ok(self.transcript.to_live()? == value.transcript
            && self
                .index_file
                .as_ref()
                .map(FrozenMetadata::to_live)
                .transpose()?
                == value.index_file
            && self.index_revision == value.index_revision)
    }

    fn auxiliary_matches_live(&self, value: &OpenClawSessionObservation) -> Result<bool> {
        Ok(self
            .index_file
            .as_ref()
            .map(FrozenMetadata::to_live)
            .transpose()?
            == value.index_file
            && self.index_revision == value.index_revision)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct HeaderAnchor {
    start: u64,
    end: u64,
    digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SessionCursor {
    provider_session_id: String,
    agent_id: Option<String>,
    parent_provider_session_id: Option<String>,
    root_provider_session_id: Option<String>,
    started_at: DateTime<Utc>,
    cwd: Option<String>,
    header_anchor: Option<HeaderAnchor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u32,
    parser_revision: u32,
    policy_revision: u32,
    generation: u64,
    source_path: PathBuf,
    source_observation: SourceObservation,
    route_source_revision: String,
    complete_prefix_end: u64,
    complete_prefix_sha256: [u8; 32],
    next_raw_ordinal: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
    session: SessionCursor,
    terminal: bool,
}

impl Checkpoint {
    fn supported(&self) -> bool {
        self.version == CURSOR_VERSION
            && self.parser_revision == PARSER_REVISION
            && self.policy_revision == POLICY_REVISION
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorWire {
    version: u32,
    kind: String,
    checkpoint: Checkpoint,
}

enum CursorDecode {
    Native(Checkpoint),
    Migrated(Checkpoint),
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedSessionState {
    provider_session_id: String,
    agent_id: Option<String>,
    started_at: DateTime<Utc>,
    cwd: Option<String>,
    index_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedParserCheckpoint {
    session: ReleasedSessionState,
    next_ordinal: u64,
    header_anchor: Option<HeaderAnchor>,
    emitted_session: bool,
    accepted_events: u64,
}

#[derive(Debug, Clone)]
struct SessionFact {
    cursor: SessionCursor,
    index: Value,
    header: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceChange {
    Fresh,
    Unchanged,
    Append,
    Rewrite,
    Truncation,
    Replacement,
}

#[derive(Debug)]
struct CoreEvent {
    raw_ordinal: u64,
    provider_event_index: u64,
    provider_event_sequence_index: u64,
    provider_event_hash: String,
    cursor: String,
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: DateTime<Utc>,
    payload: Value,
    metadata: Value,
}

#[derive(Debug)]
struct CoreTouch {
    raw_ordinal: u64,
    event_ordinal: Option<u64>,
    path: String,
    old_path: Option<String>,
    change_kind: Option<FileChangeKind>,
    occurred_at: DateTime<Utc>,
}

#[derive(Debug)]
struct OutputFact {
    raw_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    occurred_at: DateTime<Utc>,
    kind: OutputObservationKind,
    native_record_id: String,
    call_id: Option<String>,
    command: Option<crate::OutputCommandContext>,
    outcome: crate::OutputOutcomeMetadata,
    content: Vec<u8>,
}

#[derive(Debug)]
struct Rejection {
    raw_ordinal: u64,
    reason: String,
}

#[derive(Debug)]
struct Page {
    expected_checkpoint: Checkpoint,
    next_checkpoint: Checkpoint,
    source_change: SourceChange,
    session: SessionFact,
    events: Vec<CoreEvent>,
    touches: Vec<CoreTouch>,
    outputs: Vec<OutputFact>,
    rejections: Vec<Rejection>,
    logical_units: usize,
    conservative_serialized_bytes: usize,
    terminal: bool,
}

struct ScanOutcome {
    checkpoint: Checkpoint,
    source_change: SourceChange,
    accepted_events: u64,
    rejected_records: u64,
}

struct PageReader {
    path: PathBuf,
    imported_at: DateTime<Utc>,
    collect_outputs: bool,
    observation: OpenClawSessionObservation,
    source_revision: String,
    path_identity: String,
    generation: u64,
    reader: BufReader<File>,
    prefix_hasher: Sha256,
    complete_prefix_end: u64,
    next_raw_ordinal: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
    session: SessionFact,
    source_change: SourceChange,
    skip_scan: bool,
    finished: bool,
    outcome: Option<ScanOutcome>,
}

fn encode_cursor(checkpoint: &Checkpoint) -> Result<String> {
    Ok(serde_json::to_string(&CursorWire {
        version: CURSOR_VERSION,
        kind: "openclaw-nativepath-jsonl".to_owned(),
        checkpoint: checkpoint.clone(),
    })?)
}

fn decode_cursor(
    encoded_store_cursor: &str,
    path: &Path,
    observation: &OpenClawSessionObservation,
) -> Result<CursorDecode> {
    let encoded = decode_native_path_committed_cursor(encoded_store_cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| encoded_store_cursor.to_owned());
    if let Ok(wire) = serde_json::from_str::<CursorWire>(&encoded) {
        if wire.version == CURSOR_VERSION
            && wire.kind == "openclaw-nativepath-jsonl"
            && wire.checkpoint.supported()
        {
            return Ok(CursorDecode::Native(wire.checkpoint));
        }
        return Ok(CursorDecode::Reset);
    }
    migrate_released_cursor(&encoded, path, observation)
}

fn migrate_released_cursor(
    encoded: &str,
    path: &Path,
    observation: &OpenClawSessionObservation,
) -> Result<CursorDecode> {
    let Some(released) = CertifiedProviderCursor::decode_if_certified(encoded)? else {
        return Ok(CursorDecode::Reset);
    };
    if released.parser_revision() != OPENCLAW_RELEASED_CAPTURE_REVISION
        || released.policy_revision() != OPENCLAW_RELEASED_POLICY_REVISION
        || released.source_revision() != observation.source_revision()
    {
        return Ok(CursorDecode::Reset);
    }
    let complete_prefix_end = released_jsonl_position_offset(released.native_position())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if complete_prefix_end > observation.transcript.length {
        return Ok(CursorDecode::Reset);
    }
    let legacy: ReleasedParserCheckpoint = released.parser_checkpoint().deserialize()?;
    if legacy.session.index_revision != observation.index_revision {
        return Ok(CursorDecode::Reset);
    }
    let agent_id = legacy.session.agent_id;
    let parent_provider_session_id = related_session_id(
        &observation.index,
        agent_id.as_deref(),
        &["parentSessionId", "parent_session_id"],
    );
    let root_provider_session_id = related_session_id(
        &observation.index,
        agent_id.as_deref(),
        &["rootSessionId", "root_session_id"],
    )
    .or_else(|| parent_provider_session_id.clone());
    Ok(CursorDecode::Migrated(Checkpoint {
        version: CURSOR_VERSION,
        parser_revision: PARSER_REVISION,
        policy_revision: POLICY_REVISION,
        generation: 0,
        source_path: fs::canonicalize(path)?,
        source_observation: SourceObservation::from_live(observation),
        route_source_revision: observation.source_revision(),
        complete_prefix_end,
        complete_prefix_sha256: prefix_sha256(path, complete_prefix_end)?,
        next_raw_ordinal: legacy.next_ordinal,
        accepted_events: legacy.accepted_events,
        accepted_file_touches: 0,
        rejected_records: released.rejected_records(),
        session: SessionCursor {
            provider_session_id: legacy.session.provider_session_id,
            agent_id,
            parent_provider_session_id,
            root_provider_session_id,
            started_at: legacy.session.started_at,
            cwd: legacy.session.cwd,
            header_anchor: legacy.header_anchor,
        },
        terminal: complete_prefix_end == observation.transcript.length,
    }))
}

fn open_pages(
    path: &Path,
    imported_at: DateTime<Utc>,
    collect_outputs: bool,
    inventory_observation_token: Option<&str>,
    reactivate_retired_route: bool,
    previous: Option<&Checkpoint>,
) -> Result<PageReader> {
    let observation = OpenClawSessionObservation::read(path)?;
    let canonical_path = observation.canonical_path.clone();
    let path_identity = provider_path_identity(&canonical_path)?;
    let source_revision = source_revision(&observation, inventory_observation_token);
    let mut file = File::open(&canonical_path)?;
    if OpenClawFrozenFileMetadata::from_metadata(&file.metadata()?)? != observation.transcript {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let mut prefix_hasher = new_prefix_hasher();
    let mut complete_prefix_end = 0_u64;
    let mut next_raw_ordinal = 0_u64;
    let mut accepted_events = 0_u64;
    let mut accepted_file_touches = 0_u64;
    let mut rejected_records = 0_u64;
    let mut session = fresh_session(&canonical_path, imported_at, &observation.index);
    let mut source_change = SourceChange::Fresh;
    let mut generation = 0_u64;
    let mut skip_scan = false;

    if let Some(previous) = previous.filter(|checkpoint| checkpoint.supported()) {
        if reactivate_retired_route {
            source_change = SourceChange::Replacement;
            generation = next_generation(previous)?;
        } else {
            let same_path = previous.source_path == canonical_path;
            let same_physical = same_file_identity(
                &previous.source_observation.transcript,
                &observation.transcript,
            )?;
            let enough_bytes = observation.transcript.length >= previous.complete_prefix_end;
            if same_path && same_physical && enough_bytes {
                let observed_prefix =
                    hash_prefix(&mut file, previous.complete_prefix_end, new_prefix_hasher())?;
                if prefix_digest(&observed_prefix) == previous.complete_prefix_sha256
                    && previous
                        .source_observation
                        .auxiliary_matches_live(&observation)?
                {
                    prefix_hasher = observed_prefix;
                    complete_prefix_end = previous.complete_prefix_end;
                    next_raw_ordinal = previous.next_raw_ordinal;
                    accepted_events = previous.accepted_events;
                    accepted_file_touches = previous.accepted_file_touches;
                    rejected_records = previous.rejected_records;
                    session = resume_session(previous, &observation)?;
                    generation = previous.generation;
                    source_change = if observation.transcript.length == previous.complete_prefix_end
                        && previous.terminal
                    {
                        skip_scan = true;
                        SourceChange::Unchanged
                    } else {
                        SourceChange::Append
                    };
                } else {
                    source_change = SourceChange::Rewrite;
                    generation = next_generation(previous)?;
                }
            } else if same_path && observation.transcript.length < previous.complete_prefix_end {
                source_change = SourceChange::Truncation;
                generation = next_generation(previous)?;
            } else if same_path {
                source_change = SourceChange::Replacement;
                generation = next_generation(previous)?;
            }
        }
    }

    file.seek(SeekFrom::Start(complete_prefix_end))?;
    Ok(PageReader {
        path: canonical_path,
        imported_at,
        collect_outputs,
        observation,
        source_revision,
        path_identity,
        generation,
        reader: BufReader::new(file),
        prefix_hasher,
        complete_prefix_end,
        next_raw_ordinal,
        accepted_events,
        accepted_file_touches,
        rejected_records,
        session,
        source_change,
        skip_scan,
        finished: false,
        outcome: None,
    })
}

fn next_generation(previous: &Checkpoint) -> Result<u64> {
    previous
        .generation
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "OpenClaw NativePath source generation exhausted",
        ))
}

impl PageReader {
    fn next_page(&mut self) -> Result<Option<Page>> {
        if self.finished {
            return Ok(None);
        }
        if self.skip_scan {
            self.finish(true)?;
            return Ok(None);
        }

        let expected_checkpoint = self.checkpoint(false);
        let mut events = Vec::new();
        let mut touches = Vec::new();
        let mut outputs = Vec::new();
        let mut rejections = Vec::new();
        let mut physical_records = 0_usize;
        let mut logical_units = 0_usize;
        let mut serialized_bytes = PAGE_ENVELOPE_BYTES;

        while physical_records < PAGE_MAX_RECORDS {
            let start = self.complete_prefix_end;
            let ordinal = self.next_raw_ordinal;
            let hasher_before = self.prefix_hasher.clone();
            let line = read_bounded_line(
                &mut self.reader,
                &mut self.prefix_hasher,
                self.observation.transcript.length,
                start,
            )?;
            let (bytes, end) = match line {
                Line::EndOfFile => {
                    self.finish(true)?;
                    break;
                }
                Line::IncompleteTail => {
                    self.prefix_hasher = hasher_before;
                    self.reader.seek(SeekFrom::Start(start))?;
                    self.finish(false)?;
                    break;
                }
                Line::Oversized { end } => {
                    let rejection = Rejection {
                        raw_ordinal: ordinal,
                        reason: format!(
                            "{}:{} exceeds the {} byte JSONL record limit",
                            self.path.display(),
                            ordinal.saturating_add(1),
                            MAX_PROVIDER_JSONL_LINE_BYTES
                        ),
                    };
                    let bytes = rejection_wire_bytes(&rejection);
                    if physical_records != 0
                        && serialized_bytes.saturating_add(bytes) > PAGE_MAX_BYTES
                    {
                        self.prefix_hasher = hasher_before;
                        self.reader.seek(SeekFrom::Start(start))?;
                        break;
                    }
                    self.complete_prefix_end = end;
                    self.next_raw_ordinal = self.next_raw_ordinal.saturating_add(1);
                    self.rejected_records = self.rejected_records.saturating_add(1);
                    physical_records = physical_records.saturating_add(1);
                    logical_units = logical_units.saturating_add(1);
                    serialized_bytes = serialized_bytes.saturating_add(bytes);
                    rejections.push(rejection);
                    continue;
                }
                Line::Complete { bytes, end } => (bytes, end),
            };

            let projected = self.project_line(&bytes, ordinal, start, end)?;
            if projected.logical_units > PAGE_MAX_RECORDS
                || projected.serialized_bytes > PAGE_MAX_BYTES
            {
                self.prefix_hasher = hasher_before;
                self.reader.seek(SeekFrom::Start(start))?;
                return Err(CaptureError::InvalidPayload(format!(
                    "{}:{} expands past the OpenClaw NativePath page boundary",
                    self.path.display(),
                    ordinal.saturating_add(1)
                )));
            }
            if physical_records != 0
                && (logical_units.saturating_add(projected.logical_units) > PAGE_MAX_RECORDS
                    || serialized_bytes.saturating_add(projected.serialized_bytes) > PAGE_MAX_BYTES)
            {
                self.prefix_hasher = hasher_before;
                self.reader.seek(SeekFrom::Start(start))?;
                break;
            }

            self.complete_prefix_end = end;
            self.next_raw_ordinal = self.next_raw_ordinal.saturating_add(1);
            self.accepted_events = self
                .accepted_events
                .saturating_add(projected.events.len() as u64);
            self.accepted_file_touches = self
                .accepted_file_touches
                .saturating_add(projected.touches.len() as u64);
            self.rejected_records = self
                .rejected_records
                .saturating_add(projected.rejections.len() as u64);
            physical_records = physical_records.saturating_add(1);
            logical_units = logical_units.saturating_add(projected.logical_units.max(1));
            serialized_bytes = serialized_bytes.saturating_add(projected.serialized_bytes);
            events.extend(projected.events);
            touches.extend(projected.touches);
            outputs.extend(projected.outputs);
            rejections.extend(projected.rejections);
        }

        if physical_records == 0 {
            return Ok(None);
        }
        let terminal = self.finished
            && self
                .outcome
                .as_ref()
                .is_some_and(|outcome| outcome.checkpoint.terminal);
        Ok(Some(Page {
            expected_checkpoint,
            next_checkpoint: self.checkpoint(terminal),
            source_change: self.source_change,
            session: self.session.clone(),
            events,
            touches,
            outputs,
            rejections,
            logical_units: logical_units.max(1),
            conservative_serialized_bytes: serialized_bytes,
            terminal,
        }))
    }

    fn project_line(
        &mut self,
        bytes: &[u8],
        ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
    ) -> Result<ProjectedLine> {
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(ProjectedLine::default());
        }
        let value = match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => value,
            Err(error) => {
                return Ok(ProjectedLine::rejection(Rejection {
                    raw_ordinal: ordinal,
                    reason: format!(
                        "{}:{} malformed OpenClaw JSONL: {error}",
                        self.path.display(),
                        ordinal.saturating_add(1)
                    ),
                }));
            }
        };
        let line_number = usize::try_from(ordinal)
            .ok()
            .and_then(|ordinal| ordinal.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw NativePath line number exceeds platform limits",
            ))?;
        if value.get("type").and_then(Value::as_str) == Some("session") {
            self.update_header(&value, byte_start, byte_end_exclusive, bytes);
            let mut projected = ProjectedLine::default();
            projected.logical_units = 1;
            projected.serialized_bytes = session_wire_bytes(&self.session);
            return Ok(projected);
        }

        let occurred_at =
            provider_timestamp_value(value.get("timestamp"), self.session.cursor.started_at);
        let mut touches = Vec::new();
        visit_all_file_touch_drafts(&value, |draft| {
            touches.push(CoreTouch {
                raw_ordinal: ordinal,
                event_ordinal: None,
                path: draft.path,
                old_path: draft.old_path,
                change_kind: draft.change_kind,
                occurred_at,
            });
            Ok::<(), CaptureError>(())
        })?;

        if let Some(output_metadata) =
            openclaw_output_metadata(&value, line_number, self.session.cursor.cwd.as_deref())
        {
            let content = complete_content::result_content(&value).unwrap_or_default();
            let retained_failure = matches!(
                output_metadata.outcome.outcome,
                OutputOutcome::Failure | OutputOutcome::Timeout
            );
            let mut projected = ProjectedLine {
                touches,
                ..ProjectedLine::default()
            };
            if self.collect_outputs {
                projected.outputs.push(OutputFact {
                    raw_ordinal: ordinal,
                    byte_start,
                    byte_end_exclusive,
                    occurred_at,
                    kind: output_metadata.kind,
                    native_record_id: output_metadata.native_record_id.clone(),
                    call_id: output_metadata.call_id.clone(),
                    command: output_metadata.command.clone(),
                    outcome: output_metadata.outcome.clone(),
                    content: content.as_bytes().to_vec(),
                });
            }
            if retained_failure {
                let mut event =
                    normalization::event_fact(ordinal, line_number, &value, occurred_at);
                if output_metadata.kind == OutputObservationKind::Command {
                    event.event_type = EventType::CommandOutput;
                }
                let (preview, _) = provider_local_preview(&content, FAILURE_PREVIEW_CHARS);
                event.payload["result_outcome"] = Value::String("failure".to_owned());
                event.payload["output_bytes"] = json!(content.len());
                event.payload["output_preview"] = Value::String(preview);
                event.payload["call_id"] = output_metadata
                    .call_id
                    .as_ref()
                    .map_or(Value::Null, |value| Value::String(value.clone()));
                event.payload["exit_code"] = output_metadata
                    .outcome
                    .exit_code
                    .map_or(Value::Null, |value| Value::from(i64::from(value)));
                event.payload["duration_ms"] = output_metadata
                    .outcome
                    .duration_ms
                    .map_or(Value::Null, Value::from);
                event.payload["timed_out"] =
                    Value::Bool(output_metadata.outcome.outcome == OutputOutcome::Timeout);
                if let Some(command) = &output_metadata.command {
                    event.payload["tool"] = Value::String(command.tool_name.clone());
                    event.payload["command"] = Value::String(command.command.clone());
                    event.payload["cwd"] = command
                        .working_directory
                        .as_ref()
                        .map_or(Value::Null, |value| Value::String(value.clone()));
                }
                let event_type = event.event_type;
                complete_content::attach_native_path_locators(
                    event_type,
                    &mut event.payload,
                    &mut event.metadata,
                    &value,
                    line_number,
                    bytes,
                    byte_start,
                    byte_end_exclusive,
                    &self.source_revision,
                    &self.path_identity,
                    Some(&content),
                )?;
                projected.events.push(core_event(ordinal, event));
                for touch in &mut projected.touches {
                    touch.event_ordinal = Some(ordinal);
                }
            }
            projected.recompute();
            return Ok(projected);
        }

        let mut event = normalization::event_fact(ordinal, line_number, &value, occurred_at);
        let event_type = event.event_type;
        complete_content::attach_native_path_locators(
            event_type,
            &mut event.payload,
            &mut event.metadata,
            &value,
            line_number,
            bytes,
            byte_start,
            byte_end_exclusive,
            &self.source_revision,
            &self.path_identity,
            None,
        )?;
        for touch in &mut touches {
            touch.event_ordinal = Some(ordinal);
        }
        let mut projected = ProjectedLine {
            events: vec![core_event(ordinal, event)],
            touches,
            ..ProjectedLine::default()
        };
        projected.recompute();
        Ok(projected)
    }

    fn update_header(&mut self, value: &Value, start: u64, end: u64, bytes: &[u8]) {
        if let Some(id) = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        {
            self.session.cursor.provider_session_id =
                qualify_session_id(self.session.cursor.agent_id.as_deref(), id);
        }
        self.session.cursor.started_at =
            provider_timestamp_value(value.get("timestamp"), self.imported_at);
        self.session.cursor.cwd = value.get("cwd").and_then(Value::as_str).map(capped_text);
        self.session.cursor.header_anchor = Some(HeaderAnchor {
            start,
            end,
            digest: header_digest(bytes),
        });
        self.session.header = provider_capped_json(value, PROVIDER_MAX_PREVIEW_CHARS);
    }

    fn checkpoint(&self, terminal: bool) -> Checkpoint {
        Checkpoint {
            version: CURSOR_VERSION,
            parser_revision: PARSER_REVISION,
            policy_revision: POLICY_REVISION,
            generation: self.generation,
            source_path: self.path.clone(),
            source_observation: SourceObservation::from_live(&self.observation),
            route_source_revision: self.source_revision.clone(),
            complete_prefix_end: self.complete_prefix_end,
            complete_prefix_sha256: prefix_digest(&self.prefix_hasher),
            next_raw_ordinal: self.next_raw_ordinal,
            accepted_events: self.accepted_events,
            accepted_file_touches: self.accepted_file_touches,
            rejected_records: self.rejected_records,
            session: self.session.cursor.clone(),
            terminal,
        }
    }

    fn finish(&mut self, terminal: bool) -> Result<()> {
        if !self.observation.revalidate(&self.path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        self.outcome = Some(ScanOutcome {
            checkpoint: self.checkpoint(terminal),
            source_change: self.source_change,
            accepted_events: self.accepted_events,
            rejected_records: self.rejected_records,
        });
        self.finished = true;
        Ok(())
    }
}

#[derive(Default)]
struct ProjectedLine {
    events: Vec<CoreEvent>,
    touches: Vec<CoreTouch>,
    outputs: Vec<OutputFact>,
    rejections: Vec<Rejection>,
    logical_units: usize,
    serialized_bytes: usize,
}

impl ProjectedLine {
    fn rejection(rejection: Rejection) -> Self {
        Self {
            serialized_bytes: rejection_wire_bytes(&rejection),
            logical_units: 1,
            rejections: vec![rejection],
            ..Self::default()
        }
    }

    fn recompute(&mut self) {
        self.logical_units = self
            .events
            .len()
            .saturating_add(self.touches.len())
            .saturating_add(self.outputs.len())
            .saturating_add(self.rejections.len())
            .max(1);
        self.serialized_bytes = self
            .events
            .iter()
            .map(event_wire_bytes)
            .chain(self.touches.iter().map(touch_wire_bytes))
            .chain(self.outputs.iter().map(output_wire_bytes))
            .chain(self.rejections.iter().map(rejection_wire_bytes))
            .fold(0_usize, usize::saturating_add);
    }
}

fn core_event(raw_ordinal: u64, event: normalization::OpenClawEventFact) -> CoreEvent {
    let provider_event_hash = event
        .provider_event_hash
        .clone()
        .unwrap_or_else(|| format!("line-{}", raw_ordinal.saturating_add(1)));
    CoreEvent {
        raw_ordinal,
        provider_event_index: event.provider_event_index,
        provider_event_sequence_index: event.provider_event_index,
        provider_event_hash,
        cursor: event.cursor,
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        payload: event.payload,
        metadata: event.metadata,
    }
}

fn fresh_session(path: &Path, imported_at: DateTime<Utc>, index: &Value) -> SessionFact {
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("openclaw-session");
    let agent_id = openclaw_agent_id(path).map(|value| capped_text(&value));
    let provider_session_id = qualify_session_id(agent_id.as_deref(), fallback_id);
    let parent_provider_session_id = related_session_id(
        index,
        agent_id.as_deref(),
        &["parentSessionId", "parent_session_id"],
    );
    let root_provider_session_id = related_session_id(
        index,
        agent_id.as_deref(),
        &["rootSessionId", "root_session_id"],
    )
    .or_else(|| parent_provider_session_id.clone());
    SessionFact {
        cursor: SessionCursor {
            provider_session_id,
            agent_id,
            parent_provider_session_id,
            root_provider_session_id,
            started_at: imported_at,
            cwd: None,
            header_anchor: None,
        },
        index: index.clone(),
        header: Value::Null,
    }
}

fn resume_session(
    checkpoint: &Checkpoint,
    observation: &OpenClawSessionObservation,
) -> Result<SessionFact> {
    let header = bootstrap_header(
        &checkpoint.source_path,
        checkpoint.session.header_anchor,
        observation,
    )?;
    Ok(SessionFact {
        cursor: checkpoint.session.clone(),
        index: observation.index.clone(),
        header,
    })
}

fn bootstrap_header(
    path: &Path,
    anchor: Option<HeaderAnchor>,
    observation: &OpenClawSessionObservation,
) -> Result<Value> {
    let Some(anchor) = anchor else {
        return Ok(Value::Null);
    };
    let length = anchor
        .end
        .checked_sub(anchor.start)
        .ok_or(CaptureError::SystemInvariant(
            "OpenClaw checkpoint header range is invalid",
        ))?;
    let maximum = u64::try_from(MAX_PROVIDER_JSONL_LINE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(2);
    if length > maximum || anchor.end > observation.transcript.length {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let length = usize::try_from(length).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenClaw checkpoint header range exceeds platform limits".to_owned(),
        )
    })?;
    let mut file = File::open(path)?;
    if OpenClawFrozenFileMetadata::from_metadata(&file.metadata()?)? != observation.transcript {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    file.seek(SeekFrom::Start(anchor.start))?;
    let mut bytes = vec![0_u8; length];
    file.read_exact(&mut bytes)?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if header_digest(&bytes) != anchor.digest {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let header: Value = serde_json::from_slice(&bytes)?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(provider_capped_json(&header, PROVIDER_MAX_PREVIEW_CHARS))
}

fn related_session_id(index: &Value, agent_id: Option<&str>, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| index.get(*field).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(|value| qualify_session_id(agent_id, value))
}

fn qualify_session_id(agent_id: Option<&str>, session_id: &str) -> String {
    let session_id = capped_text(session_id);
    match agent_id {
        Some(agent_id) if !session_id.contains('/') => format!("{agent_id}/{session_id}"),
        _ => session_id,
    }
}

fn capped_text(value: &str) -> String {
    provider_local_preview(value, crate::PROVIDER_MAX_TEXT_CHARS).0
}

fn header_digest(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-openclaw-header-anchor-sha256-v1\0");
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    hasher.finalize().into()
}

enum Line {
    EndOfFile,
    IncompleteTail,
    Oversized { end: u64 },
    Complete { bytes: Vec<u8>, end: u64 },
}

fn read_bounded_line(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    frozen_length: u64,
    start: u64,
) -> Result<Line> {
    if start >= frozen_length {
        return Ok(Line::EndOfFile);
    }
    let mut bytes = Vec::new();
    let mut total = 0_u64;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if total == 0 {
                Line::EndOfFile
            } else {
                Line::IncompleteTail
            });
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
        hasher.update(chunk);
        total = total.saturating_add(chunk.len() as u64);
        if !oversized {
            if bytes.len().saturating_add(chunk.len())
                > MAX_PROVIDER_JSONL_LINE_BYTES.saturating_add(2)
            {
                oversized = true;
                bytes.clear();
            } else {
                bytes.extend_from_slice(chunk);
            }
        }
        let complete = chunk.last() == Some(&b'\n');
        reader.consume(take);
        if complete {
            let end = start.saturating_add(total);
            if oversized {
                return Ok(Line::Oversized { end });
            }
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return Ok(Line::Complete { bytes, end });
        }
    }
}

fn same_file_identity(
    previous: &FrozenMetadata,
    current: &OpenClawFrozenFileMetadata,
) -> Result<bool> {
    let previous = previous.to_live()?;
    Ok(
        match (
            previous.device,
            previous.inode,
            current.device,
            current.inode,
        ) {
            (Some(previous_device), Some(previous_inode), Some(device), Some(inode)) => {
                previous_device == device && previous_inode == inode
            }
            _ => previous.modified == current.modified && previous.readonly == current.readonly,
        },
    )
}

fn new_prefix_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(PREFIX_HASH_DOMAIN);
    hasher
}

fn hash_prefix(file: &mut File, length: u64, mut hasher: Sha256) -> Result<Sha256> {
    file.seek(SeekFrom::Start(0))?;
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).map_err(|_| {
            CaptureError::SystemInvariant("OpenClaw prefix read length exceeds usize")
        })?;
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(hasher)
}

fn prefix_sha256(path: &Path, length: u64) -> Result<[u8; 32]> {
    let mut file = File::open(path)?;
    Ok(prefix_digest(&hash_prefix(
        &mut file,
        length,
        new_prefix_hasher(),
    )?))
}

fn prefix_digest(hasher: &Sha256) -> [u8; 32] {
    hasher.clone().finalize().into()
}

fn session_wire_bytes(session: &SessionFact) -> usize {
    512_usize
        .saturating_add(session.cursor.provider_session_id.len())
        .saturating_add(session.cursor.agent_id.as_deref().map_or(0, str::len))
        .saturating_add(
            session
                .cursor
                .parent_provider_session_id
                .as_deref()
                .map_or(0, str::len),
        )
        .saturating_add(
            session
                .cursor
                .root_provider_session_id
                .as_deref()
                .map_or(0, str::len),
        )
        .saturating_add(session.cursor.cwd.as_deref().map_or(0, str::len))
        .saturating_add(serde_json::to_vec(&session.index).map_or(usize::MAX, |v| v.len()))
        .saturating_add(serde_json::to_vec(&session.header).map_or(usize::MAX, |v| v.len()))
}

fn event_wire_bytes(event: &CoreEvent) -> usize {
    EVENT_ENVELOPE_BYTES
        .saturating_add(event.provider_event_hash.len())
        .saturating_add(event.cursor.len())
        .saturating_add(serde_json::to_vec(&event.payload).map_or(usize::MAX, |v| v.len()))
        .saturating_add(serde_json::to_vec(&event.metadata).map_or(usize::MAX, |v| v.len()))
}

fn touch_wire_bytes(touch: &CoreTouch) -> usize {
    256_usize
        .saturating_add(touch.path.len())
        .saturating_add(touch.old_path.as_deref().map_or(0, str::len))
}

fn output_wire_bytes(output: &OutputFact) -> usize {
    512_usize
        .saturating_add(output.native_record_id.len())
        .saturating_add(output.call_id.as_deref().map_or(0, str::len))
        .saturating_add(output.command.as_ref().map_or(0, |command| {
            command
                .tool_name
                .len()
                .saturating_add(command.command.len())
                .saturating_add(command.working_directory.as_deref().map_or(0, str::len))
        }))
        .saturating_add(output.content.len())
}

fn rejection_wire_bytes(rejection: &Rejection) -> usize {
    128_usize.saturating_add(rejection.reason.len())
}

pub(crate) fn import_openclaw_nativepath_tree(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let source_root = context
        .source_root
        .clone()
        .or(context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let inventory = discover_inventory(path)?;
    let known_routes = known_routes(store, &context.machine_id, &source_root)?;
    let sink = options.import_profile.sink().cloned();

    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            &inventory.paths,
            &source_root,
            context.imported_at,
            sink.as_deref(),
        );
        return Ok(ProviderImportSummary::default());
    }

    if inventory.paths.is_empty() {
        if known_routes.is_empty() {
            if has_source_history(store, &context.machine_id, &source_root)? {
                return Ok(ProviderImportSummary::default());
            }
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "no OpenClaw session JSONL files found",
            });
        }
        return retire_missing_routes(
            store,
            &context.machine_id,
            context.imported_at,
            &known_routes,
            &inventory.paths,
            if inventory.root_missing {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
        );
    }

    let mut summary = import_core(store, &inventory.paths, &source_root, &context, &options)?;
    if summary.work_remaining {
        return Ok(summary);
    }
    summary.merge_from(retire_missing_routes(
        store,
        &context.machine_id,
        context.imported_at,
        &known_routes,
        &inventory.paths,
        ProviderSourceRouteRetirementReason::SourceMissing,
    )?);
    replay_outputs_or_mark_behind(
        &inventory.paths,
        &source_root,
        context.imported_at,
        sink.as_deref(),
    );
    Ok(summary)
}

fn has_source_history(store: &Store, machine_id: &str, source_root: &Path) -> Result<bool> {
    let source_root = source_root.display().to_string();
    Ok(store.list_capture_sources()?.into_iter().any(|source| {
        source.descriptor.provider == CaptureProvider::OpenClaw
            && source.descriptor.machine_id == machine_id
            && source.descriptor.source_format.as_deref() == Some(OPENCLAW_SOURCE_FORMAT)
            && source.descriptor.source_root.as_deref() == Some(source_root.as_str())
    }))
}

struct Inventory {
    paths: BTreeSet<PathBuf>,
    root_missing: bool,
}

fn discover_inventory(root: &Path) -> Result<Inventory> {
    match fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Inventory {
                paths: BTreeSet::new(),
                root_missing: true,
            });
        }
        Err(error) => return Err(error.into()),
    }
    let restrict_to_sessions = root.is_dir();
    let mut paths = BTreeSet::new();
    crate::provider::providers::native_jsonl::visit_native_jsonl_files(
        root,
        CaptureProvider::OpenClaw,
        &mut |candidate| {
            if restrict_to_sessions && !path_has_component(candidate, "sessions") {
                return Ok(());
            }
            paths.insert(fs::canonicalize(candidate)?);
            Ok(())
        },
    )?;
    Ok(Inventory {
        paths,
        root_missing: false,
    })
}

fn path_has_component(path: &Path, expected: &str) -> bool {
    path.components()
        .any(|component| component.as_os_str() == expected)
}

fn import_core(
    store: &mut Store,
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let publication_context = PublicationContext {
        machine_id: &context.machine_id,
        source_root,
        imported_at: context.imported_at,
        history_record_id: options.history_record_id,
        inventory_observation_token: options.inventory_observation_token.as_deref(),
    };
    let mut accumulator = GroupAccumulator::new(
        store,
        &committed_store,
        &bulk_guard,
        publication_context,
        options.capture_work_limit,
    );
    let operation = (|| {
        for path in paths {
            if accumulator.stopped() {
                break;
            }
            if let Some(token) = options.inventory_observation_token.as_deref() {
                if crate::observe_ordinary_file(path)?.token_hex() != token {
                    return Err(CaptureError::SourceChangedDuringCapture);
                }
            }
            let observation = OpenClawSessionObservation::read(path)?;
            let locator = provider_path_identity(&observation.canonical_path)?;
            let stream = provider_source_cursor_stream_for_path(
                CaptureProvider::OpenClaw,
                OPENCLAW_SOURCE_FORMAT,
                &locator,
            );
            let stored = accumulator
                .store()
                .get_sync_cursor(None, &context.machine_id, &stream)?;
            let reactivate_retired_route = stored
                .as_ref()
                .is_some_and(|cursor| cursor_was_retired(&cursor.cursor));
            let previous = stored
                .as_ref()
                .map(|cursor| decode_cursor(&cursor.cursor, path, &observation))
                .transpose()?
                .and_then(|decoded| match decoded {
                    CursorDecode::Native(checkpoint) | CursorDecode::Migrated(checkpoint) => {
                        Some(checkpoint)
                    }
                    CursorDecode::Reset => None,
                });
            let had_previous = previous.is_some();
            let mut reader = open_pages(
                path,
                context.imported_at,
                false,
                options.inventory_observation_token.as_deref(),
                reactivate_retired_route,
                previous.as_ref(),
            )?;
            let mut emitted = false;
            while let Some(page) = reader.next_page()? {
                emitted = true;
                accumulator.push(PendingPage {
                    path: path.clone(),
                    page,
                })?;
                if accumulator.stopped() {
                    break;
                }
            }
            if !accumulator.stopped() && !emitted {
                if let Some(outcome) = reader.outcome.as_ref() {
                    if !had_previous
                        && outcome.source_change == SourceChange::Fresh
                        && observation.transcript.length == 0
                    {
                        continue;
                    } else if outcome.source_change == SourceChange::Unchanged {
                        accumulator.record_unchanged(outcome);
                    } else {
                        accumulator.push(PendingPage {
                            path: path.clone(),
                            page: observation_page(
                                outcome.checkpoint.clone(),
                                reader.session.clone(),
                                outcome.source_change,
                            ),
                        })?;
                    }
                }
            }
        }
        accumulator.finish()
    })();
    let stopped = accumulator.stopped();
    drop(accumulator);
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(mut summary), Ok(())) => {
            if stopped {
                summary.work_remaining = true;
            }
            Ok(summary)
        }
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn observation_page(
    checkpoint: Checkpoint,
    session: SessionFact,
    source_change: SourceChange,
) -> Page {
    Page {
        expected_checkpoint: checkpoint.clone(),
        next_checkpoint: checkpoint,
        source_change,
        session,
        events: Vec::new(),
        touches: Vec::new(),
        outputs: Vec::new(),
        rejections: Vec::new(),
        logical_units: 1,
        conservative_serialized_bytes: PAGE_ENVELOPE_BYTES,
        terminal: true,
    }
}

struct PublicationContext<'a> {
    machine_id: &'a str,
    source_root: &'a Path,
    imported_at: DateTime<Utc>,
    history_record_id: Option<Uuid>,
    inventory_observation_token: Option<&'a str>,
}

struct PendingPage {
    path: PathBuf,
    page: Page,
}

struct GroupAccumulator<'a> {
    store: &'a mut Store,
    committed_store: &'a Store,
    bulk_guard: &'a EventSearchBulkGuard,
    context: PublicationContext<'a>,
    work_limit: CaptureWorkLimit,
    pages: Vec<PendingPage>,
    bytes: usize,
    estimated_mutations: usize,
    sources: BTreeSet<PathBuf>,
    summary: ProviderImportSummary,
    published_groups: usize,
    stopped: bool,
}

impl<'a> GroupAccumulator<'a> {
    fn new(
        store: &'a mut Store,
        committed_store: &'a Store,
        bulk_guard: &'a EventSearchBulkGuard,
        context: PublicationContext<'a>,
        work_limit: CaptureWorkLimit,
    ) -> Self {
        Self {
            store,
            committed_store,
            bulk_guard,
            context,
            work_limit,
            pages: Vec::new(),
            bytes: 0,
            estimated_mutations: 0,
            sources: BTreeSet::new(),
            summary: ProviderImportSummary::default(),
            published_groups: 0,
            stopped: false,
        }
    }

    fn store(&self) -> &Store {
        self.store
    }

    fn stopped(&self) -> bool {
        self.stopped
    }

    fn record_unchanged(&mut self, outcome: &ScanOutcome) {
        self.summary.skipped_sessions = self.summary.skipped_sessions.saturating_add(1);
        self.summary.skipped_events = self
            .summary
            .skipped_events
            .saturating_add(usize::try_from(outcome.accepted_events).unwrap_or(usize::MAX));
        self.summary.failed = self
            .summary
            .failed
            .saturating_add(usize::try_from(outcome.rejected_records).unwrap_or(usize::MAX));
        self.summary.skipped = self
            .summary
            .skipped
            .saturating_add(1)
            .saturating_add(usize::try_from(outcome.accepted_events).unwrap_or(usize::MAX));
        self.summary.accepted_content_records = self
            .summary
            .accepted_content_records
            .saturating_add(usize::try_from(outcome.accepted_events).unwrap_or(usize::MAX));
    }

    fn push(&mut self, pending: PendingPage) -> Result<()> {
        let next_sources = self
            .sources
            .len()
            .saturating_add(usize::from(!self.sources.contains(&pending.path)));
        let next_bytes = self
            .bytes
            .saturating_add(pending.page.conservative_serialized_bytes);
        let page_mutations = pending
            .page
            .events
            .len()
            .saturating_add(pending.page.touches.len())
            .saturating_add(8);
        let next_mutations = self.estimated_mutations.saturating_add(page_mutations);
        if !self.pages.is_empty()
            && (self.pages.len() >= GROUP_MAX_PAGES
                || next_sources > GROUP_MAX_SOURCES
                || next_bytes > GROUP_MAX_BYTES
                || next_mutations > GROUP_MAX_ESTIMATED_MUTATIONS)
        {
            self.flush()?;
            if self.stopped {
                return Ok(());
            }
        }
        self.bytes = self
            .bytes
            .saturating_add(pending.page.conservative_serialized_bytes);
        self.estimated_mutations = self.estimated_mutations.saturating_add(page_mutations);
        self.sources.insert(pending.path.clone());
        self.pages.push(pending);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.pages.is_empty() {
            return Ok(());
        }
        let pages = std::mem::take(&mut self.pages);
        let summary = publish_group(
            self.store,
            self.committed_store,
            self.bulk_guard,
            &self.context,
            &pages,
        )?;
        self.summary.merge_from(summary);
        self.bytes = 0;
        self.estimated_mutations = 0;
        self.sources.clear();
        self.published_groups = self.published_groups.saturating_add(1);
        if self.work_limit == CaptureWorkLimit::OneSafeGroup {
            self.stopped = true;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<ProviderImportSummary> {
        if !self.stopped {
            self.flush()?;
        }
        Ok(std::mem::take(&mut self.summary))
    }
}

struct ResolvedSource {
    source_id: Uuid,
    session: Session,
}

fn publish_group(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &PublicationContext<'_>,
    pages: &[PendingPage],
) -> Result<ProviderImportSummary> {
    if pages.is_empty() {
        return Ok(ProviderImportSummary::default());
    }
    let source_paths = pages
        .iter()
        .map(|pending| pending.path.clone())
        .collect::<BTreeSet<_>>();
    for path in &source_paths {
        let expected = &pages
            .iter()
            .rev()
            .find(|pending| &pending.path == path)
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw publication lost its source page",
            ))?
            .page
            .next_checkpoint
            .source_observation;
        let observed = OpenClawSessionObservation::read(path)?;
        if !expected.matches_live(&observed)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }

    let mut transitions = Vec::with_capacity(source_paths.len());
    for path in &source_paths {
        let locator = provider_path_identity(path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::OpenClaw,
            OPENCLAW_SOURCE_FORMAT,
            &locator,
        );
        let stored = store.get_sync_cursor(None, context.machine_id, &stream)?;
        let final_checkpoint = &pages
            .iter()
            .rev()
            .find(|pending| &pending.path == path)
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw publication lost its final checkpoint",
            ))?
            .page
            .next_checkpoint;
        transitions.push(NativePathCursorTransition::new(
            stored.as_ref().map(|cursor| cursor.cursor.clone()),
            provider_sync_cursor(
                context.machine_id,
                stream,
                encode_cursor(final_checkpoint)?,
                context.imported_at,
            ),
        ));
    }
    let publication_id = publication_id(context, pages, &transitions)?;
    let retained_bytes = pages.iter().fold(0_usize, |total, pending| {
        total.saturating_add(pending.page.conservative_serialized_bytes)
    });
    let replacements = source_paths
        .iter()
        .filter_map(|path| {
            let pending = pages
                .iter()
                .find(|pending| &pending.path == path)
                .expect("source path came from pending pages");
            let starts_generation = pending.page.expected_checkpoint.complete_prefix_end == 0
                && pending.page.expected_checkpoint.next_raw_ordinal == 0;
            (starts_generation
                && matches!(
                    pending.page.source_change,
                    SourceChange::Rewrite | SourceChange::Truncation | SourceChange::Replacement
                ))
            .then(|| {
                current_route_for_path(
                    committed_store,
                    context.machine_id,
                    context.source_root,
                    path,
                )
            })
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let accounting =
        NativePathGroupAccounting::new(pages.len(), source_paths.len(), retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, &transitions)? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.skipped_events = pages.iter().map(|pending| pending.page.events.len()).sum();
            summary.skipped = summary.skipped_events;
            for rejection in pages
                .iter()
                .flat_map(|pending| pending.page.rejections.iter())
            {
                summary.record_failure(ProviderImportFailure {
                    line: line_number(rejection.raw_ordinal),
                    error: rejection.reason.clone(),
                });
            }
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }
    for route in &replacements {
        let disposition = group.retire_provider_source_route(&route_retirement(
            context.imported_at,
            route,
            ProviderSourceRouteRetirementReason::Replaced,
        ))?;
        if disposition != ProviderSourceRouteRetirementDisposition::Retired {
            return Err(CaptureError::SystemInvariant(
                "OpenClaw replacement route was already retired before publication",
            ));
        }
    }

    let mut summary = ProviderImportSummary::default();
    let mut resolved = BTreeMap::<PathBuf, ResolvedSource>::new();
    for path in &source_paths {
        let pending = pages
            .iter()
            .rev()
            .find(|pending| &pending.path == path)
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw publication lost its source facts",
            ))?;
        let live = OpenClawSessionObservation::read(path)?;
        let source_revision = source_revision(&live, context.inventory_observation_token);
        let raw_source_path = path.display().to_string();
        let source_root = context.source_root.display().to_string();
        let path_identity = provider_path_identity(path)?;
        let root_source_identity = provider_source_identity(
            CaptureProvider::OpenClaw,
            OPENCLAW_SOURCE_FORMAT,
            Some(&source_root),
            Some(&raw_source_path),
            None,
            &Value::Null,
        )
        .ok_or(CaptureError::SystemInvariant(
            "OpenClaw source has no canonical identity",
        ))?;
        let generation = pending.page.next_checkpoint.generation;
        let proposed_source_identity =
            generation_source_identity(&root_source_identity, generation);
        let locator_identity = source_locator_identity(&path_identity, generation);
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::OpenClaw,
            OPENCLAW_SOURCE_FORMAT,
            &path_identity,
        );
        let resolution =
            group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                provider: CaptureProvider::OpenClaw,
                source_format: OPENCLAW_SOURCE_FORMAT.to_owned(),
                machine_id: context.machine_id.to_owned(),
                locator_identity,
                cursor_stream: stream,
                proposed_source_identity,
                raw_source_path: Some(raw_source_path.clone()),
                source_revision: source_revision.clone(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            })?;
        let source_id = committed_store
            .capture_source_by_canonical_identity_session(
                CaptureProvider::OpenClaw,
                OPENCLAW_SOURCE_FORMAT,
                context.machine_id,
                &resolution.canonical_source_identity,
                &pending.page.session.cursor.provider_session_id,
            )?
            .map(|source| source.id)
            .unwrap_or_else(|| {
                native_source_id(
                    &resolution.canonical_source_identity,
                    &pending.page.session.cursor.provider_session_id,
                )
            });
        let source = capture_source(
            context,
            &pending.page.session,
            generation,
            source_id,
            &raw_source_path,
            &source_root,
            &resolution.canonical_source_identity,
            &source_revision,
        );
        group.upsert_capture_source(&source)?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

        let session = canonical_session(
            committed_store,
            context,
            &pending.page.session,
            source_id,
            &resolution.canonical_source_identity,
            replacements
                .iter()
                .find(|route| route.raw_source_path.as_path() == path.as_path())
                .map(|route| route.capture_source_id),
        )?;
        if let Some(parent_id) = session.parent_session_id {
            if committed_store.get_session(parent_id).is_err() {
                group.upsert_session(&relationship_placeholder(
                    context,
                    source_id,
                    parent_id,
                    pending
                        .page
                        .session
                        .cursor
                        .parent_provider_session_id
                        .as_deref()
                        .unwrap_or("unknown-parent"),
                    &resolution.canonical_source_identity,
                ))?;
            }
        }
        let existed = committed_store.get_session(session.id).is_ok();
        group.upsert_session(&session)?;
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
        if let Some(parent_id) = session.parent_session_id {
            let edge = relationship_edge(
                context,
                source_id,
                &session,
                parent_id,
                &resolution.canonical_source_identity,
            );
            let edge_existed = committed_store.session_edge_exists(edge.id)?;
            group.upsert_projection_neutral_session_edge(&actor(&session), &edge)?;
            if edge_existed {
                summary.skipped_edges = summary.skipped_edges.saturating_add(1);
            } else {
                summary.imported_edges = summary.imported_edges.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
            }
        }
        resolved.insert(path.clone(), ResolvedSource { source_id, session });
    }

    let mut event_ids = BTreeMap::<(PathBuf, u64), Uuid>::new();
    for pending in pages {
        let source = resolved
            .get(&pending.path)
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw publication lost its resolved source",
            ))?;
        for event in &pending.page.events {
            let event_id = publish_event(
                &mut group,
                committed_store,
                context,
                source.source_id,
                &source.session,
                event,
                &mut summary,
            )?;
            event_ids.insert((pending.path.clone(), event.raw_ordinal), event_id);
        }
        let mut touch_subrecords = BTreeMap::<u64, u64>::new();
        for touch in &pending.page.touches {
            let subrecord = touch_subrecords.entry(touch.raw_ordinal).or_default();
            publish_touch(
                &mut group,
                committed_store,
                context,
                source.source_id,
                &source.session,
                touch,
                *subrecord,
                touch
                    .event_ordinal
                    .and_then(|ordinal| event_ids.get(&(pending.path.clone(), ordinal)).copied()),
            )?;
            *subrecord = subrecord.saturating_add(1);
        }
        for rejection in &pending.page.rejections {
            summary.record_failure(ProviderImportFailure {
                line: line_number(rejection.raw_ordinal),
                error: rejection.reason.clone(),
            });
        }
    }

    for path in &source_paths {
        let expected = &pages
            .iter()
            .rev()
            .find(|pending| &pending.path == path)
            .ok_or(CaptureError::SystemInvariant(
                "OpenClaw publication lost its revalidation source",
            ))?
            .page
            .next_checkpoint
            .source_observation;
        let observed = OpenClawSessionObservation::read(path)?;
        if !expected.matches_live(&observed)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn capture_source(
    context: &PublicationContext<'_>,
    session: &SessionFact,
    generation: u64,
    source_id: Uuid,
    raw_source_path: &str,
    source_root: &str,
    source_identity: &str,
    source_revision: &str,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::OpenClaw,
            machine_id: context.machine_id.to_owned(),
            process_id: None,
            cwd: session.cursor.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(OPENCLAW_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: Some(session.cursor.provider_session_id.clone()),
        },
        started_at: session.cursor.started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": session.cursor.provider_session_id,
                "source_format": OPENCLAW_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::OpenClaw,
                    &session.cursor.provider_session_id,
                    OPENCLAW_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "source_metadata": {
                    "adapter": OPENCLAW_SOURCE_FORMAT,
                    "index": provider_capped_json(&session.index, PROVIDER_MAX_PREVIEW_CHARS),
                    "header": provider_capped_json(&session.header, PROVIDER_MAX_PREVIEW_CHARS),
                    "support_level": "beta",
                },
                "nativepath_publication": "openclaw-v1",
                "nativepath_generation": generation,
            }),
        ),
    }
}

fn canonical_session(
    committed_store: &Store,
    context: &PublicationContext<'_>,
    fact: &SessionFact,
    source_id: Uuid,
    source_identity: &str,
    prior_source_id: Option<Uuid>,
) -> Result<Session> {
    let session_id = generation_session_id(
        committed_store,
        prior_source_id,
        &fact.cursor.provider_session_id,
        source_id,
        source_identity,
    )?;
    let parent_session_id = fact
        .cursor
        .parent_provider_session_id
        .as_deref()
        .map(|parent| {
            generation_session_id(
                committed_store,
                prior_source_id,
                parent,
                source_id,
                source_identity,
            )
        })
        .transpose()?;
    let root_session_id = fact
        .cursor
        .root_provider_session_id
        .as_deref()
        .map(|root| {
            generation_session_id(
                committed_store,
                prior_source_id,
                root,
                source_id,
                source_identity,
            )
        })
        .transpose()?
        .or(parent_session_id);
    Ok(Session {
        id: session_id,
        history_record_id: context.history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::OpenClaw,
        external_session_id: Some(fact.cursor.provider_session_id.clone()),
        external_agent_id: fact.cursor.agent_id.clone(),
        agent_type: AgentType::Primary,
        role_hint: Some("personal-agent".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fact.cursor.started_at,
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": fact.cursor.provider_session_id,
                "parent_provider_session_id": fact.cursor.parent_provider_session_id,
                "root_provider_session_id": fact.cursor.root_provider_session_id,
                "source_format": OPENCLAW_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": {
                    "source_format": OPENCLAW_SOURCE_FORMAT,
                    "agent_id": fact.cursor.agent_id,
                    "session_index": provider_capped_json(
                        &fact.index,
                        PROVIDER_MAX_PREVIEW_CHARS,
                    ),
                    "fidelity_gap": "OpenClaw session JSONL is current native storage, but upstream keeps a storage-neutral accessor for future schema changes",
                    "nativepath_publication": "openclaw-v1",
                },
            }),
        ),
    })
}

fn generation_session_id(
    store: &Store,
    prior_source_id: Option<Uuid>,
    provider_session_id: &str,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Uuid> {
    if let Some(existing) = prior_source_id
        .map(|prior_source_id| {
            store.session_by_capture_source_and_external_session(
                prior_source_id,
                CaptureProvider::OpenClaw,
                provider_session_id,
            )
        })
        .transpose()?
        .flatten()
    {
        return Ok(existing.id);
    }
    provider_import_session_uuid(
        store,
        CaptureProvider::OpenClaw,
        provider_session_id,
        source_id,
        Some(source_identity),
    )
}

fn relationship_placeholder(
    context: &PublicationContext<'_>,
    source_id: Uuid,
    id: Uuid,
    external_session_id: &str,
    source_identity: &str,
) -> Session {
    Session {
        id,
        history_record_id: context.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::OpenClaw,
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
                "provider_session_id": external_session_id,
                "source_format": OPENCLAW_SOURCE_FORMAT,
                "source_identity": source_identity,
                "relationship_placeholder": true,
            }),
        ),
    }
}

fn relationship_edge(
    context: &PublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    parent_id: Uuid,
    source_identity: &str,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "provider-source-root:{source_identity}:session:{}:parent_child",
                session.external_session_id.as_deref().unwrap_or_default()
            ),
            "session-edge",
        ),
        from_session_id: session.id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "source_format": OPENCLAW_SOURCE_FORMAT,
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

#[allow(clippy::too_many_arguments)]
fn publish_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &PublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    event: &CoreEvent,
    summary: &mut ProviderImportSummary,
) -> Result<Uuid> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::OpenClaw,
        provider_session_id,
        source_id,
        event.provider_event_index,
        event.provider_event_sequence_index,
        &event.provider_event_hash,
        None,
        Some(event.raw_ordinal),
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::OpenClaw,
                provider_session_id,
            ),
    )?;
    let mut provider_metadata = event.metadata.clone();
    let verified_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        &event.provider_event_hash,
    )
    .unwrap_or(identity.dedupe_key);
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": event.provider_event_hash,
        "provider_event_hash_authority": "provider_supplied",
        "cursor": event.cursor,
        "source_format": OPENCLAW_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": event.raw_ordinal.saturating_add(1),
        "imported_at": context.imported_at,
        "source_record_ordinal": event.raw_ordinal,
        "source_record_subrecord_index": 0,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (sync_metadata.as_object_mut(), verified_locators) {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::OpenClaw.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": event.provider_event_hash,
            "cursor": event.cursor,
            "artifacts": [],
            "body": crate::provider::importer::compact_provider_result_payload(
                event.event_type,
                &event.payload,
            ),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Partial, sync_metadata),
    };
    if group.reconcile_provider_event(&normalized, ProviderEventHashAuthority::ProviderSupplied)? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(normalized.id)
}

#[allow(clippy::too_many_arguments)]
fn publish_touch(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &PublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    touch: &CoreTouch,
    subrecord: u64,
    event_id: Option<Uuid>,
) -> Result<()> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let touch_index = touch
        .raw_ordinal
        .checked_mul(u64::from(u16::MAX) + 1)
        .and_then(|base| base.checked_add(subrecord))
        .ok_or(CaptureError::SystemInvariant(
            "OpenClaw file-touch identity overflowed",
        ))?;
    let id = provider_file_touch_import_id(
        committed_store,
        CaptureProvider::OpenClaw,
        provider_session_id,
        source_id,
        Some(touch.raw_ordinal),
        touch_index,
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::OpenClaw,
                provider_session_id,
            ),
    )?;
    group.upsert_file_touched(&FileTouched {
        id,
        history_record_id: context.history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: touch.path.clone(),
        change_kind: touch.change_kind,
        old_path: touch.old_path.clone(),
        line_count_delta: None,
        confidence: Confidence::Explicit,
        timestamps: timestamps(touch.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::OpenClaw.as_str(),
                "provider_session_id": provider_session_id,
                "provider_touch_index": touch_index,
                "provider_event_index": touch.raw_ordinal,
                "source_format": OPENCLAW_SOURCE_FORMAT,
                "session_id": session.id,
            }),
        ),
    })?;
    Ok(())
}

fn native_source_id(source_identity: &str, provider_session_id: &str) -> Uuid {
    stable_capture_uuid(
        &format!(
            "native-path-provider-source-v1:{}:{}:{}:{}",
            source_identity.len(),
            source_identity,
            provider_session_id.len(),
            provider_session_id,
        ),
        "source",
    )
}

fn generation_source_identity(root_source_identity: &str, generation: u64) -> String {
    format!("{root_source_identity}:openclaw-generation:{generation}")
}

fn source_locator_identity(path_identity: &str, generation: u64) -> String {
    format!("{path_identity}#openclaw-generation:{generation}")
}

fn source_revision(
    observation: &OpenClawSessionObservation,
    inventory_token: Option<&str>,
) -> String {
    let revision = observation.source_revision();
    let Some(token) = inventory_token else {
        return revision;
    };
    let mut digest = Sha256::new();
    digest.update(b"ctx-openclaw-inventory-observation-v1\0");
    digest.update((revision.len() as u64).to_be_bytes());
    digest.update(revision.as_bytes());
    digest.update((token.len() as u64).to_be_bytes());
    digest.update(token.as_bytes());
    format!("inventory-observation-sha256-v1:{:x}", digest.finalize())
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
                CaptureProvider::OpenClaw.as_str(),
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

fn publication_id(
    context: &PublicationContext<'_>,
    pages: &[PendingPage],
    transitions: &[NativePathCursorTransition],
) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(PUBLICATION_DOMAIN);
    digest.update(context.machine_id.as_bytes());
    digest.update(context.source_root.as_os_str().as_encoded_bytes());
    digest.update((pages.len() as u64).to_be_bytes());
    for pending in pages {
        let path = provider_path_identity(&pending.path)?;
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path.as_bytes());
        let expected = serde_json::to_vec(&pending.page.expected_checkpoint)?;
        let next = serde_json::to_vec(&pending.page.next_checkpoint)?;
        digest.update((expected.len() as u64).to_be_bytes());
        digest.update(expected);
        digest.update((next.len() as u64).to_be_bytes());
        digest.update(next);
        digest.update([u8::from(pending.page.terminal)]);
        for event in &pending.page.events {
            digest.update(event.provider_event_index.to_be_bytes());
            digest.update((event.provider_event_hash.len() as u64).to_be_bytes());
            digest.update(event.provider_event_hash.as_bytes());
        }
    }
    for transition in transitions {
        digest.update(transition.key().stream().as_bytes());
        if let Some(expected) = transition.expected_cursor() {
            digest.update((expected.len() as u64).to_be_bytes());
            digest.update(expected.as_bytes());
        }
        digest.update((transition.next().cursor.len() as u64).to_be_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    Ok(format!(
        "openclaw-nativepath-publication-sha256-v1:{:x}",
        digest.finalize()
    ))
}

fn line_number(ordinal: u64) -> usize {
    usize::try_from(ordinal)
        .unwrap_or(usize::MAX)
        .saturating_add(1)
}

#[derive(Clone)]
struct KnownRoute {
    capture_source_id: Uuid,
    raw_source_path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    provider_cursor: String,
}

fn known_routes(store: &Store, machine_id: &str, source_root: &Path) -> Result<Vec<KnownRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::OpenClaw
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(OPENCLAW_SOURCE_FORMAT)
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
        let path_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::OpenClaw,
            OPENCLAW_SOURCE_FORMAT,
            &path_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        if cursor_was_retired(&current_cursor.cursor) {
            continue;
        }
        let provider_cursor = decode_native_path_committed_cursor(&current_cursor.cursor)
            .map(|cursor| cursor.provider_cursor().to_owned())
            .unwrap_or_else(|_| current_cursor.cursor.clone());
        let Some(checkpoint) = native_checkpoint_from_cursor(&provider_cursor) else {
            continue;
        };
        let Some(generation) = source
            .sync
            .metadata
            .get("nativepath_generation")
            .and_then(Value::as_u64)
        else {
            continue;
        };
        if checkpoint.generation != generation
            || source.descriptor.external_session_id.as_deref()
                != Some(checkpoint.session.provider_session_id.as_str())
        {
            continue;
        }
        let locator_identity = source_locator_identity(&path_identity, generation);
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let route = KnownRoute {
            capture_source_id: source.id,
            raw_source_path: path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            current_cursor,
            provider_cursor,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "OpenClaw persisted duplicate current routes for one transcript",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

fn native_checkpoint_from_cursor(provider_cursor: &str) -> Option<Checkpoint> {
    let wire = serde_json::from_str::<CursorWire>(provider_cursor).ok()?;
    (wire.version == CURSOR_VERSION
        && wire.kind == "openclaw-nativepath-jsonl"
        && wire.checkpoint.supported())
    .then_some(wire.checkpoint)
}

fn cursor_was_retired(encoded_store_cursor: &str) -> bool {
    decode_native_path_committed_cursor(encoded_store_cursor).is_ok_and(|cursor| {
        cursor
            .publication_id()
            .starts_with("openclaw-nativepath-retirement-sha256-v1:")
    })
}

fn current_route_for_path(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
    path: &Path,
) -> Result<Option<KnownRoute>> {
    let path_identity = provider_path_identity(path)?;
    let mut matches = known_routes(store, machine_id, source_root)?
        .into_iter()
        .filter(|route| {
            provider_path_identity(&route.raw_source_path)
                .is_ok_and(|identity| identity == path_identity)
        })
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => Err(CaptureError::SystemInvariant(
            "OpenClaw has multiple current source generations for one route",
        )),
    }
}

fn retire_missing_routes(
    store: &mut Store,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    known_routes: &[KnownRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let live_locators = live_paths
        .iter()
        .map(|path| provider_path_identity(path))
        .collect::<Result<BTreeSet<_>>>()?;
    let missing = known_routes
        .iter()
        .filter(|route| {
            provider_path_identity(&route.raw_source_path)
                .map(|identity| !live_locators.contains(&identity))
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(ProviderImportSummary::default());
    }
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        for route in missing {
            if retire_route(store, &bulk_guard, machine_id, retired_at, route, reason)? {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
            }
        }
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

fn retire_route(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    route: &KnownRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let stream = route.current_cursor.stream.clone();
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(
            machine_id,
            stream.clone(),
            route.provider_cursor.clone(),
            retired_at,
        ),
    );
    let retirement = route_retirement(retired_at, route, reason);
    let publication_id = retirement_publication_id(&retirement);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
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
    Ok(changed)
}

fn route_retirement(
    retired_at: DateTime<Utc>,
    route: &KnownRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> ProviderSourceRouteRetirement {
    ProviderSourceRouteRetirement {
        provider: CaptureProvider::OpenClaw,
        source_format: OPENCLAW_SOURCE_FORMAT.to_owned(),
        machine_id: route.current_cursor.device_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.current_cursor.stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: retired_at.timestamp_millis(),
        reason,
    }
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(RETIREMENT_DOMAIN);
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(retirement.retired_at_ms.to_be_bytes());
    digest.update([match retirement.reason {
        ProviderSourceRouteRetirementReason::SourceMissing => 0,
        ProviderSourceRouteRetirementReason::RootMissing => 1,
        ProviderSourceRouteRetirementReason::Replaced => 2,
    }]);
    format!(
        "openclaw-nativepath-retirement-sha256-v1:{:x}",
        digest.finalize()
    )
}

fn replay_outputs_or_mark_behind(
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_outputs(paths, source_root, imported_at, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "openclaw_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_outputs(
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    for path in paths {
        let locator_identity = provider_path_identity(path)?;
        let source = OutputSourceIdentity {
            provider: CaptureProvider::OpenClaw.as_str().to_owned(),
            namespace_id: source_root.display().to_string(),
            source_id: locator_identity.clone(),
        };
        let progress = match sink.observe_source(&source) {
            Ok(progress) => progress,
            Err(error) => {
                sink.mark_behind(error);
                continue;
            }
        };
        replay_source(path, imported_at, sink, source, locator_identity, progress)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn replay_source(
    path: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
    output_source: OutputSourceIdentity,
    locator_identity: String,
    progress: Option<ProOutputProgress>,
) -> Result<()> {
    let progress_cursor = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<Checkpoint>(&cursor.payload).ok())
        .filter(Checkpoint::supported);
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress_cursor.is_some()
    });
    let previous = can_resume.then_some(progress_cursor.as_ref()).flatten();
    let mut reader = open_pages(path, imported_at, true, None, false, previous)?;
    let source_change = reader.source_change;
    let observed_revision = reader.source_revision.clone();
    let mut output_state = OutputState::new(
        output_source,
        progress,
        source_change,
        can_resume,
        sink.materializer_revision(),
    )?;

    while let Some(page) = reader.next_page()? {
        let expected_frontier = safe_frontier(&page.expected_checkpoint)?;
        let next_safe_frontier = safe_frontier(&page.next_checkpoint)?;
        let observations = page
            .outputs
            .into_iter()
            .map(|output| output_observation(&page.session, &locator_identity, output))
            .collect::<Result<Vec<_>>>()?;
        let accounting = NativePageAccounting {
            logical_units: page.logical_units.max(1),
            conservative_serialized_bytes: page.conservative_serialized_bytes,
        };
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_state.source.clone(),
            source_epoch: output_state.source_epoch,
            observed_revision: observed_revision.clone(),
            parser_revision: OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: output_state.disposition,
            expected_prior_source_epoch: output_state.expected_source_epoch,
            expected_prior_frontier: output_state.expected_sink_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::OpenClaw.as_str(), &locator_identity),
            expected_frontier,
            next_safe_frontier.clone(),
            page.terminal,
            accounting,
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if process_pro_replay_only(replay, sink).is_err() {
            break;
        }
        output_state.expected_source_epoch = Some(output_state.source_epoch);
        output_state.expected_sink_frontier = Some(next_safe_frontier);
        output_state.disposition = ProOutputSourceDisposition::AppendOrResume;
    }
    Ok(())
}

struct OutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl OutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        source_change: SourceChange,
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
        let prior_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let rewrite = !can_resume
            || progress.materializer_revision != materializer_revision
            || matches!(
                source_change,
                SourceChange::Fresh
                    | SourceChange::Rewrite
                    | SourceChange::Truncation
                    | SourceChange::Replacement
            );
        Ok(Self {
            source,
            source_epoch: if rewrite {
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "OpenClaw output source epoch exhausted",
                    ))?
            } else {
                progress.source_epoch
            },
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: prior_frontier,
            disposition: if rewrite {
                ProOutputSourceDisposition::Rewrite
            } else {
                ProOutputSourceDisposition::AppendOrResume
            },
        })
    }
}

fn safe_frontier(checkpoint: &Checkpoint) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(OUTPUT_FRONTIER_VERSION, serde_json::to_vec(checkpoint)?)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn output_observation(
    session: &SessionFact,
    locator_identity: &str,
    output: OutputFact,
) -> Result<ProOutputObservation> {
    let locator = output_locator(
        locator_identity,
        output.byte_start,
        output.byte_end_exclusive,
    )?;
    let direct_session_id = session.cursor.provider_session_id.clone();
    Ok(ProOutputObservation {
        kind: output.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: output.native_record_id.clone(),
            native_sequence: output.raw_ordinal,
            native_record_id: Some(output.native_record_id),
            source_record_ordinal: Some(output.raw_ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: Some(output.byte_start),
            byte_end_exclusive: Some(output.byte_end_exclusive),
        },
        occurred_at_unix_ms: Some(output.occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: direct_session_id.clone(),
            root_session_id: session
                .cursor
                .root_provider_session_id
                .clone()
                .unwrap_or_else(|| direct_session_id.clone()),
            parent_session_id: session.cursor.parent_provider_session_id.clone(),
            provider_session_id: Some(direct_session_id),
            agent_id: session.cursor.agent_id.clone(),
            repository: None,
        },
        call_id: output.call_id,
        command: output.command,
        outcome: output.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: "jsonl-source-item-byte-range-v1".to_owned(),
            payload: locator,
        },
        content: output.content,
    })
}

fn output_locator(
    locator_identity: &str,
    byte_start: u64,
    byte_end_exclusive: u64,
) -> Result<Vec<u8>> {
    if byte_start >= byte_end_exclusive {
        return Err(CaptureError::SystemInvariant(
            "OpenClaw output locator range is empty",
        ));
    }
    let identity = locator_identity.as_bytes();
    let length = u32::try_from(identity.len()).map_err(|_| {
        CaptureError::InvalidPayload(
            "OpenClaw output path identity exceeds locator limits".to_owned(),
        )
    })?;
    let mut locator = Vec::with_capacity(4 + identity.len() + 16);
    locator.extend_from_slice(&length.to_be_bytes());
    locator.extend_from_slice(identity);
    locator.extend_from_slice(&byte_start.to_be_bytes());
    locator.extend_from_slice(&byte_end_exclusive.to_be_bytes());
    Ok(locator)
}
